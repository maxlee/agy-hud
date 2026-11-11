#![forbid(unsafe_code)]

use hud_cli::collector::{
    MAX_CURSOR_DELTA_BYTES, MAX_TASK_MD_BYTES, SystemProbeRunner, collect_render_inputs,
    drain_stdin_nested, exec_file_safe, read_bounded_string, read_stdin_payload,
    read_transcript_slices,
};
use hud_cli::install::{install_status_line, uninstall_status_line};
use hud_cli::paths::{
    AgyPaths, atomic_write_bytes, build_logic_paths, canonical_or_parent, existing_data_dir,
    prepare_data_dir, undo_config, with_exclusive_lock_file, write_config_with_undo,
};
use hud_cli::quota_refresh::{maybe_spawn_background_quota_refresh, run_quota_subcommand};
use hud_cli::update::{maybe_spawn_background_update_check, run_update_subcommand};
use hud_core::ansi::terminal_filter_with_repo;
use hud_core::commands::{CommandContext, execute_subcommand};
use hud_core::config::{MAX_CONFIG_BYTES, parse_config_str};
use hud_core::probes::{
    apply_speed_cache, parse_cached_quota_info, parse_cost_ledger_info, parse_memory_info,
    parse_speed_cache_info, parse_update_cache_info, parse_vcs_info, reconcile_cost_ledger,
};
use hud_core::render::{RenderInput, format_config_problem_line, render_hud_lines_full};
use hud_core::types::{
    AgyStdinData, CostLedgerSnapshot, DoctorFacts, HeartbeatInfo, InstalledPluginFacts,
    PluginFacts, RuntimeFacts,
};
use serde_json::{Value, json};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const VERSION_BANNER: &str = concat!("agy-hud ", env!("CARGO_PKG_VERSION"), " (rust-native)");

const HELP_LINES: &[&str] = &[
    "agy-hud — 100% Pure Rust real-time statusLine HUD for Google Antigravity CLI (agy)",
    "",
    "Usage:",
    "  agy-hud                                 (statusLine mode: reads JSON from stdin)",
    "  agy-hud doctor [--pretty]               Check setup, binary, settings.json, and glyph alignment",
    "  agy-hud install [--replace] [--stack]   Deploy native binary & configure settings.json",
    "  agy-hud update [--check] [--force]      Check for updates or upgrade to the latest verified GitHub release",
    "  agy-hud uninstall [--keep-config] [--reset-default]  Cleanly remove HUD & restore official default",
    "  agy-hud preview [options]               Render live previews (--state showcase|idle|tool_use|confirm|high-context, --width N, --set k=v, --last)",
    "  agy-hud schema [query]                  List configurable HUD keys and descriptions",
    "  agy-hud set <k=v...>|--undo             Update or undo HUD configuration",
    "  agy-hud quota refresh                   Manually refresh official `/usage` quota cache",
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

fn is_nested_hud() -> bool {
    std::env::var("AGY_HUD_NESTED").as_deref() == Ok("1")
}

fn detect_platform() -> String {
    if cfg!(windows) {
        "windows".to_string()
    } else if cfg!(target_os = "macos") {
        "macos".to_string()
    } else if cfg!(target_os = "linux") {
        "linux".to_string()
    } else {
        std::env::consts::OS.to_string()
    }
}

fn detect_terminal() -> String {
    if let Ok(tp) = std::env::var("TERM_PROGRAM")
        && !tp.trim().is_empty()
    {
        return tp.trim().to_string();
    }
    if std::env::var_os("WT_SESSION").is_some() {
        return "Windows Terminal".to_string();
    }
    if std::env::var_os("KITTY_WINDOW_ID").is_some() {
        return "kitty".to_string();
    }
    if std::env::var_os("ALACRITTY_WINDOW_ID").is_some() {
        return "Alacritty".to_string();
    }
    if let Ok(term) = std::env::var("TERM")
        && !term.trim().is_empty()
    {
        return term.trim().to_string();
    }
    "unknown".to_string()
}

fn detect_remote_session() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_TTY").is_some()
        || std::env::var_os("SSH_CLIENT").is_some()
}

fn parse_plugin_list_json(raw: &str) -> Option<(String, Option<InstalledPluginFacts>)> {
    let val: Value = serde_json::from_str(raw.trim()).ok()?;
    let arr = val
        .as_array()
        .or_else(|| val.get("plugins").and_then(Value::as_array))?;
    for item in arr {
        let name = item.get("name").and_then(Value::as_str).unwrap_or("");
        if name == "agy-hud" {
            let version = item
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let path = item
                .get("path")
                .or_else(|| item.get("installPath"))
                .or_else(|| item.get("root"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let enabled = item.get("enabled").and_then(Value::as_bool).unwrap_or(true);
            return Some((
                "installed".to_string(),
                Some(InstalledPluginFacts {
                    version,
                    path,
                    enabled,
                }),
            ));
        }
    }
    Some(("not-installed".to_string(), None))
}

fn detect_installed_plugin(paths: &AgyPaths) -> (String, Option<InstalledPluginFacts>) {
    if let Ok(override_json) = std::env::var("AGY_HUD_PLUGIN_LIST_JSON")
        && let Some(res) = parse_plugin_list_json(&override_json)
    {
        return res;
    }

    // Fast filesystem check under `~/.gemini/config/plugins/agy-hud/plugin.json`
    let gemini_cfg_dir = paths.home.join(".gemini").join("config");
    let default_plugin_dir = gemini_cfg_dir.join("plugins").join("agy-hud");
    let manifest = default_plugin_dir.join("plugin.json");
    if let Some(raw) = read_bounded_string(&manifest, 32 * 1024)
        && let Ok(val) = serde_json::from_str::<Value>(&raw)
    {
        let version = val
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let enabled = read_bounded_string(&gemini_cfg_dir.join("config.json"), 64 * 1024)
            .and_then(|cfg_raw| serde_json::from_str::<Value>(&cfg_raw).ok())
            .and_then(|cfg_val| {
                cfg_val
                    .pointer("/plugins/agy-hud/enabled")
                    .and_then(Value::as_bool)
            })
            .unwrap_or(true);
        return (
            "installed".to_string(),
            Some(InstalledPluginFacts {
                version,
                path: canonical_or_parent(&default_plugin_dir)
                    .to_string_lossy()
                    .into_owned(),
                enabled,
            }),
        );
    }

    if std::env::var("AGY_SKIP_PLUGIN_INSTALL").as_deref() != Ok("1") {
        let args = vec![
            "plugin".to_string(),
            "list".to_string(),
            "--json".to_string(),
        ];
        let tmp = std::env::temp_dir();
        if let Some(out) = exec_file_safe("agy", &args, Some(&tmp))
            && let Some(res) = parse_plugin_list_json(&out)
        {
            return res;
        }
    }

    ("unavailable".to_string(), None)
}

fn print_filtered_with_repo<S: AsRef<str>>(lines: &[S], allowed_github_repo: Option<&str>) {
    if lines.is_empty() {
        return;
    }
    let mut stdout = std::io::stdout().lock();
    for line in lines {
        let safe = terminal_filter_with_repo(line.as_ref(), allowed_github_repo);
        let _ = writeln!(stdout, "{safe}");
    }
    let _ = stdout.flush();
}

fn print_filtered<S: AsRef<str>>(lines: &[S]) {
    print_filtered_with_repo(lines, None);
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
        print_filtered(&[VERSION_BANNER]);
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

    if sub == Some("update") {
        match run_update_subcommand(&paths, &argv[1..], now_epoch_ms()) {
            Ok(Some(report)) => {
                let pretty = serde_json::to_string_pretty(&report).unwrap_or_default();
                let lines: Vec<&str> = pretty.lines().collect();
                print_filtered(&lines);
                return 0;
            }
            Ok(None) => return 0,
            Err(err) => {
                print_filtered(&[format!("update failed: {err}")]);
                return 1;
            }
        }
    }

    if sub == Some("quota") {
        match run_quota_subcommand(&paths, &argv[1..], now_epoch_ms()) {
            Ok(Some(report)) => {
                let pretty = serde_json::to_string_pretty(&report).unwrap_or_default();
                let lines: Vec<&str> = pretty.lines().collect();
                print_filtered(&lines);
                return 0;
            }
            Ok(None) => return 0,
            Err(err) => {
                print_filtered(&[format!("quota refresh failed: {err}")]);
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

        let (installed_plugin_status, installed_plugin) = if cmd_name == "doctor" {
            detect_installed_plugin(&paths)
        } else {
            (String::new(), None)
        };

        let facts = DoctorFacts {
            cwd: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .to_string_lossy()
                .into_owned(),
            home_dir: canonical_or_parent(&paths.home)
                .to_string_lossy()
                .into_owned(),
            platform: detect_platform(),
            terminal: detect_terminal(),
            remote_session: detect_remote_session(),
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
            installed_plugin_status,
            installed_plugin,
            config_path: logic_paths.config.clone(),
            heartbeat: read_heartbeat(&paths.heartbeat_path),
        };

        let config_text = read_bounded_string(&paths.config_path, MAX_CONFIG_BYTES as u64);
        let cli_settings_text = read_bounded_string(&paths.cli_settings_path, 128 * 1024);
        let last_snapshot_text = logic_paths
            .last_snapshot
            .as_deref()
            .and_then(|p| read_bounded_string(Path::new(p), 256 * 1024));
        let update_cache_text = logic_paths
            .update_cache
            .as_deref()
            .and_then(|p| read_bounded_string(Path::new(p), 16 * 1024));

        // If `preview --last` needs transcript/task.md from the stored snapshot, load them safely
        let mut tx_head: Option<String> = None;
        let mut tx_tail: Option<String> = None;
        let mut task_md_text: Option<String> = None;
        if cmd_name == "preview"
            && argv.iter().any(|a| a == "--last")
            && let Some(raw_snap) = last_snapshot_text.as_deref()
            && let Ok(val) = serde_json::from_str::<Value>(raw_snap)
            && let Some(stdin_val) = val.get("stdin")
            && let Ok(snap_stdin) = serde_json::from_value::<AgyStdinData>(stdin_val.clone())
        {
            let snap_paths = build_logic_paths(&paths, Some(&snap_stdin), data_real.as_deref());
            if let Some(tp_str) = snap_paths.transcript.as_deref() {
                let tp = Path::new(tp_str);
                if let Some(full) = read_bounded_string(tp, MAX_CURSOR_DELTA_BYTES) {
                    tx_head = full.lines().next().map(str::to_string);
                    tx_tail = Some(full);
                } else {
                    let (h, t) = read_transcript_slices(tp);
                    tx_head = h;
                    tx_tail = t;
                }
            }
            if let Some(tm_str) = snap_paths.task_md.as_deref() {
                task_md_text = read_bounded_string(Path::new(tm_str), MAX_TASK_MD_BYTES);
            }
        }

        let ctx = CommandContext {
            facts: &facts,
            paths: &logic_paths,
            now_ms,
            config_text: config_text.as_deref(),
            cli_settings_text: cli_settings_text.as_deref(),
            last_snapshot_text: last_snapshot_text.as_deref(),
            update_cache_text: update_cache_text.as_deref(),
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
    if is_nested_hud() || is_hud_disabled() {
        drain_stdin_nested(Duration::from_millis(150));
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

    let mut collected = collect_render_inputs(
        &paths,
        &logic_paths,
        &loaded_cfg.config,
        &stdin_payload,
        started_ms,
        &SystemProbeRunner,
    );

    if loaded_cfg.config.display.show_speed {
        let existing_speed_cache = parse_speed_cache_info(&collected.probes);
        if let Some(next_speed_cache) = apply_speed_cache(
            Some(&stdin_payload),
            &mut collected.transcript,
            existing_speed_cache.as_ref(),
            started_ms,
        ) && let Ok(speed_json) = serde_json::to_string(&next_speed_cache)
        {
            if let Some(ref sc_path) = logic_paths.speed_cache {
                let _ = atomic_write_bytes(Path::new(sc_path), speed_json.as_bytes(), 0o600);
            }
            collected.probes.speed_cache = Some(speed_json);
        }
    }

    let cost_totals = if stdin_payload.cost.is_some()
        || loaded_cfg.config.display.show_daily_cost
        || loaded_cfg.config.display.show_weekly_cost
    {
        if let Some(ref ledger_str_path) = logic_paths.cost_ledger {
            let ledger_path = Path::new(ledger_str_path);
            let lock_path = ledger_path.with_extension("lock");
            with_exclusive_lock_file(&lock_path, 2_000, || {
                let fresh_raw = read_bounded_string(ledger_path, 64 * 1024);
                let existing_ledger = fresh_raw
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<CostLedgerSnapshot>(s).ok())
                    .filter(|l| l.version == 1 && !l.day_key.is_empty());
                let (totals, maybe_updated) = reconcile_cost_ledger(
                    existing_ledger.as_ref(),
                    Some(&stdin_payload),
                    started_ms,
                );
                if let Some(updated) = maybe_updated
                    && let Ok(serialized) = serde_json::to_string(&updated)
                {
                    let _ = atomic_write_bytes(ledger_path, serialized.as_bytes(), 0o600);
                    collected.probes.cost_ledger = Some(serialized);
                } else if fresh_raw.is_some() {
                    collected.probes.cost_ledger = fresh_raw;
                }
                totals
            })
            .unwrap_or_else(|| {
                let existing = parse_cost_ledger_info(&collected.probes);
                reconcile_cost_ledger(existing.as_ref(), Some(&stdin_payload), started_ms).0
            })
        } else {
            let existing = parse_cost_ledger_info(&collected.probes);
            reconcile_cost_ledger(existing.as_ref(), Some(&stdin_payload), started_ms).0
        }
    } else {
        None
    };

    let vcs = parse_vcs_info(Some(&stdin_payload), &collected.probes);
    let memory = parse_memory_info(&collected.probes);
    let cached_quota = parse_cached_quota_info(&collected.probes);
    let update_info = parse_update_cache_info(&collected.probes);
    let columns = stdin_payload.terminal_width.filter(|&w| w > 0);

    let mut lines = render_hud_lines_full(
        RenderInput {
            stdin: Some(&stdin_payload),
            config: &loaded_cfg.config,
            transcript: &collected.transcript,
            vcs: vcs.as_ref(),
            memory: memory.as_ref(),
            cached_quota: cached_quota.as_ref(),
            update_info: update_info.as_ref(),
            columns,
            now_ms: started_ms,
        },
        cost_totals.as_ref(),
    );
    if let Some(problem_line) =
        format_config_problem_line(&loaded_cfg.problems, &loaded_cfg.config, columns)
    {
        lines.push(problem_line);
    }

    // Persist last snapshot inside symlink-swept `<hudHome>/data/last-snapshot.json`
    if let Some(ref snap_path) = logic_paths.last_snapshot {
        let snap_json = json!({
            "at": started_ms,
            "stdin": &stdin_payload,
            "probes": &collected.probes,
            "compactionCount": collected.transcript.compaction_count,
            "lastCompactionTokens": collected.transcript.last_compaction_tokens,
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

    let allowed_github_repo = vcs.as_ref().and_then(|v| v.repo_url.as_deref());
    print_filtered_with_repo(&lines, allowed_github_repo);

    if loaded_cfg.config.quota.background_refresh {
        maybe_spawn_background_quota_refresh(&paths, &current_exe, &stdin_payload, started_ms);
    }
    if loaded_cfg.config.updates.enabled {
        maybe_spawn_background_update_check(&paths, &current_exe, started_ms);
    }

    0
}

fn main() -> ExitCode {
    ExitCode::from(run())
}
