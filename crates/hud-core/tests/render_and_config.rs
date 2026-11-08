use hud_core::ansi::{strip_ansi, text_width};
use hud_core::commands::{CommandContext, execute_subcommand};
use hud_core::config::{CONFIG_SCHEMA, HudConfig, parse_config_str, parse_set_pairs};
use hud_core::probes::parse_vcs_info;
use hud_core::render::{RenderInput, render_hud_lines};
use hud_core::types::{
    AgyStdinData, ContextWindowStdin, CostStdin, CurrentUsageStdin, DoctorFacts, Language,
    LogicPaths, ModelStdin, PluginFacts, ProbeAnswers, RawQuotaBucket, RuntimeFacts, SandboxStdin,
    TranscriptSummary, VimStdin, WorkspaceStdin,
};
use serde_json::json;
use std::collections::BTreeMap;

fn sample_stdin(width: usize) -> AgyStdinData {
    let mut quota = BTreeMap::new();
    quota.insert(
        "gemini-weekly".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.75),
            remaining_amount: None,
            reset_time: None,
            reset_in_seconds: Some(86_400.0),
            disabled: None,
        },
    );

    AgyStdinData {
        cwd: Some("/Users/dev/demo-app".to_string()),
        workspace: Some(WorkspaceStdin {
            current_dir: Some("/Users/dev/demo-app".to_string()),
            project_dir: Some("/Users/dev/demo-app".to_string()),
        }),
        model: Some(ModelStdin {
            id: Some("Gemini 4 Argon (Medium)".to_string()),
            display_name: Some("Gemini 4 Argon (Medium)".to_string()),
            effort: Some("medium".to_string()),
        }),
        context_window: Some(ContextWindowStdin {
            total_input_tokens: None,
            total_output_tokens: None,
            context_window_size: Some(1_048_576),
            used_percentage: Some(42.5),
            remaining_percentage: Some(57.5),
            current_usage: Some(CurrentUsageStdin {
                input_tokens: Some(400_000),
                output_tokens: Some(5_000),
                cache_creation_input_tokens: Some(0),
                cache_read_input_tokens: Some(100_000),
            }),
        }),
        exceeds_200k_tokens: Some(true),
        quota: Some(quota),
        agent_state: Some("idle".to_string()),
        cycle_mode: Some("plan".to_string()),
        vim: Some(VimStdin {
            mode: Some("INSERT".to_string()),
        }),
        sandbox: Some(SandboxStdin { enabled: Some(true) }),
        terminal_width: Some(width),
        cost: Some(CostStdin {
            total_usd: Some(1.25),
            subagent_usd: None,
            estimated: Some(false),
        }),
        ..Default::default()
    }
}

#[test]
fn adaptive_layout_handles_ultrawide_standard_narrow_and_zh_hans() {
    let probes = ProbeAnswers {
        vcs: Some(
            json!({
                "type": "git",
                "branch": "main",
                "dirty": true,
                "ahead": 1,
                "behind": 0,
                "modified": 2,
                "added": 0,
                "deleted": 0,
                "untracked": 0,
                "conflicts": 0
            })
            .to_string(),
        ),
        memory: None,
    };

    // 1. Ultra-wide 200 cols -> collapses to 1 sleek line when idle without active tools/todos
    let wide_stdin = sample_stdin(200);
    let vcs = parse_vcs_info(Some(&wide_stdin), &probes);
    let wide_lines = render_hud_lines(RenderInput {
        stdin: Some(&wide_stdin),
        config: &HudConfig::default(),
        transcript: &TranscriptSummary::default(),
        vcs: vcs.as_ref(),
        memory: None,
        columns: Some(200),
        now_ms: 1_700_000_000_000,
    });
    assert_eq!(wide_lines.len(), 1);
    let wide_plain = strip_ansi(&wide_lines[0]);
    assert!(wide_plain.contains("READY"));
    assert!(wide_plain.contains("[PLAN]"));
    assert!(wide_plain.contains("[INSERT]"));
    assert!(wide_plain.contains("200k+"));
    assert!(wide_plain.contains("Quota(weekly)"));

    // 2. Narrow 62 cols -> auto-compacts and never overflows 62 display columns
    let mut narrow_stdin = sample_stdin(62);
    narrow_stdin.tool_confirmation_pending = Some(true);
    let narrow_lines = render_hud_lines(RenderInput {
        stdin: Some(&narrow_stdin),
        config: &HudConfig::default(),
        transcript: &TranscriptSummary::default(),
        vcs: vcs.as_ref(),
        memory: None,
        columns: Some(62),
        now_ms: 1_700_000_000_000,
    });
    assert!(narrow_lines.len() >= 2);
    assert!(strip_ansi(&narrow_lines[0]).contains("CONFIRM?"));
    for line in &narrow_lines {
        assert!(
            text_width(line) <= 62,
            "Line exceeded 62 cols ({}): {}",
            text_width(line),
            strip_ansi(line)
        );
    }

    // 3. Simplified Chinese (`zh-Hans`) rendering at 110 cols preserves right-edge sandbox badge
    let zh_cfg = HudConfig {
        language: Language::ZhHans,
        ..HudConfig::default()
    };
    let zh_stdin = sample_stdin(110);
    let zh_lines = render_hud_lines(RenderInput {
        stdin: Some(&zh_stdin),
        config: &zh_cfg,
        transcript: &TranscriptSummary::default(),
        vcs: vcs.as_ref(),
        memory: None,
        columns: Some(110),
        now_ms: 1_700_000_000_000,
    });
    let zh_joined: String = zh_lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(zh_joined.contains("就绪"));
    assert!(zh_joined.contains("[规划]"));
    assert!(zh_joined.contains("上下文"));
    assert!(zh_joined.contains("配额(weekly)"));
    assert!(zh_joined.contains("沙箱开启"));
}

#[test]
fn config_schema_validates_all_38_keys_and_preview_showcase_renders_5_scenarios() {
    assert_eq!(CONFIG_SCHEMA.len(), 38);

    let loaded = parse_config_str(Some(
        r#"{"language":"zh-Hans","pathLevels":"full","vcs":{"cacheTtlMs":2500},"unknownKey":1}"#,
    ));
    assert_eq!(loaded.config.language, Language::ZhHans);
    assert_eq!(loaded.config.vcs.cache_ttl_ms, 2500);
    assert_eq!(loaded.problems.len(), 1);
    assert!(loaded.problems[0].contains("unknownKey"));

    let bad_set = parse_set_pairs(&["display.barLength=999".to_string()], &HudConfig::default());
    assert_eq!(bad_set.problems.len(), 1);

    let facts = DoctorFacts {
        cwd: "/tmp".to_string(),
        runtime: RuntimeFacts {
            engine: "rust-native".to_string(),
            version: "1.0.0".to_string(),
            exec_path: "/tmp/agy-hud".to_string(),
            supported: true,
        },
        plugin: PluginFacts {
            root: "/tmp/plugin".to_string(),
            version: "1.0.0".to_string(),
        },
        config_path: "/tmp/config.json".to_string(),
        heartbeat: None,
    };
    let paths = LogicPaths::default();
    let ctx = CommandContext {
        facts: &facts,
        paths: &paths,
        now_ms: 1_700_000_000_000,
        config_text: None,
        cli_settings_text: None,
        last_snapshot_text: None,
        transcript_head: None,
        transcript_tail: None,
        task_md_text: None,
    };

    let showcase = execute_subcommand(
        "preview",
        &["--state".to_string(), "showcase".to_string()],
        &ctx,
    );
    assert!(!showcase.failed);
    let plain = showcase
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(plain.contains("1) Standard Expanded"));
    assert!(plain.contains("5) 简体中文界面模式"));
}
