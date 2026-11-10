use hud_core::ansi::{sanitize_title_text, strip_ansi, text_width};
use hud_core::commands::{CommandContext, execute_subcommand};
use hud_core::config::{CONFIG_SCHEMA, HudConfig, parse_config_str, parse_set_pairs};
use hud_core::probes::{
    apply_speed_cache, is_newer_semver, parse_github_latest_release, parse_usage_command_output,
    parse_vcs_info,
};
use hud_core::render::{RenderInput, render_hud_lines};
use hud_core::stdin::{extract_quota_buckets_reconciled, format_session_cost, normalize_plan_tier};
use hud_core::transcript::parse_transcript_and_tasks;
use hud_core::types::{
    AgyStdinData, CachedQuotaSnapshot, ContextWindowStdin, CostStdin, CurrentUsageStdin,
    DoctorFacts, EffortFormatMode, GlyphMode, InstalledPluginFacts, Language, LogicPaths,
    ModelStdin, PluginFacts, ProbeAnswers, RawQuotaBucket, RuntimeFacts, SandboxStdin,
    SubagentEntry, SubagentStatus, TodoItem, TodoStatus, ToolEntry, ToolStatus, TranscriptSummary,
    UpdateCacheSnapshot, VimStdin, WorkspaceStdin,
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
        sandbox: Some(SandboxStdin {
            enabled: Some(true),
            allow_network: None,
        }),
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
        quota_cache: None,
        update_cache: None,
        ..Default::default()
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
        cached_quota: None,
        update_info: None,
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
        cached_quota: None,
        update_info: None,
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
        cached_quota: None,
        update_info: None,
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
fn config_schema_validates_all_43_keys_and_preview_showcase_renders_5_scenarios() {
    assert_eq!(CONFIG_SCHEMA.len(), 43);
    let default_cfg = HudConfig::default();
    assert!(!default_cfg.quota.background_refresh);
    assert!(default_cfg.updates.enabled);
    assert!(default_cfg.display.show_speed);
    assert!(!default_cfg.display.show_title);
    assert_eq!(default_cfg.display.glyph_mode, GlyphMode::Unicode);

    let loaded = parse_config_str(Some(
        r#"{"language":"zh-Hans","pathLevels":"full","vcs":{"cacheTtlMs":2500},"quota":{"backgroundRefresh":true},"display":{"showTitle":true,"glyphMode":"nerd"},"unknownKey":1}"#,
    ));
    assert_eq!(loaded.config.language, Language::ZhHans);
    assert_eq!(loaded.config.vcs.cache_ttl_ms, 2500);
    assert!(loaded.config.quota.background_refresh);
    assert!(loaded.config.display.show_title);
    assert_eq!(loaded.config.display.glyph_mode, GlyphMode::Nerd);
    assert_eq!(loaded.problems.len(), 1);
    assert!(loaded.problems[0].contains("unknownKey"));

    let bad_set = parse_set_pairs(
        &["display.barLength=999".to_string()],
        &HudConfig::default(),
    );
    assert_eq!(bad_set.problems.len(), 1);

    let facts = DoctorFacts {
        cwd: "/Users/alice/project".to_string(),
        home_dir: "/Users/alice".to_string(),
        platform: "macos".to_string(),
        terminal: "Ghostty".to_string(),
        remote_session: false,
        runtime: RuntimeFacts {
            engine: "rust-native".to_string(),
            version: "1.0.1".to_string(),
            exec_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/bin/agy-hud".to_string(),
            supported: true,
        },
        plugin: PluginFacts {
            root: "/Users/alice/.gemini/config/plugins/agy-hud".to_string(),
            version: "1.0.1".to_string(),
        },
        installed_plugin_status: "installed".to_string(),
        installed_plugin: Some(InstalledPluginFacts {
            version: "1.0.1".to_string(),
            path: "/Users/alice/.gemini/config/plugins/agy-hud".to_string(),
            enabled: true,
        }),
        config_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/config.json".to_string(),
        heartbeat: None,
    };
    let paths = LogicPaths {
        config: "/Users/alice/.gemini/antigravity-cli/agy-hud/config.json".to_string(),
        cli_settings: "/Users/alice/.gemini/antigravity-cli/settings.json".to_string(),
        ..LogicPaths::default()
    };
    let ctx = CommandContext {
        facts: &facts,
        paths: &paths,
        now_ms: 1_700_000_000_000,
        config_text: None,
        cli_settings_text: None,
        last_snapshot_text: None,
        update_cache_text: None,
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

    // Verify `doctor --pretty` abbreviates home paths to `~` and prints 3-row aligned glyph probe
    let doc_pretty = execute_subcommand("doctor", &["--pretty".to_string()], &ctx);
    assert!(!doc_pretty.failed);
    let doc_plain = doc_pretty
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!doc_plain.contains("/Users/alice"));
    assert!(doc_plain.contains("~/.gemini/antigravity-cli/agy-hud/bin/agy-hud"));
    assert!(doc_plain.contains("Glyph Alignment Probe"));
    let probe_widths: Vec<usize> = doc_pretty
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .filter(|l| {
            l.starts_with("  unicode :")
                || l.starts_with("  nerd    :")
                || l.starts_with("  ascii   :")
        })
        .map(|l| text_width(&l))
        .collect();
    assert_eq!(probe_widths.len(), 3);
    assert_eq!(probe_widths[0], probe_widths[1]);
    assert_eq!(probe_widths[1], probe_widths[2]);
}

#[test]
fn model_aware_quota_routing_expired_windows_and_monotonic_cache_reconciliation() {
    let now_ms = 1_762_650_000_000i64; // 2025-11-09T01:00:00Z
    let active_reset = "2099-01-01T00:00:00Z".to_string();
    let expired_reset = "2020-01-01T00:00:00Z".to_string();

    let mut multi_quota = BTreeMap::new();
    multi_quota.insert(
        "3p-5h".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.90),
            reset_time: Some(active_reset.clone()),
            ..Default::default()
        },
    );
    multi_quota.insert(
        "3p-weekly".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.70),
            reset_time: Some(active_reset.clone()),
            ..Default::default()
        },
    );
    multi_quota.insert(
        "gemini-5h".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.80),
            reset_time: Some(active_reset.clone()),
            ..Default::default()
        },
    );
    multi_quota.insert(
        "gemini-argon-wtu-weekly".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.60),
            reset_time: Some(active_reset.clone()),
            ..Default::default()
        },
    );
    multi_quota.insert(
        "gemini-weekly".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.50),
            reset_time: Some(active_reset.clone()),
            ..Default::default()
        },
    );

    // 1. Pro-plan Argon session -> routes exclusively to `gemini-argon-wtu-weekly`
    let mut argon_stdin = sample_stdin(108);
    argon_stdin.quota = Some(multi_quota.clone());
    let argon_buckets = extract_quota_buckets_reconciled(Some(&argon_stdin), None, now_ms);
    assert_eq!(argon_buckets.len(), 1);
    assert_eq!(argon_buckets[0].id, "gemini-argon-wtu-weekly");
    assert_eq!(argon_buckets[0].label, "argon-weekly");
    assert_eq!(argon_buckets[0].remaining_percent, Some(60.0));

    // 2. Non-Argon Gemini session -> routes to `gemini-5h` + `gemini-weekly`
    let mut gemini_stdin = argon_stdin.clone();
    gemini_stdin.model = Some(ModelStdin {
        id: Some("Gemini 2.5 Pro".to_string()),
        display_name: Some("Gemini 2.5 Pro".to_string()),
        effort: None,
    });
    let gemini_buckets = extract_quota_buckets_reconciled(Some(&gemini_stdin), None, now_ms);
    let gemini_ids: Vec<&str> = gemini_buckets.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(gemini_ids, vec!["gemini-5h", "gemini-weekly"]);

    // 3. Claude / 3P session -> routes to `3p-5h` + `3p-weekly`
    let mut claude_stdin = argon_stdin.clone();
    claude_stdin.model = Some(ModelStdin {
        id: Some("Claude Sonnet 4.6 (Thinking)".to_string()),
        display_name: Some("Claude Sonnet 4.6 (Thinking)".to_string()),
        effort: Some("high".to_string()),
    });
    let claude_buckets = extract_quota_buckets_reconciled(Some(&claude_stdin), None, now_ms);
    let claude_ids: Vec<&str> = claude_buckets.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(claude_ids, vec!["3p-5h", "3p-weekly"]);

    // 4. Expired window is filtered out cleanly
    let mut expired_map = multi_quota.clone();
    expired_map.get_mut("3p-5h").unwrap().reset_time = Some(expired_reset);
    claude_stdin.quota = Some(expired_map);
    let after_expiry = extract_quota_buckets_reconciled(Some(&claude_stdin), None, now_ms);
    assert_eq!(after_expiry.len(), 1);
    assert_eq!(after_expiry[0].id, "3p-weekly");

    // 5. Monotonic reconciliation with `/usage` cache within the same active window
    let usage_stdout = json!({
        "command": {
            "data": {
                "groups": [{
                    "buckets": [{
                        "id": "gemini-argon-wtu-weekly",
                        "remaining_fraction": 0.42,
                        "reset_time": active_reset
                    }]
                }]
            }
        }
    })
    .to_string();
    let parsed_usage = parse_usage_command_output(&usage_stdout).expect("parse /usage JSON");
    let cached_snap = CachedQuotaSnapshot {
        at: now_ms,
        source: "agy-usage".to_string(),
        quota: parsed_usage,
    };
    let reconciled =
        extract_quota_buckets_reconciled(Some(&argon_stdin), Some(&cached_snap), now_ms);
    assert_eq!(reconciled.len(), 1);
    assert_eq!(reconciled[0].remaining_percent, Some(42.0));
    assert_eq!(reconciled[0].used_percent, Some(58.0));
}

#[test]
fn sub_cent_cost_plan_tier_title_zwj_glyph_modes_and_progressive_degradation() {
    assert_eq!(format_session_cost(0.0).as_deref(), Some("$0.00"));
    assert_eq!(format_session_cost(0.0004).as_deref(), Some("<$0.001"));
    assert_eq!(format_session_cost(0.004).as_deref(), Some("$0.004"));
    assert_eq!(format_session_cost(0.38).as_deref(), Some("$0.38"));
    assert_eq!(normalize_plan_tier("Google AI Pro"), "Pro");
    assert_eq!(normalize_plan_tier("Google AI Ultra"), "Ultra");

    // ZWJ family/coder emoji preserved while bidi/ANSI/newlines stripped
    let cleaned_title = sanitize_title_text("  Fix 👨‍💻\n\x1b[31mquota\u{202e}\u{200b} bug  ");
    assert_eq!(cleaned_title, "Fix 👨‍💻 quota bug");

    let mut stdin = sample_stdin(120);
    stdin.plan_tier = Some("Google AI Pro".to_string());
    stdin.conversation_title = Some("Implement pure Rust HUD glyphs".to_string());
    stdin.cost = Some(CostStdin {
        total_usd: Some(0.004),
        subagent_usd: None,
        estimated: Some(true),
    });

    let tx = TranscriptSummary {
        tools: vec![ToolEntry {
            id: "1".to_string(),
            name: "edit_file".to_string(),
            target: Some("render.rs".to_string()),
            status: ToolStatus::Completed,
            count: 2,
            started_at: None,
            ended_at: None,
        }],
        subagents: vec![SubagentEntry {
            name: "research".to_string(),
            role: Some("Auditor".to_string()),
            model: None,
            status: SubagentStatus::Running,
            started_at: Some(1_699_999_970_000),
            ended_at: None,
        }],
        todos: vec![TodoItem {
            text: "Test ASCII mode".to_string(),
            status: TodoStatus::InProgress,
        }],
        session_started_at: Some(1_699_999_400_000),
        last_step_at: Some(1_699_999_990_000),
        step_count: 5,
        ..Default::default()
    };

    // 1. Pure 7-bit ASCII mode check
    let mut ascii_cfg = HudConfig::default();
    ascii_cfg.display.glyph_mode = GlyphMode::Ascii;
    ascii_cfg.display.show_title = true;
    ascii_cfg.display.show_plan_tier = true;
    ascii_cfg.display.effort_format = EffortFormatMode::Symbol;
    let ascii_lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &ascii_cfg,
        transcript: &tx,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(120),
        now_ms: 1_700_000_000_000,
    });
    for line in &ascii_lines {
        assert!(
            line.is_ascii(),
            "Non-ASCII char leaked in glyphMode=ascii: {}",
            strip_ansi(line)
        );
    }
    let ascii_plain = ascii_lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(ascii_plain.contains("~$0.004"));
    assert!(ascii_plain.contains("Pro"));
    assert!(!ascii_plain.contains("Google AI Pro"));
    assert!(ascii_plain.contains("\"Implement pure Rust H...\""));

    // 2. Progressive priority degradation across widths never overflows column budget
    let mut rich_cfg = HudConfig::default();
    rich_cfg.display.show_title = true;
    rich_cfg.display.custom_tag = "TEAM-INFRA".to_string();
    for width in [140usize, 108, 92, 78, 64, 48] {
        let lines = render_hud_lines(RenderInput {
            stdin: Some(&stdin),
            config: &rich_cfg,
            transcript: &tx,
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(width),
            now_ms: 1_700_000_000_000,
        });
        for line in &lines {
            assert!(
                text_width(line) <= width,
                "Exceeded width {width} (got {}): {}",
                text_width(line),
                strip_ansi(line)
            );
        }
    }
}

#[test]
fn update_semver_parsing_badge_rendering_and_doctor_findings() {
    assert!(is_newer_semver("1.1.0", "v1.2.0"));
    assert!(is_newer_semver("v1.1.0", "2.0.0"));
    assert!(!is_newer_semver("1.1.0", "1.1.0"));
    assert!(!is_newer_semver("1.2.0", "1.1.9"));
    assert!(!is_newer_semver("1.1.0", "v1.2.0-rc.1"));

    assert_eq!(
        parse_github_latest_release(
            r#"{"tag_name":"v1.2.0","draft":false,"prerelease":false,"html_url":"https://github.com/maxlee/agy-hud/releases/tag/v1.2.0"}"#
        ),
        Some((
            "1.2.0".to_string(),
            Some("https://github.com/maxlee/agy-hud/releases/tag/v1.2.0".to_string())
        ))
    );
    assert!(
        parse_github_latest_release(r#"{"tag_name":"v1.2.0-beta.1","prerelease":true}"#).is_none()
    );
    assert!(parse_github_latest_release(r#"{"tag_name":"v1.2.0","draft":true}"#).is_none());

    let stdin = sample_stdin(140);
    let update = UpdateCacheSnapshot {
        checked_at: 1_700_000_000_000,
        latest_version: "9.9.0".to_string(),
        release_url: Some("https://github.com/maxlee/agy-hud/releases/tag/v9.9.0".to_string()),
    };

    // 1. Unicode badge
    let uni_lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &HudConfig::default(),
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: Some(&update),
        columns: Some(140),
        now_ms: 1_700_000_000_000,
    });
    let uni_plain = strip_ansi(&uni_lines[0]);
    assert!(uni_plain.contains("↑ v9.9.0 · /agy-hud:hud-update"));

    // 2. ASCII badge (strictly 7-bit)
    let mut ascii_cfg = HudConfig::default();
    ascii_cfg.display.glyph_mode = GlyphMode::Ascii;
    let ascii_lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &ascii_cfg,
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: Some(&update),
        columns: Some(140),
        now_ms: 1_700_000_000_000,
    });
    assert!(ascii_lines[0].is_ascii());
    assert!(strip_ansi(&ascii_lines[0]).contains("^ v9.9.0 . /agy-hud:hud-update"));

    // 3. `updates.enabled = false` hides badge completely
    let mut disabled_cfg = HudConfig::default();
    disabled_cfg.updates.enabled = false;
    let disabled_lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &disabled_cfg,
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: Some(&update),
        columns: Some(140),
        now_ms: 1_700_000_000_000,
    });
    assert!(!strip_ansi(&disabled_lines[0]).contains("/agy-hud:hud-update"));

    // 4. `doctor` surfaces available update in JSON and `--pretty`
    let facts = DoctorFacts {
        cwd: "/Users/alice/project".to_string(),
        home_dir: "/Users/alice".to_string(),
        platform: "macos".to_string(),
        terminal: "Ghostty".to_string(),
        remote_session: false,
        runtime: RuntimeFacts {
            engine: "rust-native".to_string(),
            version: "1.1.0".to_string(),
            exec_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/bin/agy-hud".to_string(),
            supported: true,
        },
        plugin: PluginFacts {
            root: "/Users/alice/.gemini/config/plugins/agy-hud".to_string(),
            version: "1.1.0".to_string(),
        },
        installed_plugin_status: "installed".to_string(),
        installed_plugin: None,
        config_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/config.json".to_string(),
        heartbeat: None,
    };
    let paths = LogicPaths {
        config: "/Users/alice/.gemini/antigravity-cli/agy-hud/config.json".to_string(),
        cli_settings: "/Users/alice/.gemini/antigravity-cli/settings.json".to_string(),
        ..LogicPaths::default()
    };
    let cache_json = serde_json::to_string(&update).unwrap();
    let ctx = CommandContext {
        facts: &facts,
        paths: &paths,
        now_ms: 1_700_000_000_000,
        config_text: None,
        cli_settings_text: None,
        last_snapshot_text: None,
        update_cache_text: Some(&cache_json),
        transcript_head: None,
        transcript_tail: None,
        task_md_text: None,
    };
    let doc = execute_subcommand("doctor", &[], &ctx);
    let doc_val: serde_json::Value = serde_json::from_str(&doc.lines.join("\n")).unwrap();
    assert_eq!(doc_val["updates"]["updateAvailable"], true);
    assert_eq!(doc_val["updates"]["latestVersion"], "9.9.0");
}

#[test]
fn theme_adaptive_palette_plan_tier_default_sandbox_suppression_and_slash_mode() {
    let default_cfg = HudConfig::default();
    assert!(default_cfg.display.show_plan_tier);
    assert_eq!(default_cfg.display.tools_max_visible, 3);

    let mut stdin = sample_stdin(120);
    stdin.cycle_mode = None;
    stdin.plan_tier = Some("Google AI Ultra".to_string());
    stdin.sandbox = Some(SandboxStdin {
        enabled: Some(false),
        allow_network: Some(true),
    });

    let tx = TranscriptSummary {
        tools: vec![
            ToolEntry {
                id: "1".to_string(),
                name: "edit_file".to_string(),
                target: Some("render.rs".to_string()),
                status: ToolStatus::Completed,
                count: 5,
                started_at: None,
                ended_at: None,
            },
            ToolEntry {
                id: "2".to_string(),
                name: "read_file".to_string(),
                target: Some("transcript.rs".to_string()),
                status: ToolStatus::Completed,
                count: 8,
                started_at: None,
                ended_at: None,
            },
        ],
        skills: vec!["plan".to_string(), "hud".to_string()],
        active_slash_mode: Some("plan".to_string()),
        session_started_at: Some(1_699_999_280_000),
        last_step_at: Some(1_699_999_990_000),
        step_count: 14,
        ..Default::default()
    };

    let lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &default_cfg,
        transcript: &tx,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(120),
        now_ms: 1_700_000_000_000,
    });

    // Zero hardcoded white (`37m`/`97m`) or grey (`90m`) foreground codes that turn invisible on Solarized Light
    for line in &lines {
        assert!(
            !line.contains("\x1b[97m"),
            "Leaked BRIGHT_WHITE in: {line:?}"
        );
        assert!(!line.contains("\x1b[37m"), "Leaked WHITE in: {line:?}");
        assert!(!line.contains("\x1b[90m"), "Leaked GRAY in: {line:?}");
    }

    let plain_lines: Vec<String> = lines.iter().map(|l| strip_ansi(l)).collect();
    assert_eq!(
        plain_lines.len(),
        5,
        "Expected 5 semantic rows: {plain_lines:#?}"
    );
    assert!(plain_lines[0].contains("[PLAN]") && plain_lines[0].contains("Ultra"));
    assert!(!plain_lines[0].contains("Google AI Ultra"));
    assert!(plain_lines[1].contains("Ctx") && !plain_lines[1].contains("Quota"));
    assert!(!plain_lines[1].contains("Sandbox off"));
    assert!(plain_lines[2].contains("Quota(weekly)") && !plain_lines[2].contains("Ctx"));
    assert!(
        plain_lines[3].contains("edit_file ×5: render.rs") && !plain_lines[3].contains("Skills:")
    );
    assert!(plain_lines[4].contains("Skills: plan, hud"));
}

#[test]
fn output_speed_estimation_idle_cache_locking_and_header_badge() {
    let steps = [
        json!({
            "step_index": 10,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": "2026-11-09T03:44:24Z",
            "output_tokens": 806,
            "tool_calls": [{ "name": "run_command", "args": { "CommandLine": "cargo test" } }]
        }),
        json!({
            "step_index": 11,
            "source": "MODEL",
            "type": "GENERIC",
            "status": "DONE",
            "created_at": "2026-11-09T03:44:38Z",
            "content": "Command exited with code 0."
        }),
        json!({
            "step_index": 12,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": "2026-11-09T03:44:40Z",
            "output_tokens": 1980,
            "content": "## Final response"
        }),
    ];
    let jsonl = steps
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");

    let mut stdin = sample_stdin(140);
    stdin.conversation_id = Some("conv-speed-test".to_string());
    stdin.plan_tier = Some("Google AI Ultra".to_string());
    stdin.agent_state = Some("idle".to_string());

    // 1. First idle render at `03:45:09Z` (+29s after step 12 started -> `1980 / 29s = 68.3 tok/s`)
    let first_idle_ms = 1_794_195_909_000i64; // 2026-11-09T03:45:09Z
    let mut tx = parse_transcript_and_tasks(jsonl.lines().next(), Some(&jsonl), None, Some(&stdin));
    assert_eq!(tx.completed_step_tps_x10, Some(576)); // Step 10 -> 11: 806 / 14s = 57.6 tok/s
    let locked_cache =
        apply_speed_cache(Some(&stdin), &mut tx, None, first_idle_ms).expect("locks final step");
    assert_eq!(locked_cache.step_index, 12);
    assert_eq!(locked_cache.tps_x10, 683);

    let lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &HudConfig::default(),
        transcript: &tx,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(140),
        now_ms: first_idle_ms,
    });
    let header_plain = strip_ansi(&lines[0]);
    assert!(
        header_plain.contains("[Gemini 4 Argon · med] Ultra · ~68 tok/s"),
        "Unexpected header: {header_plain}"
    );

    // 2. Render 10 minutes later (`+600s`): locked `SpeedCacheSnapshot` prevents decay!
    let ten_min_later = first_idle_ms + 600_000;
    let mut tx_later =
        parse_transcript_and_tasks(jsonl.lines().next(), Some(&jsonl), None, Some(&stdin));
    let next_write = apply_speed_cache(
        Some(&stdin),
        &mut tx_later,
        Some(&locked_cache),
        ten_min_later,
    );
    assert!(next_write.is_none(), "Already locked, must not rewrite");
    let lines_later = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &HudConfig::default(),
        transcript: &tx_later,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(140),
        now_ms: ten_min_later,
    });
    assert!(strip_ansi(&lines_later[0]).contains("[Gemini 4 Argon · med] Ultra · ~68 tok/s"));

    // 3. `display.showSpeed = false` hides the throughput badge cleanly
    let mut hidden_cfg = HudConfig::default();
    hidden_cfg.display.show_speed = false;
    let hidden_lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &hidden_cfg,
        transcript: &tx_later,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(140),
        now_ms: ten_min_later,
    });
    assert!(!strip_ansi(&hidden_lines[0]).contains("tok/s"));
}

#[test]
fn agy_1_5_4_schema_vcs_fallback_show_project_decoupling_and_subagent_statuses() {
    let raw_154 = json!({
        "version": "1.5.4",
        "cwd": "/Users/dev/agy-hud",
        "agent_state": "tool_use",
        "vcs": { "type": "git" },
        "sandbox": { "enabled": true, "allow_network": false },
        "battle": { "status": "voting", "focused_arm": 1 },
        "subagents": [
            { "name": "research", "role": "API Auditor", "status": "running", "model": "gemini-3.1-pro" },
            { "name": "self", "role": "Waiter", "status": "blocked", "model": "gemini-3-flash" },
            { "name": "verifier", "role": "Broken Runner", "status": "error" },
            { "name": "docs", "role": "Doc Writer", "status": "completed" }
        ]
    })
    .to_string();

    let parsed: AgyStdinData = serde_json::from_str(&raw_154).expect("valid 1.5.4 payload");
    assert_eq!(
        parsed.sandbox.as_ref().and_then(|s| s.allow_network),
        Some(false)
    );
    assert_eq!(
        parsed.battle.as_ref().and_then(|b| b.status.as_deref()),
        Some("voting")
    );
    assert_eq!(parsed.battle.as_ref().and_then(|b| b.focused_arm), Some(1));

    // 1. `{"type": "git"}` without `branch` or `client` must NOT produce `git:(git)`
    let empty_probes = ProbeAnswers::default();
    assert!(parse_vcs_info(Some(&parsed), &empty_probes).is_none());

    // 2. CitC / client workspace fallback (`client` populated without `branch`) works & redacts in `report`
    let mut citc_stdin = parsed.clone();
    citc_stdin.vcs.as_mut().unwrap().vcs_type = Some("citc".to_string());
    citc_stdin.vcs.as_mut().unwrap().client = Some("secret-citc-ws".to_string());
    let citc_vcs = parse_vcs_info(Some(&citc_stdin), &empty_probes).expect("client fallback");
    assert_eq!(citc_vcs.branch, "secret-citc-ws");

    let snap_json = json!({
        "stdin": citc_stdin,
        "probes": {}
    })
    .to_string();
    let facts = DoctorFacts {
        cwd: "/Users/dev/agy-hud".to_string(),
        home_dir: "/Users/dev".to_string(),
        platform: "darwin-arm64".to_string(),
        terminal: "xterm-256color".to_string(),
        remote_session: false,
        runtime: RuntimeFacts {
            engine: "rust-native".to_string(),
            version: "1.3.1".to_string(),
            exec_path: "/bin/agy-hud".to_string(),
            supported: true,
        },
        plugin: PluginFacts {
            root: "/plugin".to_string(),
            version: "1.3.1".to_string(),
        },
        installed_plugin_status: "installed".to_string(),
        installed_plugin: None,
        config_path: "/cfg/config.json".to_string(),
        heartbeat: None,
    };
    let paths = LogicPaths::default();
    let report_out = execute_subcommand(
        "report",
        &[],
        &CommandContext {
            config_text: None,
            cli_settings_text: None,
            last_snapshot_text: Some(&snap_json),
            update_cache_text: None,
            transcript_head: None,
            transcript_tail: None,
            task_md_text: None,
            facts: &facts,
            paths: &paths,
            now_ms: 1_700_000_000_000,
        },
    );
    let report_joined = report_out.lines.join("\n");
    assert!(!report_joined.contains("secret-citc-ws"));
    assert!(report_joined.contains("<redacted-client>"));

    // 3. Subagent models & 4 statuses (`Running`, `Blocked`, `Error`, `Completed`) + `showProject=false` decoupling
    let tx = parse_transcript_and_tasks(None, None, None, Some(&parsed));
    assert_eq!(tx.subagents.len(), 4);
    assert_eq!(tx.subagents[0].model.as_deref(), Some("gemini-3.1-pro"));
    assert_eq!(tx.subagents[0].status, SubagentStatus::Running);
    assert_eq!(tx.subagents[1].status, SubagentStatus::Blocked);
    assert_eq!(tx.subagents[2].status, SubagentStatus::Error);
    assert_eq!(tx.subagents[3].status, SubagentStatus::Completed);

    let mut cfg = HudConfig::default();
    cfg.display.show_project = false;
    cfg.vcs.enabled = true;
    let rich_vcs = parse_vcs_info(
        Some(&parsed),
        &ProbeAnswers {
            vcs: Some(json!({ "type": "git", "branch": "feat/154", "dirty": false }).to_string()),
            ..Default::default()
        },
    );
    let lines = render_hud_lines(RenderInput {
        stdin: Some(&parsed),
        config: &cfg,
        transcript: &tx,
        vcs: rich_vcs.as_ref(),
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(140),
        now_ms: 1_700_000_000_000,
    });
    let header_plain = strip_ansi(&lines[0]);
    assert!(
        !header_plain.contains("agy-hud") && header_plain.contains("git:(feat/154)"),
        "showProject=false must still keep VCS when vcs.enabled=true: {header_plain}"
    );
    let subagent_plain = lines
        .iter()
        .map(|l| strip_ansi(l))
        .find(|l| l.contains("API Auditor"))
        .expect("subagents line");
    assert!(
        subagent_plain.contains("◐")
            && subagent_plain.contains("◷")
            && subagent_plain.contains("✗")
            && subagent_plain.contains("✓"),
        "Expected all 4 distinct status glyphs: {subagent_plain}"
    );
}

#[test]
fn hybrid_option_a_speed_strips_ttft_isolates_tool_exec_resists_short_dilution_and_resets_on_model_switch()
 {
    let base_ms = 1_794_195_600_000i64; // 2026-11-09T03:40:00Z
    let mut stdin = sample_stdin(140);
    stdin.conversation_id = Some("conv-option-a".to_string());

    // 1. Live 300ms ticks: 5.2s in `thinking` (TTFT/prefill), then 1.5s in `streaming` producing 191 BPE tokens
    stdin.agent_state = Some("thinking".to_string());
    let mut tx_think = parse_transcript_and_tasks(None, None, None, Some(&stdin));
    let (spd_t0, c0) =
        hud_core::probes::resolve_output_speed(Some(&stdin), &tx_think, None, base_ms + 300);
    assert!(spd_t0.is_none());
    let c0 = c0.expect("records turn_started_at_ms");

    // +5,500ms: first streaming tick (`decode_started_at_ms = base_ms + 5500`)
    stdin.agent_state = Some("streaming".to_string());
    let running_step_t1 = json!({
        "step_index": 1,
        "source": "MODEL",
        "type": "PLANNER_RESPONSE",
        "status": "RUNNING",
        "created_at": "2026-11-09T03:40:00Z",
        "content": "a".repeat(12)
    })
    .to_string();
    tx_think = parse_transcript_and_tasks(
        Some(&running_step_t1),
        Some(&running_step_t1),
        None,
        Some(&stdin),
    );
    let (_, c1) =
        hud_core::probes::resolve_output_speed(Some(&stdin), &tx_think, Some(&c0), base_ms + 5_500);
    let c1 = c1.expect("records decode_started_at_ms");
    assert_eq!(
        c1.active.as_ref().and_then(|a| a.decode_started_at_ms),
        Some(base_ms + 5_500)
    );

    // +6,400ms (+900ms into decode): live streaming ~108 estimated tokens -> live tok/s visible!
    let running_step_t2 = json!({
        "step_index": 1,
        "source": "MODEL",
        "type": "PLANNER_RESPONSE",
        "status": "RUNNING",
        "created_at": "2026-11-09T03:40:00Z",
        "content": "a".repeat(444)
    })
    .to_string();
    let tx_stream = parse_transcript_and_tasks(
        Some(&running_step_t2),
        Some(&running_step_t2),
        None,
        Some(&stdin),
    );
    let (live_spd, c2) = hud_core::probes::resolve_output_speed(
        Some(&stdin),
        &tx_stream,
        Some(&c1),
        base_ms + 6_400,
    );
    let (live_tps, _) = live_spd.expect("live streaming tok/s");
    assert!(
        (110.0..=130.0).contains(&live_tps),
        "unexpected live_tps: {live_tps}"
    );
    let c2 = c2.unwrap();

    // +7,000ms (+1,500ms decode window, 7s total wall time): step 1 finishes and launches `run_command` (`tool_use`)
    stdin.agent_state = Some("tool_use".to_string());
    let done_step_1 = json!({
        "step_index": 1,
        "source": "MODEL",
        "type": "PLANNER_RESPONSE",
        "status": "DONE",
        "created_at": "2026-11-09T03:40:00Z",
        "output_tokens": 191,
        "tool_calls": [{ "name": "run_command", "args": { "CommandLine": "sleep 20" } }]
    })
    .to_string();
    let mut tx_done1 =
        parse_transcript_and_tasks(Some(&done_step_1), Some(&done_step_1), None, Some(&stdin));
    let locked_1 = apply_speed_cache(Some(&stdin), &mut tx_done1, Some(&c2), base_ms + 7_000)
        .expect("locks TTFT-stripped speed immediately at tool_use");
    // 191 tokens / 1.5s = 127.3 tok/s (whereas naive 191 / 7s would have been 27.3 tok/s!)
    assert_eq!(locked_1.tps_x10, 1273);

    // 20 seconds later, `sleep 20` finishes and writes `GENERIC` at `03:40:27Z` -> locked step 1 MUST NOT decay!
    let after_slow_tool = format!(
        "{done_step_1}\n{}",
        json!({
            "step_index": 2,
            "source": "MODEL",
            "type": "GENERIC",
            "status": "DONE",
            "created_at": "2026-11-09T03:40:27Z",
            "content": "done"
        })
    );
    stdin.agent_state = Some("idle".to_string());
    let mut tx_after_tool = parse_transcript_and_tasks(
        after_slow_tool.lines().next(),
        Some(&after_slow_tool),
        None,
        Some(&stdin),
    );
    let rewrite = apply_speed_cache(
        Some(&stdin),
        &mut tx_after_tool,
        Some(&locked_1),
        base_ms + 27_500,
    );
    assert!(
        rewrite.is_none(),
        "locked step must ignore slow tool GENERIC"
    );
    assert_eq!(tx_after_tool.resolved_tps_x10, Some(1273));

    // 2. Coarse transcript resilience: 3,688-token coding step followed by short TTFT-heavy tool calls
    let coarse_steps = [
        json!({ "step_index": 10, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-09T03:41:00Z", "output_tokens": 3688 }),
        json!({ "step_index": 11, "type": "GENERIC", "status": "DONE", "created_at": "2026-11-09T03:41:35Z", "content": "ok" }),
        json!({ "step_index": 12, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-09T03:41:36Z", "output_tokens": 133 }),
        json!({ "step_index": 13, "type": "GENERIC", "status": "DONE", "created_at": "2026-11-09T03:41:40Z", "content": "ok" }),
        json!({ "step_index": 14, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-09T03:41:41Z", "output_tokens": 191 }),
        json!({ "step_index": 15, "type": "GENERIC", "status": "DONE", "created_at": "2026-11-09T03:41:49Z", "content": "ok" }),
    ];
    let coarse_jsonl = coarse_steps
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    let tx_coarse = parse_transcript_and_tasks(
        coarse_jsonl.lines().next(),
        Some(&coarse_jsonl),
        None,
        Some(&stdin),
    );
    let coarse_tps = tx_coarse.completed_step_tps_x10.unwrap() as f64 / 10.0;
    assert!(
        coarse_tps >= 100.0,
        "Short TTFT-heavy tool steps must not crash 105 tok/s rate (got {coarse_tps})"
    );

    // 3. Mid-session model switch (`Gemini 4 Argon` -> `Gemini 3 Flash`) invalidates stale Pro speed
    let mut switched_stdin = stdin.clone();
    switched_stdin.model = Some(ModelStdin {
        id: Some("Gemini 3 Flash".to_string()),
        display_name: Some("Gemini 3 Flash".to_string()),
        effort: None,
    });
    let mut tx_switch = tx_after_tool.clone();
    let barrier_cache = apply_speed_cache(
        Some(&switched_stdin),
        &mut tx_switch,
        Some(&locked_1),
        base_ms + 30_000,
    )
    .expect("writes model-switch barrier");
    assert_eq!(tx_switch.resolved_tps_x10, None);
    assert_eq!(tx_switch.completed_step_tps_x10, None);

    // Sub-2s fast Flash burst (120 tokens in 800ms = 150.0 tok/s) succeeds on the new model!
    let flash_step = format!(
        "{after_slow_tool}\n{}",
        json!({
            "step_index": 3,
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": "2026-11-09T03:40:31Z",
            "output_tokens": 120,
            "content": "Fast flash answer"
        })
    );
    let mut tx_flash = parse_transcript_and_tasks(
        flash_step.lines().next(),
        Some(&flash_step),
        None,
        Some(&switched_stdin),
    );
    let flash_locked = apply_speed_cache(
        Some(&switched_stdin),
        &mut tx_flash,
        Some(&barrier_cache),
        base_ms + 31_800,
    )
    .expect("locks 800ms Flash burst");
    assert_eq!(flash_locked.tps_x10, 1500);
    assert_eq!(tx_flash.resolved_tps_x10, Some(1500));

    // 4. Stale cold-idle window (>45s with low apparent rate) and `tool_confirmation_pending` are rejected
    let stale_tx =
        parse_transcript_and_tasks(Some(&done_step_1), Some(&done_step_1), None, Some(&stdin));
    let (stale_spd, _) =
        hud_core::probes::resolve_output_speed(Some(&stdin), &stale_tx, None, base_ms + 120_000);
    assert!(
        stale_spd.is_none(),
        "2-minute stale idle must not lock ~1.6 tok/s"
    );

    let mut confirm_stdin = stdin.clone();
    confirm_stdin.tool_confirmation_pending = Some(true);
    let (confirm_spd, _) = hud_core::probes::resolve_output_speed(
        Some(&confirm_stdin),
        &stale_tx,
        None,
        base_ms + 8_000,
    );
    assert!(confirm_spd.is_none());
}
