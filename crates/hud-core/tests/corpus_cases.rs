use hud_core::ansi::{strip_ansi, text_width};
use hud_core::commands::{CommandContext, execute_subcommand};
use hud_core::config::parse_config_str;
use hud_core::probes::{
    parse_cached_quota_info, parse_cost_ledger_info, parse_memory_info, parse_vcs_info,
    reconcile_cost_ledger,
};
use hud_core::render::{RenderInput, render_hud_lines_full};
use hud_core::types::{
    AgyStdinData, DoctorFacts, LogicPaths, PluginFacts, ProbeAnswers, RuntimeFacts,
    TranscriptSummary,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CorpusCase {
    id: String,
    now_ms: i64,
    columns: Option<usize>,
    #[serde(default)]
    config: Value,
    stdin: Option<AgyStdinData>,
    #[serde(default)]
    probes: ProbeAnswers,
    #[serde(default)]
    transcript: TranscriptSummary,
    expected_plain_lines: Vec<String>,
}

fn evaluate_corpus_case(case: &CorpusCase) -> Result<(), String> {
    let config_json = if case.config.is_null() {
        "{}".to_string()
    } else {
        serde_json::to_string(&case.config).expect("serialize case config")
    };
    let loaded = parse_config_str(Some(&config_json));
    if !loaded.problems.is_empty() {
        return Err(format!(
            "case '{}' had invalid config problems: {:?}",
            case.id, loaded.problems
        ));
    }

    let vcs = parse_vcs_info(case.stdin.as_ref(), &case.probes);
    let memory = parse_memory_info(&case.probes);
    let cached_quota = parse_cached_quota_info(&case.probes);
    let ledger_snap = parse_cost_ledger_info(&case.probes);
    let (cost_totals, _) =
        reconcile_cost_ledger(ledger_snap.as_ref(), case.stdin.as_ref(), case.now_ms);

    let live_cols = case
        .columns
        .or_else(|| case.stdin.as_ref().and_then(|s| s.terminal_width));
    let rendered = render_hud_lines_full(
        RenderInput {
            stdin: case.stdin.as_ref(),
            config: &loaded.config,
            transcript: &case.transcript,
            vcs: vcs.as_ref(),
            memory: memory.as_ref(),
            cached_quota: cached_quota.as_ref(),
            update_info: None,
            columns: live_cols,
            now_ms: case.now_ms,
        },
        cost_totals.as_ref(),
    );

    let effective_width = match (loaded.config.max_width, live_cols) {
        (Some(mw), Some(lw)) => Some(mw.min(lw)),
        (Some(mw), None) => Some(mw),
        (None, lw) => lw,
    };
    if let Some(ew) = effective_width {
        for (idx, line) in rendered.iter().enumerate() {
            let w = text_width(line);
            if w > ew {
                return Err(format!(
                    "case '{}' line {idx} exceeded effective_width={ew} (got {w}): {}",
                    case.id,
                    strip_ansi(line)
                ));
            }
        }
    }

    let plain_lines: Vec<String> = rendered.iter().map(|l| strip_ansi(l)).collect();
    if plain_lines != case.expected_plain_lines {
        return Err(format!(
            "case '{}' rendered lines diverged.\nActual JSON:\n{}",
            case.id,
            serde_json::to_string_pretty(&plain_lines).unwrap()
        ));
    }
    Ok(())
}

#[test]
fn declarative_corpus_cases_match_golden_snapshots_and_width_budgets() {
    let corpus_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut entries: Vec<PathBuf> = fs::read_dir(&corpus_dir)
        .expect("read tests/corpus directory")
        .filter_map(|e| e.ok().map(|d| d.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    entries.sort();
    assert!(
        entries.len() >= 5,
        "expected at least 5 declarative corpus JSON fixtures in {}, found {}",
        corpus_dir.display(),
        entries.len()
    );

    let mut failures = Vec::new();
    for path in entries {
        let raw = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed reading {}: {e}", path.display()));
        let case: CorpusCase = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("failed parsing {}: {e}", path.display()));
        if let Err(msg) = evaluate_corpus_case(&case) {
            failures.push(msg);
        }
    }
    assert!(
        failures.is_empty(),
        "Corpus failures ({}):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn scrubbed_report_case_roundtrips_directly_into_corpus_evaluator() {
    let snap = json!({
        "stdin": {
            "cwd": "/Users/alice/private-fintech-app/services/ledger",
            "conversation_id": "conv-secret-999",
            "conversation_title": "Fix Stripe webhook race condition",
            "agent_state": "streaming",
            "cycle_mode": "auto_edit",
            "plan_tier": "Ultra",
            "terminal_width": 116,
            "workspace": {
                "project_dir": "/Users/alice/private-fintech-app",
                "current_dir": "/Users/alice/private-fintech-app/services/ledger"
            },
            "model": {
                "id": "Gemini 4 Argon (High)",
                "display_name": "Gemini 4 Argon (High)",
                "effort": "high"
            },
            "context_window": {
                "context_window_size": 1048576,
                "used_percentage": 38.0
            },
            "quota": {
                "gemini-5h": {
                    "remaining_fraction": 0.65,
                    "reset_in_seconds": 14400.0
                }
            },
            "cost": {
                "total_usd": 0.92,
                "estimated": false
            }
        },
        "probes": {
            "vcs": json!({
                "type": "git",
                "branch": "feat/confidential-ledger-fix",
                "dirty": true,
                "ahead": 3,
                "repoUrl": "https://github.com/private-org/private-fintech-app"
            }).to_string()
        }
    })
    .to_string();

    let facts = DoctorFacts {
        cwd: "/Users/alice/private-fintech-app/services/ledger".to_string(),
        home_dir: "/Users/alice".to_string(),
        platform: "macos".to_string(),
        terminal: "Ghostty".to_string(),
        remote_session: false,
        runtime: RuntimeFacts {
            engine: "rust-native".to_string(),
            version: "1.4.0".to_string(),
            exec_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/bin/agy-hud".to_string(),
            supported: true,
        },
        plugin: PluginFacts {
            root: "/Users/alice/.gemini/config/plugins/agy-hud".to_string(),
            version: "1.4.0".to_string(),
        },
        installed_plugin_status: "installed".to_string(),
        installed_plugin: None,
        config_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/config.json".to_string(),
        heartbeat: None,
    };

    let out = execute_subcommand(
        "report",
        &["Corpus roundtrip check".to_string()],
        &CommandContext {
            config_text: Some(r#"{"display":{"showLaunchDir":true,"showTitle":true}}"#),
            cli_settings_text: None,
            last_snapshot_text: Some(&snap),
            update_cache_text: None,
            transcript_head: None,
            transcript_tail: None,
            task_md_text: None,
            facts: &facts,
            paths: &LogicPaths::default(),
            now_ms: 1_794_218_400_000,
        },
    );
    assert!(!out.failed);
    let report_json: Value =
        serde_json::from_str(&out.lines.join("\n")).expect("valid report JSON");
    let exported_case: CorpusCase =
        serde_json::from_value(report_json["case"].clone()).expect("deserialize report.case");

    // Must contain zero raw secrets and evaluate deterministically to the exact scrubbed output!
    let raw_case_str = serde_json::to_string(&exported_case).unwrap();
    assert!(!raw_case_str.contains("private-fintech-app"));
    assert!(!raw_case_str.contains("confidential-ledger-fix"));
    assert!(!raw_case_str.contains("Stripe webhook"));
    evaluate_corpus_case(&exported_case).expect("exported report case must evaluate cleanly");
}
