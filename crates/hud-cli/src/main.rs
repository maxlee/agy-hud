#![forbid(unsafe_code)]

use hud_cli::collector::{
    MAX_TASK_MD_BYTES, SystemProbeRunner, collect_render_inputs, read_bounded_string,
    read_stdin_payload, read_transcript_slices,
};
use hud_cli::install::{install_status_line, uninstall_status_line};
use hud_cli::paths::{
    AgyPaths, atomic_write_bytes, build_logic_paths, canonical_or_parent, derive_task_md_path,
    existing_data_dir, prepare_data_dir, undo_config, write_config_with_undo,
};
use hud_core::ansi::terminal_filter;
use hud_core::commands::{CommandContext, execute_subcommand};
use hud_core::config::{MAX_CONFIG_BYTES, parse_config_str};
use hud_core::probes::{parse_memory_info, parse_vcs_info};
use hud_core::render::{RenderInput, render_hud_lines};
use hud_core::types::{AgyStdinData, DoctorFacts, HeartbeatInfo, PluginFacts, RuntimeFacts};
use serde_json::{Value, json};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP_LINES: &[&str] = &[
    "agy-hud — 100% Pure Rust real-time statusLine HUD for Google Antigravity CLI (agy)",
    "",
    "Usage:",
    "  agy-hud                                 (statusLine mode: reads JSON from stdin)",
    "  agy-hud doctor                          Check setup, binary, settings.json, and health",
    "  agy-hud install [--replace] [--stack]   Deploy native binary & configure settings.json",
    "  agy-hud uninstall [--keep-config] [--reset-default]  Cleanly remove HUD & restore official default",
    "  agy-hud preview [options]               Render live previews (--state showcase|idle|tool_use|confirm|high-context, --width N, --set k=v, --last)",
    "  agy-hud schema [query]                  List configurable HUD keys and descriptions",
    "  agy-hud set <k=v...>|--undo             Update or undo HUD configuration",
    "  agy-hud report [--note text]            Generate a privacy-scrubbed diagnostic report",
    "  agy-hud --version                       Print version",
];

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn is_hud_disabled() -> bool {
    let Ok(raw) = std::env::var("AGY_HUD_DISABLE") else {
        return false;
    };
    let trimmed = raw.trim().to_ascii_lowercase();
    !trimmed.is_empty() && !matches!(trimmed.as_str(), "0" | "false" | "off" | "no")
}

fn print_filtered<S: AsRef<str>>(lines: &[S]) {
    if lines.is_empty() {
        return;
    }
    let mut stdout = std::io::stdout().lock();
    for line in lines {
        let safe = terminal_filter(line.as_ref());
        let _ = writeln!(stdout, "{safe}");
    }
    let _ = stdout.flush();
}

fn resolve_plugin_root(current_exe: &Path, paths: &AgyPaths) -> PathBuf {
    let exe_real = canonical_or_parent(current_exe);
    // Walk up to 4 parent directories looking for `plugin.json` (handles `scripts/bin/agy-hud` & `target/release/agy-hud`)
    let mut cursor = exe_real.parent();
    for _ in 0..4 {
        let Some(dir) = cursor else { break };
        if dir.join("plugin.json").is_file() {
            return dir.to_path_buf();
        }
        cursor = dir.parent();
    }

    // Check install-meta.json written when `install` ran from the plugin directory
    if let Some(meta_str) = read_bounded_string(&paths.hud_home.join("install-meta.json"), 8192)
        && let Ok(val) = serde_json::from_str::<Value>(&meta_str)
        && let Some(root_str) = val.get("pluginRoot").and_then(Value::as_str)
    {
        let p = PathBuf::from(root_str);
        if p.exists() {
            return p;
        }
    }

    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn read_plugin_version(plugin_root: &Path) -> String {
    if let Some(raw) = read_bounded_string(&plugin_root.join("plugin.json"), 32 * 1024)
        && let Ok(val) = serde_json::from_str::<Value>(&raw)
        && let Some(ver) = val.get("version").and_then(Value::as_str)
    {
        return ver.to_string();
    }
    VERSION.to_string()
}

fn read_heartbeat(heartbeat_path: &Path) -> Option<HeartbeatInfo> {
    let raw = read_bounded_string(heartbeat_path, 16 * 1024)?;
    serde_json::from_str::<HeartbeatInfo>(&raw).ok()
}

fn write_heartbeat(heartbeat_path: &Path, info: &HeartbeatInfo) {
    if let Ok(bytes) = serde_json::to_vec(info) {
        let _ = atomic_write_bytes(heartbeat_path, &bytes, 0o600);
    }
}

fn run() -> u8 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let paths = AgyPaths::from_process_env();
    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("agy-hud"));
    let sub = argv.first().map(String::as_str);

    if matches!(sub, Some("help" | "--help" | "-h")) {
        print_filtered(HELP_LINES);
        return 0;
    }
    if matches!(sub, Some("version" | "--version" | "-V")) {
        print_filtered(&[format!("agy-hud {VERSION} (rust-native)")]);
        return 0;
    }

    if sub == Some("install") {
        let plugin_root = resolve_plugin_root(&current_exe, &paths);
        match install_status_line(&paths, &current_exe, &plugin_root, &argv[1..]) {
            Ok(report) => {
                let pretty = serde_json::to_string_pretty(&report).unwrap_or_default();
                let lines: Vec<&str> = pretty.lines().collect();
                print_filtered(&lines);
                return 0;
            }
            Err(err) => {
                print_filtered(&[format!("install failed: {err}")]);
                return 1;
            }
        }
    }

    if sub == Some("uninstall") {
        match uninstall_status_line(&paths, &argv[1..]) {
            Ok(report) => {
                let pretty = serde_json::to_string_pretty(&report).unwrap_or_default();
                let lines: Vec<&str> = pretty.lines().collect();
                print_filtered(&lines);
                return 0;
            }
            Err(err) => {
                print_filtered(&[format!("uninstall failed: {err}")]);
                return 1;
            }
        }
    }

    if sub == Some("set") && argv.iter().any(|a| a == "--undo") {
        match undo_config(&paths) {
            Ok(()) => {
                print_filtered(&[format!(
                    "Reverted {} to previous configuration.",
                    paths.config_path.display()
                )]);
                return 0;
            }
            Err(err) => {
                print_filtered(&[err]);
                return 1;
            }
        }
    }

    if let Some(cmd_name) = sub {
        let now_ms = now_epoch_ms();
        let data_real = existing_data_dir(&paths);
        let logic_paths = build_logic_paths(&paths, None, data_real.as_deref());
        let plugin_root = resolve_plugin_root(&current_exe, &paths);

        let facts = DoctorFacts {
            cwd: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .to_string_lossy()
                .into_owned(),
            runtime: RuntimeFacts {
                engine: "rust-native".to_string(),
                version: VERSION.to_string(),
                exec_path: canonical_or_parent(&current_exe)
                    .to_string_lossy()
                    .into_owned(),
                supported: true,
            },
            plugin: PluginFacts {
                root: canonical_or_parent(&plugin_root)
                    .to_string_lossy()
                    .into_owned(),
                version: read_plugin_version(&plugin_root),
            },
            config_path: logic_paths.config.clone(),
            heartbeat: read_heartbeat(&paths.heartbeat_path),
        };

        let config_text = read_bounded_string(&paths.config_path, MAX_CONFIG_BYTES as u64);
        let cli_settings_text = read_bounded_string(&paths.cli_settings_path, 128 * 1024);
        let last_snapshot_text = logic_paths
            .last_snapshot
            .as_deref()
            .and_then(|p| read_bounded_string(Path::new(p), 256 * 1024));

        // If `preview --last` needs transcript/task.md from the stored snapshot, load them safely
        let mut tx_head: Option<String> = None;
        let mut tx_tail: Option<String> = None;
        let mut task_md_text: Option<String> = None;
        if cmd_name == "preview"
            && argv.iter().any(|a| a == "--last")
            && let Some(raw_snap) = last_snapshot_text.as_deref()
            && let Ok(val) = serde_json::from_str::<Value>(raw_snap)
            && let Some(tp_str) = val
                .get("stdin")
                .and_then(|s| s.get("transcript_path"))
                .and_then(Value::as_str)
        {
            let tp = Path::new(tp_str);
            if tp.is_absolute() {
                let (h, t) = read_transcript_slices(tp);
                tx_head = h;
                tx_tail = t;
                if let Some(tm) = derive_task_md_path(tp) {
                    task_md_text = read_bounded_string(&tm, MAX_TASK_MD_BYTES);
                }
            }
        }

        let ctx = CommandContext {
            facts: &facts,
            paths: &logic_paths,
            now_ms,
            config_text: config_text.as_deref(),
            cli_settings_text: cli_settings_text.as_deref(),
            last_snapshot_text: last_snapshot_text.as_deref(),
            transcript_head: tx_head.as_deref(),
            transcript_tail: tx_tail.as_deref(),
            task_md_text: task_md_text.as_deref(),
        };

        let output = execute_subcommand(cmd_name, &argv[1..], &ctx);
        if let Some(ref next_cfg) = output.config_to_write
            && let Err(err) = write_config_with_undo(&paths, next_cfg)
        {
            print_filtered(&[format!("Failed to write config: {err}")]);
            return 1;
        }
        print_filtered(&output.lines);
        return u8::from(output.failed);
    }

    // Live Antigravity CLI `statusLine` render mode
    if is_hud_disabled() {
        return 0;
    }

    let wall_started = Instant::now();
    let started_ms = now_epoch_ms();
    let Some(stdin_payload): Option<AgyStdinData> = read_stdin_payload() else {
        if std::io::stdin().is_terminal() {
            print_filtered(&[
                "agy-hud: pipe Antigravity CLI statusLine JSON on stdin, or run `agy-hud preview --state showcase`.",
            ]);
        }
        return 0;
    };

    let data_real = prepare_data_dir(&paths);
    let logic_paths = build_logic_paths(&paths, Some(&stdin_payload), data_real.as_deref());
    let config_text = read_bounded_string(&paths.config_path, MAX_CONFIG_BYTES as u64);
    let loaded_cfg = parse_config_str(config_text.as_deref());

    let collected = collect_render_inputs(
        &paths,
        &logic_paths,
        &loaded_cfg.config,
        &stdin_payload,
        started_ms,
        &SystemProbeRunner,
    );

    let vcs = parse_vcs_info(Some(&stdin_payload), &collected.probes);
    let memory = parse_memory_info(&collected.probes);
    let columns = stdin_payload.terminal_width.filter(|&w| w > 0);

    let lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin_payload),
        config: &loaded_cfg.config,
        transcript: &collected.transcript,
        vcs: vcs.as_ref(),
        memory: memory.as_ref(),
        columns,
        now_ms: started_ms,
    });

    // Persist last snapshot inside symlink-swept `<hudHome>/data/last-snapshot.json`
    if let Some(ref snap_path) = logic_paths.last_snapshot {
        let snap_json = json!({
            "at": started_ms,
            "stdin": &stdin_payload,
            "probes": &collected.probes,
        });
        if let Ok(bytes) = serde_json::to_vec(&snap_json) {
            let _ = atomic_write_bytes(Path::new(snap_path), &bytes, 0o600);
        }
    }

    let elapsed_ms = wall_started.elapsed().as_millis() as u64;
    write_heartbeat(
        &paths.heartbeat_path,
        &HeartbeatInfo {
            at: started_ms,
            runtime: format!("rust-native/{VERSION}"),
            path: canonical_or_parent(&current_exe)
                .to_string_lossy()
                .into_owned(),
            outcome: "ok".to_string(),
            columns,
            duration_ms: Some(elapsed_ms),
        },
    );

    print_filtered(&lines);
    0
}

fn main() -> ExitCode {
    ExitCode::from(run())
}
