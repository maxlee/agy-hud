use crate::ansi::{BOLD, DIM, colors, paint, strip_ansi};
use crate::config::{
    CONFIG_SCHEMA, SchemaValueKind, get_dot_value, parse_config_str, parse_set_pairs, set_dot_value,
};
use crate::probes::{
    apply_speed_cache, is_newer_semver, normalize_semver_tag, parse_cached_quota_info,
    parse_memory_info, parse_speed_cache_info, parse_update_cache_info, parse_vcs_info,
};
use crate::render::{RenderInput, render_hud_lines};
use crate::transcript::parse_transcript_and_tasks;
use crate::types::{
    AgyStdinData, CommandOutput, ContextWindowStdin, CostStdin, CurrentUsageStdin, DoctorFacts,
    GlyphMode, Language, LogicPaths, ModelStdin, ProbeAnswers, RawQuotaBucket, SandboxStdin,
    SubagentEntry, SubagentStatus, SubagentStdin, TodoItem, TodoStatus, ToolEntry, ToolStatus,
    TranscriptSummary, UpdateCacheSnapshot, VimStdin, WorkspaceStdin,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};

pub struct CommandContext<'a> {
    pub facts: &'a DoctorFacts,
    pub paths: &'a LogicPaths,
    pub now_ms: i64,
    pub config_text: Option<&'a str>,
    pub cli_settings_text: Option<&'a str>,
    pub last_snapshot_text: Option<&'a str>,
    pub update_cache_text: Option<&'a str>,
    pub transcript_head: Option<&'a str>,
    pub transcript_tail: Option<&'a str>,
    pub task_md_text: Option<&'a str>,
}

fn parse_cli_args(args: &[String]) -> (HashMap<String, Vec<String>>, Vec<String>) {
    let mut flags: HashMap<String, Vec<String>> = HashMap::new();
    let mut words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !a.starts_with("--") {
            words.push(a.clone());
            i += 1;
            continue;
        }
        let next_is_val = i + 1 < args.len() && !args[i + 1].starts_with("--");
        let val = if next_is_val {
            i += 1;
            args[i].clone()
        } else {
            "true".to_string()
        };
        flags.entry(a.clone()).or_default().push(val);
        i += 1;
    }
    (flags, words)
}

fn is_secret_key_name(key: &str) -> bool {
    let lower = key.trim_start_matches('-').to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "token" | "api_key" | "api-key" | "apikey" | "secret" | "password" | "auth"
    )
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
}

/// Redacts common bearer tokens, `--token=...` / `api_key=...` flags, `sk-...`, and `AIza...` keys.
pub fn redact_command(cmd: &str) -> String {
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    let mut out_tokens: Vec<String> = Vec::with_capacity(tokens.len());
    let mut redact_next = false;

    for raw_tok in tokens {
        if redact_next {
            out_tokens.push("[REDACTED]".to_string());
            redact_next = false;
            continue;
        }
        if raw_tok.eq_ignore_ascii_case("bearer") {
            out_tokens.push(raw_tok.to_string());
            redact_next = true;
            continue;
        }
        if let Some(sep_idx) = raw_tok.find(['=', ':']) {
            let lhs = &raw_tok[..sep_idx];
            let sep_ch = &raw_tok[sep_idx..=sep_idx];
            let rhs = &raw_tok[sep_idx + 1..];
            if is_secret_key_name(lhs) {
                if rhs.is_empty() {
                    out_tokens.push(format!("{lhs}{sep_ch}[REDACTED]"));
                    redact_next = true;
                } else {
                    out_tokens.push(format!("{lhs}{sep_ch}[REDACTED]"));
                }
                continue;
            }
        }

        let unquoted = raw_tok.trim_matches(['"', '\'']);
        if let Some(rest) = unquoted.strip_prefix("sk-")
            && rest.len() >= 8
            && rest.bytes().all(is_token_char)
        {
            out_tokens.push("sk-[REDACTED]".to_string());
            continue;
        }
        if let Some(rest) = unquoted.strip_prefix("AIza")
            && rest.len() >= 20
            && rest.bytes().all(is_token_char)
        {
            out_tokens.push("AIza[REDACTED]".to_string());
            continue;
        }

        out_tokens.push(raw_tok.to_string());
    }

    let joined = out_tokens.join(" ");
    joined.chars().take(180).collect()
}

fn parse_json_object_safe(raw_text: Option<&str>) -> Option<Map<String, Value>> {
    let text = raw_text?;
    let stripped = text.strip_prefix('\u{feff}').unwrap_or(text);
    match serde_json::from_str::<Value>(stripped).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn normalize_compare_path(p: &str) -> String {
    p.replace('\\', "/").trim_end_matches('/').to_string()
}

pub fn abbreviate_home_path(text: &str, home_dir: &str) -> String {
    let clean_home = home_dir.trim_end_matches(['/', '\\']);
    if clean_home.is_empty() {
        return text.to_string();
    }
    let mut out = text.replace(clean_home, "~");
    let slash_home = clean_home.replace('\\', "/");
    if slash_home != clean_home {
        out = out.replace(&slash_home, "~");
    }
    out
}

fn run_doctor(args: &[String], ctx: &CommandContext<'_>) -> CommandOutput {
    let (flags, _) = parse_cli_args(args);
    let pretty_mode = flags.contains_key("--pretty");

    let loaded = parse_config_str(ctx.config_text);
    let cli_settings = parse_json_object_safe(ctx.cli_settings_text);
    let sl = cli_settings
        .as_ref()
        .and_then(|m| m.get("statusLine"))
        .and_then(Value::as_object);

    let cmd = sl
        .and_then(|s| s.get("command"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let enabled = sl
        .and_then(|s| s.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let stack_with_default = sl
        .and_then(|s| s.get("stack_with_default"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let kind = if cmd.is_empty() {
        "none"
    } else if cmd.contains("agy-hud") || cmd.contains("hud.mjs") || cmd.contains("statusline.mjs") {
        "agy-hud"
    } else {
        "other"
    };

    let mut findings: Vec<String> = Vec::new();
    if kind == "none" {
        findings.push(
            "No statusLine is configured in ~/.gemini/antigravity-cli/settings.json. Fix: run `agy-hud install` (or `/statusline` in agy CLI).".to_string(),
        );
    } else if kind == "other" {
        findings.push(format!(
            "statusLine currently runs another script ({}). Fix: ask the user first, then run `agy-hud install --replace`.",
            redact_command(cmd)
        ));
    } else if !enabled {
        findings.push(
            "statusLine is configured for agy-hud but currently disabled (`enabled: false`). Fix: run `/statusline enable` inside agy CLI or `agy-hud install`.".to_string(),
        );
    }

    if kind == "agy-hud" && ctx.facts.platform == "windows" {
        if cmd.contains('"') {
            findings.push(
                "Windows statusLine.command contains double quotes (\"), which breaks Antigravity CLI cmd.exe invocation (tightship#89). Fix: run `agy-hud install --replace`.".to_string(),
            );
        } else if cmd.contains(' ') {
            findings.push(
                "Windows statusLine.command path contains spaces without 8.3 short-path form. Fix: move agy-hud to a space-free path or configure its 8.3 short path.".to_string(),
            );
        }
    }

    let runs_installed_copy = ctx.facts.installed_plugin.as_ref().is_some_and(|ip| {
        let ip_norm = normalize_compare_path(&ip.path);
        !ip_norm.is_empty()
            && (normalize_compare_path(&ctx.facts.plugin.root) == ip_norm
                || normalize_compare_path(&ctx.facts.runtime.exec_path).starts_with(&ip_norm)
                || normalize_compare_path(cmd).contains(&ip_norm))
    });

    if let Some(ip) = ctx.facts.installed_plugin.as_ref() {
        if !ip.enabled {
            findings.push(
                "Installed agy-hud plugin is disabled in `agy plugin list`. Fix: enable the plugin in agy CLI.".to_string(),
            );
        }
        if !ip.version.is_empty() && ip.version != ctx.facts.plugin.version {
            findings.push(format!(
                "Installed plugin version ({}) differs from active binary version ({}). Fix: update the plugin and run `agy-hud install --replace`.",
                ip.version, ctx.facts.plugin.version
            ));
        }
    }

    if kind == "agy-hud" && enabled && ctx.facts.heartbeat.is_none() {
        findings.push(
            "agy CLI has not executed agy-hud yet. Fix: start `agy` and send a prompt to trigger the first statusLine render.".to_string(),
        );
    } else if let Some(hb) = ctx.facts.heartbeat.as_ref()
        && hb.outcome != "ok"
    {
        findings.push(format!(
            "Last render reported outcome \"{}\". Fix: run `agy-hud preview --last` to inspect.",
            hb.outcome
        ));
    }

    let cached_update: Option<UpdateCacheSnapshot> = ctx
        .update_cache_text
        .and_then(|s| serde_json::from_str::<UpdateCacheSnapshot>(s).ok())
        .and_then(|mut u| {
            u.latest_version = normalize_semver_tag(&u.latest_version)?;
            Some(u)
        });
    let update_available = loaded.config.updates.enabled
        && cached_update
            .as_ref()
            .is_some_and(|u| is_newer_semver(&ctx.facts.runtime.version, &u.latest_version));
    if update_available && let Some(ref u) = cached_update {
        findings.push(format!(
            "New agy-hud release available: v{} (current: v{}). Fix: run `/agy-hud:hud-update` inside agy CLI or `agy-hud update`.",
            u.latest_version, ctx.facts.runtime.version
        ));
    }

    for p in &loaded.problems {
        findings.push(format!(
            "Config issue: {p} Fix: run `agy-hud set <key>=<value>` or `agy-hud schema`."
        ));
    }

    let healthy_msg =
        "None. agy-hud (native Rust engine) is healthy, configured, and ready to render."
            .to_string();
    let final_findings = if findings.is_empty() {
        vec![healthy_msg]
    } else {
        findings
    };

    if pretty_mode {
        let home = &ctx.facts.home_dir;
        let platform = if ctx.facts.platform.is_empty() {
            "unknown"
        } else {
            ctx.facts.platform.as_str()
        };
        let terminal = if ctx.facts.terminal.is_empty() {
            "unknown"
        } else {
            ctx.facts.terminal.as_str()
        };
        let session_label = if ctx.facts.remote_session {
            "remote (SSH)"
        } else {
            "local"
        };
        let glyph_str = match loaded.config.display.glyph_mode {
            GlyphMode::Unicode => "unicode",
            GlyphMode::Nerd => "nerd",
            GlyphMode::Ascii => "ascii",
        };
        let cmd_disp = if cmd.is_empty() {
            "<not set>".to_string()
        } else {
            abbreviate_home_path(&redact_command(cmd), home)
        };
        let plugin_status = if ctx.facts.installed_plugin_status.is_empty() {
            "unknown"
        } else {
            ctx.facts.installed_plugin_status.as_str()
        };

        let mut out = Vec::new();
        out.push(paint(
            &format!(
                "agy-hud doctor (v{} · {})",
                ctx.facts.plugin.version, ctx.facts.runtime.engine
            ),
            &[colors::CYAN, BOLD],
        ));
        out.push(format!(
            "  Runtime          : {}",
            abbreviate_home_path(&ctx.facts.runtime.exec_path, home)
        ));
        out.push(format!(
            "  Environment      : {platform} · terminal={terminal} · session={session_label}"
        ));
        out.push(format!(
            "  StatusLine       : kind={kind} · enabled={enabled} · command={cmd_disp}"
        ));
        out.push(format!(
            "  Settings File    : {}",
            abbreviate_home_path(&ctx.paths.cli_settings, home)
        ));
        if let Some(ip) = ctx.facts.installed_plugin.as_ref() {
            out.push(format!(
                "  Installed Plugin : {plugin_status} (v{}, enabled={}, runsInstalledCopy={}, path={})",
                ip.version,
                ip.enabled,
                runs_installed_copy,
                abbreviate_home_path(&ip.path, home)
            ));
        } else {
            out.push(format!("  Installed Plugin : {plugin_status}"));
        }
        out.push(format!(
            "  Config           : {} (glyphMode={glyph_str}, updates.enabled={}, quota.backgroundRefresh={})",
            abbreviate_home_path(&ctx.paths.config, home),
            loaded.config.updates.enabled,
            loaded.config.quota.background_refresh
        ));
        if let Some(ref u) = cached_update {
            if update_available {
                out.push(format!(
                    "  Updates          : v{} available! (current v{} -> run `/agy-hud:hud-update` or `agy-hud update`)",
                    u.latest_version, ctx.facts.runtime.version
                ));
            } else {
                out.push(format!(
                    "  Updates          : up to date (latest checked: v{})",
                    u.latest_version
                ));
            }
        }
        if let Some(hb) = ctx.facts.heartbeat.as_ref() {
            let secs = ((ctx.now_ms - hb.at).max(0) as f64 / 1000.0).round() as u64;
            out.push(format!(
                "  Last Render      : outcome={} · {}s ago · duration={}ms",
                hb.outcome,
                secs,
                hb.duration_ms.unwrap_or(0)
            ));
        } else {
            out.push("  Last Render      : none yet".to_string());
        }
        out.push(String::new());
        out.push(paint(
            "Glyph Alignment Probe (right '|' bars should align vertically):",
            &[BOLD],
        ));
        out.push("  unicode : |● READY │ █▓▒░· │ ⏱ 12m|".to_string());
        out.push("  nerd    : |\u{f07c} READY │ █▓▒░· │ \u{ee9c}  12m|".to_string());
        out.push("  ascii   : |* READY | ##==- | tt 12m|".to_string());
        if ctx.facts.remote_session && loaded.config.display.glyph_mode != GlyphMode::Ascii {
            out.push(paint(
                "  Tip: SSH session detected; if glyphs misalign, run `agy-hud set display.glyphMode=ascii`.",
                &[colors::YELLOW],
            ));
        } else {
            out.push(paint(
                "  Tip: switch via `agy-hud set display.glyphMode=unicode|nerd|ascii`.",
                &[DIM],
            ));
        }
        out.push(String::new());
        out.push(paint("Findings:", &[BOLD]));
        for f in &final_findings {
            out.push(format!("  - {}", abbreviate_home_path(f, home)));
        }

        return CommandOutput {
            lines: out,
            failed: false,
            config_to_write: None,
        };
    }

    let last_render = ctx.facts.heartbeat.as_ref().map(|hb| {
        let secs = ((ctx.now_ms - hb.at).max(0) as f64 / 1000.0).round() as u64;
        json!({
            "secondsAgo": secs,
            "outcome": hb.outcome,
            "durationMs": hb.duration_ms,
            "columns": hb.columns,
        })
    });

    let installed_plugin_status = if ctx.facts.installed_plugin_status.is_empty() {
        "unknown"
    } else {
        ctx.facts.installed_plugin_status.as_str()
    };

    let report = json!({
        "runtime": ctx.facts.runtime,
        "environment": {
            "platform": ctx.facts.platform,
            "terminal": ctx.facts.terminal,
            "remoteSession": ctx.facts.remote_session,
        },
        "plugin": ctx.facts.plugin,
        "installedPlugin": {
            "status": installed_plugin_status,
            "details": ctx.facts.installed_plugin.as_ref().map(|ip| json!({
                "version": ip.version,
                "path": ip.path,
                "enabled": ip.enabled,
                "runsInstalledCopy": runs_installed_copy,
            })),
        },
        "statusLine": {
            "settingsFile": ctx.paths.cli_settings,
            "kind": kind,
            "enabled": enabled,
            "command": if cmd.is_empty() { Value::Null } else { Value::String(redact_command(cmd)) },
            "stackWithDefault": stack_with_default,
        },
        "lastRender": last_render,
        "updates": {
            "enabled": loaded.config.updates.enabled,
            "currentVersion": ctx.facts.runtime.version,
            "latestVersion": cached_update.as_ref().map(|u| u.latest_version.as_str()),
            "releaseUrl": cached_update.as_ref().and_then(|u| u.release_url.as_deref()),
            "checkedAt": cached_update.as_ref().map(|u| u.checked_at),
            "updateAvailable": update_available,
        },
        "config": {
            "path": ctx.paths.config,
            "vcsCacheTtlMs": loaded.config.vcs.cache_ttl_ms,
            "glyphMode": loaded.config.display.glyph_mode,
            "updatesEnabled": loaded.config.updates.enabled,
            "quotaBackgroundRefresh": loaded.config.quota.background_refresh,
            "problems": loaded.problems,
        },
        "findings": final_findings,
    });

    let pretty = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    CommandOutput {
        lines: pretty.lines().map(str::to_string).collect(),
        failed: false,
        config_to_write: None,
    }
}

fn run_schema(words: &[String], ctx: &CommandContext<'_>) -> CommandOutput {
    let loaded = parse_config_str(ctx.config_text);
    let cfg_val = serde_json::to_value(&loaded.config).unwrap_or(Value::Null);
    let query: Vec<String> = words.iter().map(|w| w.to_ascii_lowercase()).collect();

    let matches: Vec<_> = CONFIG_SCHEMA
        .iter()
        .filter(|entry| {
            if query.is_empty() {
                return true;
            }
            let hay = format!(
                "{} {} {}",
                entry.key,
                entry.kind.type_name(),
                entry.description
            )
            .to_ascii_lowercase();
            query.iter().all(|q| hay.contains(q))
        })
        .collect();

    if matches.is_empty() {
        return CommandOutput {
            lines: vec![format!(
                "No config keys matched \"{}\". Run `agy-hud schema` with no arguments to list all {} keys.",
                words.join(" "),
                CONFIG_SCHEMA.len()
            )],
            failed: true,
            config_to_write: None,
        };
    }

    let lines = matches
        .into_iter()
        .map(|m| {
            let current_val = get_dot_value(&cfg_val, m.key)
                .and_then(|v| serde_json::to_string(v).ok())
                .unwrap_or_else(|| "null".to_string());
            let range = match m.kind {
                SchemaValueKind::Enum(allowed) => format!("[{}]", allowed.join(" | ")),
                SchemaValueKind::Number { min, max } => format!("[{min}..{max}]"),
                SchemaValueKind::NullableNumber { min, max } => format!("[{min}..{max} | null]"),
                other => other.type_name().to_string(),
            };
            format!(
                "{} = {} {} — {}",
                paint(m.key, &[colors::CYAN, BOLD]),
                paint(&current_val, &[colors::YELLOW, BOLD]),
                paint(&range, &[DIM]),
                m.description,
            )
        })
        .collect();

    CommandOutput {
        lines,
        failed: false,
        config_to_write: None,
    }
}

struct SamplePreset {
    stdin: AgyStdinData,
    transcript: TranscriptSummary,
    probes: ProbeAnswers,
}

fn sample_presets(now_ms: i64) -> HashMap<&'static str, SamplePreset> {
    let mut base_quota = BTreeMap::new();
    base_quota.insert(
        "gemini-weekly".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.84),
            remaining_amount: None,
            reset_time: None,
            reset_in_seconds: Some(412_200.0),
            disabled: None,
        },
    );

    let base_stdin = AgyStdinData {
        cwd: Some("/Users/dev/agy-hud".to_string()),
        workspace: Some(WorkspaceStdin {
            current_dir: Some("/Users/dev/agy-hud".to_string()),
            project_dir: Some("/Users/dev/agy-hud".to_string()),
        }),
        model: Some(ModelStdin {
            id: Some("Gemini 4 Argon (Medium)".to_string()),
            display_name: Some("Gemini 4 Argon (Medium)".to_string()),
            effort: Some("medium".to_string()),
        }),
        version: Some("1.5.3".to_string()),
        product: Some("antigravity".to_string()),
        context_window: Some(ContextWindowStdin {
            total_input_tokens: Some(142_800),
            total_output_tokens: Some(18_400),
            context_window_size: Some(1_048_576),
            used_percentage: Some(28.4),
            remaining_percentage: Some(71.6),
            current_usage: Some(CurrentUsageStdin {
                input_tokens: Some(34_000),
                output_tokens: Some(1_850),
                cache_creation_input_tokens: Some(4_000),
                cache_read_input_tokens: Some(112_000),
            }),
        }),
        exceeds_200k_tokens: Some(false),
        quota: Some(base_quota),
        agent_state: Some("idle".to_string()),
        cycle_mode: Some("auto".to_string()),
        sandbox: Some(SandboxStdin {
            enabled: Some(true),
        }),
        artifact_count: Some(2),
        plan_tier: Some("Pro".to_string()),
        task_count: Some(1),
        terminal_width: Some(108),
        cost: Some(CostStdin {
            total_usd: Some(0.38),
            subagent_usd: Some(0.09),
            estimated: Some(true),
        }),
        ..Default::default()
    };

    let rich_transcript = TranscriptSummary {
        tools: vec![
            ToolEntry {
                id: "4:0".to_string(),
                name: "run_command".to_string(),
                target: Some("Cargo test execution".to_string()),
                status: ToolStatus::Running,
                count: 1,
                started_at: Some(now_ms - 4_000),
                ended_at: None,
            },
            ToolEntry {
                id: "3:0".to_string(),
                name: "edit_file".to_string(),
                target: Some("render.rs".to_string()),
                status: ToolStatus::Completed,
                count: 2,
                started_at: None,
                ended_at: None,
            },
            ToolEntry {
                id: "2:0".to_string(),
                name: "read_file".to_string(),
                target: Some("SKILL.md".to_string()),
                status: ToolStatus::Completed,
                count: 5,
                started_at: None,
                ended_at: None,
            },
        ],
        skills: vec!["hud".to_string(), "agy-customizations".to_string()],
        mcp_servers: vec!["context7".to_string()],
        mcp_errors: Vec::new(),
        subagents: vec![
            SubagentEntry {
                name: "research".to_string(),
                role: Some("CLI Docs Researcher".to_string()),
                model: None,
                status: SubagentStatus::Running,
                started_at: Some(now_ms - 75_000),
                ended_at: None,
            },
            SubagentEntry {
                name: "self".to_string(),
                role: Some("Security Auditor".to_string()),
                model: None,
                status: SubagentStatus::Completed,
                started_at: Some(now_ms - 140_000),
                ended_at: Some(now_ms - 80_000),
            },
        ],
        todos: vec![
            TodoItem {
                text: "Build zero-I/O hud-core crate".to_string(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                text: "Implement parallel VCS & transcript collector".to_string(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                text: "Verify compile-time arch_guard boundaries".to_string(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                text: "Validate plugin manifest with agy plugin validate --strict".to_string(),
                status: TodoStatus::Pending,
            },
        ],
        session_started_at: Some(now_ms - 740_000),
        last_step_at: Some(now_ms - 2_000),
        step_count: 24,
        active_slash_mode: None,
        completed_step_tps_x10: Some(640),
        latest_planner_step: None,
    };

    let rich_probes = ProbeAnswers {
        vcs: Some(
            json!({
                "type": "git",
                "branch": "feat/rust-hud",
                "dirty": true,
                "ahead": 2,
                "behind": 0,
                "modified": 3,
                "added": 2,
                "deleted": 0,
                "untracked": 1,
                "conflicts": 0
            })
            .to_string(),
        ),
        memory: Some(
            json!({
                "totalBytes": 34_359_738_368u64,
                "usedBytes": 21_990_232_555u64,
                "usedPercent": 64u64
            })
            .to_string(),
        ),
        quota_cache: None,
        update_cache: None,
        ..Default::default()
    };

    let mut presets = HashMap::new();

    // 1. idle
    let mut idle_stdin = base_stdin.clone();
    idle_stdin.artifact_count = Some(0);
    idle_stdin.task_count = Some(0);
    presets.insert(
        "idle",
        SamplePreset {
            stdin: idle_stdin,
            transcript: TranscriptSummary {
                session_started_at: Some(now_ms - 320_000),
                last_step_at: Some(now_ms - 10_000),
                step_count: 8,
                ..Default::default()
            },
            probes: rich_probes.clone(),
        },
    );

    // 2. tool_use
    let mut tool_stdin = base_stdin.clone();
    tool_stdin.agent_state = Some("tool_use".to_string());
    tool_stdin.subagents = Some(vec![
        SubagentStdin {
            name: Some("research".to_string()),
            role: Some("CLI Docs Researcher".to_string()),
            status: Some("running".to_string()),
        },
        SubagentStdin {
            name: Some("self".to_string()),
            role: Some("Security Auditor".to_string()),
            status: Some("completed".to_string()),
        },
    ]);
    presets.insert(
        "tool_use",
        SamplePreset {
            stdin: tool_stdin,
            transcript: rich_transcript.clone(),
            probes: rich_probes.clone(),
        },
    );

    // 3. confirm
    let mut confirm_stdin = base_stdin.clone();
    confirm_stdin.agent_state = Some("tool_use".to_string());
    confirm_stdin.cycle_mode = Some("plan".to_string());
    confirm_stdin.tool_confirmation_pending = Some(true);
    confirm_stdin.pending_input_count = Some(1);
    confirm_stdin.vim = Some(VimStdin {
        mode: Some("NORMAL".to_string()),
    });
    presets.insert(
        "confirm",
        SamplePreset {
            stdin: confirm_stdin,
            transcript: rich_transcript.clone(),
            probes: rich_probes.clone(),
        },
    );

    // 4. high-context
    let mut high_quota = BTreeMap::new();
    high_quota.insert(
        "gemini-weekly".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.08),
            remaining_amount: None,
            reset_time: None,
            reset_in_seconds: Some(5_400.0),
            disabled: None,
        },
    );
    let mut high_stdin = base_stdin;
    high_stdin.agent_state = Some("working".to_string());
    high_stdin.exceeds_200k_tokens = Some(true);
    high_stdin.context_window = Some(ContextWindowStdin {
        total_input_tokens: Some(142_800),
        total_output_tokens: Some(18_400),
        context_window_size: Some(1_048_576),
        used_percentage: Some(89.6),
        remaining_percentage: Some(10.4),
        current_usage: Some(CurrentUsageStdin {
            input_tokens: Some(820_000),
            output_tokens: Some(19_500),
            cache_creation_input_tokens: Some(0),
            cache_read_input_tokens: Some(100_000),
        }),
    });
    high_stdin.quota = Some(high_quota);
    presets.insert(
        "high-context",
        SamplePreset {
            stdin: high_stdin,
            transcript: rich_transcript,
            probes: rich_probes,
        },
    );

    presets
}

#[derive(Debug, Deserialize)]
struct StoredSnapshot {
    stdin: Option<AgyStdinData>,
    #[serde(default)]
    probes: ProbeAnswers,
}

fn run_preview(args: &[String], ctx: &CommandContext<'_>) -> CommandOutput {
    let (flags, _) = parse_cli_args(args);
    let loaded = parse_config_str(ctx.config_text);
    let set_pairs = flags.get("--set").cloned().unwrap_or_default();
    let overridden = parse_set_pairs(&set_pairs, &loaded.config);
    if !overridden.problems.is_empty() {
        return CommandOutput {
            lines: overridden.problems,
            failed: true,
            config_to_write: None,
        };
    }
    let config = overridden.config;

    let explicit_width = flags
        .get("--width")
        .and_then(|v| v.first())
        .and_then(|s| s.parse::<usize>().ok());

    if flags.contains_key("--last") {
        let Some(raw_snap) = ctx.last_snapshot_text else {
            return CommandOutput {
                lines: vec![
                    "No previous live render snapshot found yet. Run a prompt inside `agy` first, or run `agy-hud preview` without `--last`.".to_string(),
                ],
                failed: true,
                config_to_write: None,
            };
        };
        let Ok(snap) = serde_json::from_str::<StoredSnapshot>(raw_snap) else {
            return CommandOutput {
                lines: vec!["Last snapshot JSON could not be parsed.".to_string()],
                failed: true,
                config_to_write: None,
            };
        };
        let mut transcript = parse_transcript_and_tasks(
            ctx.transcript_head,
            ctx.transcript_tail,
            ctx.task_md_text,
            snap.stdin.as_ref(),
        );
        let speed_cache = parse_speed_cache_info(&snap.probes);
        let _ = apply_speed_cache(
            snap.stdin.as_ref(),
            &mut transcript,
            speed_cache.as_ref(),
            ctx.now_ms,
        );
        let vcs = parse_vcs_info(snap.stdin.as_ref(), &snap.probes);
        let memory = parse_memory_info(&snap.probes);
        let cached_quota = parse_cached_quota_info(&snap.probes);
        let update_info = parse_update_cache_info(&snap.probes);
        let cols = explicit_width
            .or_else(|| snap.stdin.as_ref().and_then(|s| s.terminal_width))
            .or(Some(100));
        let lines = render_hud_lines(RenderInput {
            stdin: snap.stdin.as_ref(),
            config: &config,
            transcript: &transcript,
            vcs: vcs.as_ref(),
            memory: memory.as_ref(),
            cached_quota: cached_quota.as_ref(),
            update_info: update_info.as_ref(),
            columns: cols,
            now_ms: ctx.now_ms,
        });
        return CommandOutput {
            lines,
            failed: false,
            config_to_write: None,
        };
    }

    let presets = sample_presets(ctx.now_ms);
    let state_arg = flags
        .get("--state")
        .and_then(|v| v.first())
        .map(String::as_str)
        .unwrap_or("tool_use");

    if state_arg == "showcase" {
        let scenarios: [(&str, &str, usize, Option<Language>); 5] = [
            (
                "1) Standard Expanded / Active Multi-Agent + Tools + Checklist (108 cols)",
                "tool_use",
                108,
                Some(Language::En),
            ),
            (
                "2) Pending Tool Confirmation + Plan Mode + Vim Badge (108 cols)",
                "confirm",
                108,
                Some(Language::En),
            ),
            (
                "3) Ultra-Wide Adaptive Single-Line Idle (208 cols)",
                "idle",
                208,
                Some(Language::En),
            ),
            (
                "4) Narrow Terminal Auto-Compact + Critical Context Alert (68 cols)",
                "high-context",
                68,
                Some(Language::En),
            ),
            (
                "5) 简体中文界面模式 (`language=zh-Hans`, 108 cols)",
                "tool_use",
                108,
                Some(Language::ZhHans),
            ),
        ];

        let mut out = Vec::new();
        for (idx, (title, key, width, lang_override)) in scenarios.iter().enumerate() {
            let p = presets.get(key).expect("preset exists");
            let mut cfg = config.clone();
            if let Some(lang) = lang_override {
                cfg.language = *lang;
            }
            out.push(paint(&format!("── {title} ──"), &[DIM, BOLD]));
            let vcs = parse_vcs_info(Some(&p.stdin), &p.probes);
            let memory = parse_memory_info(&p.probes);
            let rendered = render_hud_lines(RenderInput {
                stdin: Some(&p.stdin),
                config: &cfg,
                transcript: &p.transcript,
                vcs: vcs.as_ref(),
                memory: memory.as_ref(),
                cached_quota: None,
                update_info: None,
                columns: Some(explicit_width.unwrap_or(*width)),
                now_ms: ctx.now_ms,
            });
            out.extend(rendered);
            if idx + 1 < scenarios.len() {
                out.push(String::new());
            }
        }
        return CommandOutput {
            lines: out,
            failed: false,
            config_to_write: None,
        };
    }

    if let Some(p) = presets.get(state_arg) {
        let vcs = parse_vcs_info(Some(&p.stdin), &p.probes);
        let memory = parse_memory_info(&p.probes);
        let cols = explicit_width.or(p.stdin.terminal_width).or(Some(108));
        let lines = render_hud_lines(RenderInput {
            stdin: Some(&p.stdin),
            config: &config,
            transcript: &p.transcript,
            vcs: vcs.as_ref(),
            memory: memory.as_ref(),
            cached_quota: None,
            update_info: None,
            columns: cols,
            now_ms: ctx.now_ms,
        });
        return CommandOutput {
            lines,
            failed: false,
            config_to_write: None,
        };
    }

    // Custom inline JSON state
    if let Ok(custom_stdin) = serde_json::from_str::<AgyStdinData>(state_arg) {
        let empty_probes = ProbeAnswers::default();
        let vcs = parse_vcs_info(Some(&custom_stdin), &empty_probes);
        let cols = explicit_width.or(custom_stdin.terminal_width).or(Some(100));
        let lines = render_hud_lines(RenderInput {
            stdin: Some(&custom_stdin),
            config: &config,
            transcript: &TranscriptSummary::default(),
            vcs: vcs.as_ref(),
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: cols,
            now_ms: ctx.now_ms,
        });
        return CommandOutput {
            lines,
            failed: false,
            config_to_write: None,
        };
    }

    CommandOutput {
        lines: vec![format!(
            "Unknown --state \"{state_arg}\". Use one of: showcase, idle, tool_use, confirm, high-context, or a valid JSON object."
        )],
        failed: true,
        config_to_write: None,
    }
}

fn run_set(args: &[String], ctx: &CommandContext<'_>) -> CommandOutput {
    let (_, words) = parse_cli_args(args);
    if words.is_empty() {
        return CommandOutput {
            lines: vec![
                "Usage: `agy-hud set <key>=<value> ...` or `agy-hud set --undo`. Run `agy-hud schema` to inspect available keys.".to_string(),
            ],
            failed: true,
            config_to_write: None,
        };
    }

    let loaded = parse_config_str(ctx.config_text);
    let parsed = parse_set_pairs(&words, &loaded.config);
    if !parsed.problems.is_empty() {
        return CommandOutput {
            lines: parsed.problems,
            failed: true,
            config_to_write: None,
        };
    }

    let mut next_raw = loaded.raw;
    let mut lines = vec![format!(
        "Updated {} setting(s) in {}:",
        parsed.assignments.len(),
        ctx.paths.config
    )];
    for (k, v) in parsed.assignments {
        let formatted_val = serde_json::to_string(&v).unwrap_or_else(|_| "null".to_string());
        lines.push(format!("  {k} = {formatted_val}"));
        set_dot_value(&mut next_raw, &k, v);
    }

    CommandOutput {
        lines,
        failed: false,
        config_to_write: Some(next_raw),
    }
}

fn scrub_stdin(stdin: Option<&AgyStdinData>) -> Option<AgyStdinData> {
    let s = stdin?;
    let mut out = s.clone();
    if out.cwd.is_some() {
        out.cwd = Some("/redacted/project".to_string());
    }
    if out.session_id.is_some() {
        out.session_id = Some("00000000-0000-0000-0000-000000000000".to_string());
    }
    if out.conversation_id.is_some() {
        out.conversation_id = Some("00000000-0000-0000-0000-000000000000".to_string());
    }
    if out.conversation_title.is_some() {
        out.conversation_title = Some("<redacted-title>".to_string());
    }
    if out.transcript_path.is_some() {
        out.transcript_path = Some("/redacted/transcript.jsonl".to_string());
    }
    if out.email.is_some() {
        out.email = Some("user@example.com".to_string());
    }
    if out.workspace.is_some() {
        out.workspace = Some(WorkspaceStdin {
            current_dir: Some("/redacted/project".to_string()),
            project_dir: Some("/redacted/project".to_string()),
        });
    }
    out.extra.clear();
    Some(out)
}

fn run_report(args: &[String], ctx: &CommandContext<'_>) -> CommandOutput {
    let (flags, _) = parse_cli_args(args);
    let note = flags
        .get("--note")
        .and_then(|v| v.first())
        .cloned()
        .unwrap_or_default();
    let loaded = parse_config_str(ctx.config_text);
    let snap = ctx
        .last_snapshot_text
        .and_then(|t| serde_json::from_str::<StoredSnapshot>(t).ok());
    let scrubbed_stdin = scrub_stdin(snap.as_ref().and_then(|s| s.stdin.as_ref()));

    let empty_probes = ProbeAnswers::default();
    let vcs = parse_vcs_info(scrubbed_stdin.as_ref(), &empty_probes);
    let cols = scrubbed_stdin
        .as_ref()
        .and_then(|s| s.terminal_width)
        .or(Some(100));

    let rendered: Vec<String> = render_hud_lines(RenderInput {
        stdin: scrubbed_stdin.as_ref(),
        config: &loaded.config,
        transcript: &TranscriptSummary::default(),
        vcs: vcs.as_ref(),
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: cols,
        now_ms: ctx.now_ms,
    })
    .into_iter()
    .map(|l| strip_ansi(&l))
    .collect();

    let payload = json!({
        "note": note,
        "pluginVersion": ctx.facts.plugin.version,
        "engine": ctx.facts.runtime.engine,
        "engineVersion": ctx.facts.runtime.version,
        "config": loaded.raw,
        "scrubbedStdin": scrubbed_stdin,
        "renderedPlainLines": rendered,
    });

    let pretty = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());
    CommandOutput {
        lines: pretty.lines().map(str::to_string).collect(),
        failed: false,
        config_to_write: None,
    }
}

pub fn execute_subcommand(name: &str, args: &[String], ctx: &CommandContext<'_>) -> CommandOutput {
    match name {
        "doctor" => run_doctor(args, ctx),
        "schema" => run_schema(args, ctx),
        "preview" => run_preview(args, ctx),
        "set" => run_set(args, ctx),
        "report" => run_report(args, ctx),
        other => CommandOutput {
            lines: vec![
                format!("Unknown subcommand \"{other}\"."),
                "Available commands: doctor, install, uninstall, update, preview, schema, set, quota, report"
                    .to_string(),
            ],
            failed: true,
            config_to_write: None,
        },
    }
}
