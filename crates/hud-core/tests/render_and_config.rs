use hud_core::ansi::{
    BOLD, colors, github_branch_link, mask_preserving_width, paint, sanitize_title_text,
    strip_ansi, terminal_filter_with_repo, text_width,
};
use hud_core::commands::{CommandContext, execute_subcommand};
use hud_core::config::{
    CONFIG_SCHEMA, HudConfig, get_dot_value, parse_config_str, parse_set_pairs, validate_setting,
};
use hud_core::probes::{
    apply_speed_cache, is_newer_semver, normalize_github_repo_url, parse_compaction_ceiling_info,
    parse_compaction_ceiling_record, parse_cost_ledger_info, parse_db_ceiling_info,
    parse_db_ceiling_record, parse_github_latest_release, parse_usage_command_output,
    parse_vcs_info, reconcile_compaction_ceiling, reconcile_cost_ledger,
};
use hud_core::render::{
    RenderExtras, RenderInput, format_config_problem_line, format_config_problem_line_with_cmd,
    render_hud_lines, render_hud_lines_full, render_title_line,
};
use hud_core::stdin::{
    CeilingSource, CompactionInputs, CompactionScope, EffectiveCeiling, SENTINEL_RESET_CUTOFF_MS,
    compute_quota_pace, extract_context_metrics_scaled, extract_quota_buckets_reconciled,
    format_session_cost, format_workspace_breadcrumb, normalize_model_family, normalize_plan_tier,
    resolve_compaction_ceiling, resolve_compaction_ceiling_with_db, snap_compaction_ceiling,
};
use hud_core::transcript::{parse_transcript_and_tasks, parse_transcript_with_cursor};
use hud_core::types::{
    AgyStdinData, COMPACTION_CEILING_VERSION, CachedQuotaSnapshot, CompactionCeilingRecord,
    CompactionCeilingState, ContextScaleMode, ContextValueMode, ContextWindowStdin, CostStdin,
    CurrentUsageStdin, DbCeilingInfo, DbCeilingSource, DoctorFacts, EffortFormatMode, GlyphMode,
    InstalledPluginFacts, Language, LearnedCompactionState, LineLayout, LogicPaths, ModelStdin,
    PathLevels, PluginFacts, ProbeAnswers, QuotaBucketView, QuotaPace, RawQuotaBucket,
    RuntimeFacts, SandboxStdin, SubagentEntry, SubagentStatus, SubagentStdin, TodoItem, TodoStatus,
    ToolEntry, ToolStatus, TranscriptCursor, TranscriptSummary, UnprefixedSkillsFacts,
    UpdateCacheSnapshot, VcsStdin, VimStdin, WorkspaceStdin,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

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
    assert!(!wide_plain.contains("200k+"));
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

fn collect_leaf_dot_keys(val: &Value, prefix: &str, out: &mut BTreeSet<String>) {
    match val {
        Value::Object(map) => {
            for (k, v) in map {
                let next = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                collect_leaf_dot_keys(v, &next, out);
            }
        }
        _ => {
            out.insert(prefix.to_string());
        }
    }
}

#[test]
fn config_schema_matches_hud_config_leaf_keys_and_preview_showcase_renders_5_scenarios() {
    let default_cfg = HudConfig::default();
    assert!(!default_cfg.display.show_artifacts);
    let defaults_val = serde_json::to_value(&default_cfg).expect("serialize default HudConfig");
    let mut expected_keys = BTreeSet::new();
    collect_leaf_dot_keys(&defaults_val, "", &mut expected_keys);

    let mut schema_keys = BTreeSet::new();
    for entry in CONFIG_SCHEMA {
        assert!(
            schema_keys.insert(entry.key.to_string()),
            "duplicate CONFIG_SCHEMA key: {}",
            entry.key
        );
        let default_leaf = get_dot_value(&defaults_val, entry.key).unwrap_or_else(|| {
            panic!("schema key {} missing from HudConfig::default()", entry.key)
        });
        validate_setting(entry.key, default_leaf).unwrap_or_else(|err| {
            panic!(
                "default value for {} failed validate_setting: {err}",
                entry.key
            )
        });
    }
    assert_eq!(
        schema_keys, expected_keys,
        "CONFIG_SCHEMA keys must stay in 1-to-1 bijection with HudConfig leaf fields"
    );
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
        locale: None,
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
        ..DoctorFacts::default()
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
        locale: None,
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
        ..DoctorFacts::default()
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
        locale: None,
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
        ..DoctorFacts::default()
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
        RenderExtras {
            cost_totals: reloaded_totals.as_ref(),
            compaction_ceiling: None,
            ..RenderExtras::default()
        },
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
    let db_1m = DbCeilingInfo::new(
        1_048_576,
        DbCeilingSource::GenMetadata,
        Some("Gemini 4 Argon (Medium)".to_string()),
        Some("gemini 4 argon".to_string()),
    );
    let narrow_zh = render_hud_lines_full(
        RenderInput {
            stdin: Some(&stdin),
            config: &cfg,
            transcript: &TranscriptSummary::default(),
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(84),
            now_ms: 1_700_000_000_000,
        },
        RenderExtras {
            db_ceiling: Some(&db_1m),
            ..RenderExtras::default()
        },
    );
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
        locale: None,
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
        ..DoctorFacts::default()
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
        locale: None,
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
        ..DoctorFacts::default()
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

#[test]
fn model_family_normalization_preserves_qualifiers_and_strips_effort() {
    let model = |id: Option<&str>, display: Option<&str>, effort: Option<&str>| ModelStdin {
        id: id.map(str::to_string),
        display_name: display.map(str::to_string),
        effort: effort.map(str::to_string),
    };

    assert_eq!(
        normalize_model_family(Some(&model(
            Some("Orchestrator (Carbon)"),
            Some("Orchestrator (Carbon)"),
            None
        ))),
        Some("orchestrator (carbon)".to_string())
    );
    assert_eq!(
        normalize_model_family(Some(&model(
            Some("Orchestrator (Carbon) (High)"),
            None,
            Some("high")
        ))),
        Some("orchestrator (carbon)".to_string())
    );
    assert_eq!(
        normalize_model_family(Some(&model(
            Some("Gemini 4 Argon (1M) (XHigh)"),
            None,
            Some("xhigh")
        ))),
        Some("gemini 4 argon (1m)".to_string())
    );
    assert_eq!(
        normalize_model_family(Some(&model(
            Some("gemini-4-hydrogen-xhigh"),
            None,
            Some("xhigh")
        ))),
        Some("gemini-4-hydrogen".to_string())
    );
    assert_eq!(
        normalize_model_family(Some(&model(Some("qwen-max"), None, None))),
        Some("qwen-max".to_string())
    );
    assert_eq!(
        normalize_model_family(Some(&model(Some("qwen-max"), None, Some("high")))),
        Some("qwen-max".to_string())
    );
    assert_eq!(
        normalize_model_family(Some(&model(
            Some("   "),
            Some("  Gemini\t 4  Argon (Minimal) "),
            Some("minimal")
        ))),
        Some("gemini 4 argon".to_string())
    );

    let long_id = format!("model-{}", "a".repeat(200));
    let capped = normalize_model_family(Some(&model(Some(&long_id), None, None))).unwrap();
    assert!(capped.len() <= 160);
}

#[test]
fn tier_snapping_window_thresholds_and_current_usage_absence_behave_accurately() {
    assert_eq!(snap_compaction_ceiling(31_999), None);
    assert_eq!(snap_compaction_ceiling(180_000), Some(180_000));
    assert_eq!(snap_compaction_ceiling(200_000), Some(256_000));
    assert_eq!(snap_compaction_ceiling(256_915), Some(256_000));
    assert_eq!(snap_compaction_ceiling(320_000), Some(256_000));
    assert_eq!(snap_compaction_ceiling(380_000), Some(380_000));
    assert_eq!(snap_compaction_ceiling(440_000), Some(512_000));
    assert_eq!(snap_compaction_ceiling(515_000), Some(512_000));
    assert_eq!(snap_compaction_ceiling(620_000), Some(512_000));
    assert_eq!(snap_compaction_ceiling(700_000), Some(700_000));

    let mid_win_res =
        resolve_compaction_ceiling(524_288, 0, None, None, Some(200_000), Some(200_000));
    assert_eq!(mid_win_res.effective, EffectiveCeiling::PhysicalWindow);
    assert_eq!(mid_win_res.stale_bypass, None);

    let mid_win_ckpt = resolve_compaction_ceiling(
        524_288,
        1,
        Some(256_500),
        None,
        Some(128_000),
        Some(128_000),
    );
    assert_eq!(
        mid_win_ckpt.effective,
        EffectiveCeiling::Scaled {
            ceiling_tokens: 256_000,
            source: CeilingSource::SessionCheckpoint
        }
    );

    let large_cold =
        resolve_compaction_ceiling(1_048_576, 0, None, None, Some(128_000), Some(128_000));
    assert_eq!(large_cold.effective, EffectiveCeiling::Unknown);

    let cumulative_only_stdin: AgyStdinData = serde_json::from_value(json!({
        "exceeds_200k_tokens": true,
        "model": { "id": "Gemini 4 Argon (High)", "effort": "high" },
        "context_window": {
            "total_input_tokens": 350_000,
            "total_output_tokens": 12_000,
            "context_window_size": 1_048_576
        }
    }))
    .expect("exceeds_200k_tokens remains deserializable");
    assert_eq!(cumulative_only_stdin.exceeds_200k_tokens, Some(true));

    let scaled = extract_context_metrics_scaled(
        Some(&cumulative_only_stdin),
        ContextScaleMode::Compaction,
        CompactionInputs::default(),
    );
    assert_eq!(scaled.current_usage_tokens, None);
    assert!(!scaled.ceiling_known);
    assert_eq!(scaled.window_size, None);

    let scope = CompactionScope::from_stdin(Some(&cumulative_only_stdin)).unwrap();
    let (learned_state, write_rec) = reconcile_compaction_ceiling(
        None,
        &scope,
        Some(&cumulative_only_stdin),
        &TranscriptSummary::default(),
        1_700_000_000_000,
    );
    assert_eq!(learned_state, None);
    assert_eq!(write_rec, None);

    let rendered = render_hud_lines(RenderInput {
        stdin: Some(&cumulative_only_stdin),
        config: &HudConfig::default(),
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(160),
        now_ms: 1_700_000_000_000,
    })
    .iter()
    .map(|l| strip_ansi(l))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(
        !rendered.contains("200k+"),
        "unexpected 200k+ badge: {rendered}"
    );
}

#[test]
fn compaction_ceiling_reconciliation_learns_invalidates_recovers_and_scrubs_reports() {
    let make_stdin = |model_id: &str, effort: &str, cur_tokens: u64| AgyStdinData {
        model: Some(ModelStdin {
            id: Some(model_id.to_string()),
            display_name: Some(model_id.to_string()),
            effort: Some(effort.to_string()),
        }),
        context_window: Some(ContextWindowStdin {
            context_window_size: Some(1_048_576),
            current_usage: Some(CurrentUsageStdin {
                input_tokens: Some(cur_tokens),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    let s1 = make_stdin("Gemini 5 Ultra (High)", "high", 128_000);
    let scope1 = CompactionScope::from_stdin(Some(&s1)).unwrap();
    let t1 = TranscriptSummary {
        compaction_count: 1,
        last_compaction_tokens: Some(515_000),
        last_compaction_at_ms: Some(1_700_000_100_000),
        ..Default::default()
    };
    let (eff1, rec1_opt) =
        reconcile_compaction_ceiling(None, &scope1, Some(&s1), &t1, 1_700_000_105_000);
    assert_eq!(eff1, Some(LearnedCompactionState::Ceiling(512_000)));
    let rec1 = rec1_opt.expect("first 515k checkpoint writes learned 512k record");
    assert_eq!(rec1.updated_at_ms, 1_700_000_100_000);
    assert_eq!(
        rec1.state,
        CompactionCeilingState::Learned {
            ceiling_tokens: 512_000,
            observed_tokens: 515_000
        }
    );

    let (eff_idem, write_idem) =
        reconcile_compaction_ceiling(Some(&rec1), &scope1, Some(&s1), &t1, 1_700_000_110_000);
    assert_eq!(eff_idem, Some(LearnedCompactionState::Ceiling(512_000)));
    assert_eq!(write_idem, None);

    let s2_low = make_stdin("Gemini 5 Ultra (Low)", "low", 256_000);
    let scope2 = CompactionScope::from_stdin(Some(&s2_low)).unwrap();
    assert_eq!(scope1.storage_key(), scope2.storage_key());
    let (eff2, write2) = reconcile_compaction_ceiling(
        Some(&rec1),
        &scope2,
        Some(&s2_low),
        &TranscriptSummary::default(),
        1_700_000_200_000,
    );
    assert_eq!(eff2, Some(LearnedCompactionState::Ceiling(512_000)));
    assert_eq!(write2, None);

    let stale_tx = TranscriptSummary {
        compaction_count: 1,
        last_compaction_tokens: Some(256_500),
        last_compaction_at_ms: Some(1_700_000_050_000),
        ..Default::default()
    };
    let (eff_stale, write_stale) = reconcile_compaction_ceiling(
        Some(&rec1),
        &scope2,
        Some(&s2_low),
        &stale_tx,
        1_700_000_210_000,
    );
    assert_eq!(eff_stale, Some(LearnedCompactionState::Ceiling(512_000)));
    assert_eq!(write_stale, None);

    let stale_missing_ts = TranscriptSummary {
        compaction_count: 1,
        last_compaction_tokens: Some(256_500),
        last_compaction_at_ms: None,
        ..Default::default()
    };
    assert_eq!(
        reconcile_compaction_ceiling(
            Some(&rec1),
            &scope2,
            Some(&s2_low),
            &stale_missing_ts,
            1_700_000_215_000
        )
        .1,
        None
    );

    let rec_256 = CompactionCeilingRecord {
        version: COMPACTION_CEILING_VERSION,
        model_family: scope1.model_family().to_string(),
        phys_win: scope1.phys_win(),
        state: CompactionCeilingState::Learned {
            ceiling_tokens: 256_000,
            observed_tokens: 256_500,
        },
        updated_at_ms: 1_700_000_050_000,
    };
    let bypass_stdin = make_stdin("Gemini 5 Ultra (High)", "high", 400_000);
    let (eff_inv, inv_opt) = reconcile_compaction_ceiling(
        Some(&rec_256),
        &scope1,
        Some(&bypass_stdin),
        &TranscriptSummary::default(),
        1_700_000_300_000,
    );
    assert_eq!(eff_inv, Some(LearnedCompactionState::Invalidated));
    let inv_rec = inv_opt.expect("400k uncompacted usage invalidates 256k learned ceiling");
    assert_eq!(
        inv_rec.state,
        CompactionCeilingState::Invalidated {
            bypassed_tokens: 400_000,
            previous_ceiling_tokens: Some(256_000)
        }
    );

    assert_eq!(
        reconcile_compaction_ceiling(
            Some(&inv_rec),
            &scope1,
            Some(&bypass_stdin),
            &TranscriptSummary::default(),
            1_700_000_305_000
        ),
        (Some(LearnedCompactionState::Invalidated), None)
    );

    let (eff_resurrect, write_resurrect) = reconcile_compaction_ceiling(
        Some(&inv_rec),
        &scope1,
        Some(&s1),
        &stale_tx,
        1_700_000_310_000,
    );
    assert_eq!(eff_resurrect, Some(LearnedCompactionState::Invalidated));
    assert_eq!(write_resurrect, None);

    let recover_tx = TranscriptSummary {
        compaction_count: 1,
        last_compaction_tokens: Some(515_000),
        last_compaction_at_ms: Some(1_700_000_350_000),
        ..Default::default()
    };
    let (eff_recov, recov_opt) = reconcile_compaction_ceiling(
        Some(&inv_rec),
        &scope1,
        Some(&s1),
        &recover_tx,
        1_700_000_355_000,
    );
    assert_eq!(eff_recov, Some(LearnedCompactionState::Ceiling(512_000)));
    assert!(recov_opt.is_some());

    let valid_probe_json = json!({
        "version": 1,
        "modelFamily": "gemini 5 ultra",
        "physWin": 1_048_576,
        "kind": "learned",
        "ceiling_tokens": 512_000,
        "observed_tokens": 515_000,
        "updatedAtMs": 1_700_000_350_000i64,
        "secretLeak": "/Users/alice/private"
    })
    .to_string();
    let bad_probe_json = json!({
        "version": 1,
        "modelFamily": "gemini 5 ultra",
        "physWin": 1_048_576,
        "kind": "learned",
        "ceiling_tokens": 515_000,
        "observed_tokens": 515_000,
        "updatedAtMs": 1_700_000_350_000i64
    })
    .to_string();
    assert!(parse_compaction_ceiling_record(&bad_probe_json, Some(&scope1)).is_none());
    let parsed_valid = parse_compaction_ceiling_info(
        &ProbeAnswers {
            compaction_ceiling: Some(valid_probe_json.clone()),
            ..Default::default()
        },
        Some(&scope1),
    )
    .expect("valid probe record parses");
    assert_eq!(
        parsed_valid.as_learned_state(),
        LearnedCompactionState::Ceiling(512_000)
    );

    let facts = DoctorFacts {
        cwd: "/Users/alice/app".to_string(),
        home_dir: "/Users/alice".to_string(),
        platform: "macos".to_string(),
        terminal: "Ghostty".to_string(),
        remote_session: false,
        locale: None,
        runtime: RuntimeFacts {
            engine: "rust-native".to_string(),
            version: "1.7.1".to_string(),
            exec_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/bin/agy-hud".to_string(),
            supported: true,
        },
        plugin: PluginFacts {
            root: "/Users/alice/.gemini/config/plugins/agy-hud".to_string(),
            version: "1.7.1".to_string(),
        },
        installed_plugin_status: "installed".to_string(),
        installed_plugin: None,
        config_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/config.json".to_string(),
        heartbeat: None,
        ..DoctorFacts::default()
    };
    let snap_valid = json!({
        "stdin": &s2_low,
        "probes": { "compaction_ceiling": valid_probe_json }
    })
    .to_string();
    let rep_valid = execute_subcommand(
        "report",
        &["valid-ceiling".to_string()],
        &CommandContext {
            facts: &facts,
            paths: &LogicPaths::default(),
            now_ms: 1_700_000_400_000,
            config_text: None,
            cli_settings_text: None,
            last_snapshot_text: Some(&snap_valid),
            update_cache_text: None,
            transcript_head: None,
            transcript_tail: None,
            task_md_text: None,
        },
    );
    let rep_valid_json: serde_json::Value =
        serde_json::from_str(&rep_valid.lines.join("\n")).unwrap();
    let scrubbed_cc = rep_valid_json["case"]["probes"]["compaction_ceiling"]
        .as_str()
        .expect("valid compaction_ceiling survives report");
    assert!(!scrubbed_cc.contains("secretLeak"));
    assert!(
        rep_valid_json["renderedPlainLines"]
            .to_string()
            .contains("256k/512k")
    );

    let snap_bad = json!({
        "stdin": &s2_low,
        "probes": { "compaction_ceiling": bad_probe_json }
    })
    .to_string();
    let rep_bad = execute_subcommand(
        "report",
        &["bad-ceiling".to_string()],
        &CommandContext {
            facts: &facts,
            paths: &LogicPaths::default(),
            now_ms: 1_700_000_400_000,
            config_text: None,
            cli_settings_text: None,
            last_snapshot_text: Some(&snap_bad),
            update_cache_text: None,
            transcript_head: None,
            transcript_tail: None,
            task_md_text: None,
        },
    );
    let rep_bad_json: serde_json::Value = serde_json::from_str(&rep_bad.lines.join("\n")).unwrap();
    assert!(rep_bad_json["case"]["probes"]["compaction_ceiling"].is_null());
}

#[test]
fn artifact_count_visibility_defaults_i18n_compact_meta_and_config_overrides() {
    let mut base_stdin = sample_stdin(120);
    base_stdin.artifact_count = Some(1);
    base_stdin.task_count = Some(1);
    base_stdin.pending_input_count = Some(2);

    let counts_transcript = TranscriptSummary {
        subagents: vec![
            SubagentEntry {
                name: "research".to_string(),
                role: Some("Reader".to_string()),
                model: None,
                status: SubagentStatus::Running,
                started_at: Some(1_699_999_990_000),
                ended_at: None,
            },
            SubagentEntry {
                name: "self".to_string(),
                role: Some("Auditor".to_string()),
                model: None,
                status: SubagentStatus::Completed,
                started_at: Some(1_699_999_950_000),
                ended_at: Some(1_699_999_980_000),
            },
        ],
        ..TranscriptSummary::default()
    };

    let render_plain = |stdin: &AgyStdinData, cfg: &HudConfig, cols: usize| -> Vec<String> {
        render_hud_lines(RenderInput {
            stdin: Some(stdin),
            config: cfg,
            transcript: &counts_transcript,
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(cols),
            now_ms: 1_700_000_000_000,
        })
        .into_iter()
        .map(|l| strip_ansi(&l))
        .collect()
    };

    let default_cfg = HudConfig::default();
    assert!(!default_cfg.display.show_artifacts);
    let default_joined = render_plain(&base_stdin, &default_cfg, 120).join("\n");
    assert!(!default_joined.contains("art"));
    assert!(!default_joined.contains("产物"));
    assert!(default_joined.contains("agents 1/2"));
    assert!(default_joined.contains("tasks 1"));
    assert!(default_joined.contains("+2 queued"));

    let mut opt_in_en = HudConfig::default();
    opt_in_en.display.show_artifacts = true;
    let mut art2_stdin = base_stdin.clone();
    art2_stdin.artifact_count = Some(2);
    let en_joined = render_plain(&art2_stdin, &opt_in_en, 120).join("\n");
    let agents_pos = en_joined.find("agents 1/2").expect("agents count");
    let tasks_pos = en_joined.find("tasks 1").expect("tasks count");
    let art_pos = en_joined.find("art 2").expect("opt-in art count");
    let queued_pos = en_joined.find("+2 queued").expect("queued count");
    assert!(agents_pos < tasks_pos && tasks_pos < art_pos && art_pos < queued_pos);

    let mut right_align_cfg = opt_in_en.clone();
    right_align_cfg.display.right_align = vec!["counts".to_string()];
    right_align_cfg.display.show_sandbox = false;
    let mut right_align_stdin = art2_stdin.clone();
    right_align_stdin.pending_input_count = None;
    let right_lines = render_hud_lines(RenderInput {
        stdin: Some(&right_align_stdin),
        config: &right_align_cfg,
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(120),
        now_ms: 1_700_000_000_000,
    });
    let ctx_row = right_lines
        .iter()
        .find(|l| strip_ansi(l).contains("Ctx"))
        .expect("context row");
    assert_eq!(text_width(ctx_row), 120);
    assert!(strip_ansi(ctx_row).trim_end().ends_with("art 2"));

    let mut stdin_subagents = art2_stdin.clone();
    stdin_subagents.subagents = Some(vec![
        SubagentStdin {
            name: Some("research".to_string()),
            role: Some("Reader".to_string()),
            status: Some("running".to_string()),
            model: None,
        },
        SubagentStdin {
            name: Some("self".to_string()),
            role: Some("Auditor".to_string()),
            status: Some("completed".to_string()),
            model: None,
        },
    ]);
    let stdin_subagent_out = render_hud_lines(RenderInput {
        stdin: Some(&stdin_subagents),
        config: &opt_in_en,
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(120),
        now_ms: 1_700_000_000_000,
    })
    .into_iter()
    .map(|l| strip_ansi(&l))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(stdin_subagent_out.contains("agents 1/2"));
    assert!(stdin_subagent_out.contains("art 2"));

    let mut opt_in_zh = opt_in_en.clone();
    opt_in_zh.language = Language::ZhHans;
    let zh_joined = render_plain(&art2_stdin, &opt_in_zh, 120).join("\n");
    assert!(zh_joined.contains("产物 2"));
    assert!(zh_joined.contains("子代理 1/2"));
    assert!(zh_joined.contains("后台 1"));
    assert!(!zh_joined.contains("art 2"));

    for count in [Some(0), None] {
        let mut hidden_stdin = base_stdin.clone();
        hidden_stdin.artifact_count = count;
        let out = render_plain(&hidden_stdin, &opt_in_en, 120).join("\n");
        assert!(!out.contains("art"));
        assert!(!out.contains("产物"));
        assert!(out.contains("tasks 1"));
    }

    let mut compact_stdin = art2_stdin.clone();
    compact_stdin.quota = None;
    compact_stdin.sandbox = None;
    compact_stdin.pending_input_count = None;
    compact_stdin.cost = None;
    if let Some(ref mut cw) = compact_stdin.context_window
        && let Some(ref mut cu) = cw.current_usage
    {
        cu.cache_read_input_tokens = None;
    }
    let compact_lines = render_hud_lines(RenderInput {
        stdin: Some(&compact_stdin),
        config: &opt_in_en,
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(68),
        now_ms: 1_700_000_000_000,
    });
    let compact_out = compact_lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!compact_out.contains("art"), "{compact_out}");
    assert!(compact_out.contains("tasks 1"), "{compact_out}");

    let mut overflow_cfg = opt_in_en.clone();
    overflow_cfg.line_layout = LineLayout::Expanded;
    overflow_cfg.display.show_sandbox = false;
    overflow_cfg.display.show_daily_cost = true;
    let mut overflow_stdin = art2_stdin.clone();
    overflow_stdin.pending_input_count = None;
    let overflow_db = DbCeilingInfo::new(
        1_048_576,
        DbCeilingSource::GenMetadata,
        Some("Gemini 4 Argon (Medium)".to_string()),
        Some("gemini 4 argon".to_string()),
    );
    let overflow_lines = render_hud_lines_full(
        RenderInput {
            stdin: Some(&overflow_stdin),
            config: &overflow_cfg,
            transcript: &TranscriptSummary::default(),
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(88),
            now_ms: 1_700_000_000_000,
        },
        RenderExtras {
            db_ceiling: Some(&overflow_db),
            ..RenderExtras::default()
        },
    );
    let overflow_ctx = overflow_lines
        .iter()
        .map(|l| strip_ansi(l))
        .find(|l| l.contains("Ctx"))
        .expect("expanded overflow context row");
    assert!(
        !overflow_ctx.contains("art"),
        "expected ctx_steps[0] compact_meta to drop `art`: {overflow_ctx}"
    );
    assert!(overflow_ctx.contains("Cache"), "{overflow_ctx}");
    assert!(overflow_ctx.contains("tasks 1"), "{overflow_ctx}");
    assert!(overflow_ctx.contains("$1.25"), "{overflow_ctx}");

    for json_cfg in [
        r#"{"display":{"showCounts":false,"showArtifacts":true}}"#,
        r#"{"display":{"showCounts":false}}"#,
    ] {
        let loaded = parse_config_str(Some(json_cfg));
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert!(!loaded.config.display.show_counts);
        let suppressed = render_plain(&art2_stdin, &loaded.config, 120).join("\n");
        assert!(!suppressed.contains("agents"));
        assert!(!suppressed.contains("tasks"));
        assert!(!suppressed.contains("art"));
        assert!(!suppressed.contains("queued"));
    }

    let mut merge_cfg = HudConfig::default();
    merge_cfg.display.merge_groups = vec![vec!["context".to_string(), "meta".to_string()]];
    merge_cfg.display.show_cost = false;
    merge_cfg.display.show_sandbox = false;
    merge_cfg.display.show_memory = false;
    let mut only_art_stdin = sample_stdin(120);
    only_art_stdin.artifact_count = Some(1);
    only_art_stdin.task_count = None;
    only_art_stdin.pending_input_count = None;
    only_art_stdin.sandbox = None;
    let mut no_art_stdin = only_art_stdin.clone();
    no_art_stdin.artifact_count = None;
    let render_minimal = |stdin: &AgyStdinData| -> Vec<String> {
        render_hud_lines(RenderInput {
            stdin: Some(stdin),
            config: &merge_cfg,
            transcript: &TranscriptSummary::default(),
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(120),
            now_ms: 1_700_000_000_000,
        })
        .into_iter()
        .map(|l| strip_ansi(&l))
        .collect()
    };
    let merged_with_art = render_minimal(&only_art_stdin);
    let merged_without_art = render_minimal(&no_art_stdin);
    assert_eq!(merged_with_art, merged_without_art);
    let merged_ctx = merged_with_art
        .iter()
        .find(|l| l.contains("Ctx"))
        .expect("merged context row");
    assert!(!merged_ctx.trim_end().ends_with('│'));
    assert!(!merged_ctx.trim_end().ends_with('·'));

    let loaded_on = parse_config_str(Some(r#"{"display":{"showArtifacts":true}}"#));
    assert!(loaded_on.problems.is_empty());
    assert!(loaded_on.config.display.show_artifacts);

    let set_on = parse_set_pairs(
        &["display.showArtifacts=true".to_string()],
        &HudConfig::default(),
    );
    assert!(set_on.problems.is_empty());
    assert!(set_on.config.display.show_artifacts);

    let set_bad = parse_set_pairs(
        &["display.showArtifacts=\"yes\"".to_string()],
        &HudConfig::default(),
    );
    assert_eq!(set_bad.problems.len(), 1);

    let facts = DoctorFacts {
        cwd: "/Users/alice/app".to_string(),
        home_dir: "/Users/alice".to_string(),
        platform: "macos".to_string(),
        terminal: "Ghostty".to_string(),
        remote_session: false,
        locale: None,
        runtime: RuntimeFacts {
            engine: "rust-native".to_string(),
            version: hud_core::types::HUD_VERSION.to_string(),
            exec_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/bin/agy-hud".to_string(),
            supported: true,
        },
        plugin: PluginFacts {
            root: "/Users/alice/.gemini/config/plugins/agy-hud".to_string(),
            version: hud_core::types::HUD_VERSION.to_string(),
        },
        installed_plugin_status: "installed".to_string(),
        installed_plugin: None,
        config_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/config.json".to_string(),
        heartbeat: None,
        ..DoctorFacts::default()
    };
    let ctx = CommandContext {
        facts: &facts,
        paths: &LogicPaths::default(),
        now_ms: 1_700_000_000_000,
        config_text: None,
        cli_settings_text: None,
        last_snapshot_text: None,
        update_cache_text: None,
        transcript_head: None,
        transcript_tail: None,
        task_md_text: None,
    };

    let schema_out = execute_subcommand("schema", &["art".to_string()], &ctx);
    assert!(!schema_out.failed);
    let schema_plain = schema_out
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(schema_plain.contains("display.showArtifacts"));
    assert!(schema_plain.contains("stdin.artifact_count"));
    assert!(schema_plain.contains("产物"));

    let preview_default = execute_subcommand(
        "preview",
        &[
            "--state".to_string(),
            "tool_use".to_string(),
            "--width".to_string(),
            "120".to_string(),
        ],
        &ctx,
    );
    assert!(!preview_default.failed);
    let preview_default_plain = preview_default
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(preview_default_plain.contains("tasks 1"));
    assert!(!preview_default_plain.contains("art 2"));

    let preview_opt_in = execute_subcommand(
        "preview",
        &[
            "--state".to_string(),
            "tool_use".to_string(),
            "--width".to_string(),
            "120".to_string(),
            "--set".to_string(),
            "display.showArtifacts=true".to_string(),
        ],
        &ctx,
    );
    assert!(!preview_opt_in.failed);
    let preview_opt_in_plain = preview_opt_in
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(preview_opt_in_plain.contains("tasks 1"));
    assert!(preview_opt_in_plain.contains("art 2"));

    let preview_overflow_108_opt_in = execute_subcommand(
        "preview",
        &[
            "--state".to_string(),
            "tool_use".to_string(),
            "--width".to_string(),
            "108".to_string(),
            "--set".to_string(),
            "display.showArtifacts=true".to_string(),
        ],
        &ctx,
    );
    assert!(!preview_overflow_108_opt_in.failed);
    let preview_overflow_108_plain = preview_overflow_108_opt_in
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(preview_overflow_108_plain.contains("tasks 1"));
    assert!(
        !preview_overflow_108_plain.contains("art 2"),
        "expected preset width 108 budget overflow to trigger compact_meta and drop art 2: {preview_overflow_108_plain}"
    );
}

#[test]
fn japanese_language_rendering_and_cjk_alignment() {
    let stdin = AgyStdinData {
        agent_state: Some("tool_use".to_string()),
        cycle_mode: Some("plan".to_string()),
        cwd: Some("/Users/dev/project".to_string()),
        model: Some(ModelStdin {
            id: Some("Gemini 4 Argon".to_string()),
            display_name: Some("Gemini 4 Argon".to_string()),
            effort: Some("high".to_string()),
        }),
        context_window: Some(ContextWindowStdin {
            total_input_tokens: None,
            total_output_tokens: None,
            context_window_size: Some(1_048_576),
            used_percentage: Some(42.5),
            remaining_percentage: Some(57.5),
            current_usage: Some(CurrentUsageStdin {
                input_tokens: Some(445_644),
                output_tokens: Some(1_200),
                cache_creation_input_tokens: Some(0),
                cache_read_input_tokens: Some(50_000),
            }),
        }),
        cost: Some(CostStdin {
            total_usd: Some(0.125),
            subagent_usd: None,
            estimated: Some(false),
        }),
        terminal_width: Some(110),
        ..AgyStdinData::default()
    };

    let ja_cfg = HudConfig {
        language: Language::Ja,
        ..HudConfig::default()
    };

    let lines = render_hud_lines_full(
        RenderInput {
            stdin: Some(&stdin),
            config: &ja_cfg,
            transcript: &TranscriptSummary::default(),
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(110),
            now_ms: 1_700_000_000_000,
        },
        RenderExtras::default(),
    );

    let plain = lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        plain.contains("ツール実行"),
        "expected Japanese 'ツール実行': {plain}"
    );
    assert!(
        plain.contains("[計画]"),
        "expected Japanese '[計画]': {plain}"
    );
    assert!(
        plain.contains("コンテキスト"),
        "expected Japanese 'コンテキスト': {plain}"
    );
    assert!(
        plain.contains("キャッシュ"),
        "expected Japanese 'キャッシュ': {plain}"
    );

    // Parse config test with language: "ja" and "auto"
    let loaded_ja = parse_config_str(Some(r#"{"language":"ja"}"#));
    assert_eq!(loaded_ja.config.language, Language::Ja);
    assert!(loaded_ja.problems.is_empty());

    let loaded_auto = parse_config_str(Some(r#"{"language":"auto"}"#));
    assert_eq!(loaded_auto.config.language, Language::Auto);
    assert!(loaded_auto.problems.is_empty());
}

#[test]
fn short_vs_prefixed_update_badge_and_config_warning_line() {
    let default_extras = RenderExtras::default();
    assert!(!default_extras.use_short_hud_cmd);
    assert!(!default_extras.use_short_update_cmd);

    let stdin = sample_stdin(140);
    let update = UpdateCacheSnapshot {
        checked_at: 1_700_000_000_000,
        latest_version: "9.9.0".to_string(),
        release_url: Some("https://github.com/maxlee/agy-hud/releases/tag/v9.9.0".to_string()),
    };

    for (gm, short_expected, prefixed_expected, check_ascii) in [
        (
            GlyphMode::Unicode,
            "↑ v9.9.0 · /hud-update",
            "↑ v9.9.0 · /agy-hud:hud-update",
            false,
        ),
        (
            GlyphMode::Nerd,
            "\u{f062} v9.9.0 · /hud-update",
            "\u{f062} v9.9.0 · /agy-hud:hud-update",
            false,
        ),
        (
            GlyphMode::Ascii,
            "^ v9.9.0 . /hud-update",
            "^ v9.9.0 . /agy-hud:hud-update",
            true,
        ),
    ] {
        let mut cfg = HudConfig::default();
        cfg.display.glyph_mode = gm;

        let prefixed_lines = render_hud_lines_full(
            RenderInput {
                stdin: Some(&stdin),
                config: &cfg,
                transcript: &TranscriptSummary::default(),
                vcs: None,
                memory: None,
                cached_quota: None,
                update_info: Some(&update),
                columns: Some(140),
                now_ms: 1_700_000_000_000,
            },
            RenderExtras::default(),
        );
        if check_ascii {
            assert!(prefixed_lines[0].is_ascii());
        }
        let prefixed_plain = strip_ansi(&prefixed_lines[0]);
        assert!(
            prefixed_plain.contains(prefixed_expected),
            "expected `{prefixed_expected}` in `{prefixed_plain}`"
        );

        let short_lines = render_hud_lines_full(
            RenderInput {
                stdin: Some(&stdin),
                config: &cfg,
                transcript: &TranscriptSummary::default(),
                vcs: None,
                memory: None,
                cached_quota: None,
                update_info: Some(&update),
                columns: Some(140),
                now_ms: 1_700_000_000_000,
            },
            RenderExtras {
                use_short_update_cmd: true,
                ..RenderExtras::default()
            },
        );
        if check_ascii {
            assert!(short_lines[0].is_ascii());
        }
        let short_plain = strip_ansi(&short_lines[0]);
        assert!(
            short_plain.contains(short_expected),
            "expected `{short_expected}` in `{short_plain}`"
        );
        assert!(!short_plain.contains("/agy-hud:hud-update"));
    }

    let mut disabled_cfg = HudConfig::default();
    disabled_cfg.updates.enabled = false;
    let hidden_lines = render_hud_lines_full(
        RenderInput {
            stdin: Some(&stdin),
            config: &disabled_cfg,
            transcript: &TranscriptSummary::default(),
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: Some(&update),
            columns: Some(140),
            now_ms: 1_700_000_000_000,
        },
        RenderExtras {
            use_short_update_cmd: true,
            ..RenderExtras::default()
        },
    );
    assert!(!strip_ansi(&hidden_lines[0]).contains("/hud-update"));

    // Config problem line: prefixed fallback vs short `/hud`, including narrow 42-col preservation
    let problems = vec![
        "Unknown configuration key \"display.superLongInvalidPropertyNameForNarrowTerminal\""
            .to_string(),
    ];
    let mut ascii_cfg = HudConfig::default();
    ascii_cfg.display.glyph_mode = GlyphMode::Ascii;
    let uni_cfg = HudConfig::default();

    assert_eq!(
        format_config_problem_line(&problems, &uni_cfg, Some(80)),
        format_config_problem_line_with_cmd(&problems, &uni_cfg, Some(80), false)
    );
    assert!(format_config_problem_line_with_cmd(&[], &uni_cfg, Some(80), true).is_none());

    let short_uni_42 =
        format_config_problem_line_with_cmd(&problems, &uni_cfg, Some(42), true).unwrap();
    let short_uni_42_plain = strip_ansi(&short_uni_42);
    assert!(
        short_uni_42_plain.ends_with(" · /hud"),
        "expected ` · /hud` suffix: {short_uni_42_plain}"
    );
    assert!(
        text_width(&short_uni_42_plain) <= 42,
        "got width {}: {short_uni_42_plain}",
        text_width(&short_uni_42_plain)
    );

    let short_ascii_42 =
        format_config_problem_line_with_cmd(&problems, &ascii_cfg, Some(42), true).unwrap();
    assert!(short_ascii_42.is_ascii());
    let short_ascii_42_plain = strip_ansi(&short_ascii_42);
    assert!(
        short_ascii_42_plain.ends_with(" . /hud"),
        "expected ` . /hud` suffix: {short_ascii_42_plain}"
    );
    assert!(
        text_width(&short_ascii_42_plain) <= 42,
        "got width {}: {short_ascii_42_plain}",
        text_width(&short_ascii_42_plain)
    );

    let prefixed_ascii_42 =
        format_config_problem_line_with_cmd(&problems, &ascii_cfg, Some(42), false).unwrap();
    let prefixed_ascii_42_plain = strip_ansi(&prefixed_ascii_42);
    assert!(prefixed_ascii_42_plain.ends_with(" . /agy-hud:hud"));
    assert!(short_ascii_42_plain.len() > prefixed_ascii_42_plain.len() - 8);

    let ultra_narrow_short =
        format_config_problem_line_with_cmd(&problems, &ascii_cfg, Some(20), true).unwrap();
    let ultra_narrow_plain = strip_ansi(&ultra_narrow_short);
    assert_eq!(ultra_narrow_plain, "! Config: /hud");
    assert!(text_width(&ultra_narrow_plain) <= 20);

    // `updates.enabled` schema description documents both `/hud-update` and `/agy-hud:hud-update`
    let updates_entry = CONFIG_SCHEMA
        .iter()
        .find(|e| e.key == "updates.enabled")
        .expect("updates.enabled schema entry");
    assert!(updates_entry.description.contains("/hud-update"));
    assert!(updates_entry.description.contains("/agy-hud:hud-update"));
}

#[test]
fn doctor_reports_unprefixed_skills_installed_conflict_and_not_installed() {
    // Omitted `unprefixedSkills` in serialized DoctorFacts deserializes to defaults cleanly
    let legacy_facts_json = json!({
        "cwd": "/Users/alice/project",
        "runtime": {
            "engine": "rust-native",
            "version": "1.8.0",
            "execPath": "/Users/alice/.gemini/antigravity-cli/agy-hud/bin/agy-hud",
            "supported": true
        },
        "plugin": {
            "root": "/Users/alice/.gemini/config/plugins/agy-hud",
            "version": "1.8.0"
        },
        "configPath": "/Users/alice/.gemini/antigravity-cli/agy-hud/config.json",
        "heartbeat": null
    });
    let parsed_facts: DoctorFacts =
        serde_json::from_value(legacy_facts_json).expect("legacy DoctorFacts JSON");
    assert_eq!(
        parsed_facts.unprefixed_skills,
        UnprefixedSkillsFacts::default()
    );

    let base_facts = DoctorFacts {
        cwd: "/Users/alice/project".to_string(),
        home_dir: "/Users/alice".to_string(),
        platform: "macos".to_string(),
        terminal: "Ghostty".to_string(),
        remote_session: false,
        locale: None,
        runtime: RuntimeFacts {
            engine: "rust-native".to_string(),
            version: "1.8.0".to_string(),
            exec_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/bin/agy-hud".to_string(),
            supported: true,
        },
        plugin: PluginFacts {
            root: "/Users/alice/.gemini/config/plugins/agy-hud".to_string(),
            version: "1.8.0".to_string(),
        },
        installed_plugin_status: "installed".to_string(),
        installed_plugin: None,
        unprefixed_skills: UnprefixedSkillsFacts {
            hud: "installed".to_string(),
            hud_update: "installed".to_string(),
            skills_dir: "/Users/alice/.gemini/config/skills".to_string(),
        },
        config_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/config.json".to_string(),
        heartbeat: None,
    };
    let paths = LogicPaths {
        config: "/Users/alice/.gemini/antigravity-cli/agy-hud/config.json".to_string(),
        cli_settings: "/Users/alice/.gemini/antigravity-cli/settings.json".to_string(),
        ..LogicPaths::default()
    };
    let settings_with_hud = json!({
        "statusLine": {
            "command": "/Users/alice/.gemini/antigravity-cli/agy-hud/bin/agy-hud",
            "enabled": true
        }
    })
    .to_string();
    let update_json = serde_json::to_string(&UpdateCacheSnapshot {
        checked_at: 1_700_000_000_000,
        latest_version: "9.9.0".to_string(),
        release_url: None,
    })
    .unwrap();

    // 1. Both unprefixed skills installed -> JSON + pretty + update finding uses `/hud-update`
    let installed_ctx = CommandContext {
        facts: &base_facts,
        paths: &paths,
        now_ms: 1_700_000_000_000,
        config_text: None,
        cli_settings_text: Some(&settings_with_hud),
        last_snapshot_text: None,
        update_cache_text: Some(&update_json),
        transcript_head: None,
        transcript_tail: None,
        task_md_text: None,
    };
    let doc_installed = execute_subcommand("doctor", &[], &installed_ctx);
    let doc_installed_json: Value = serde_json::from_str(&doc_installed.lines.join("\n")).unwrap();
    assert_eq!(doc_installed_json["unprefixedSkills"]["hud"], "installed");
    assert_eq!(
        doc_installed_json["unprefixedSkills"]["hudUpdate"],
        "installed"
    );
    assert_eq!(
        doc_installed_json["unprefixedSkills"]["skillsDir"],
        "/Users/alice/.gemini/config/skills"
    );
    let findings_installed = doc_installed_json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(findings_installed.contains("`/hud-update`"));
    assert!(!findings_installed.contains("/agy-hud:hud-update"));
    assert!(!findings_installed.contains("preserved"));
    assert!(!findings_installed.contains("register `/hud`"));

    let pretty_installed = execute_subcommand("doctor", &["--pretty".to_string()], &installed_ctx);
    let pretty_installed_plain = pretty_installed
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!pretty_installed_plain.contains("/Users/alice"));
    assert!(pretty_installed_plain.contains(
        "Unprefixed Skills: /hud=installed · /hud-update=installed (~/.gemini/config/skills)"
    ));
    assert!(pretty_installed_plain.contains("run `/hud-update` or `agy-hud update`"));

    // Preview honors `unprefixed_skills.hud == "installed"` for config problem line
    let preview_err = execute_subcommand(
        "preview",
        &["--state".to_string(), "idle".to_string()],
        &CommandContext {
            config_text: Some(r#"{"unknownKey":true}"#),
            ..installed_ctx
        },
    );
    let preview_err_plain = preview_err
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(preview_err_plain.contains("· /hud"));
    assert!(!preview_err_plain.contains("/agy-hud:hud"));

    // 2. Conflict on `/hud` and `/hud-update` -> finding preserves custom skills & directs to `/agy-hud:*`
    let mut conflict_facts = base_facts.clone();
    conflict_facts.unprefixed_skills = UnprefixedSkillsFacts {
        hud: "conflict".to_string(),
        hud_update: "conflict".to_string(),
        skills_dir: "/Users/alice/.gemini/config/skills".to_string(),
    };
    let conflict_ctx = CommandContext {
        facts: &conflict_facts,
        update_cache_text: Some(&update_json),
        ..installed_ctx
    };
    let doc_conflict = execute_subcommand("doctor", &[], &conflict_ctx);
    let doc_conflict_json: Value = serde_json::from_str(&doc_conflict.lines.join("\n")).unwrap();
    assert_eq!(doc_conflict_json["unprefixedSkills"]["hud"], "conflict");
    assert_eq!(
        doc_conflict_json["unprefixedSkills"]["hudUpdate"],
        "conflict"
    );
    let findings_conflict = doc_conflict_json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(findings_conflict.contains("preserved (not overwritten)"));
    assert!(findings_conflict.contains("`/hud`"));
    assert!(findings_conflict.contains("`/hud-update`"));
    assert!(findings_conflict.contains("/agy-hud:hud"));
    assert!(findings_conflict.contains("/agy-hud:hud-update"));
    assert!(!findings_conflict.contains("run `/hud-update`"));

    let pretty_conflict = execute_subcommand("doctor", &["--pretty".to_string()], &conflict_ctx);
    let pretty_conflict_plain = pretty_conflict
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!pretty_conflict_plain.contains("/Users/alice"));
    assert!(pretty_conflict_plain.contains(
        "Unprefixed Skills: /hud=conflict · /hud-update=conflict (~/.gemini/config/skills)"
    ));
    assert!(pretty_conflict_plain.contains("run `/agy-hud:hud-update` or `agy-hud update`"));

    // 3. `not-installed` when `kind == "agy-hud"` suggests `agy-hud install`, and falls back when `kind == "none"`
    let mut missing_facts = base_facts.clone();
    missing_facts.unprefixed_skills = UnprefixedSkillsFacts {
        hud: "not-installed".to_string(),
        hud_update: "not-installed".to_string(),
        skills_dir: "/Users/alice/.gemini/config/skills".to_string(),
    };
    let missing_ctx = CommandContext {
        facts: &missing_facts,
        update_cache_text: Some(&update_json),
        ..installed_ctx
    };
    let doc_missing = execute_subcommand("doctor", &[], &missing_ctx);
    let doc_missing_json: Value = serde_json::from_str(&doc_missing.lines.join("\n")).unwrap();
    assert_eq!(doc_missing_json["unprefixedSkills"]["hud"], "not-installed");
    assert_eq!(
        doc_missing_json["unprefixedSkills"]["hudUpdate"],
        "not-installed"
    );
    let findings_missing = doc_missing_json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(findings_missing.contains("agy-hud install"));
    assert!(findings_missing.contains("/hud"));
    assert!(findings_missing.contains("/hud-update"));
    assert!(findings_missing.contains("`/agy-hud:hud-update`"));

    let pretty_missing = execute_subcommand("doctor", &["--pretty".to_string()], &missing_ctx);
    let pretty_missing_plain = pretty_missing
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(pretty_missing_plain.contains("/hud=not-installed · /hud-update=not-installed"));

    // When `statusLine` is not configured (`kind == "none"`), do not emit duplicate install shortcut finding
    let no_sl_ctx = CommandContext {
        cli_settings_text: None,
        update_cache_text: None,
        ..missing_ctx
    };
    let doc_no_sl = execute_subcommand("doctor", &[], &no_sl_ctx);
    let doc_no_sl_json: Value = serde_json::from_str(&doc_no_sl.lines.join("\n")).unwrap();
    let findings_no_sl = doc_no_sl_json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(findings_no_sl.contains("No statusLine is configured"));
    assert!(!findings_no_sl.contains("shortcut skill is not installed"));
}

#[test]
fn quota_zero_percent_render_sentinel_filtering_and_cache_reconciliation() {
    let now_ms = 1_794_218_400_000i64;
    let future_reset = "2099-01-01T00:00:00Z".to_string();

    let mut quota = BTreeMap::new();
    // Sentinel and empty buckets must be dropped before canonical/argon routing
    quota.insert(
        "gemini-argon-wtu-weekly".to_string(),
        RawQuotaBucket::default(),
    );
    quota.insert(
        "3p-5h".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.9),
            reset_time: Some("1970-01-01T00:00:00Z".to_string()),
            ..Default::default()
        },
    );
    // Live exhausted bucket (Proto3 omitted `remaining_fraction` -> 0.0 remaining = 100% used)
    quota.insert(
        "gemini-5h".to_string(),
        RawQuotaBucket {
            remaining_fraction: None,
            remaining_amount: None,
            reset_time: Some(future_reset.clone()),
            reset_in_seconds: None,
            disabled: None,
        },
    );

    let mut stdin = sample_stdin(120);
    stdin.quota = Some(quota);

    // Stale cached 35% remaining must NOT resurrect live exhausted 0% remaining
    let mut cached_map = BTreeMap::new();
    cached_map.insert(
        "gemini-5h".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.35),
            reset_time: Some(future_reset),
            ..Default::default()
        },
    );
    let cached = CachedQuotaSnapshot {
        at: now_ms - 60_000,
        source: "agy-usage".to_string(),
        quota: cached_map,
    };

    let used_lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &HudConfig::default(),
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: Some(&cached),
        update_info: None,
        columns: Some(120),
        now_ms,
    });
    let quota_row = used_lines
        .iter()
        .find(|l| strip_ansi(l).contains("Quota(5h)"))
        .expect("exhausted quota row");
    assert!(
        quota_row.contains("\x1b[31m"),
        "100% used must style critical red: {quota_row:?}"
    );
    let quota_plain = strip_ansi(quota_row);
    assert!(
        quota_plain.contains("100%") && !quota_plain.contains("65%"),
        "expected 100% used without stale cache resurrection: {quota_plain}"
    );

    let mut rem_cfg = HudConfig::default();
    rem_cfg.display.quota_value = hud_core::types::QuotaValueMode::Remaining;
    let rem_lines = render_hud_lines(RenderInput {
        stdin: Some(&stdin),
        config: &rem_cfg,
        transcript: &TranscriptSummary::default(),
        vcs: None,
        memory: None,
        cached_quota: Some(&cached),
        update_info: None,
        columns: Some(120),
        now_ms,
    });
    let rem_plain = rem_lines
        .iter()
        .map(|l| strip_ansi(l))
        .find(|l| l.contains("Quota(5h)"))
        .unwrap();
    assert!(rem_plain.contains("0% left"), "{rem_plain}");
    assert_eq!(SENTINEL_RESET_CUTOFF_MS, 946_684_800_000);
}

#[test]
fn honest_cold_start_ui_showcase_and_db_ceiling_report_scrub() {
    assert!(hud_core::is_shadow_or_utility_model("M50"));
    assert!(!hud_core::is_shadow_or_utility_model("m500"));

    // 1. Cold start on 1M window with high token count (`940k` -> would be critical if 1M or 256k guessed!)
    //    must render neutral `940k/?` with ZERO bar, ZERO percentage, ZERO yellow/red alerts across all 4 modes.
    let cold_stdin = AgyStdinData {
        model: Some(ModelStdin {
            id: Some("Gemini 4 Argon (High)".to_string()),
            display_name: Some("Gemini 4 Argon (High)".to_string()),
            effort: Some("high".to_string()),
        }),
        context_window: Some(ContextWindowStdin {
            context_window_size: Some(1_048_576),
            used_percentage: Some(89.6),
            current_usage: Some(CurrentUsageStdin {
                input_tokens: Some(840_000),
                output_tokens: Some(0),
                cache_creation_input_tokens: Some(0),
                cache_read_input_tokens: Some(100_000),
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    for cv in [
        ContextValueMode::Both,
        ContextValueMode::Tokens,
        ContextValueMode::Percent,
        ContextValueMode::Remaining,
    ] {
        let mut cfg = HudConfig::default();
        cfg.display.context_value = cv;
        let lines = render_hud_lines(RenderInput {
            stdin: Some(&cold_stdin),
            config: &cfg,
            transcript: &TranscriptSummary::default(),
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(120),
            now_ms: 1_700_000_000_000,
        });
        let ctx_raw = lines
            .iter()
            .find(|l| strip_ansi(l).contains("Ctx"))
            .expect("context line");
        let ctx_plain = strip_ansi(ctx_raw);
        let before_cache = ctx_plain.split('·').next().unwrap_or("");
        assert!(
            before_cache.contains("940k/?")
                && !before_cache.contains('%')
                && !ctx_plain.contains("256k")
                && !ctx_plain.contains('█')
                && ctx_plain.contains("11% Cache"),
            "mode {cv:?} should show honest cold start with cache hit: {ctx_plain}"
        );
        assert!(
            !ctx_raw.contains("\x1b[31m") && !ctx_raw.contains("\x1b[33m"),
            "cold-start must never trigger yellow/red warnings: {ctx_raw:?}"
        );
        assert!(
            ctx_raw.contains("\x1b[1m940k\x1b[0m\x1b[2m/?\x1b[0m"),
            "expected neutral BOLD+DIM `940k/?`: {ctx_raw:?}"
        );
    }

    // Compaction badge stays preserved on honest cold start when `last_compaction_tokens` is None
    let badge_tx = TranscriptSummary {
        compaction_count: 1,
        ..TranscriptSummary::default()
    };
    let badge_cold = render_hud_lines(RenderInput {
        stdin: Some(&cold_stdin),
        config: &HudConfig::default(),
        transcript: &badge_tx,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(120),
        now_ms: 1_700_000_000_000,
    });
    let badge_plain = strip_ansi(
        badge_cold
            .iter()
            .find(|l| strip_ansi(l).contains("Ctx"))
            .unwrap(),
    );
    assert!(badge_plain.contains("940k/? (↻1)"), "{badge_plain}");

    let mut tok_cfg = HudConfig::default();
    tok_cfg.display.context_value = ContextValueMode::Tokens;
    let tok_cold = render_hud_lines(RenderInput {
        stdin: Some(&cold_stdin),
        config: &tok_cfg,
        transcript: &badge_tx,
        vcs: None,
        memory: None,
        cached_quota: None,
        update_info: None,
        columns: Some(120),
        now_ms: 1_700_000_000_000,
    });
    let tok_plain = strip_ansi(
        tok_cold
            .iter()
            .find(|l| strip_ansi(l).contains("Ctx"))
            .unwrap(),
    );
    assert!(tok_plain.contains("940k/? · ↻1"), "{tok_plain}");

    // Narrow width budget check for cold-start UI
    for width in [48usize, 68, 90] {
        let lines = render_hud_lines(RenderInput {
            stdin: Some(&cold_stdin),
            config: &HudConfig::default(),
            transcript: &TranscriptSummary::default(),
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(width),
            now_ms: 1_700_000_000_000,
        });
        for line in lines {
            assert!(text_width(&line) <= width, "overflow at {width}: {line}");
        }
    }

    // 2. Providing matching DbCeilingInfo unlocks exact percentage & bar, whereas model switch mismatch falls back to `?`
    let db_256 = DbCeilingInfo::new(
        256_000,
        DbCeilingSource::GenMetadata,
        Some("Gemini 4 Argon (High)".to_string()),
        Some("gemini 4 argon".to_string()),
    );
    let mut mid_stdin = cold_stdin.clone();
    mid_stdin
        .context_window
        .as_mut()
        .unwrap()
        .current_usage
        .as_mut()
        .unwrap()
        .input_tokens = Some(52_000); // 152k total
    let matching_lines = render_hud_lines_full(
        RenderInput {
            stdin: Some(&mid_stdin),
            config: &HudConfig::default(),
            transcript: &TranscriptSummary::default(),
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(120),
            now_ms: 1_700_000_000_000,
        },
        RenderExtras {
            db_ceiling: Some(&db_256),
            ..RenderExtras::default()
        },
    );
    let matching_plain = strip_ansi(&matching_lines[1]);
    assert!(
        matching_plain.contains("59.4% (152k/256k)"),
        "{matching_plain}"
    );

    let mut switched_stdin = mid_stdin.clone();
    switched_stdin.model = Some(ModelStdin {
        id: Some("Gemini 3 Flash".to_string()),
        display_name: Some("Gemini 3 Flash".to_string()),
        effort: None,
    });
    let mismatched_lines = render_hud_lines_full(
        RenderInput {
            stdin: Some(&switched_stdin),
            config: &HudConfig::default(),
            transcript: &TranscriptSummary::default(),
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(120),
            now_ms: 1_700_000_000_000,
        },
        RenderExtras {
            db_ceiling: Some(&db_256),
            ..RenderExtras::default()
        },
    );
    let mismatched_plain = strip_ansi(&mismatched_lines[1]);
    assert!(
        mismatched_plain.contains("152k/?") && !mismatched_plain.contains("256k"),
        "mismatched DB ceiling must fall back to honest cold start: {mismatched_plain}"
    );

    // 3. Showcase Scenarios 3 (`idle` -> `152k/?`) & 4 (`high-context` -> `89.6%` critical alert) + `report` db_ceiling scrub
    let facts = DoctorFacts {
        cwd: "/Users/alice/app".to_string(),
        home_dir: "/Users/alice".to_string(),
        platform: "macos".to_string(),
        terminal: "Ghostty".to_string(),
        remote_session: false,
        locale: None,
        runtime: RuntimeFacts {
            engine: "rust-native".to_string(),
            version: hud_core::types::HUD_VERSION.to_string(),
            exec_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/bin/agy-hud".to_string(),
            supported: true,
        },
        plugin: PluginFacts {
            root: "/Users/alice/.gemini/config/plugins/agy-hud".to_string(),
            version: hud_core::types::HUD_VERSION.to_string(),
        },
        installed_plugin_status: "installed".to_string(),
        installed_plugin: None,
        config_path: "/Users/alice/.gemini/antigravity-cli/agy-hud/config.json".to_string(),
        heartbeat: None,
        ..DoctorFacts::default()
    };
    let showcase = execute_subcommand(
        "preview",
        &["--state".to_string(), "showcase".to_string()],
        &CommandContext {
            facts: &facts,
            paths: &LogicPaths::default(),
            now_ms: 1_700_000_000_000,
            config_text: None,
            cli_settings_text: None,
            last_snapshot_text: None,
            update_cache_text: None,
            transcript_head: None,
            transcript_tail: None,
            task_md_text: None,
        },
    );
    let showcase_plain = showcase
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(showcase_plain.contains("152k/?"), "{showcase_plain}");
    assert!(showcase_plain.contains("89.6%"), "{showcase_plain}");

    let raw_db_json = json!({
        "version": 1,
        "ceilingTokens": 256_000,
        "source": "gen_metadata",
        "modelId": "Gemini 4 Argon (High)",
        "secretLeak": "/Users/alice/conversations/secret.db"
    })
    .to_string();
    let snap = json!({
        "stdin": &mid_stdin,
        "probes": { "db_ceiling": raw_db_json }
    })
    .to_string();
    let rep = execute_subcommand(
        "report",
        &["db-ceiling-check".to_string()],
        &CommandContext {
            facts: &facts,
            paths: &LogicPaths::default(),
            now_ms: 1_700_000_000_000,
            config_text: None,
            cli_settings_text: None,
            last_snapshot_text: Some(&snap),
            update_cache_text: None,
            transcript_head: None,
            transcript_tail: None,
            task_md_text: None,
        },
    );
    let rep_val: Value = serde_json::from_str(&rep.lines.join("\n")).unwrap();
    let scrubbed_db_str = rep_val["case"]["probes"]["db_ceiling"]
        .as_str()
        .expect("valid db_ceiling survives report");
    assert!(!scrubbed_db_str.contains("secretLeak"));
    assert!(!scrubbed_db_str.contains("/Users/alice"));
    let parsed_scrubbed = parse_db_ceiling_info(&ProbeAnswers {
        db_ceiling: Some(scrubbed_db_str.to_string()),
        ..Default::default()
    })
    .expect("scrubbed db_ceiling parses");
    assert_eq!(parsed_scrubbed.ceiling_tokens, 256_000);
    assert!(
        rep_val["renderedPlainLines"]
            .to_string()
            .contains("59.4% (152k/256k)")
    );

    // Ensure `resolve_compaction_ceiling_with_db` and `parse_db_ceiling_record` are exercised cleanly
    let direct_res = resolve_compaction_ceiling_with_db(
        1_048_576,
        &CompactionInputs {
            db_ceiling: parse_db_ceiling_record(scrubbed_db_str),
            ..Default::default()
        },
        Some(152_000),
        Some(152_000),
        Some("gemini 4 argon"),
    );
    assert_eq!(
        direct_res.effective,
        EffectiveCeiling::Scaled {
            ceiling_tokens: 256_000,
            source: CeilingSource::DbGenMetadata
        }
    );
}

#[test]
fn pr1_auto_edit_mode_subagent_status_fallback_and_standalone_right_align() {
    let render_plain = |stdin: &AgyStdinData, cfg: &HudConfig, cols: usize| -> Vec<String> {
        render_hud_lines(RenderInput {
            stdin: Some(stdin),
            config: cfg,
            transcript: &TranscriptSummary::default(),
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(cols),
            now_ms: 1_700_000_000_000,
        })
        .into_iter()
        .map(|l| strip_ansi(&l))
        .collect()
    };

    // 1. Both `auto_edit` and `accept-edits` render `[ACCEPT]`
    for mode in ["auto_edit", "accept-edits"] {
        let mut stdin = sample_stdin(120);
        stdin.cycle_mode = Some(mode.to_string());
        let plain = render_plain(&stdin, &HudConfig::default(), 120).join("\n");
        assert!(plain.contains("[ACCEPT]"), "mode={mode}: {plain}");

        let zh_cfg = HudConfig {
            language: Language::ZhHans,
            ..HudConfig::default()
        };
        let zh_plain = render_plain(&stdin, &zh_cfg, 120).join("\n");
        assert!(zh_plain.contains("[接受编辑]"), "mode={mode}: {zh_plain}");
    }

    // 2. `stdin.subagents` fallback uses `parse_subagent_status` (running/starting/in_progress case-insensitively)
    let mut sub_stdin = sample_stdin(120);
    sub_stdin.subagents = Some(vec![
        SubagentStdin {
            name: Some("a1".to_string()),
            role: None,
            status: Some("running".to_string()),
            model: None,
        },
        SubagentStdin {
            name: Some("a2".to_string()),
            role: None,
            status: Some(" Starting ".to_string()),
            model: None,
        },
        SubagentStdin {
            name: Some("a3".to_string()),
            role: None,
            status: Some("IN_PROGRESS".to_string()),
            model: None,
        },
        SubagentStdin {
            name: Some("a4".to_string()),
            role: None,
            status: Some("blocked".to_string()),
            model: None,
        },
        SubagentStdin {
            name: Some("a5".to_string()),
            role: None,
            status: Some("failed".to_string()),
            model: None,
        },
        SubagentStdin {
            name: Some("a6".to_string()),
            role: None,
            status: Some("completed".to_string()),
            model: None,
        },
    ]);
    let sub_plain = render_plain(&sub_stdin, &HudConfig::default(), 120).join("\n");
    assert!(
        sub_plain.contains("agents 3/6"),
        "expected 3/6 running subagents: {sub_plain}"
    );

    // 3. Standalone `rightAlign` across all 4 standalone cases in `compose_expanded_rows`
    let mut wide_stdin = sample_stdin(120);
    wide_stdin.cost = Some(CostStdin {
        total_usd: Some(0.55),
        subagent_usd: None,
        estimated: Some(false),
    });

    // 3a. `merge_groups.is_empty()` + `rightAlign: ["quota"]`
    let mut cfg_empty = HudConfig::default();
    cfg_empty.display.right_align = vec!["quota".to_string()];
    let lines_empty = render_plain(&wide_stdin, &cfg_empty, 120);
    let quota_empty = lines_empty
        .iter()
        .find(|l| l.contains("Quota("))
        .expect("quota row");
    assert_eq!(text_width(quota_empty), 120);
    assert!(quota_empty.starts_with("╰─   "), "{quota_empty:?}");

    // 3b. Ungrouped row (`quota` not in `merge_groups`)
    let mut cfg_ungrouped = HudConfig::default();
    cfg_ungrouped.display.merge_groups = vec![vec!["context".to_string(), "meta".to_string()]];
    cfg_ungrouped.display.right_align = vec!["quota".to_string()];
    let lines_ungrouped = render_plain(&wide_stdin, &cfg_ungrouped, 120);
    let quota_ungrouped = lines_ungrouped
        .iter()
        .find(|l| l.contains("Quota("))
        .expect("ungrouped quota row");
    assert_eq!(text_width(quota_ungrouped), 120);
    assert!(quota_ungrouped.starts_with("╰─   "), "{quota_ungrouped:?}");

    // 3c. Single member of group present (`["quota", "meta"]`, but `meta` not split cleanly -> hide cost/counts so `meta` is empty)
    let mut cfg_single = HudConfig::default();
    cfg_single.display.show_cost = false;
    cfg_single.display.show_counts = false;
    cfg_single.display.show_sandbox = false;
    cfg_single.display.merge_groups = vec![vec!["quota".to_string(), "meta".to_string()]];
    cfg_single.display.right_align = vec!["quota".to_string()];
    let lines_single = render_plain(&wide_stdin, &cfg_single, 120);
    let quota_single = lines_single
        .iter()
        .find(|l| l.contains("Quota("))
        .expect("single-member quota row");
    assert_eq!(text_width(quota_single), 120);
    assert!(quota_single.starts_with("╰─   "), "{quota_single:?}");

    // 3d. Auto-stack non-bar-aligned row (`["context", "meta"]` overflows narrow 78-col width and stacks, `meta` right-aligns)
    let mut cfg_autostack = HudConfig {
        line_layout: hud_core::types::LineLayout::Expanded,
        ..HudConfig::default()
    };
    cfg_autostack.display.show_daily_cost = true;
    cfg_autostack.display.show_weekly_cost = true;
    cfg_autostack.display.merge_groups = vec![vec!["context".to_string(), "meta".to_string()]];
    cfg_autostack.display.right_align = vec!["meta".to_string()];
    let mut dense_stdin = sample_stdin(78);
    dense_stdin.cost = Some(CostStdin {
        total_usd: Some(1.23),
        subagent_usd: None,
        estimated: Some(false),
    });
    dense_stdin.task_count = Some(3);
    let lines_autostack = render_plain(&dense_stdin, &cfg_autostack, 78);
    let meta_stacked = lines_autostack
        .iter()
        .find(|l| l.contains("$1.23") && !l.contains("Ctx"))
        .expect("auto-stacked standalone meta row");
    assert_eq!(text_width(meta_stacked), 78, "{lines_autostack:#?}");
    assert!(meta_stacked.starts_with("├─   "), "{meta_stacked:?}");

    // 3e. Ultrawide >= 180 Adaptive overflow fallback routes through `compose_expanded_rows`
    let mut cfg_180 = HudConfig::default();
    cfg_180.display.show_title = true;
    cfg_180.display.custom_tag = "ultrawide-overflow-tag".to_string();
    cfg_180.display.right_align = vec!["quota".to_string()];
    cfg_180.path_levels = hud_core::types::PathLevels::Full;
    let mut stdin_180 = wide_stdin;
    stdin_180.terminal_width = Some(180);
    stdin_180.conversation_title =
        Some("Very long conversation title forcing 180 col wrap".to_string());
    stdin_180.cwd = Some(
        "/Users/dev/workspace/org/division/extremely-long-monorepo-project-folder-name".to_string(),
    );
    stdin_180.workspace = Some(WorkspaceStdin {
        project_dir: Some(
            "/Users/dev/workspace/org/division/extremely-long-monorepo-project-folder-name"
                .to_string(),
        ),
        current_dir: Some(
            "/Users/dev/workspace/org/division/extremely-long-monorepo-project-folder-name"
                .to_string(),
        ),
    });
    let lines_180 = render_plain(&stdin_180, &cfg_180, 180);
    assert!(
        lines_180.len() >= 3,
        "expected ultrawide overflow to stack: {lines_180:#?}"
    );
    let quota_180 = lines_180
        .iter()
        .find(|l| l.contains("Quota("))
        .expect("180-col overflow quota row");
    assert_eq!(text_width(quota_180), 180, "{quota_180:?}");
    assert!(quota_180.starts_with("╰─   "), "{quota_180:?}");

    // 3f. Family quota bucket outranks `ai-credits` under 2-bucket display cap
    let mut fam_stdin = sample_stdin(120);
    fam_stdin.model = Some(ModelStdin {
        id: Some("gemini-4-neon-xhigh".to_string()),
        display_name: Some("Gemini 4 Neon (XHigh)".to_string()),
        effort: Some("xhigh".to_string()),
    });
    let mut fam_quota = BTreeMap::new();
    fam_quota.insert(
        "gemini-5h".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.8),
            reset_time: Some("2099-01-01T00:00:00Z".to_string()),
            ..Default::default()
        },
    );
    fam_quota.insert(
        "gemini-neon-weekly".to_string(),
        RawQuotaBucket {
            remaining_fraction: Some(0.5),
            reset_time: Some("2099-01-01T00:00:00Z".to_string()),
            ..Default::default()
        },
    );
    fam_quota.insert(
        "ai-credits".to_string(),
        RawQuotaBucket {
            remaining_amount: Some(42.0),
            reset_time: Some("2099-01-01T00:00:00Z".to_string()),
            ..Default::default()
        },
    );
    fam_stdin.quota = Some(fam_quota);
    let fam_out = render_plain(&fam_stdin, &HudConfig::default(), 120).join("\n");
    assert!(
        fam_out.contains("neon-weekly") && !fam_out.contains("ai-credits"),
        "expected dedicated family bucket before ai-credits under 2-bucket cap: {fam_out}"
    );
}

#[test]
fn pr3_subagent_model_badges_zero_io_stdin_telemetry_and_short_session_id() {
    let default_cfg = HudConfig::default();
    assert!(default_cfg.display.show_subagent_model);
    assert!(!default_cfg.display.show_session_id);

    let cfg_override = parse_config_str(Some(
        r#"{"display":{"showSubagentModel":false,"showSessionId":true}}"#,
    ));
    assert!(cfg_override.problems.is_empty());
    assert!(!cfg_override.config.display.show_subagent_model);
    assert!(cfg_override.config.display.show_session_id);

    let bad_cfg = parse_config_str(Some(
        r#"{"display":{"showSubagentModel":"yes","showSessionId":123}}"#,
    ));
    assert_eq!(bad_cfg.problems.len(), 2);

    let now_ms = 1_794_218_400_000i64;
    let render_with = |stdin: &AgyStdinData,
                       tx: &TranscriptSummary,
                       cfg: &HudConfig,
                       cols: usize|
     -> Vec<String> {
        render_hud_lines(RenderInput {
            stdin: Some(stdin),
            config: cfg,
            transcript: tx,
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(cols),
            now_ms,
        })
    };

    // 1. Subagent model badges (with role, without role, Blocked yellow icon, noise filtering, ASCII/Unicode/Nerd, toggle off)
    let sub_tx = TranscriptSummary {
        subagents: vec![
            SubagentEntry {
                name: "research".to_string(),
                role: Some("Architecture Reviewer".to_string()),
                model: Some("gemini-3.1-pro".to_string()),
                status: SubagentStatus::Running,
                started_at: Some(now_ms - 12_000),
                ended_at: None,
            },
            SubagentEntry {
                name: "builder".to_string(),
                role: None,
                model: Some("Gemini 4 Hydrogen (XHigh)".to_string()),
                status: SubagentStatus::Blocked,
                started_at: Some(now_ms - 8_000),
                ended_at: None,
            },
            SubagentEntry {
                name: "fallback".to_string(),
                role: Some("Noise Model".to_string()),
                model: Some("inherit".to_string()),
                status: SubagentStatus::Completed,
                started_at: Some(now_ms - 25_000),
                ended_at: Some(now_ms - 5_000),
            },
            SubagentEntry {
                name: "plain".to_string(),
                role: None,
                model: Some("AUTO".to_string()),
                status: SubagentStatus::Completed,
                started_at: Some(now_ms - 15_000),
                ended_at: Some(now_ms - 5_000),
            },
        ],
        ..TranscriptSummary::default()
    };
    let sub_stdin = sample_stdin(180);
    for gm in [GlyphMode::Unicode, GlyphMode::Nerd] {
        let mut cfg = HudConfig::default();
        cfg.display.glyph_mode = gm;
        let lines = render_with(&sub_stdin, &sub_tx, &cfg, 180);
        let sub_raw = lines
            .iter()
            .find(|l| strip_ansi(l).contains("Architecture Reviewer"))
            .expect("subagents line");
        let sub_plain = strip_ansi(sub_raw);
        assert!(
            sub_plain.contains("◐ Architecture Reviewer [research · 3.1-pro] (12s)"),
            "{gm:?}: {sub_plain}"
        );
        assert!(
            sub_plain.contains("◷ builder [Hydrogen] (8s)"),
            "{gm:?}: {sub_plain}"
        );
        assert!(
            sub_plain.contains("✓ Noise Model [fallback] (20s)"),
            "{gm:?}: {sub_plain}"
        );
        assert!(sub_plain.contains("✓ plain (10s)"), "{gm:?}: {sub_plain}");
        assert!(
            sub_raw.contains(&paint("◷", &[colors::YELLOW, BOLD])),
            "Blocked must stay yellow bold: {sub_raw:?}"
        );
        assert!(
            sub_raw.contains(&paint("3.1-pro", &[colors::MAGENTA]))
                && sub_raw.contains(&paint("Hydrogen", &[colors::MAGENTA])),
            "Model segment should be magenta: {sub_raw:?}"
        );
    }

    let mut ascii_cfg = HudConfig::default();
    ascii_cfg.display.glyph_mode = GlyphMode::Ascii;
    let ascii_sub_lines = render_with(&sub_stdin, &sub_tx, &ascii_cfg, 180);
    for l in &ascii_sub_lines {
        assert!(l.is_ascii(), "Non-ASCII leak: {}", strip_ansi(l));
    }
    let ascii_sub_raw = ascii_sub_lines
        .iter()
        .find(|l| strip_ansi(l).contains("Architecture Reviewer"))
        .unwrap();
    let ascii_sub_plain = strip_ansi(ascii_sub_raw);
    assert!(
        ascii_sub_plain.contains("~ Architecture Reviewer [research . 3.1-pro] (12s)"),
        "{ascii_sub_plain}"
    );
    assert!(
        ascii_sub_plain.contains("! builder [Hydrogen] (8s)"),
        "{ascii_sub_plain}"
    );
    assert!(
        ascii_sub_raw.contains(&paint("!", &[colors::YELLOW, BOLD])),
        "ASCII Blocked `!` must stay yellow bold: {ascii_sub_raw:?}"
    );

    let mut hidden_model_cfg = HudConfig::default();
    hidden_model_cfg.display.show_subagent_model = false;
    let hidden_sub_plain = render_with(&sub_stdin, &sub_tx, &hidden_model_cfg, 180)
        .into_iter()
        .map(|l| strip_ansi(&l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        hidden_sub_plain.contains("◐ Architecture Reviewer [research] (12s)")
            && hidden_sub_plain.contains("◷ builder (8s)")
            && !hidden_sub_plain.contains("3.1-pro")
            && !hidden_sub_plain.contains("Hydrogen"),
        "{hidden_sub_plain}"
    );

    // 2. `sandbox.allow_network` + `cost.subagent_usd` + `battle.{status, focused_arm}` across EN/ZH/JA and ASCII/Unicode/Nerd
    let mut rich_stdin = sample_stdin(150);
    rich_stdin.sandbox = Some(SandboxStdin {
        enabled: Some(true),
        allow_network: Some(true),
    });
    rich_stdin.cost = Some(CostStdin {
        total_usd: Some(1.38),
        subagent_usd: Some(0.42),
        estimated: Some(false),
    });
    rich_stdin.battle = Some(hud_core::types::BattleStdin {
        status: Some("voting".to_string()),
        focused_arm: Some(2),
    });

    for (lang, exp_battle, exp_cost, exp_sandbox) in [
        (
            Language::En,
            "[BATTLE #2]",
            "$1.38 (sub $0.42)",
            "🛡 Sandbox ON (net)",
        ),
        (
            Language::ZhHans,
            "[对战 #2]",
            "$1.38 (子代理 $0.42)",
            "🛡 沙箱开启 (联网)",
        ),
        (
            Language::Ja,
            "[対戦 #2]",
            "$1.38 (子 $0.42)",
            "🛡 サンドボックスON (ネット)",
        ),
    ] {
        let cfg = HudConfig {
            language: lang,
            ..HudConfig::default()
        };
        let lines = render_with(&rich_stdin, &TranscriptSummary::default(), &cfg, 150);
        let joined_raw = lines.join("\n");
        let joined_plain = lines
            .iter()
            .map(|l| strip_ansi(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined_plain.contains(exp_battle),
            "{lang:?}: {joined_plain}"
        );
        assert!(joined_plain.contains(exp_cost), "{lang:?}: {joined_plain}");
        assert!(
            joined_plain.contains(exp_sandbox),
            "{lang:?}: {joined_plain}"
        );
        assert!(
            joined_raw.contains(&paint(exp_battle, &[colors::MAGENTA, BOLD])),
            "Battle badge must be magenta+bold: {joined_raw:?}"
        );
        assert!(
            joined_raw.contains(&paint(exp_sandbox, &[colors::YELLOW, BOLD])),
            "Sandbox (net) must be yellow+bold: {joined_raw:?}"
        );
        assert!(
            joined_raw.contains("\x1b[2m("),
            "Subagent cost suffix must be DIM: {joined_raw:?}"
        );
    }

    // Nerd & ASCII checks for sandbox(net) + battle + subagent cost
    let mut nerd_cfg = HudConfig::default();
    nerd_cfg.display.glyph_mode = GlyphMode::Nerd;
    let nerd_plain = render_with(&rich_stdin, &TranscriptSummary::default(), &nerd_cfg, 150)
        .into_iter()
        .map(|l| strip_ansi(&l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(nerd_plain.contains("\u{f0a3} Sandbox ON (net)"));
    assert!(nerd_plain.contains("[BATTLE #2]"));
    assert!(nerd_plain.contains("$1.38 (sub $0.42)"));

    let ascii_rich_lines = render_with(&rich_stdin, &TranscriptSummary::default(), &ascii_cfg, 150);
    for l in &ascii_rich_lines {
        assert!(l.is_ascii(), "Non-ASCII leak: {}", strip_ansi(l));
    }
    let ascii_rich_plain = ascii_rich_lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(ascii_rich_plain.contains("Sandbox ON (net)"));
    assert!(ascii_rich_plain.contains("[BATTLE #2]"));
    assert!(ascii_rich_plain.contains("$1.38 (sub $0.42)"));

    // Sandbox allow_network=false keeps green Sandbox ON; disabled sandbox hides completely
    let mut safe_sb_stdin = rich_stdin.clone();
    safe_sb_stdin.sandbox = Some(SandboxStdin {
        enabled: Some(true),
        allow_network: Some(false),
    });
    let safe_sb_raw = render_with(
        &safe_sb_stdin,
        &TranscriptSummary::default(),
        &HudConfig::default(),
        150,
    )
    .join("\n");
    assert!(safe_sb_raw.contains(&paint("🛡 Sandbox ON", &[colors::GREEN, BOLD])));
    assert!(!strip_ansi(&safe_sb_raw).contains("(net)"));

    // Battle without focused_arm or focused_arm == 0 omits #N; inactive statuses suppressed
    for (status, arm, expected) in [
        ("active", None, Some("[BATTLE]")),
        ("active", Some(0), Some("[BATTLE]")),
        ("active", Some(-1), Some("[BATTLE]")),
        ("none", Some(1), None),
        ("INACTIVE", Some(1), None),
        ("off", Some(1), None),
        ("Disabled", Some(1), None),
        ("  ", Some(1), None),
    ] {
        let mut b_stdin = rich_stdin.clone();
        b_stdin.battle = Some(hud_core::types::BattleStdin {
            status: Some(status.to_string()),
            focused_arm: arm,
        });
        let b_plain = strip_ansi(
            &render_with(
                &b_stdin,
                &TranscriptSummary::default(),
                &HudConfig::default(),
                150,
            )[0],
        );
        match expected {
            Some(tok) => assert!(
                b_plain.contains(tok),
                "status={status} arm={arm:?}: {b_plain}"
            ),
            None => assert!(
                !b_plain.contains("BATTLE"),
                "status={status} arm={arm:?} should suppress: {b_plain}"
            ),
        }
    }

    // Subagent cost suppression (< 0.001, NaN, compact mode)
    for bad_sub in [None, Some(0.0), Some(0.0005), Some(-0.1), Some(f64::NAN)] {
        let mut c_stdin = rich_stdin.clone();
        c_stdin.cost = Some(CostStdin {
            total_usd: Some(1.38),
            subagent_usd: bad_sub,
            estimated: Some(false),
        });
        let c_plain = render_with(
            &c_stdin,
            &TranscriptSummary::default(),
            &HudConfig::default(),
            150,
        )
        .into_iter()
        .map(|l| strip_ansi(&l))
        .collect::<Vec<_>>()
        .join("\n");
        assert!(c_plain.contains("$1.38") && !c_plain.contains("(sub"));
    }
    let compact_cfg = HudConfig {
        line_layout: LineLayout::Compact,
        ..HudConfig::default()
    };
    let compact_plain = render_with(
        &rich_stdin,
        &TranscriptSummary::default(),
        &compact_cfg,
        180,
    )
    .into_iter()
    .map(|l| strip_ansi(&l))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(
        compact_plain.contains("$1.38") && !compact_plain.contains("(sub"),
        "Compact meta must omit subagent cost breakdown: {compact_plain}"
    );

    // 3. `exceeds_200k_tokens` cold-start subtle yellow `(>200k)` hint (and hidden when ceiling is known)
    let mut cold_200k_stdin = sample_stdin(140);
    cold_200k_stdin.exceeds_200k_tokens = Some(true);
    cold_200k_stdin.context_window = Some(ContextWindowStdin {
        context_window_size: Some(1_048_576),
        used_percentage: Some(34.5),
        current_usage: Some(CurrentUsageStdin {
            input_tokens: Some(362_000),
            ..Default::default()
        }),
        ..Default::default()
    });
    for cv in [
        ContextValueMode::Both,
        ContextValueMode::Tokens,
        ContextValueMode::Percent,
        ContextValueMode::Remaining,
    ] {
        let mut cfg = HudConfig::default();
        cfg.display.context_value = cv;
        let lines = render_with(&cold_200k_stdin, &TranscriptSummary::default(), &cfg, 140);
        let ctx_raw = lines
            .iter()
            .find(|l| strip_ansi(l).contains("Ctx"))
            .unwrap();
        let ctx_plain = strip_ansi(ctx_raw);
        assert!(
            ctx_plain.contains("362k/? (>200k)")
                && !ctx_plain.contains('█')
                && !ctx_plain.contains("34.5%"),
            "{cv:?}: {ctx_plain}"
        );
        assert!(
            ctx_raw.contains("\x1b[33m(>200k)\x1b[0m"),
            "Expected subtle yellow (>200k): {ctx_raw:?}"
        );
    }

    // Combined cleanly with compaction badge when ceiling is still unknown
    let comp_unknown_tx = TranscriptSummary {
        compaction_count: 1,
        ..TranscriptSummary::default()
    };
    let comp_both = strip_ansi(
        &render_with(
            &cold_200k_stdin,
            &comp_unknown_tx,
            &HudConfig::default(),
            140,
        )[1],
    );
    assert!(comp_both.contains("362k/? (>200k) (↻1)"), "{comp_both}");

    let mut tok_cfg = HudConfig::default();
    tok_cfg.display.context_value = ContextValueMode::Tokens;
    let comp_tok = strip_ansi(&render_with(&cold_200k_stdin, &comp_unknown_tx, &tok_cfg, 140)[1]);
    assert!(comp_tok.contains("362k/? (>200k) · ↻1"), "{comp_tok}");

    // Hidden when ceiling becomes known via DB metadata
    let db_512 = DbCeilingInfo::new(
        512_000,
        DbCeilingSource::GenMetadata,
        Some("Gemini 4 Argon (Medium)".to_string()),
        Some("gemini 4 argon".to_string()),
    );
    let known_lines = render_hud_lines_full(
        RenderInput {
            stdin: Some(&cold_200k_stdin),
            config: &HudConfig::default(),
            transcript: &TranscriptSummary::default(),
            vcs: None,
            memory: None,
            cached_quota: None,
            update_info: None,
            columns: Some(140),
            now_ms,
        },
        RenderExtras {
            db_ceiling: Some(&db_512),
            ..RenderExtras::default()
        },
    );
    let known_plain = strip_ansi(&known_lines[1]);
    assert!(known_plain.contains("(362k/512k)") && !known_plain.contains(">200k"));

    // 4. `display.showSessionId` extraction, fallback, sanitization & narrow-width step-0 drop
    let mut id_stdin = sample_stdin(160);
    id_stdin.conversation_id = Some("4a93ad0a-5f06-4795-ac00-41997a6de596".to_string());
    id_stdin.session_id = Some("sess9999-1111".to_string());
    id_stdin.conversation_title = Some("PR-3 Telemetry".to_string());

    // Default false -> hidden
    let no_id_header = strip_ansi(
        &render_with(
            &id_stdin,
            &TranscriptSummary::default(),
            &HudConfig::default(),
            160,
        )[0],
    );
    assert!(!no_id_header.contains("#4a93ad0a"), "{no_id_header}");

    // Enabled -> `#4a93ad0a` in DIM
    let mut sid_cfg = HudConfig::default();
    sid_cfg.display.show_session_id = true;
    sid_cfg.display.show_title = true;
    sid_cfg.display.custom_tag = "CORE-TAG".to_string();
    let sid_header_raw = &render_with(&id_stdin, &TranscriptSummary::default(), &sid_cfg, 160)[0];
    let sid_header_plain = strip_ansi(sid_header_raw);
    assert!(
        sid_header_plain.contains("#4a93ad0a") && sid_header_plain.contains("PR-3 Telemetry"),
        "{sid_header_plain}"
    );
    assert!(
        sid_header_raw.contains("\x1b[2m#4a93ad0a\x1b[0m"),
        "Session ID must use DIM styling: {sid_header_raw:?}"
    );

    // Fallback to `session_id` when `conversation_id` is blank or lacks ASCII alphanumeric chars
    for bad_conv_id in ["   ", "___", "---"] {
        let mut fallback_id_stdin = id_stdin.clone();
        fallback_id_stdin.conversation_id = Some(bad_conv_id.to_string());
        fallback_id_stdin.session_id = Some("\x1b[31mxy_98-ab123456\x1b[0m".to_string());
        let fallback_header = strip_ansi(
            &render_with(
                &fallback_id_stdin,
                &TranscriptSummary::default(),
                &sid_cfg,
                160,
            )[0],
        );
        assert!(
            fallback_header.contains("#xy98-ab1") && !fallback_header.contains("#---"),
            "bad_conv_id={bad_conv_id:?}: {fallback_header}"
        );
    }

    // Hyphen/punctuation-only IDs on both `conversation_id` and `session_id` are suppressed
    let mut suppressed_id_stdin = id_stdin.clone();
    suppressed_id_stdin.conversation_id = Some("---".to_string());
    suppressed_id_stdin.session_id = Some("___---".to_string());
    let suppressed_header = strip_ansi(
        &render_with(
            &suppressed_id_stdin,
            &TranscriptSummary::default(),
            &sid_cfg,
            160,
        )[0],
    );
    assert!(
        !suppressed_header.contains("│ #"),
        "hyphen-only IDs must be suppressed: {suppressed_header}"
    );

    // Narrow width drops BOTH `showTitle` and `showSessionId` in header_steps[0] while keeping `customTag`
    let step0_header =
        strip_ansi(&render_with(&id_stdin, &TranscriptSummary::default(), &sid_cfg, 92)[0]);
    assert!(
        !step0_header.contains("#4a93ad0a")
            && !step0_header.contains("PR-3 Telemetry")
            && step0_header.contains("CORE-TAG"),
        "header_steps[0] should drop title and session ID together before customTag: {step0_header}"
    );
}

#[test]
fn pr4_render_title_line_preview_title_and_doctor_title_diagnostics() {
    let assert_plain_title_invariants = |line: &str| {
        assert_eq!(
            strip_ansi(line),
            line,
            "title must have zero ANSI: {line:?}"
        );
        assert!(
            !line.contains('\x1b') && !line.contains('\n') && !line.contains('\r'),
            "title must be single-line without escapes: {line:?}"
        );
        assert!(
            !line.chars().any(|c| c.is_control()),
            "title must have zero control chars: {line:?}"
        );
        assert!(
            text_width(line) <= 80,
            "title width {} > 80: {line:?}",
            text_width(line)
        );
    };

    // 1. Minimal None stdin renders single state glyph
    let min_title = render_title_line(None, &HudConfig::default());
    assert_eq!(min_title, "●");
    assert_plain_title_invariants(&min_title);

    // 2. All agent_state glyphs in Unicode and ASCII modes
    for (state, uni_glyph, asc_glyph) in [
        ("idle", "●", "*"),
        ("thinking", "◆", "~"),
        ("working", "⚙", "*"),
        ("tool_use", "🔧", ">"),
        ("reviewing", "👁", "?"),
        ("authenticating", "🔑", "@"),
        ("initializing", "◌", "."),
        ("error", "✖", "X"),
        ("custom_unknown", "●", "*"),
    ] {
        let s = AgyStdinData {
            agent_state: Some(state.to_string()),
            cwd: Some("/work/agy-hud".to_string()),
            ..Default::default()
        };
        let uni = render_title_line(Some(&s), &HudConfig::default());
        assert_eq!(uni, format!("{uni_glyph} agy-hud"), "state={state}");
        assert_plain_title_invariants(&uni);

        let mut asc_cfg = HudConfig::default();
        asc_cfg.display.glyph_mode = GlyphMode::Ascii;
        let asc = render_title_line(Some(&s), &asc_cfg);
        assert_eq!(asc, format!("{asc_glyph} agy-hud"), "ascii state={state}");
        assert!(asc.is_ascii());
    }

    // 3. tool_confirmation_pending takes priority across languages & glyph modes
    let confirm_stdin = AgyStdinData {
        agent_state: Some("tool_use".to_string()),
        tool_confirmation_pending: Some(true),
        cwd: Some("/work/agy-hud".to_string()),
        model: Some(ModelStdin {
            display_name: Some("Gemini 4 Neon (High)".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        render_title_line(Some(&confirm_stdin), &HudConfig::default()),
        "⚠ CONFIRM? agy-hud · Neon"
    );
    let mut asc_cfg = HudConfig::default();
    asc_cfg.display.glyph_mode = GlyphMode::Ascii;
    let confirm_asc = render_title_line(Some(&confirm_stdin), &asc_cfg);
    assert_eq!(confirm_asc, "! CONFIRM? agy-hud . Neon");
    assert!(confirm_asc.is_ascii());
    let zh_cfg = HudConfig {
        language: Language::ZhHans,
        ..HudConfig::default()
    };
    assert_eq!(
        render_title_line(Some(&confirm_stdin), &zh_cfg),
        "⚠ 待确认! agy-hud · Neon"
    );

    // 4. Full prioritized composition: workspace priority, VCS branch/client/dirty, short model, context % & cold tokens, title
    let rich_stdin = AgyStdinData {
        agent_state: Some("idle".to_string()),
        cwd: Some("/fallback/cwd-dir".to_string()),
        workspace: Some(WorkspaceStdin {
            current_dir: Some(r"C:\Users\dev\agy-hud\".to_string()),
            project_dir: Some("/fallback/proj-dir".to_string()),
        }),
        vcs: Some(VcsStdin {
            vcs_type: Some("git".to_string()),
            branch: Some("main".to_string()),
            dirty: Some(true),
            client: None,
        }),
        model: Some(ModelStdin {
            id: Some("Gemini 4 Neon (High)".to_string()),
            display_name: Some("Gemini 4 Neon (High)".to_string()),
            effort: Some("high".to_string()),
        }),
        context_window: Some(ContextWindowStdin {
            used_percentage: Some(34.4),
            context_window_size: Some(256_000),
            ..Default::default()
        }),
        conversation_title: Some("Fix auth race".to_string()),
        ..Default::default()
    };
    let full_uni = render_title_line(Some(&rich_stdin), &HudConfig::default());
    assert_eq!(full_uni, "● agy-hud (main*) · Neon · 34% — Fix auth race");
    assert_plain_title_invariants(&full_uni);

    let full_asc = render_title_line(Some(&rich_stdin), &asc_cfg);
    assert_eq!(full_asc, "* agy-hud (main*) . Neon . 34% - Fix auth race");
    assert!(full_asc.is_ascii());

    // VCS client fallback + show_dirty=false + vcs.enabled=false + show_model=false
    let mut client_stdin = rich_stdin.clone();
    client_stdin.vcs = Some(VcsStdin {
        vcs_type: Some("citc".to_string()),
        branch: Some("   ".to_string()),
        dirty: Some(true),
        client: Some("citc-ws-42".to_string()),
    });
    assert_eq!(
        render_title_line(Some(&client_stdin), &HudConfig::default()),
        "● agy-hud (citc-ws-42*) · Neon · 34% — Fix auth race"
    );
    let mut cfg_flags = HudConfig::default();
    cfg_flags.vcs.show_dirty = false;
    cfg_flags.display.show_model = false;
    assert_eq!(
        render_title_line(Some(&client_stdin), &cfg_flags),
        "● agy-hud (citc-ws-42) · 34% — Fix auth race"
    );
    cfg_flags.vcs.enabled = false;
    assert_eq!(
        render_title_line(Some(&client_stdin), &cfg_flags),
        "● agy-hud · 34% — Fix auth race"
    );

    // Cold-start 1M window renders compact tokens (`149k`) without fabricating a percentage
    let cold_1m_stdin = AgyStdinData {
        agent_state: Some("working".to_string()),
        cwd: Some("/work/demo".to_string()),
        model: Some(ModelStdin {
            display_name: Some("Gemini 4 Argon (Medium)".to_string()),
            ..Default::default()
        }),
        context_window: Some(ContextWindowStdin {
            context_window_size: Some(1_048_576),
            used_percentage: Some(14.2),
            current_usage: Some(CurrentUsageStdin {
                input_tokens: Some(49_000),
                cache_read_input_tokens: Some(100_000),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let cold_title = render_title_line(Some(&cold_1m_stdin), &HudConfig::default());
    assert_eq!(cold_title, "⚙ demo · Argon · 149k");
    assert!(!cold_title.contains('%'));

    let mut win_scale_cfg = HudConfig::default();
    win_scale_cfg.display.context_scale = ContextScaleMode::Window;
    assert_eq!(
        render_title_line(Some(&cold_1m_stdin), &win_scale_cfg),
        "⚙ demo · Argon · 14%"
    );

    // 5. Control chars, newlines, ANSI/OSC hijacks, bidi overrides, long CJK titles & <= 80 col clamp
    let hostile_stdin = AgyStdinData {
        agent_state: Some("working".to_string()),
        workspace: Some(WorkspaceStdin {
            current_dir: Some(format!("/work/{}", "superlong-workspace-folder-".repeat(3))),
            project_dir: None,
        }),
        vcs: Some(VcsStdin {
            branch: Some("feat/\x1b[31mvery-long-branch-name\x1b[0m\n".to_string()),
            dirty: Some(true),
            ..Default::default()
        }),
        model: Some(ModelStdin {
            display_name: Some("Gemini 4 Hydrogen (XHigh)".to_string()),
            ..Default::default()
        }),
        conversation_title: Some(
            "\x1b]0;hijack\x07\x1b[31m中文超长标题测试一二三四五六七八九十\n第二行\u{202e}👨‍💻"
                .to_string(),
        ),
        ..Default::default()
    };
    let clamped_uni = render_title_line(Some(&hostile_stdin), &HudConfig::default());
    assert_plain_title_invariants(&clamped_uni);
    assert!(!clamped_uni.contains("hijack"));
    assert!(!clamped_uni.contains('\u{202e}'));

    let cjk_title_only = AgyStdinData {
        cwd: Some("/work/app".to_string()),
        conversation_title: Some("中文超长标题测试一二三四五六七八九十零一二三四".to_string()),
        ..Default::default()
    };
    let clipped_title = render_title_line(Some(&cjk_title_only), &HudConfig::default());
    assert_plain_title_invariants(&clipped_title);
    let (_, after_dash) = clipped_title.split_once(" — ").expect("em-dash separator");
    assert!(
        text_width(after_dash) <= 28 && after_dash.ends_with('…'),
        "conversation_title must clip to <=28 cols with ellipsis: {after_dash:?}"
    );

    // 6. `preview --title` across presets, showcase, custom JSON, --last, and --set overrides + `doctor` title diagnostics
    let facts = DoctorFacts {
        cwd: "/Users/alice/demo".to_string(),
        home_dir: "/Users/alice".to_string(),
        platform: "windows".to_string(),
        terminal: "Windows Terminal".to_string(),
        remote_session: false,
        locale: None,
        runtime: RuntimeFacts {
            engine: "rust-native".to_string(),
            version: "1.9.0".to_string(),
            exec_path: r"C:\Users\alice\.gemini\antigravity-cli\agy-hud\bin\agy-hud.exe"
                .to_string(),
            supported: true,
        },
        plugin: PluginFacts {
            root: r"C:\Users\alice\.gemini\config\plugins\agy-hud".to_string(),
            version: "1.9.0".to_string(),
        },
        installed_plugin_status: "installed".to_string(),
        installed_plugin: None,
        unprefixed_skills: UnprefixedSkillsFacts {
            hud: "installed".to_string(),
            hud_update: "installed".to_string(),
            skills_dir: String::new(),
        },
        config_path: r"C:\Users\alice\.gemini\antigravity-cli\agy-hud\config.json".to_string(),
        heartbeat: Some(hud_core::types::HeartbeatInfo {
            at: 1_700_000_000_000,
            runtime: "rust-native/1.9.0".to_string(),
            path: r"C:\Users\alice\.gemini\antigravity-cli\agy-hud\bin\agy-hud.exe".to_string(),
            outcome: "ok".to_string(),
            columns: Some(120),
            duration_ms: Some(2),
        }),
    };
    let snap_json = json!({
        "at": 1_700_000_000_000i64,
        "stdin": rich_stdin
    })
    .to_string();

    // 6a. `preview --title` with default state, --state confirm, --state showcase, --state JSON, --last, --set
    let base_ctx = CommandContext {
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

    let prev_default = execute_subcommand("preview", &["--title".to_string()], &base_ctx);
    assert!(!prev_default.failed && prev_default.lines.len() == 1);
    assert_plain_title_invariants(&prev_default.lines[0]);
    assert!(prev_default.lines[0].contains("🔧 agy-hud"));

    let prev_confirm = execute_subcommand(
        "preview",
        &[
            "--title".to_string(),
            "--state".to_string(),
            "confirm".to_string(),
            "--set".to_string(),
            "display.glyphMode=ascii".to_string(),
        ],
        &base_ctx,
    );
    assert!(!prev_confirm.failed && prev_confirm.lines.len() == 1);
    assert_eq!(prev_confirm.lines[0], "! CONFIRM? agy-hud . Argon . 152k");

    let prev_last = execute_subcommand(
        "preview",
        &[
            "--last".to_string(),
            "--title".to_string(),
            "--set".to_string(),
            "display.showModel=false".to_string(),
        ],
        &base_ctx,
    );
    assert!(!prev_last.failed && prev_last.lines.len() == 1);
    assert_eq!(
        prev_last.lines[0],
        "● agy-hud (main*) · 34% — Fix auth race"
    );

    let prev_json = execute_subcommand(
        "preview",
        &[
            "--state".to_string(),
            r#"{"cwd":"/work/repo","agent_state":"thinking","model":{"display_name":"Gemini 4 Neon"}}"#
                .to_string(),
            "--title".to_string(),
        ],
        &base_ctx,
    );
    assert!(!prev_json.failed && prev_json.lines.len() == 1);
    assert_eq!(prev_json.lines[0], "◆ repo · Neon");

    let prev_showcase = execute_subcommand(
        "preview",
        &[
            "--state".to_string(),
            "showcase".to_string(),
            "--title".to_string(),
        ],
        &base_ctx,
    );
    assert!(!prev_showcase.failed && prev_showcase.lines.len() >= 10);
    for l in &prev_showcase.lines {
        assert_eq!(strip_ansi(l), *l, "showcase --title must have zero ANSI");
    }

    // 6b. `doctor` title diagnostics across none, other (with secret redaction), agy-hud disabled, Windows quotes/spaces
    let doc_none_ctx = CommandContext {
        cli_settings_text: Some(
            r#"{"statusLine":{"type":"command","command":"C:\\Users\\alice\\agy-hud.exe"}}"#,
        ),
        ..base_ctx
    };
    let doc_none: Value = serde_json::from_str(
        &execute_subcommand("doctor", &[], &doc_none_ctx)
            .lines
            .join("\n"),
    )
    .unwrap();
    assert_eq!(doc_none["title"]["kind"], "none");
    assert_eq!(doc_none["title"]["enabled"], false);
    assert_eq!(doc_none["title"]["command"], Value::Null);
    assert!(
        doc_none["findings"][0]
            .as_str()
            .unwrap()
            .starts_with("None.")
    );

    let doc_other_ctx = CommandContext {
        cli_settings_text: Some(
            r#"{"statusLine":{"type":"command","command":"C:\\Users\\alice\\agy-hud.exe"},"title":{"type":"command","command":"/usr/local/bin/title.sh --token=topsecret123","enabled":true}}"#,
        ),
        ..base_ctx
    };
    let doc_other: Value = serde_json::from_str(
        &execute_subcommand("doctor", &[], &doc_other_ctx)
            .lines
            .join("\n"),
    )
    .unwrap();
    assert_eq!(doc_other["title"]["kind"], "other");
    assert_eq!(doc_other["title"]["enabled"], true);
    assert!(
        !doc_other["title"]["command"]
            .as_str()
            .unwrap()
            .contains("topsecret123")
    );
    assert!(
        doc_other["title"]["command"]
            .as_str()
            .unwrap()
            .contains("[REDACTED]")
    );
    assert!(
        doc_other["findings"][0]
            .as_str()
            .unwrap()
            .starts_with("None.")
    );

    // Clean Windows `agy-hud.exe title` MUST NOT trigger space warning because of `" title"` suffix!
    let doc_win_clean_ctx = CommandContext {
        cli_settings_text: Some(
            r#"{"statusLine":{"type":"command","command":"C:\\Users\\alice\\agy-hud.exe"},"title":{"type":"command","command":"C:\\Users\\alice\\agy-hud.exe title","enabled":true}}"#,
        ),
        ..base_ctx
    };
    let doc_win_clean: Value = serde_json::from_str(
        &execute_subcommand("doctor", &[], &doc_win_clean_ctx)
            .lines
            .join("\n"),
    )
    .unwrap();
    assert_eq!(doc_win_clean["title"]["kind"], "agy-hud");
    assert!(
        doc_win_clean["findings"][0]
            .as_str()
            .unwrap()
            .starts_with("None."),
        "clean Windows `agy-hud.exe title` must not false-positive on space: {:?}",
        doc_win_clean["findings"]
    );
    let doc_pretty = execute_subcommand("doctor", &["--pretty".to_string()], &doc_win_clean_ctx)
        .lines
        .iter()
        .map(|l| strip_ansi(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(doc_pretty.contains("Title            : kind=agy-hud · enabled=true"));

    // Disabled title + Windows double quotes + Windows path spaces trigger explicit findings
    let doc_win_quote_ctx = CommandContext {
        cli_settings_text: Some(
            r#"{"statusLine":{"type":"command","command":"C:\\Users\\alice\\agy-hud.exe"},"title":{"type":"command","command":"\"C:\\Users\\alice\\agy-hud.exe\" title","enabled":false}}"#,
        ),
        ..base_ctx
    };
    let doc_win_quote: Value = serde_json::from_str(
        &execute_subcommand("doctor", &[], &doc_win_quote_ctx)
            .lines
            .join("\n"),
    )
    .unwrap();
    let quote_findings = doc_win_quote["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        quote_findings.contains("enabled: false"),
        "{quote_findings}"
    );
    assert!(
        quote_findings.contains("Windows title.command contains double quotes"),
        "{quote_findings}"
    );

    let doc_win_space_ctx = CommandContext {
        cli_settings_text: Some(
            r#"{"statusLine":{"type":"command","command":"C:\\Users\\alice\\agy-hud.exe"},"title":{"type":"command","command":"C:\\Users\\Max Lee\\agy-hud.exe title","enabled":true}}"#,
        ),
        ..base_ctx
    };
    let doc_win_space: Value = serde_json::from_str(
        &execute_subcommand("doctor", &[], &doc_win_space_ctx)
            .lines
            .join("\n"),
    )
    .unwrap();
    let space_findings = doc_win_space["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        space_findings.contains("Windows title.command path contains spaces"),
        "{space_findings}"
    );
}
