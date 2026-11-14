use hud_core::ansi::{
    github_branch_link, mask_preserving_width, sanitize_title_text, strip_ansi,
    terminal_filter_with_repo, text_width,
};
use hud_core::commands::{CommandContext, execute_subcommand};
use hud_core::config::{CONFIG_SCHEMA, HudConfig, parse_config_str, parse_set_pairs};
use hud_core::probes::{
    apply_speed_cache, is_newer_semver, normalize_github_repo_url, parse_cost_ledger_info,
    parse_github_latest_release, parse_usage_command_output, parse_vcs_info, reconcile_cost_ledger,
};
use hud_core::render::{
    RenderInput, format_config_problem_line, render_hud_lines, render_hud_lines_full,
};
use hud_core::stdin::{
    compute_quota_pace, extract_quota_buckets_reconciled, format_session_cost,
    format_workspace_breadcrumb, normalize_plan_tier,
};
use hud_core::transcript::{parse_transcript_and_tasks, parse_transcript_with_cursor};
use hud_core::types::{
    AgyStdinData, CachedQuotaSnapshot, ContextScaleMode, ContextValueMode, ContextWindowStdin,
    CostStdin, CurrentUsageStdin, DoctorFacts, EffortFormatMode, GlyphMode, InstalledPluginFacts,
    Language, LogicPaths, ModelStdin, PathLevels, PluginFacts, ProbeAnswers, QuotaBucketView,
    QuotaPace, RawQuotaBucket, RuntimeFacts, SandboxStdin, SubagentEntry, SubagentStatus, TodoItem,
    TodoStatus, ToolEntry, ToolStatus, TranscriptCursor, TranscriptSummary, UpdateCacheSnapshot,
    VimStdin, WorkspaceStdin,
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
fn config_schema_validates_all_53_keys_and_preview_showcase_renders_5_scenarios() {
    assert_eq!(CONFIG_SCHEMA.len(), 53);
    let default_cfg = HudConfig::default();
    assert_eq!(
        default_cfg.display.context_scale,
        ContextScaleMode::Compaction
    );
    assert!(!default_cfg.quota.background_refresh);
    assert!(default_cfg.updates.enabled);
    assert!(default_cfg.display.show_speed);
    assert!(!default_cfg.display.show_title);
    assert!(!default_cfg.display.show_launch_dir);
    assert!(default_cfg.display.quota_pace);
    assert_eq!(default_cfg.display.quota_threshold, 0);
    assert!(!default_cfg.display.show_daily_cost);
    assert!(!default_cfg.display.show_weekly_cost);
    assert!(default_cfg.display.merge_groups.is_empty());
    assert!(default_cfg.display.right_align.is_empty());
    assert_eq!(default_cfg.vcs.push_warning_threshold, 0);
    assert_eq!(default_cfg.vcs.push_critical_threshold, 0);
    assert_eq!(default_cfg.display.glyph_mode, GlyphMode::Unicode);

    let loaded = parse_config_str(Some(
        r#"{"language":"zh-Hans","pathLevels":"full","vcs":{"cacheTtlMs":2500,"pushWarningThreshold":5,"pushCriticalThreshold":15},"quota":{"backgroundRefresh":true},"display":{"showTitle":true,"glyphMode":"nerd","showLaunchDir":false,"quotaPace":false,"quotaThreshold":25,"showDailyCost":true,"showWeeklyCost":true,"mergeGroups":[["context","quota"],["tools","subagents"]],"rightAlign":["cost","duration"]},"unknownKey":1}"#,
    ));
    assert_eq!(loaded.config.language, Language::ZhHans);
    assert_eq!(loaded.config.vcs.cache_ttl_ms, 2500);
    assert_eq!(loaded.config.vcs.push_warning_threshold, 5);
    assert_eq!(loaded.config.vcs.push_critical_threshold, 15);
    assert!(loaded.config.quota.background_refresh);
    assert!(loaded.config.display.show_title);
    assert!(!loaded.config.display.show_launch_dir);
    assert!(!loaded.config.display.quota_pace);
    assert_eq!(loaded.config.display.quota_threshold, 25);
    assert!(loaded.config.display.show_daily_cost);
    assert!(loaded.config.display.show_weekly_cost);
    assert_eq!(loaded.config.display.merge_groups.len(), 2);
    assert_eq!(loaded.config.display.right_align, vec!["cost", "duration"]);
    assert_eq!(loaded.config.display.glyph_mode, GlyphMode::Nerd);
    assert_eq!(loaded.problems.len(), 1);
    assert!(loaded.problems[0].contains("unknownKey"));

    // Invalid mergeGroups (duplicate row across groups / < 2 items) and invalid rightAlign segment must report problems
    let bad_layout = parse_config_str(Some(
        r#"{"display":{"mergeGroups":[["context","quota"],["quota","counts"]],"rightAlign":["bogus"],"quotaThreshold":100}}"#,
    ));
    assert!(bad_layout.problems.len() >= 3);
    assert!(bad_layout.config.display.merge_groups.is_empty());
    assert!(bad_layout.config.display.right_align.is_empty());
    assert_eq!(bad_layout.config.display.quota_threshold, 0);

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
        header_plain.contains("[Gemini 4 Argon · med] Ultra · 🛵 ~68 tok/s (58~68)"),
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
    assert!(
        strip_ansi(&lines_later[0]).contains("[Gemini 4 Argon · med] Ultra · 🛵 ~68 tok/s (58~68)")
    );

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
    let hidden_header = strip_ansi(&hidden_lines[0]);
    assert!(!hidden_header.contains("tok/s"));
    assert!(!hidden_header.contains("🛵"));
    assert!(!hidden_header.contains("(58~68)"));
}

#[test]
fn speed_badge_header_integration_range_hiding_glyph_modes_model_switch_and_narrow_drop() {
    let base_ms = 1_794_195_600_000i64; // 2026-11-09T03:40:00Z
    let mut stdin = sample_stdin(140);
    stdin.conversation_id = Some("conv-speed-header-int".to_string());
    stdin.plan_tier = Some("Google AI Ultra".to_string());
    stdin.agent_state = Some("idle".to_string());

    // 1. Single completed sample (`min == max`) hides `(min~max)` in the rendered header
    let single_step_jsonl = [
        json!({
            "step_index": 1,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": "2026-11-09T03:40:00Z",
            "output_tokens": 806
        }),
        json!({
            "step_index": 2,
            "source": "MODEL",
            "type": "GENERIC",
            "status": "DONE",
            "created_at": "2026-11-09T03:40:14Z",
            "content": "ok"
        }),
    ]
    .iter()
    .map(serde_json::to_string)
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
    .join("\n");
    let mut tx_single = parse_transcript_and_tasks(
        single_step_jsonl.lines().next(),
        Some(&single_step_jsonl),
        None,
        Some(&stdin),
    );
    assert_eq!(tx_single.session_min_tps_x10, Some(576));
    assert_eq!(tx_single.session_max_tps_x10, Some(576));
    let single_cache = apply_speed_cache(Some(&stdin), &mut tx_single, None, base_ms + 15_000)
        .expect("locks single step");
    let single_lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &HudConfig::default(),
        transcript: &tx_single,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(140),
        now_ms: base_ms + 15_000,
    });
    let single_header = strip_ansi(&single_lines[0]);
    assert!(
        single_header.contains("[Gemini 4 Argon · med] Ultra · 🛵 ~58 tok/s"),
        "Unexpected single-sample header: {single_header}"
    );
    assert!(
        !single_header.contains('(') && !single_header.contains("58~58"),
        "Single-sample range must stay hidden: {single_header}"
    );

    // Distinct x10 bounds that round to the same integer (68.1 vs 68.4 -> 68) also hide `(min~max)`
    let mut tx_same_rounded = tx_single.clone();
    tx_same_rounded.resolved_tps_x10 = Some(683);
    tx_same_rounded.session_min_tps_x10 = Some(681);
    tx_same_rounded.session_max_tps_x10 = Some(684);
    let same_rounded_header = strip_ansi(
        &render_hud_lines(RenderInput {
            stdin: Some(&stdin),
            config: &HudConfig::default(),
            transcript: &tx_same_rounded,
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(140),
            now_ms: base_ms + 15_000,
        })[0],
    );
    assert!(
        same_rounded_header.contains("[Gemini 4 Argon · med] Ultra · 🛵 ~68 tok/s"),
        "Unexpected same-rounded header: {same_rounded_header}"
    );
    assert!(!same_rounded_header.contains("68~68"));

    // 2. Mid-session model switch preserves `(min~max)` across models in the rendered header
    let mut switched_stdin = stdin.clone();
    switched_stdin.model = Some(ModelStdin {
        id: Some("Gemini 3 Flash".to_string()),
        display_name: Some("Gemini 3 Flash".to_string()),
        effort: None,
    });
    let mut tx_barrier = tx_single.clone();
    let barrier_cache = apply_speed_cache(
        Some(&switched_stdin),
        &mut tx_barrier,
        Some(&single_cache),
        base_ms + 20_000,
    )
    .expect("writes model-switch barrier");
    assert_eq!(tx_barrier.resolved_tps_x10, None);
    assert_eq!(tx_barrier.completed_step_tps_x10, None);
    assert!(tx_barrier.latest_planner_step.is_none());
    assert_eq!(tx_barrier.session_min_tps_x10, Some(576));
    assert_eq!(tx_barrier.session_max_tps_x10, Some(576));
    let barrier_header = strip_ansi(
        &render_hud_lines(RenderInput {
            stdin: Some(&switched_stdin),
            config: &HudConfig::default(),
            transcript: &tx_barrier,
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(140),
            now_ms: base_ms + 20_000,
        })[0],
    );
    assert!(
        !barrier_header.contains("tok/s")
            && !barrier_header.contains("🛵")
            && !barrier_header.contains('('),
        "Immediate barrier render must hide stale model speed/emoji/range: {barrier_header}"
    );

    // Subsequent idle render with a freshly parsed transcript + persisted `barrier_cache` also suppresses stale speed
    let mut tx_barrier_reparse = parse_transcript_and_tasks(
        single_step_jsonl.lines().next(),
        Some(&single_step_jsonl),
        None,
        Some(&switched_stdin),
    );
    let reparse_write = apply_speed_cache(
        Some(&switched_stdin),
        &mut tx_barrier_reparse,
        Some(&barrier_cache),
        base_ms + 20_500,
    );
    assert!(reparse_write.is_none());
    assert!(tx_barrier_reparse.latest_planner_step.is_none());
    assert_eq!(tx_barrier_reparse.session_min_tps_x10, Some(576));
    assert_eq!(tx_barrier_reparse.session_max_tps_x10, Some(576));
    let reparse_barrier_header = strip_ansi(
        &render_hud_lines(RenderInput {
            stdin: Some(&switched_stdin),
            config: &HudConfig::default(),
            transcript: &tx_barrier_reparse,
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(140),
            now_ms: base_ms + 20_500,
        })[0],
    );
    assert!(
        !reparse_barrier_header.contains("tok/s")
            && !reparse_barrier_header.contains("🛵")
            && !reparse_barrier_header.contains('('),
        "Subsequent barrier render must also hide stale model speed/emoji/range: {reparse_barrier_header}"
    );

    // New model completes a fast 150.0 tok/s step (120 tokens in 800ms -> 🚅 ~150 tok/s (58~150))
    let flash_jsonl = format!(
        "{single_step_jsonl}\n{}",
        json!({
            "step_index": 3,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": "2026-11-09T03:40:21Z",
            "output_tokens": 120,
            "content": "Fast flash answer"
        })
    );
    let mut tx_flash = parse_transcript_and_tasks(
        flash_jsonl.lines().next(),
        Some(&flash_jsonl),
        None,
        Some(&switched_stdin),
    );
    let _flash_locked = apply_speed_cache(
        Some(&switched_stdin),
        &mut tx_flash,
        Some(&barrier_cache),
        base_ms + 21_800,
    )
    .expect("locks Flash burst");
    let flash_lines = render_hud_lines(RenderInput {
        stdin: Some(&switched_stdin),
        config: &HudConfig::default(),
        transcript: &tx_flash,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(140),
        now_ms: base_ms + 21_800,
    });
    let flash_header = strip_ansi(&flash_lines[0]);
    assert!(
        flash_header.contains("[Gemini 3 Flash] Ultra · 🚅 ~150 tok/s (58~150)"),
        "Expected cross-model min~max preserved in header: {flash_header}"
    );

    // 3. `GlyphMode::Ascii` omits emoji while preserving `(min~max)`, and `Nerd` keeps emoji
    let mut ascii_cfg = HudConfig::default();
    ascii_cfg.display.glyph_mode = GlyphMode::Ascii;
    let ascii_header = strip_ansi(
        &render_hud_lines(RenderInput {
            stdin: Some(&switched_stdin),
            config: &ascii_cfg,
            transcript: &tx_flash,
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(140),
            now_ms: base_ms + 21_800,
        })[0],
    );
    assert!(
        ascii_header.contains("[Gemini 3 Flash] Ultra . ~150 tok/s (58~150)"),
        "Expected ASCII speed badge without emoji: {ascii_header}"
    );
    assert!(!ascii_header.contains("🚅"));

    let mut nerd_cfg = HudConfig::default();
    nerd_cfg.display.glyph_mode = GlyphMode::Nerd;
    let nerd_header = strip_ansi(
        &render_hud_lines(RenderInput {
            stdin: Some(&switched_stdin),
            config: &nerd_cfg,
            transcript: &tx_flash,
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(140),
            now_ms: base_ms + 21_800,
        })[0],
    );
    assert!(
        nerd_header.contains("[Gemini 3 Flash] Ultra · 🚅 ~150 tok/s (58~150)"),
        "Expected Nerd mode speed badge with emoji: {nerd_header}"
    );

    // 4. Narrow-terminal fallback drops `show_speed` cleanly without dangling fragments
    let narrow_cfg = HudConfig {
        line_layout: hud_core::types::LineLayout::Expanded,
        ..Default::default()
    };
    let narrow_lines = render_hud_lines(RenderInput {
        stdin: Some(&switched_stdin),
        config: &narrow_cfg,
        transcript: &tx_flash,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(68),
        now_ms: base_ms + 21_800,
    });
    let narrow_header = strip_ansi(&narrow_lines[0]);
    assert!(
        text_width(&narrow_lines[0]) <= 68,
        "Narrow header exceeded 68 cols: {narrow_header}"
    );
    assert!(
        narrow_header.contains("[Gemini 3 Flash]"),
        "Narrow header should preserve model badge: {narrow_header}"
    );
    assert!(
        !narrow_header.contains("tok/s")
            && !narrow_header.contains("🚅")
            && !narrow_header.contains("(58~150)")
            && !narrow_header.contains("· │"),
        "Narrow header must drop speed badge cleanly without dangling fragments: {narrow_header}"
    );
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

#[test]
fn quota_pace_burn_rate_threshold_hiding_and_static_severity_floor() {
    let mk_bucket = |id: &str, used: f64, reset_sec: f64| QuotaBucketView {
        id: id.to_string(),
        label: id.to_string(),
        remaining_percent: Some(100.0 - used),
        used_percent: Some(used),
        remaining_amount: None,
        reset_in_seconds: Some(reset_sec),
    };

    // 1. < 10% used at start of window must NOT trigger false-positive pace alarm
    assert_eq!(
        compute_quota_pace(&mk_bucket("gemini-5h", 8.0, 17_900.0)),
        QuotaPace::Normal
    );

    // 2. 5h window (`18_000s`): 1h elapsed (`14_400s` left => 20% elapsed), 30% used => projected 150% => Critical!
    assert_eq!(
        compute_quota_pace(&mk_bucket("gemini-5h", 30.0, 14_400.0)),
        QuotaPace::Critical
    );

    // 3. weekly window (`604_800s`): half elapsed (`302_400s` left => 50% elapsed), 47.5% used => projected 95% => Warning!
    assert_eq!(
        compute_quota_pace(&mk_bucket("gemini-weekly", 47.5, 302_400.0)),
        QuotaPace::Warning
    );

    // 4. Static critical (`>= 90%` used) near end of window (`projected ~ 97%` = Warning) MUST stay Red (never downgraded!)
    let mut stdin = sample_stdin(120);
    let mut quota = BTreeMap::new();
    quota.insert(
        "gemini-5h".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.08), // 92% used (Critical static!)
            remaining_amount: None,
            reset_time: None,
            reset_in_seconds: Some(900.0), // 95% elapsed => 92 / 0.95 = 96.8% (Warning pace)
            disabled: None,
        },
    );
    stdin.quota = Some(quota);
    let lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &HudConfig::default(),
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(120),
        now_ms: 1_700_000_000_000,
    });
    assert!(
        lines[2].contains("\x1b[31m") && strip_ansi(&lines[2]).contains("▲"),
        "Static >=90% must stay red while displaying pace marker: {}",
        lines[2]
    );

    // 5. `display.quotaThreshold`: hides normal low-usage quota, UNLESS pace > Normal!
    let mut thr_cfg = HudConfig::default();
    thr_cfg.display.quota_threshold = 50;

    // 25% used with 1 day left on weekly (`85.7%` elapsed => projected ~29% = Normal) -> hidden!
    let low_normal = sample_stdin(120);
    let hidden_lines = render_hud_lines(RenderInput {
        stdin: Some(&low_normal),
        config: &thr_cfg,
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(120),
        now_ms: 1_700_000_000_000,
    });
    let hidden_plain: String = hidden_lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !hidden_plain.contains("Quota"),
        "Normal low-usage quota below threshold should hide: {hidden_plain}"
    );

    // 30% used in 1h of 5h window (below 50% threshold, but Critical pace!) -> MUST surface with `^` in Ascii mode!
    let mut fast_burn = sample_stdin(120);
    let mut fast_q = BTreeMap::new();
    fast_q.insert(
        "gemini-5h".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.70), // 30% used (< 50 threshold!)
            remaining_amount: None,
            reset_time: None,
            reset_in_seconds: Some(14_400.0), // 20% elapsed -> 150% projected!
            disabled: None,
        },
    );
    fast_burn.quota = Some(fast_q);
    thr_cfg.display.glyph_mode = GlyphMode::Ascii;
    let surfaced_lines = render_hud_lines(RenderInput {
        stdin: Some(&fast_burn),
        config: &thr_cfg,
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(120),
        now_ms: 1_700_000_000_000,
    });
    let surfaced_plain = strip_ansi(&surfaced_lines[2]);
    assert!(
        surfaced_plain.contains("Quota(5h)") && surfaced_plain.contains("^"),
        "Fast-burning quota must override threshold and use ASCII '^': {surfaced_plain}"
    );
}

#[test]
fn completed_subagent_60s_retirement_active_priority_and_config_problem_line() {
    let now_ms = 1_700_000_100_000i64;
    let stdin = sample_stdin(120);
    let tx = TranscriptSummary {
        subagents: vec![
            SubagentEntry {
                name: "done-stale".to_string(),
                role: Some("Old Auditor".to_string()),
                model: None,
                status: SubagentStatus::Completed,
                started_at: Some(now_ms - 95_000),
                ended_at: Some(now_ms - 65_000), // > 60s ago -> retired!
            },
            SubagentEntry {
                name: "done-fresh".to_string(),
                role: Some("Fresh Reviewer".to_string()),
                model: None,
                status: SubagentStatus::Completed,
                started_at: Some(now_ms - 40_000),
                ended_at: Some(now_ms - 20_000), // <= 60s ago -> kept, sorted AFTER active!
            },
            SubagentEntry {
                name: "active-worker".to_string(),
                role: Some("Live Builder".to_string()),
                model: Some("gemini-3-flash".to_string()),
                status: SubagentStatus::Running,
                started_at: Some(now_ms - 12_000),
                ended_at: None,
            },
        ],
        ..TranscriptSummary::default()
    };

    let lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &HudConfig::default(),
        transcript: &tx,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(120),
        now_ms,
    });
    let sub_line = lines
        .iter()
        .map(|l| strip_ansi(l))
        .find(|l| l.contains("Live Builder"))
        .expect("subagent row present");
    assert!(
        !sub_line.contains("Old Auditor"),
        "Completed >60s must retire: {sub_line}"
    );
    let live_idx = sub_line.find("Live Builder").unwrap();
    let fresh_idx = sub_line.find("Fresh Reviewer").unwrap();
    assert!(
        live_idx < fresh_idx,
        "Active subagents must precede completed: {sub_line}"
    );

    // Config problem line preserves `/agy-hud:hud` even at ultra-narrow 42 columns!
    let mut ascii_cfg = HudConfig::default();
    ascii_cfg.display.glyph_mode = GlyphMode::Ascii;
    let problems = vec![
        "Unknown configuration key \"display.superLongInvalidPropertyNameForNarrowTerminal\""
            .to_string(),
    ];
    let warn = format_config_problem_line(&problems, &ascii_cfg, Some(42)).expect("warning line");
    let warn_plain = strip_ansi(&warn);
    assert!(
        warn_plain.ends_with(" . /agy-hud:hud"),
        "Remediation command must never be truncated off: {warn_plain}"
    );
    assert!(
        text_width(&warn_plain) <= 42,
        "Problem line must fit 42 columns (got {}): {warn_plain}",
        text_width(&warn_plain)
    );
}

#[test]
fn cost_ledger_watermark_idempotency_rollover_and_restart_carry() {
    // 2026-11-09T10:00:00Z (Monday)
    let mon_morning_ms = 1_794_218_400_000i64;
    let mut s1 = sample_stdin(120);
    s1.quota = None;
    s1.conversation_id = Some("conv-alpha".to_string());
    s1.cost = Some(CostStdin {
        total_usd: Some(0.45),
        subagent_usd: None,
        estimated: Some(false),
    });

    // Tick 1: writes initial snapshot (`today_usd = $0.45`, `week_usd = $0.45`)
    let (t1, w1) = reconcile_cost_ledger(None, Some(&s1), mon_morning_ms);
    let snap1 = w1.expect("first write");
    assert!((t1.unwrap().today_usd - 0.45).abs() < 1e-6);

    // Tick 2 (300ms later, identical session cost): ZERO disk write (`w2.is_none()`) and NO double counting!
    let (t2, w2) = reconcile_cost_ledger(Some(&snap1), Some(&s1), mon_morning_ms + 300);
    assert!(
        w2.is_none(),
        "Unchanged 300ms tick must not rewrite cost-ledger.json"
    );
    assert!((t2.unwrap().today_usd - 0.45).abs() < 1e-6);

    // Session B joins on same day with `$0.80` -> `Today $1.25`, `Week $1.25`
    let mut s2 = sample_stdin(120);
    s2.quota = None;
    s2.conversation_id = Some("conv-beta".to_string());
    s2.cost = Some(CostStdin {
        total_usd: Some(0.80),
        subagent_usd: None,
        estimated: Some(false),
    });
    let (t3, w3) = reconcile_cost_ledger(Some(&snap1), Some(&s2), mon_morning_ms + 5_000);
    let snap3 = w3.expect("second session write");
    let totals3 = t3.unwrap();
    assert!((totals3.today_usd - 1.25).abs() < 1e-6);
    assert!((totals3.week_usd - 1.25).abs() < 1e-6);

    // Next UTC day (Tuesday `mon_morning_ms + 86_400_000`), Session B advances from `$0.80` -> `$1.10`:
    // Today must only count Tuesday's `$0.30` delta, while Week counts `$0.45 + $1.10 = $1.55`!
    s2.cost.as_mut().unwrap().total_usd = Some(1.10);
    let (t4, w4) = reconcile_cost_ledger(Some(&snap3), Some(&s2), mon_morning_ms + 86_400_000);
    let snap4 = w4.expect("day rollover write");
    let totals4 = t4.unwrap();
    assert!(
        (totals4.today_usd - 0.30).abs() < 1e-6,
        "expected $0.30 daily delta after midnight rollover, got {}",
        totals4.today_usd
    );
    assert!(
        (totals4.week_usd - 1.55).abs() < 1e-6,
        "expected $1.55 weekly total, got {}",
        totals4.week_usd
    );

    // Process restart on Session B resets `total_usd` to `$0.15` -> folds prior `$0.30` into carry => `$0.45` daily!
    s2.cost.as_mut().unwrap().total_usd = Some(0.15);
    let (t5, w5) = reconcile_cost_ledger(Some(&snap4), Some(&s2), mon_morning_ms + 90_000_000);
    let snap5 = w5.expect("restart carry write");
    let totals5 = t5.unwrap();
    assert!((totals5.today_usd - 0.45).abs() < 1e-6);
    assert!((totals5.week_usd - 1.70).abs() < 1e-6);

    // Roundtrip `CostLedgerSnapshot` via `parse_cost_ledger_info` and render `Today $0.45` & `Week $1.70`
    let mut cfg = HudConfig::default();
    cfg.display.show_daily_cost = true;
    cfg.display.show_weekly_cost = true;
    let probes = ProbeAnswers {
        cost_ledger: Some(serde_json::to_string(&snap5).unwrap()),
        ..ProbeAnswers::default()
    };
    let parsed_snap = parse_cost_ledger_info(&probes).expect("valid ledger snapshot");
    let (reloaded_totals, _) =
        reconcile_cost_ledger(Some(&parsed_snap), Some(&s2), mon_morning_ms + 90_000_000);
    let rendered = render_hud_lines_full(
        RenderInput {
            stdin: Some(&s2),
            config: &cfg,
            transcript: &TranscriptSummary::default(),
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(140),
            now_ms: mon_morning_ms + 90_000_000,
        },
        reloaded_totals.as_ref(),
    );
    let joined: String = rendered
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("Today $0.45") && joined.contains("Week $1.70"),
        "{joined}"
    );
}

#[test]
fn launch_dir_breadcrumb_git_push_thresholds_and_scoped_github_osc8() {
    // 1. URL normalization rejects credentials, non-GitHub hosts, and path traversal
    assert_eq!(
        normalize_github_repo_url("git@github.com:org-1/agy-hud.git").as_deref(),
        Some("https://github.com/org-1/agy-hud")
    );
    assert_eq!(
        normalize_github_repo_url("https://github.com/org-1/agy-hud/"),
        Some("https://github.com/org-1/agy-hud".to_string())
    );
    assert_eq!(
        normalize_github_repo_url("https://token:secret@github.com/org-1/agy-hud.git"),
        None
    );
    assert_eq!(
        normalize_github_repo_url("https://evil.example.com/org-1/agy-hud.git"),
        None
    );

    // 2. Breadcrumb formatting (`agy-hud › hud-core`)
    let mut stdin = sample_stdin(140);
    stdin.workspace = Some(WorkspaceStdin {
        project_dir: Some("/Users/dev/agy-hud".to_string()),
        current_dir: Some("/Users/dev/agy-hud/crates/hud-core".to_string()),
    });
    assert_eq!(
        format_workspace_breadcrumb(
            Some("/Users/dev/agy-hud"),
            Some("/Users/dev/agy-hud/crates/hud-core"),
            PathLevels::One,
            true,
            GlyphMode::Unicode,
        ),
        "agy-hud › hud-core"
    );
    assert_eq!(
        format_workspace_breadcrumb(
            Some("/Users/dev/agy-hud"),
            Some("/Users/dev/agy-hud/crates/hud-core"),
            PathLevels::One,
            true,
            GlyphMode::Ascii,
        ),
        "agy-hud > hud-core"
    );

    // 3. Git push thresholds + capability-scoped GitHub OSC 8 link
    let repo = "https://github.com/Max/agy-hud";
    let probes = ProbeAnswers {
        vcs: Some(
            json!({
                "type": "git",
                "branch": "feat/p2",
                "dirty": true,
                "ahead": 12,
                "repoUrl": repo
            })
            .to_string(),
        ),
        ..ProbeAnswers::default()
    };
    let vcs = parse_vcs_info(Some(&stdin), &probes).expect("vcs");
    assert_eq!(vcs.repo_url.as_deref(), Some(repo));

    let mut cfg = HudConfig::default();
    cfg.vcs.show_ahead_behind = true;
    cfg.vcs.push_warning_threshold = 5;
    cfg.vcs.push_critical_threshold = 10;

    let lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &cfg,
        transcript: &TranscriptSummary::default(),
        vcs: Some(&vcs),
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(140),
        now_ms: 1_700_000_000_000,
    });
    let filtered = terminal_filter_with_repo(&lines[0], Some(repo));
    assert!(
        filtered.contains("\x1b]8;;https://github.com/Max/agy-hud/tree/feat/p2\x1b\\")
            && strip_ansi(&filtered).contains("feat/p2"),
        "Matching GitHub repo tree link must pass terminal filter: {filtered:?}"
    );
    assert!(
        filtered.contains("\x1b[31m") && strip_ansi(&filtered).contains("↑12"),
        "ahead=12 >= pushCriticalThreshold=10 must color ↑12 red: {filtered:?}"
    );

    // Spoofed OSC 8 URL to another GitHub repo MUST be stripped by `terminal_filter_with_repo`!
    let spoof = github_branch_link("https://github.com/attacker/exfil", "secret", "main", true);
    let stripped_spoof = terminal_filter_with_repo(&spoof, Some(repo));
    assert!(!stripped_spoof.contains("attacker/exfil"));
    assert_eq!(strip_ansi(&stripped_spoof), "main");
}

#[test]
fn composable_merge_groups_cjk_bar_alignment_and_right_align() {
    let mut stdin = sample_stdin(160);
    stdin.cost = Some(CostStdin {
        total_usd: Some(1.24),
        subagent_usd: None,
        estimated: Some(false),
    });
    let mut cfg = HudConfig::default();
    cfg.display.merge_groups = vec![vec!["context".to_string(), "quota".to_string()]];
    cfg.display.right_align = vec!["quota".to_string()];

    // 1. At 160 columns, `["context", "quota"]` merges onto a single row AND `quota` right-aligns to effective width (`160`)!
    let wide_lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &cfg,
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(160),
        now_ms: 1_700_000_000_000,
    });
    let merged_row = strip_ansi(&wide_lines[1]);
    assert!(
        merged_row.contains("Ctx") && merged_row.contains("Quota(weekly)"),
        "Wide terminal should merge context + quota onto one row: {merged_row}"
    );
    assert_eq!(
        text_width(&merged_row),
        160, // rail (3) + line_budget (157) = 160
        "rightAlign group member should pad row flush to effective width (160): '{merged_row}'"
    );

    // 2. At 84 columns in `zh-Hans` (`LineLayout::Expanded`), `["context", "quota"]` cannot fit on one line,
    // so it auto-stacks AND aligns bar start columns across CJK `上下文` (6 cols) and `配额(weekly)` (12 cols)!
    cfg.language = Language::ZhHans;
    cfg.line_layout = hud_core::types::LineLayout::Expanded;
    let narrow_zh = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &cfg,
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(84),
        now_ms: 1_700_000_000_000,
    });
    let ctx_plain = strip_ansi(&narrow_zh[1]);
    let qta_plain = strip_ansi(&narrow_zh[2]);
    let ctx_bar_col = text_width(&ctx_plain[..ctx_plain.find('█').unwrap()]);
    let qta_bar_col = text_width(&qta_plain[..qta_plain.find('█').unwrap()]);
    assert_eq!(
        ctx_bar_col, qta_bar_col,
        "Auto-stacked CJK bars must align at identical display column!\nctx: {ctx_plain}\nqta: {qta_plain}"
    );
}

#[test]
fn width_preserving_report_scrub_and_incremental_transcript_cursor_parity() {
    // 1. Width-preserving privacy mask preserves exact visual width for ASCII, CJK, and Emoji
    for sample in [
        "/Users/max/ secret-客户项目",
        "feat/登录修复-🚀",
        "Investigate payment timeout in checkout_v2",
    ] {
        let masked = mask_preserving_width(sample);
        assert_eq!(
            text_width(&masked),
            text_width(sample),
            "Masked width mismatch for {sample}"
        );
        assert_ne!(masked, sample);
    }

    // 2. `report` outputs minimal non-default config diff + scrubbed `"case"` fixture
    let stdin = sample_stdin(120);
    let snap = json!({
        "stdin": stdin,
        "probes": {
            "vcs": json!({ "type": "git", "branch": "feat/secret-branch", "dirty": true, "repoUrl": "https://github.com/private/repo" }).to_string(),
            "costLedger": json!({ "todayUsd": 2.5, "weekUsd": 9.0 }).to_string()
        }
    })
    .to_string();
    let facts = DoctorFacts {
        cwd: "/Users/max/secret-repo".to_string(),
        home_dir: "/Users/max".to_string(),
        platform: "macos".to_string(),
        terminal: "Ghostty".to_string(),
        remote_session: false,
        runtime: RuntimeFacts {
            engine: "rust-native".to_string(),
            version: "1.4.0".to_string(),
            exec_path: "/Users/max/.gemini/antigravity-cli/agy-hud/bin/agy-hud".to_string(),
            supported: true,
        },
        plugin: PluginFacts {
            root: "/Users/max/.gemini/config/plugins/agy-hud".to_string(),
            version: "1.4.0".to_string(),
        },
        installed_plugin_status: "installed".to_string(),
        installed_plugin: None,
        config_path: "/Users/max/.gemini/antigravity-cli/agy-hud/config.json".to_string(),
        heartbeat: None,
    };
    let report = execute_subcommand(
        "report",
        &[],
        &CommandContext {
            config_text: Some(r#"{"language":"zh-Hans","display":{"barLength":10}}"#),
            cli_settings_text: None,
            last_snapshot_text: Some(&snap),
            update_cache_text: None,
            transcript_head: None,
            transcript_tail: None,
            task_md_text: None,
            facts: &facts,
            paths: &LogicPaths::default(),
            now_ms: 1_700_000_000_000,
        },
    );
    let joined = report.lines.join("\n");
    assert!(!joined.contains("secret-branch"));
    assert!(!joined.contains("private/repo"));
    let parsed_report: serde_json::Value = serde_json::from_str(&joined).expect("report JSON");
    assert_eq!(parsed_report["config"]["language"], "zh-Hans");
    assert!(
        parsed_report["config"]["vcs"].is_null(),
        "Default sections must be omitted from minimal report config"
    );
    assert!(parsed_report["case"]["expectedPlainLines"].is_array());

    // 3. Incremental `parse_transcript_with_cursor` across 3 chunks matches full one-pass parse!
    let steps = [
        json!({
            "step_index": 0,
            "type": "USER_INPUT",
            "status": "DONE",
            "created_at": "2026-11-10T01:00:00Z",
            "content": "<USER_REQUEST>/plan Architect incremental HUD cursor</USER_REQUEST>"
        }),
        json!({
            "step_index": 1,
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": "2026-11-10T01:00:05Z",
            "output_tokens": 240,
            "tool_calls": [
                { "name": "read_file", "status": "DONE", "args": { "Path": "/Users/dev/project/skills/hud/SKILL.md" } },
                { "name": "call_mcp_tool", "status": "ERROR", "args": { "ServerName": "supabase-mcp-server", "ToolName": "list_tables" } }
            ]
        }),
        json!({
            "step_index": 2,
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": "2026-11-10T01:00:12Z",
            "output_tokens": 310,
            "tool_calls": [
                { "name": "call_mcp_tool", "status": "DONE", "args": { "ServerName": "supabase-mcp-server", "ToolName": "list_tables" } },
                { "name": "edit_file", "status": "DONE", "args": { "Path": "/Users/dev/project/src/lib.rs" } }
            ]
        }),
        json!({
            "step_index": 3,
            "type": "PLANNER_RESPONSE",
            "status": "RUNNING",
            "created_at": "2026-11-10T01:00:18Z",
            "content": "Streaming uncommitted tail...",
            "tool_calls": [
                { "name": "run_command", "status": "RUNNING", "args": { "CommandLine": "cargo test" } }
            ]
        }),
    ];
    let lines_str: Vec<String> = steps
        .iter()
        .map(|v| format!("{}\n", serde_json::to_string(v).unwrap()))
        .collect();
    let head_line = lines_str[0].trim_end();
    let full_text = lines_str.concat();

    // Full one-pass reference
    let full_ref =
        parse_transcript_and_tasks(Some(head_line), Some(&full_text), None, Some(&stdin));

    // Chunked incremental cursor: step 0+1 -> step 1+2 -> step 2+3 (uncommitted tail stays re-evaluated)
    let chunk1 = format!("{}{}", lines_str[0], lines_str[1]);
    let (_, c1_opt) =
        parse_transcript_with_cursor(None, Some(head_line), Some(&chunk1), 0, None, Some(&stdin));
    let c1 = c1_opt.expect("cursor after step 0+1");
    assert_eq!(
        c1.offset,
        lines_str[0].len() as u64,
        "Cursor commits step 0 and keeps latest PLANNER_RESPONSE (step 1) uncommitted"
    );

    let chunk2 = format!("{}{}", lines_str[1], lines_str[2]);
    let (_, c2_opt) = parse_transcript_with_cursor(
        Some(&c1),
        Some(head_line),
        Some(&chunk2),
        c1.offset,
        None,
        Some(&stdin),
    );
    let c2 = c2_opt.expect("cursor after step 2");
    assert_eq!(
        c2.offset,
        (lines_str[0].len() + lines_str[1].len()) as u64,
        "Cursor advances past step 1 and keeps latest PLANNER_RESPONSE (step 2) uncommitted"
    );

    let chunk3 = format!("{}{}", lines_str[2], lines_str[3]);
    let (inc_summary, c3_opt) = parse_transcript_with_cursor(
        Some(&c2),
        Some(head_line),
        Some(&chunk3),
        c2.offset,
        None,
        Some(&stdin),
    );
    let c3 = c3_opt.expect("cursor after step 3");
    assert_eq!(
        c3.offset,
        (lines_str[0].len() + lines_str[1].len() + lines_str[2].len()) as u64,
        "Cursor advances past step 2 and keeps RUNNING step 3 uncommitted"
    );
    assert_eq!(inc_summary.session_started_at, full_ref.session_started_at);
    assert_eq!(inc_summary.active_slash_mode, full_ref.active_slash_mode);
    assert_eq!(inc_summary.skills, full_ref.skills);
    assert_eq!(inc_summary.mcp_servers, full_ref.mcp_servers);
    assert_eq!(inc_summary.mcp_errors, full_ref.mcp_errors);
    assert_eq!(inc_summary.tools, full_ref.tools);
    assert_eq!(
        inc_summary.latest_planner_step,
        full_ref.latest_planner_step
    );
    assert_eq!(
        inc_summary.completed_step_tps_x10,
        full_ref.completed_step_tps_x10
    );

    // Confirm `TranscriptCursor` roundtrips cleanly through JSON
    let cursor_json = serde_json::to_string(&c3).unwrap();
    let decoded_cursor: TranscriptCursor = serde_json::from_str(&cursor_json).unwrap();
    assert_eq!(decoded_cursor, c3);
}

#[test]
fn compaction_scale_watermark_bar_colors_badge_modes_and_snapshot_report_fidelity() {
    let mut stdin = sample_stdin(128);
    stdin.exceeds_200k_tokens = Some(false);
    stdin.context_window = Some(ContextWindowStdin {
        total_input_tokens: None,
        total_output_tokens: None,
        context_window_size: Some(1_048_576),
        used_percentage: Some(13.9),
        remaining_percentage: Some(86.1),
        current_usage: Some(CurrentUsageStdin {
            input_tokens: Some(26_000),
            output_tokens: Some(0),
            cache_creation_input_tokens: Some(0),
            cache_read_input_tokens: Some(120_000),
        }),
    });

    let mut transcript = TranscriptSummary {
        compaction_count: 4,
        last_compaction_tokens: Some(256_915),
        ..TranscriptSummary::default()
    };

    // 1. Default `ContextScaleMode::Compaction`: 146k / 256k = 57.0% + `↻4` badge in English & zh-Hans
    let en_lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &HudConfig::default(),
        transcript: &transcript,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(128),
        now_ms: 1_700_000_000_000,
    });
    let en_plain = en_lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        en_plain.contains("57.0% (146k/256k · ↻4)"),
        "expected compaction-scaled bar & badge, got: {en_plain}"
    );
    // `compaction_count >= 3` paints `↻4` yellow (`\x1b[33m\x1b[1m`)
    let en_raw = en_lines.join("\n");
    assert!(en_raw.contains("\x1b[33m\x1b[1m↻4\x1b[0m"));

    let zh_cfg = HudConfig {
        language: Language::ZhHans,
        ..HudConfig::default()
    };
    let zh_lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &zh_cfg,
        transcript: &transcript,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(128),
        now_ms: 1_700_000_000_000,
    });
    let zh_plain = zh_lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(zh_plain.contains("上下文 "));
    assert!(zh_plain.contains("57.0% (146k/256k · ↻4)"));

    // 2. Warning (>= 60% -> 160k/256k = 62.5%) & Critical (>= 85% -> 224k/256k = 87.5%) colors
    let mut warn_stdin = stdin.clone();
    warn_stdin
        .context_window
        .as_mut()
        .unwrap()
        .current_usage
        .as_mut()
        .unwrap()
        .cache_read_input_tokens = Some(134_000); // 26k + 134k = 160k (62.5%)
    let warn_raw = render_hud_lines(RenderInput {
        stdin: Some(&warn_stdin),
        config: &HudConfig::default(),
        transcript: &transcript,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(128),
        now_ms: 1_700_000_000_000,
    })
    .join("\n");
    assert!(warn_raw.contains("\x1b[33m\x1b[1m62.5%\x1b[0m"));

    let mut crit_stdin = stdin.clone();
    crit_stdin
        .context_window
        .as_mut()
        .unwrap()
        .current_usage
        .as_mut()
        .unwrap()
        .cache_read_input_tokens = Some(198_000); // 26k + 198k = 224k (87.5%)
    let crit_raw = render_hud_lines(RenderInput {
        stdin: Some(&crit_stdin),
        config: &HudConfig::default(),
        transcript: &transcript,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(128),
        now_ms: 1_700_000_000_000,
    })
    .join("\n");
    assert!(crit_raw.contains("\x1b[31m\x1b[1m87.5%\x1b[0m"));

    // 3. `display.contextScale = "window"` shows physical 1.0M ceiling while retaining `↻4`
    let win_cfg = parse_config_str(Some(r#"{"display":{"contextScale":"window"}}"#)).config;
    assert_eq!(win_cfg.display.context_scale, ContextScaleMode::Window);
    let win_plain = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &win_cfg,
        transcript: &transcript,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(128),
        now_ms: 1_700_000_000_000,
    })
    .iter()
    .map(|l| strip_ansi(l))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(
        win_plain.contains("13.9% (146k/1.0M · ↻4)"),
        "expected raw window scale with badge, got: {win_plain}"
    );

    // 4. ASCII glyph mode (`c4`) and `Tokens` / `Percent` / `Remaining` contextValue modes
    transcript.compaction_count = 2; // < 3 -> cyan badge
    let mut mode_cfg = HudConfig::default();
    mode_cfg.display.glyph_mode = GlyphMode::Ascii;
    let ascii_plain = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &mode_cfg,
        transcript: &transcript,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(128),
        now_ms: 1_700_000_000_000,
    })
    .iter()
    .map(|l| strip_ansi(l))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(ascii_plain.contains("57.0% (146k/256k . c2)"));

    mode_cfg.display.glyph_mode = GlyphMode::Unicode;
    for (cv, expected_sub) in [
        (ContextValueMode::Tokens, "146k/256k · ↻2"),
        (ContextValueMode::Percent, "57.0% (↻2)"),
        (ContextValueMode::Remaining, "43.0% left (↻2)"),
    ] {
        mode_cfg.display.context_value = cv;
        let cv_plain = render_hud_lines(RenderInput {
            stdin: Some(&stdin),
            config: &mode_cfg,
            transcript: &transcript,
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(128),
            now_ms: 1_700_000_000_000,
        })
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
        assert!(
            cv_plain.contains(expected_sub),
            "mode {cv:?} missing '{expected_sub}' in: {cv_plain}"
        );
    }

    // 5. `preview --last` and `report` preserve `compactionCount` & `lastCompactionTokens` from snapshot
    let snap_json = json!({
        "at": 1_700_000_000_000i64,
        "stdin": &stdin,
        "probes": {},
        "compactionCount": 5,
        "lastCompactionTokens": 257_369
    })
    .to_string();
    let facts = DoctorFacts {
        cwd: "/Users/dev/demo-app".to_string(),
        home_dir: "/Users/dev".to_string(),
        platform: "macos".to_string(),
        terminal: "Ghostty".to_string(),
        remote_session: false,
        runtime: RuntimeFacts {
            engine: "rust-native".to_string(),
            version: "1.5.1".to_string(),
            exec_path: "/Users/dev/.gemini/antigravity-cli/agy-hud/bin/agy-hud".to_string(),
            supported: true,
        },
        plugin: PluginFacts {
            root: "/Users/dev/.gemini/config/plugins/agy-hud".to_string(),
            version: "1.5.1".to_string(),
        },
        installed_plugin_status: "installed".to_string(),
        installed_plugin: None,
        config_path: "/Users/dev/.gemini/antigravity-cli/agy-hud/config.json".to_string(),
        heartbeat: None,
    };
    let cmd_ctx = CommandContext {
        facts: &facts,
        paths: &LogicPaths::default(),
        now_ms: 1_700_000_000_000,
        config_text: None,
        cli_settings_text: None,
        last_snapshot_text: Some(&snap_json),
        update_cache_text: None,
        transcript_head: None,
        transcript_tail: None,
        task_md_text: None,
    };
    let last_prev = execute_subcommand("preview", &["--last".to_string()], &cmd_ctx);
    assert!(!last_prev.failed);
    let last_plain = last_prev
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(last_plain.contains("57.0% (146k/256k · ↻5)"));

    let rep = execute_subcommand("report", &["compaction-check".to_string()], &cmd_ctx);
    assert!(!rep.failed);
    let rep_val: serde_json::Value = serde_json::from_str(&rep.lines.join("\n")).unwrap();
    assert_eq!(rep_val["case"]["transcript"]["compaction_count"], 5);
    let rep_lines = rep_val["renderedPlainLines"].to_string();
    assert!(rep_lines.contains("57.0% (146k/256k · ↻5)"));
}
