use crate::ansi::{
    BOLD, DIM, ITALIC, RESET, colors, file_link, github_branch_link, paint, render_bar_with_mode,
    sanitize_text, sanitize_title_text, text_width, truncate_ansi_with_ellipsis,
};
use crate::config::HudConfig;
use crate::i18n::{Labels, get_labels};
use crate::probes::{is_newer_semver, normalize_semver_tag, resolve_output_speed_display};
use crate::stdin::{
    compute_quota_pace, extract_context_metrics_scaled, extract_quota_buckets_reconciled,
    format_compact_tokens, format_duration_seconds, format_model_name_with_glyphs,
    format_session_cost, format_workspace_breadcrumb, normalize_plan_tier,
};
use crate::types::{
    AgyStdinData, CachedQuotaSnapshot, ContextValueMode, CostTotals, GlyphMode, HUD_VERSION,
    LineLayout, MemoryProbeInfo, ModelFormatMode, PathLevels, QuotaPace, QuotaValueMode,
    SubagentEntry, SubagentStatus, TodoStatus, ToolStatus, TranscriptSummary, UpdateCacheSnapshot,
    VcsProbeInfo,
};

pub struct RenderInput<'a> {
    pub stdin: Option<&'a AgyStdinData>,
    pub config: &'a HudConfig,
    pub transcript: &'a TranscriptSummary,
    pub vcs: Option<&'a VcsProbeInfo>,
    pub memory: Option<&'a MemoryProbeInfo>,
    pub cached_quota: Option<&'a CachedQuotaSnapshot>,
    pub update_info: Option<&'a UpdateCacheSnapshot>,
    pub columns: Option<usize>,
    pub now_ms: i64,
}

fn sep(gm: GlyphMode) -> String {
    match gm {
        GlyphMode::Ascii => format!("{DIM} | {RESET}"),
        GlyphMode::Unicode | GlyphMode::Nerd => format!("{DIM} │ {RESET}"),
    }
}

fn dot(gm: GlyphMode) -> String {
    match gm {
        GlyphMode::Ascii => format!("{DIM} . {RESET}"),
        GlyphMode::Unicode | GlyphMode::Nerd => format!("{DIM} · {RESET}"),
    }
}

fn ellipsis(gm: GlyphMode) -> &'static str {
    match gm {
        GlyphMode::Ascii => "...",
        GlyphMode::Unicode | GlyphMode::Nerd => "…",
    }
}

fn render_state_badge(stdin: Option<&AgyStdinData>, labels: &Labels, gm: GlyphMode) -> String {
    let ascii = gm == GlyphMode::Ascii;
    if stdin
        .and_then(|s| s.tool_confirmation_pending)
        .unwrap_or(false)
    {
        let icon = if ascii { "!" } else { "⚠" };
        return paint(
            &format!("{icon} {}", labels.state.confirm),
            &[colors::YELLOW, BOLD],
        );
    }
    let state = stdin
        .and_then(|s| s.agent_state.as_deref())
        .unwrap_or("idle")
        .to_ascii_lowercase();

    match state.as_str() {
        "idle" => paint(
            &format!("{} {}", if ascii { "*" } else { "●" }, labels.state.idle),
            &[colors::GREEN, BOLD],
        ),
        "thinking" => paint(
            &format!(
                "{} {}",
                if ascii { "~" } else { "◆" },
                labels.state.thinking
            ),
            &[colors::YELLOW, BOLD],
        ),
        "working" => paint(
            &format!("{} {}", if ascii { "*" } else { "⚙" }, labels.state.working),
            &[colors::CYAN, BOLD],
        ),
        "tool_use" => paint(
            &format!(
                "{} {}",
                if ascii { ">" } else { "🔧" },
                labels.state.tool_use
            ),
            &[colors::MAGENTA, BOLD],
        ),
        "reviewing" => paint(
            &format!(
                "{} {}",
                if ascii { "?" } else { "👁" },
                labels.state.reviewing
            ),
            &[colors::BLUE, BOLD],
        ),
        "authenticating" => paint(
            &format!(
                "{} {}",
                if ascii { "@" } else { "🔑" },
                labels.state.authenticating
            ),
            &[colors::YELLOW, BOLD],
        ),
        "initializing" => paint(
            &format!(
                "{} {}",
                if ascii { "." } else { "◌" },
                labels.state.initializing
            ),
            &[colors::CYAN, BOLD],
        ),
        "error" => paint(
            &format!("{} {}", if ascii { "X" } else { "✖" }, labels.state.error),
            &[colors::RED, BOLD],
        ),
        other => {
            let clean = sanitize_text(other).to_ascii_uppercase();
            let text = if clean.is_empty() {
                labels.state.idle
            } else {
                clean.as_str()
            };
            paint(
                &format!("{} {text}", if ascii { "*" } else { "●" }),
                &[BOLD],
            )
        }
    }
}

fn render_mode_badges(
    stdin: Option<&AgyStdinData>,
    transcript: &TranscriptSummary,
    labels: &Labels,
) -> String {
    let mut parts = Vec::new();
    let raw_mode = stdin
        .and_then(|s| s.cycle_mode.as_deref())
        .filter(|s| !s.trim().is_empty())
        .or(transcript.active_slash_mode.as_deref())
        .unwrap_or("");
    let cycle = sanitize_text(raw_mode).to_ascii_lowercase();
    match cycle.as_str() {
        "plan" => parts.push(paint(
            &format!("[{}]", labels.mode.plan),
            &[colors::CYAN, BOLD],
        )),
        "auto" => parts.push(paint(
            &format!("[{}]", labels.mode.auto),
            &[colors::GREEN, BOLD],
        )),
        "accept-edits" => parts.push(paint(
            &format!("[{}]", labels.mode.accept_edits),
            &[colors::YELLOW, BOLD],
        )),
        _ => {}
    }

    let vim_mode = sanitize_text(
        stdin
            .and_then(|s| s.vim.as_ref())
            .and_then(|v| v.mode.as_deref())
            .unwrap_or(""),
    );
    if !vim_mode.is_empty() {
        let vim_color = if vim_mode == "INSERT" {
            colors::GREEN
        } else if vim_mode.starts_with("VISUAL") {
            colors::MAGENTA
        } else {
            colors::BLUE
        };
        parts.push(paint(&format!("[{vim_mode}]"), &[vim_color, BOLD]));
    }

    let custom_agent = sanitize_text(
        stdin
            .and_then(|s| s.agent.as_ref())
            .and_then(|a| a.name.as_deref())
            .unwrap_or(""),
    );
    if !custom_agent.is_empty() {
        parts.push(paint(&format!("@{custom_agent}"), &[colors::CYAN]));
    }

    parts.join(" ")
}

fn render_vcs_segment(
    vcs: Option<&VcsProbeInfo>,
    config: &HudConfig,
    show_extras: bool,
    gm: GlyphMode,
) -> String {
    let Some(vcs) = vcs else {
        return String::new();
    };
    if !config.vcs.enabled || vcs.branch.is_empty() {
        return String::new();
    }

    let prefix = vcs.vcs_type.as_str();
    let dirty_mark = if config.vcs.show_dirty && vcs.dirty {
        paint("*", &[colors::YELLOW, BOLD])
    } else {
        String::new()
    };
    let branch_color = if vcs.dirty && config.vcs.show_dirty {
        colors::RED
    } else {
        colors::BLUE
    };

    let (up_arrow, down_arrow) = match gm {
        GlyphMode::Ascii => ("^", "v"),
        GlyphMode::Unicode | GlyphMode::Nerd => ("↑", "↓"),
    };

    let mut extras = Vec::new();
    if show_extras && config.vcs.show_ahead_behind {
        let ahead_elevated = vcs.ahead > 0
            && ((config.vcs.push_critical_threshold > 0
                && vcs.ahead >= config.vcs.push_critical_threshold)
                || (config.vcs.push_warning_threshold > 0
                    && vcs.ahead >= config.vcs.push_warning_threshold));
        if ahead_elevated {
            let ahead_styles: &[&str] = if config.vcs.push_critical_threshold > 0
                && vcs.ahead >= config.vcs.push_critical_threshold
            {
                &[colors::RED, BOLD]
            } else {
                &[colors::YELLOW, BOLD]
            };
            let mut combined = paint(&format!("{up_arrow}{}", vcs.ahead), ahead_styles);
            if vcs.behind > 0 {
                combined.push_str(&paint(
                    &format!("{down_arrow}{}", vcs.behind),
                    &[colors::CYAN],
                ));
            }
            extras.push(combined);
        } else {
            let mut ab = String::new();
            if vcs.ahead > 0 {
                ab.push_str(&format!("{up_arrow}{}", vcs.ahead));
            }
            if vcs.behind > 0 {
                ab.push_str(&format!("{down_arrow}{}", vcs.behind));
            }
            if !ab.is_empty() {
                extras.push(paint(&ab, &[colors::CYAN]));
            }
        }
    }
    if vcs.conflicts > 0 {
        extras.push(paint(&format!("!{}", vcs.conflicts), &[colors::RED, BOLD]));
    }
    if show_extras && config.vcs.show_file_stats {
        let mut stats = Vec::new();
        if vcs.added > 0 {
            stats.push(paint(&format!("+{}", vcs.added), &[colors::GREEN]));
        }
        if vcs.modified > 0 {
            stats.push(paint(&format!("~{}", vcs.modified), &[colors::YELLOW]));
        }
        if vcs.deleted > 0 {
            stats.push(paint(&format!("-{}", vcs.deleted), &[colors::RED]));
        }
        if !stats.is_empty() {
            extras.push(stats.join(""));
        }
    }

    let extra_text = if extras.is_empty() {
        String::new()
    } else {
        format!(" {}", extras.join(" "))
    };

    let nerd_prefix = if gm == GlyphMode::Nerd {
        paint("\u{e725} ", &[colors::BLUE])
    } else {
        String::new()
    };

    let styled_branch = paint(&vcs.branch, &[branch_color, BOLD]);
    let linked_branch = match vcs.repo_url.as_deref() {
        Some(repo_url)
            if config.hyperlinks && !vcs.branch.starts_with('@') && vcs.branch != "detached" =>
        {
            github_branch_link(repo_url, &vcs.branch, &styled_branch, true)
        }
        _ => styled_branch,
    };

    format!(
        "{nerd_prefix}{}{linked_branch}{dirty_mark}{extra_text}{}",
        paint(&format!("{prefix}:("), &[DIM]),
        paint(")", &[DIM]),
    )
}

#[derive(Clone, Copy)]
struct HeaderOpts {
    show_title: bool,
    show_custom_tag: bool,
    show_duration: bool,
    show_update: bool,
    path_levels: PathLevels,
    show_launch_dir: bool,
    model_format: ModelFormatMode,
    show_plan_tier: bool,
    show_speed: bool,
    show_vcs_extras: bool,
    show_folder: bool,
    show_vcs: bool,
    show_mode: bool,
}

#[derive(Clone, Copy)]
struct MetricsOpts {
    compact_meta: bool,
    show_cache_hit_rate: bool,
    quota_bar_enabled: bool,
    context_value: ContextValueMode,
    show_200k: bool,
    show_compactions: bool,
    show_cost: bool,
    show_context_bar: bool,
    show_quota_reset: bool,
    max_quota_buckets: usize,
    show_meta: bool,
}

fn render_context_segment(
    stdin: Option<&AgyStdinData>,
    transcript: &TranscriptSummary,
    config: &HudConfig,
    labels: &Labels,
    effective_width: Option<usize>,
    opts: &MetricsOpts,
) -> String {
    let gm = config.display.glyph_mode;
    let metrics = extract_context_metrics_scaled(
        stdin,
        config.display.context_scale,
        transcript.compaction_count,
        transcript.last_compaction_tokens,
    );
    let pct_str = format!("{:.1}%", metrics.used_percent);
    let rem_str = format!(
        "{:.1}% {}",
        metrics.remaining_percent, labels.metrics.remaining
    );
    let tok_str = match (metrics.used_tokens, metrics.window_size) {
        (Some(u), Some(w)) => Some(format!(
            "{}/{}",
            format_compact_tokens(u),
            format_compact_tokens(w)
        )),
        (Some(u), None) => Some(format_compact_tokens(u)),
        _ => None,
    };

    let compaction_badge = if opts.show_compactions && transcript.compaction_count > 0 {
        let raw_badge = match gm {
            GlyphMode::Ascii => format!("c{}", transcript.compaction_count),
            GlyphMode::Unicode | GlyphMode::Nerd => format!("↻{}", transcript.compaction_count),
        };
        let badge_styles: &[&str] = if transcript.compaction_count >= 3 {
            &[colors::YELLOW, BOLD]
        } else {
            &[colors::CYAN, BOLD]
        };
        Some(paint(&raw_badge, badge_styles))
    } else {
        None
    };
    let mid_dot = match gm {
        GlyphMode::Ascii => " . ",
        GlyphMode::Unicode | GlyphMode::Nerd => " · ",
    };
    let num_styles: &[&str] =
        if metrics.used_percent >= (config.display.context_critical_threshold as f64) {
            &[colors::RED, BOLD]
        } else if metrics.used_percent >= (config.display.context_warning_threshold as f64) {
            &[colors::YELLOW, BOLD]
        } else {
            &[BOLD]
        };

    let value_text = match opts.context_value {
        ContextValueMode::Tokens => {
            let base = paint(&tok_str.unwrap_or(pct_str), &[BOLD]);
            match compaction_badge {
                Some(badge) => format!("{base}{}{badge}", paint(mid_dot, &[DIM])),
                None => base,
            }
        }
        ContextValueMode::Remaining => {
            let base = paint(&rem_str, &[BOLD]);
            match compaction_badge {
                Some(badge) => format!(
                    "{base} {}{}{}",
                    paint("(", &[DIM]),
                    badge,
                    paint(")", &[DIM])
                ),
                None => base,
            }
        }
        ContextValueMode::Both => match (tok_str, compaction_badge) {
            (Some(tok), Some(badge)) => format!(
                "{} {}{}{}",
                paint(&pct_str, num_styles),
                paint(&format!("({tok}{mid_dot}"), &[DIM]),
                badge,
                paint(")", &[DIM]),
            ),
            (Some(tok), None) => format!(
                "{} {}",
                paint(&pct_str, num_styles),
                paint(&format!("({tok})"), &[DIM])
            ),
            (None, Some(badge)) => format!(
                "{} {}{}{}",
                paint(&pct_str, num_styles),
                paint("(", &[DIM]),
                badge,
                paint(")", &[DIM]),
            ),
            (None, None) => paint(&pct_str, num_styles),
        },
        ContextValueMode::Percent => {
            let base = paint(&pct_str, num_styles);
            match compaction_badge {
                Some(badge) => format!(
                    "{base} {}{}{}",
                    paint("(", &[DIM]),
                    badge,
                    paint(")", &[DIM])
                ),
                None => base,
            }
        }
    };

    let bar_len =
        if config.line_layout == LineLayout::Adaptive && effective_width.is_some_and(|w| w < 120) {
            config.display.bar_length.min(8)
        } else {
            config.display.bar_length
        };

    let warn_at = config.display.context_warning_threshold as f64;
    let crit_at = config.display.context_critical_threshold as f64;

    let bar = if opts.show_context_bar {
        format!(
            "{} ",
            render_bar_with_mode(
                metrics.used_percent,
                bar_len,
                warn_at,
                crit_at,
                colors::GREEN,
                gm,
            )
        )
    } else {
        String::new()
    };

    let over_200k = if opts.show_200k && metrics.exceeds_200k {
        format!(" {}", paint("200k+", &[colors::YELLOW, BOLD]))
    } else {
        String::new()
    };

    let cache_part = match (opts.show_cache_hit_rate, metrics.cache_hit_percent) {
        (true, Some(hit)) => {
            let prefix = if gm == GlyphMode::Ascii { "" } else { "⚡ " };
            format!(
                "{}{}",
                dot(gm),
                paint(
                    &format!("{prefix}{hit}% {}", labels.metrics.cache),
                    &[colors::CYAN]
                )
            )
        }
        _ => String::new(),
    };

    format!(
        "{} {bar}{value_text}{over_200k}{cache_part}",
        paint(labels.metrics.context, &[DIM]),
    )
}

fn render_quota_segment(
    stdin: Option<&AgyStdinData>,
    cached_quota: Option<&CachedQuotaSnapshot>,
    now_ms: i64,
    config: &HudConfig,
    labels: &Labels,
    effective_width: Option<usize>,
    opts: &MetricsOpts,
) -> String {
    if !config.display.show_quota {
        return String::new();
    }
    let gm = config.display.glyph_mode;
    let buckets = extract_quota_buckets_reconciled(stdin, cached_quota, now_ms);
    if buckets.is_empty() {
        return String::new();
    }

    let mini_len =
        if config.line_layout == LineLayout::Adaptive && effective_width.is_some_and(|w| w < 120) {
            5
        } else {
            ((config.display.bar_length as f64 * 0.65).floor() as usize).max(5)
        };

    let rendered: Vec<String> = buckets
        .into_iter()
        .filter(|b| {
            if config.display.quota_threshold == 0 {
                return true;
            }
            let pace = if config.display.quota_pace {
                compute_quota_pace(b)
            } else {
                QuotaPace::Normal
            };
            if pace > QuotaPace::Normal {
                return true;
            }
            b.used_percent
                .is_none_or(|u| u >= config.display.quota_threshold as f64)
        })
        .take(opts.max_quota_buckets)
        .map(|b| {
            let label = if b.label.is_empty() {
                labels.metrics.quota.to_string()
            } else {
                format!("{}({})", labels.metrics.quota, b.label)
            };
            if let (Some(used), Some(rem)) = (b.used_percent, b.remaining_percent) {
                let pace = if config.display.quota_pace {
                    compute_quota_pace(&b)
                } else {
                    QuotaPace::Normal
                };
                let static_sev = if used >= 90.0 {
                    QuotaPace::Critical
                } else if used >= 70.0 {
                    QuotaPace::Warning
                } else {
                    QuotaPace::Normal
                };
                let effective_sev = static_sev.max(pace);
                let display_pct = match config.display.quota_value {
                    QuotaValueMode::Remaining => {
                        format!("{:.0}% {}", rem, labels.metrics.remaining)
                    }
                    QuotaValueMode::Used => format!("{:.0}%", used),
                };
                let pct_styles: &[&str] = match effective_sev {
                    QuotaPace::Critical => &[colors::RED, BOLD],
                    QuotaPace::Warning => &[colors::YELLOW, BOLD],
                    QuotaPace::Normal => &[BOLD],
                };
                let pace_suffix = match pace {
                    QuotaPace::Critical => paint(
                        if gm == GlyphMode::Ascii { "^" } else { "▲" },
                        &[colors::RED, BOLD],
                    ),
                    QuotaPace::Warning => paint(
                        if gm == GlyphMode::Ascii { "^" } else { "▲" },
                        &[colors::YELLOW, BOLD],
                    ),
                    QuotaPace::Normal => String::new(),
                };
                let bar_normal_color = match effective_sev {
                    QuotaPace::Critical => colors::RED,
                    QuotaPace::Warning => colors::YELLOW,
                    QuotaPace::Normal => colors::CYAN,
                };
                let mini_bar = if opts.quota_bar_enabled {
                    format!(
                        "{} ",
                        render_bar_with_mode(used, mini_len, 70.0, 90.0, bar_normal_color, gm)
                    )
                } else {
                    String::new()
                };
                let reset = match (opts.show_quota_reset, b.reset_in_seconds) {
                    (true, Some(sec)) => {
                        let dur = format_duration_seconds(sec);
                        let inner = if gm == GlyphMode::Nerd {
                            format!("(↻ {dur})")
                        } else {
                            format!("({dur})")
                        };
                        format!(" {}", paint(&inner, &[DIM]))
                    }
                    _ => String::new(),
                };
                format!(
                    "{} {mini_bar}{}{pace_suffix}{reset}",
                    paint(&label, &[DIM]),
                    paint(&display_pct, pct_styles),
                )
            } else {
                let amt = b
                    .remaining_amount
                    .map(|v| {
                        if v.fract() == 0.0 {
                            format!("{v:.0}")
                        } else {
                            v.to_string()
                        }
                    })
                    .unwrap_or_default();
                format!("{} {}", paint(&label, &[DIM]), paint(&amt, &[BOLD]),)
            }
        })
        .collect();

    rendered.join(&sep(gm))
}

struct MetaTaggedItem {
    tag: &'static str,
    rendered: String,
}

fn collect_counts_and_meta_items(
    stdin: Option<&AgyStdinData>,
    transcript: &TranscriptSummary,
    memory: Option<&MemoryProbeInfo>,
    cost_totals: Option<&CostTotals>,
    config: &HudConfig,
    labels: &Labels,
    opts: &MetricsOpts,
) -> Vec<MetaTaggedItem> {
    if !opts.show_meta {
        return Vec::new();
    }
    let gm = config.display.glyph_mode;
    let mut items = Vec::new();

    if opts.show_cost {
        if config.display.show_cost
            && let Some(cost) = stdin.and_then(|s| s.cost.as_ref())
            && let Some(usd) = cost.total_usd
            && let Some(formatted_usd) = format_session_cost(usd)
        {
            let prefix = if cost.estimated.unwrap_or(false) {
                "~"
            } else {
                ""
            };
            items.push(MetaTaggedItem {
                tag: "cost",
                rendered: paint(&format!("{prefix}{formatted_usd}"), &[colors::YELLOW, BOLD]),
            });
        }

        let fallback_est = stdin
            .and_then(|s| s.cost.as_ref())
            .and_then(|c| c.estimated)
            .unwrap_or(true);
        let fallback_usd = stdin
            .and_then(|s| s.cost.as_ref())
            .and_then(|c| c.total_usd);

        if config.display.show_daily_cost {
            let (usd_opt, est) = match cost_totals {
                Some(ct) => (Some(ct.today_usd), ct.estimated),
                None => (fallback_usd, fallback_est),
            };
            if let Some(usd) = usd_opt
                && let Some(formatted) = format_session_cost(usd)
            {
                let prefix = if est { "~" } else { "" };
                items.push(MetaTaggedItem {
                    tag: "dailyCost",
                    rendered: format!(
                        "{} {}",
                        paint(labels.metrics.today, &[DIM]),
                        paint(&format!("{prefix}{formatted}"), &[colors::YELLOW, BOLD]),
                    ),
                });
            }
        }

        if config.display.show_weekly_cost {
            let (usd_opt, est) = match cost_totals {
                Some(ct) => (Some(ct.week_usd), ct.estimated),
                None => (fallback_usd, fallback_est),
            };
            if let Some(usd) = usd_opt
                && let Some(formatted) = format_session_cost(usd)
            {
                let prefix = if est { "~" } else { "" };
                items.push(MetaTaggedItem {
                    tag: "weeklyCost",
                    rendered: format!(
                        "{} {}",
                        paint(labels.metrics.week, &[DIM]),
                        paint(&format!("{prefix}{formatted}"), &[colors::YELLOW, BOLD]),
                    ),
                });
            }
        }
    }

    if config.display.show_counts {
        if !transcript.subagents.is_empty() {
            let running = transcript
                .subagents
                .iter()
                .filter(|s| s.status == SubagentStatus::Running)
                .count();
            let total = transcript.subagents.len();
            let agent_label = if running > 0 {
                format!("{running}/{total}")
            } else {
                total.to_string()
            };
            let val_styles: &[&str] = if running > 0 {
                &[colors::CYAN, BOLD]
            } else {
                &[BOLD]
            };
            items.push(MetaTaggedItem {
                tag: "counts",
                rendered: format!(
                    "{} {}",
                    paint(labels.metrics.subagents, &[DIM]),
                    paint(&agent_label, val_styles),
                ),
            });
        } else if let Some(subagents) = stdin.and_then(|s| s.subagents.as_ref())
            && !subagents.is_empty()
        {
            let running = subagents
                .iter()
                .filter(|s| s.status.as_deref() == Some("running"))
                .count();
            let total = subagents.len();
            let agent_label = if running > 0 {
                format!("{running}/{total}")
            } else {
                total.to_string()
            };
            let val_styles: &[&str] = if running > 0 {
                &[colors::CYAN, BOLD]
            } else {
                &[BOLD]
            };
            items.push(MetaTaggedItem {
                tag: "counts",
                rendered: format!(
                    "{} {}",
                    paint(labels.metrics.subagents, &[DIM]),
                    paint(&agent_label, val_styles),
                ),
            });
        }

        if let Some(tasks) = stdin.and_then(|s| s.task_count)
            && tasks > 0
        {
            items.push(MetaTaggedItem {
                tag: "counts",
                rendered: format!(
                    "{} {}",
                    paint(labels.metrics.tasks, &[DIM]),
                    paint(&tasks.to_string(), &[colors::CYAN, BOLD]),
                ),
            });
        }

        if !opts.compact_meta
            && let Some(arts) = stdin.and_then(|s| s.artifact_count)
            && arts > 0
        {
            items.push(MetaTaggedItem {
                tag: "counts",
                rendered: format!(
                    "{} {}",
                    paint(labels.metrics.artifacts, &[DIM]),
                    paint(&arts.to_string(), &[BOLD]),
                ),
            });
        }

        if let Some(queued) = stdin.and_then(|s| s.pending_input_count)
            && queued > 0
        {
            items.push(MetaTaggedItem {
                tag: "counts",
                rendered: paint(
                    &format!("+{queued} {}", labels.metrics.queued),
                    &[colors::YELLOW, BOLD],
                ),
            });
        }
    }

    if config.display.show_memory
        && let Some(mem) = memory
    {
        let mem_styles: &[&str] = if mem.used_percent >= 90 {
            &[colors::RED, BOLD]
        } else if mem.used_percent >= 75 {
            &[colors::YELLOW, BOLD]
        } else {
            &[BOLD]
        };
        items.push(MetaTaggedItem {
            tag: "memory",
            rendered: format!(
                "{} {}",
                paint(labels.metrics.memory, &[DIM]),
                paint(&format!("{}%", mem.used_percent), mem_styles),
            ),
        });
    }

    if config.display.show_sandbox
        && stdin
            .and_then(|s| s.sandbox.as_ref())
            .and_then(|sb| sb.enabled)
            == Some(true)
    {
        let badge = match gm {
            GlyphMode::Ascii => labels.metrics.sandbox_on.to_string(),
            GlyphMode::Nerd => format!("\u{f0a3} {}", labels.metrics.sandbox_on),
            GlyphMode::Unicode => format!("🛡 {}", labels.metrics.sandbox_on),
        };
        items.push(MetaTaggedItem {
            tag: "sandbox",
            rendered: paint(&badge, &[colors::GREEN, BOLD]),
        });
    }

    items
}

fn render_counts_and_meta(
    stdin: Option<&AgyStdinData>,
    transcript: &TranscriptSummary,
    memory: Option<&MemoryProbeInfo>,
    cost_totals: Option<&CostTotals>,
    config: &HudConfig,
    labels: &Labels,
    opts: &MetricsOpts,
) -> String {
    let gm = config.display.glyph_mode;
    let items =
        collect_counts_and_meta_items(stdin, transcript, memory, cost_totals, config, labels, opts);
    items
        .into_iter()
        .map(|i| i.rendered)
        .collect::<Vec<_>>()
        .join(&dot(gm))
}

fn build_tools_line_with_limit(
    transcript: &TranscriptSummary,
    config: &HudConfig,
    max_tools: usize,
) -> Option<String> {
    if !config.display.show_tools || transcript.tools.is_empty() || max_tools == 0 {
        return None;
    }
    let gm = config.display.glyph_mode;
    let ascii = gm == GlyphMode::Ascii;

    let formatted: Vec<String> = transcript
        .tools
        .iter()
        .take(max_tools)
        .map(|t| {
            let (icon, name_styles): (String, &[&str]) = match t.status {
                ToolStatus::Running => (
                    paint(if ascii { "~" } else { "◐" }, &[colors::CYAN, BOLD]),
                    &[BOLD],
                ),
                ToolStatus::Error => (
                    paint(if ascii { "x" } else { "✗" }, &[colors::RED, BOLD]),
                    &[colors::RED, BOLD],
                ),
                ToolStatus::Completed => {
                    (paint(if ascii { "+" } else { "✓" }, &[colors::GREEN]), &[])
                }
            };
            let count_part = if t.count > 1 {
                let mult = if ascii { "x" } else { "×" };
                paint(&format!(" {mult}{}", t.count), &[colors::CYAN])
            } else {
                String::new()
            };
            let target_part = match t.target.as_deref() {
                Some(tgt) if !tgt.is_empty() => paint(&format!(": {tgt}"), &[DIM]),
                _ => String::new(),
            };
            format!(
                "{icon} {}{count_part}{target_part}",
                paint(&t.name, name_styles)
            )
        })
        .collect();

    (!formatted.is_empty()).then(|| formatted.join(&dot(gm)))
}

fn render_tools_line(
    transcript: &TranscriptSummary,
    config: &HudConfig,
    line_budget: Option<usize>,
) -> Option<String> {
    let max_visible = config.display.tools_max_visible;
    let candidate = build_tools_line_with_limit(transcript, config, max_visible)?;
    if let Some(budget) = line_budget
        && text_width(&candidate) > budget
        && max_visible > 2
    {
        return build_tools_line_with_limit(transcript, config, 2);
    }
    Some(candidate)
}

fn render_skills_mcp_line(
    transcript: &TranscriptSummary,
    config: &HudConfig,
    labels: &Labels,
) -> Option<String> {
    if !config.display.show_skills_and_mcp {
        return None;
    }
    let gm = config.display.glyph_mode;
    let mut parts = Vec::new();

    if !transcript.skills.is_empty() {
        let start = transcript.skills.len().saturating_sub(4);
        let shown = transcript.skills[start..].join(", ");
        parts.push(format!(
            "{} {}",
            paint(&format!("{}:", labels.metrics.skills), &[DIM]),
            paint(&shown, &[colors::MAGENTA]),
        ));
    }
    if !transcript.mcp_servers.is_empty() {
        let start = transcript.mcp_servers.len().saturating_sub(4);
        let mcp_items: Vec<String> = transcript.mcp_servers[start..]
            .iter()
            .map(|srv| {
                if transcript.mcp_errors.contains(srv) {
                    paint(&format!("{srv}!"), &[colors::RED, BOLD])
                } else {
                    paint(srv, &[colors::CYAN])
                }
            })
            .collect();
        parts.push(format!(
            "{} {}",
            paint(&format!("{}:", labels.metrics.mcp), &[DIM]),
            mcp_items.join(", "),
        ));
    }

    (!parts.is_empty()).then(|| parts.join(&sep(gm)))
}

const SUBAGENT_COMPLETE_HOLD_MS: i64 = 60_000;

fn subagent_sort_rank(status: SubagentStatus) -> u8 {
    match status {
        SubagentStatus::Running => 0,
        SubagentStatus::Blocked => 1,
        SubagentStatus::Error => 2,
        SubagentStatus::Completed => 3,
    }
}

fn render_subagents_line(
    transcript: &TranscriptSummary,
    config: &HudConfig,
    now_ms: i64,
) -> Option<String> {
    if !config.display.show_subagents || transcript.subagents.is_empty() {
        return None;
    }
    let gm = config.display.glyph_mode;
    let ascii = gm == GlyphMode::Ascii;

    let mut visible: Vec<&SubagentEntry> = transcript
        .subagents
        .iter()
        .filter(|ag| {
            if ag.status != SubagentStatus::Completed {
                return true;
            }
            match ag.ended_at {
                Some(end_ms) if now_ms > 0 => {
                    now_ms.saturating_sub(end_ms) <= SUBAGENT_COMPLETE_HOLD_MS
                }
                _ => true,
            }
        })
        .collect();
    if visible.is_empty() {
        return None;
    }
    visible.sort_by_key(|ag| subagent_sort_rank(ag.status));

    let formatted: Vec<String> = visible
        .into_iter()
        .take(4)
        .map(|ag| {
            let active = matches!(ag.status, SubagentStatus::Running | SubagentStatus::Blocked);
            let icon = match ag.status {
                SubagentStatus::Running => {
                    paint(if ascii { "~" } else { "◐" }, &[colors::CYAN, BOLD])
                }
                SubagentStatus::Blocked => {
                    paint(if ascii { "!" } else { "◷" }, &[colors::YELLOW, BOLD])
                }
                SubagentStatus::Error => paint(if ascii { "x" } else { "✗" }, &[colors::RED, BOLD]),
                SubagentStatus::Completed => paint(if ascii { "+" } else { "✓" }, &[colors::GREEN]),
            };
            let title = match ag.role.as_deref() {
                Some(role) if !role.is_empty() => {
                    let styled_role = if active {
                        paint(role, &[BOLD])
                    } else {
                        role.to_string()
                    };
                    format!("{styled_role} {}", paint(&format!("[{}]", ag.name), &[DIM]))
                }
                _ => {
                    if active {
                        paint(&ag.name, &[BOLD])
                    } else {
                        ag.name.clone()
                    }
                }
            };
            let elapsed = if let Some(start) = ag.started_at {
                let end = ag.ended_at.unwrap_or(now_ms);
                if end >= start {
                    format!(
                        " {}",
                        paint(
                            &format!(
                                "({})",
                                format_duration_seconds(((end - start) as f64) / 1000.0)
                            ),
                            &[DIM],
                        )
                    )
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            format!("{icon} {title}{elapsed}")
        })
        .collect();

    Some(formatted.join(&sep(gm)))
}

fn render_todos_line(
    transcript: &TranscriptSummary,
    config: &HudConfig,
    labels: &Labels,
) -> Option<String> {
    if !config.display.show_todos || transcript.todos.is_empty() {
        return None;
    }
    let gm = config.display.glyph_mode;
    let ascii = gm == GlyphMode::Ascii;

    let total = transcript.todos.len();
    let completed = transcript
        .todos
        .iter()
        .filter(|t| t.status == TodoStatus::Completed)
        .count();
    let active = transcript
        .todos
        .iter()
        .find(|t| t.status == TodoStatus::InProgress)
        .or_else(|| {
            transcript
                .todos
                .iter()
                .find(|t| t.status == TodoStatus::Pending)
        });

    let Some(active) = active else {
        return Some(format!(
            "{} {}",
            paint(if ascii { "+" } else { "✓" }, &[colors::GREEN, BOLD]),
            paint(
                &format!("{}: {completed}/{total} (100%)", labels.metrics.todos),
                &[colors::GREEN, BOLD]
            ),
        ));
    };

    let pct = ((completed as f64 / total as f64) * 100.0).round() as usize;
    let icon = if active.status == TodoStatus::InProgress {
        paint(if ascii { ">" } else { "▸" }, &[colors::CYAN, BOLD])
    } else {
        paint(if ascii { "-" } else { "◦" }, &[DIM])
    };
    let mid_dot = if ascii { "." } else { "·" };
    Some(format!(
        "{icon} {} {}",
        paint(&active.text, &[BOLD]),
        paint(
            &format!("({completed}/{total} {mid_dot} {pct}%)"),
            &[colors::CYAN]
        ),
    ))
}

fn join_with_rail(lines: Vec<String>, use_box_rail: bool, gm: GlyphMode) -> Vec<String> {
    if !use_box_rail || lines.len() <= 1 {
        return lines;
    }
    let (top, mid, bot) = match gm {
        GlyphMode::Ascii => ("+-", "|-", "+-"),
        GlyphMode::Unicode | GlyphMode::Nerd => ("╭─", "├─", "╰─"),
    };
    let last = lines.len() - 1;
    lines
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            if idx == 0 {
                format!("{DIM}{top}{RESET} {line}")
            } else if idx == last {
                format!("{DIM}{bot}{RESET} {line}")
            } else {
                format!("{DIM}{mid}{RESET} {line}")
            }
        })
        .collect()
}

fn join_left_right(
    left: String,
    right: String,
    fallback_sep: &str,
    line_budget: Option<usize>,
) -> String {
    if left.is_empty() {
        return right;
    }
    if right.is_empty() {
        return left;
    }
    if let Some(budget) = line_budget {
        let lw = text_width(&left);
        let rw = text_width(&right);
        if lw + 2 + rw <= budget {
            let pad = " ".repeat(budget - lw - rw);
            return format!("{left}{pad}{right}");
        }
    }
    format!("{left}{fallback_sep}{right}")
}

fn align_bar_prefix_pair(ctx_line: &mut String, quota_line: &mut String) {
    let Some(ctx_sp) = ctx_line.find(' ') else {
        return;
    };
    let Some(quota_sp) = quota_line.find(' ') else {
        return;
    };
    let ctx_label_w = text_width(&ctx_line[..ctx_sp]);
    let quota_label_w = text_width(&quota_line[..quota_sp]);
    if ctx_label_w < quota_label_w {
        ctx_line.insert_str(ctx_sp, &" ".repeat(quota_label_w - ctx_label_w));
    } else if quota_label_w < ctx_label_w {
        quota_line.insert_str(quota_sp, &" ".repeat(ctx_label_w - quota_label_w));
    }
}

fn round_tps(tps: f64) -> u64 {
    if tps.is_finite() && tps > 0.0 {
        tps.round() as u64
    } else {
        0
    }
}

/// Maps a throughput value (`tok/s`, rounded to the nearest displayed integer) to its tier emoji.
pub fn speed_tier_emoji(tps: f64) -> &'static str {
    match round_tps(tps) {
        0..=49 => "🐢",
        50..=99 => "🚲",
        100..=149 => "🚜",
        150..=199 => "🚗",
        200..=249 => "🚄",
        250..=299 => "✈️",
        _ => "🚀",
    }
}

/// Formats the header throughput badge (e.g. `🚲 ~68 tok/s (42~112)`), omitting the emoji in
/// `GlyphMode::Ascii` and hiding `(min~max)` until two distinct rounded integers exist.
pub fn format_speed_badge(
    current_tps: f64,
    estimated: bool,
    min_tps: Option<f64>,
    max_tps: Option<f64>,
    gm: GlyphMode,
) -> String {
    let rounded = round_tps(current_tps);
    let prefix = if estimated { "~" } else { "" };
    let emoji_prefix = match gm {
        GlyphMode::Ascii => String::new(),
        GlyphMode::Unicode | GlyphMode::Nerd => format!("{} ", speed_tier_emoji(current_tps)),
    };
    let range_suffix = match (min_tps, max_tps) {
        (Some(min_v), Some(max_v))
            if min_v.is_finite() && max_v.is_finite() && min_v >= 0.0 && max_v >= 0.0 =>
        {
            let min_r = round_tps(min_v);
            let max_r = round_tps(max_v);
            if min_r < max_r {
                format!(" {}", paint(&format!("({min_r}~{max_r})"), &[DIM]))
            } else {
                String::new()
            }
        }
        _ => String::new(),
    };
    format!(
        "{emoji_prefix}{} {}{range_suffix}",
        paint(&format!("{prefix}{rounded}"), &[colors::CYAN, BOLD]),
        paint("tok/s", &[DIM]),
    )
}

fn build_header_line(
    input: &RenderInput<'_>,
    labels: &Labels,
    opts: &HeaderOpts,
    line_budget: Option<usize>,
) -> String {
    let RenderInput {
        stdin,
        config,
        transcript,
        vcs,
        update_info,
        now_ms,
        ..
    } = *input;
    let gm = config.display.glyph_mode;
    let mut header_parts: Vec<String> = Vec::new();
    let mut right_parts: Vec<String> = Vec::new();
    let right_align = &config.display.right_align;

    if config.display.show_state {
        header_parts.push(render_state_badge(stdin, labels, gm));
    }
    if opts.show_mode {
        let modes = render_mode_badges(stdin, transcript, labels);
        if !modes.is_empty() {
            header_parts.push(modes);
        }
    }
    if config.display.show_model {
        let model_str = format_model_name_with_glyphs(
            stdin.and_then(|s| s.model.as_ref()),
            opts.model_format,
            config.display.effort_format,
            gm,
        );
        if !model_str.is_empty() {
            let tier = match (
                opts.show_plan_tier,
                stdin.and_then(|s| s.plan_tier.as_deref()),
            ) {
                (true, Some(pt)) => {
                    let norm = normalize_plan_tier(pt);
                    if norm.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", paint(&norm, &[colors::YELLOW, BOLD]))
                    }
                }
                _ => String::new(),
            };
            let speed_suffix = if opts.show_speed {
                match resolve_output_speed_display(stdin, transcript, None, now_ms) {
                    Some(d) => format!(
                        "{}{}",
                        dot(gm),
                        format_speed_badge(d.current_tps, d.estimated, d.min_tps, d.max_tps, gm),
                    ),
                    None => String::new(),
                }
            } else {
                String::new()
            };
            header_parts.push(format!(
                "{}{tier}{speed_suffix}",
                paint(&format!("[{model_str}]"), &[colors::MAGENTA, ITALIC])
            ));
        }
    }
    if opts.show_folder || opts.show_vcs {
        let project_dir = stdin.and_then(|s| {
            s.workspace
                .as_ref()
                .and_then(|w| w.project_dir.as_deref())
                .filter(|d| !d.trim().is_empty())
        });
        let cur_or_cwd = stdin.and_then(|s| {
            s.workspace
                .as_ref()
                .and_then(|w| w.current_dir.as_deref())
                .filter(|d| !d.trim().is_empty())
                .or_else(|| s.cwd.as_deref().filter(|d| !d.trim().is_empty()))
        });
        let raw_dir = cur_or_cwd.or(project_dir);
        let linked_dir = if opts.show_folder {
            let short_dir = format_workspace_breadcrumb(
                project_dir,
                raw_dir,
                opts.path_levels,
                opts.show_launch_dir,
                gm,
            );
            match raw_dir {
                Some(dir) if !short_dir.is_empty() => {
                    let folder_prefix = if gm == GlyphMode::Nerd {
                        paint("\u{f07c} ", &[colors::BLUE])
                    } else {
                        String::new()
                    };
                    format!(
                        "{folder_prefix}{}",
                        file_link(dir, &paint(&short_dir, &[BOLD]), config.hyperlinks)
                    )
                }
                _ => String::new(),
            }
        } else {
            String::new()
        };
        let vcs_seg = if opts.show_vcs {
            render_vcs_segment(vcs, config, opts.show_vcs_extras, gm)
        } else {
            String::new()
        };
        let proj_vcs: Vec<&str> = [linked_dir.as_str(), vcs_seg.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        if !proj_vcs.is_empty() {
            header_parts.push(proj_vcs.join(" "));
        }
    }
    if opts.show_title
        && let Some(raw_title) = stdin.and_then(|s| s.conversation_title.as_deref())
    {
        let clean_title = sanitize_title_text(raw_title);
        if !clean_title.is_empty() {
            let clipped = truncate_ansi_with_ellipsis(&clean_title, 24, ellipsis(gm));
            header_parts.push(format!("\"{clipped}\""));
        }
    }
    if opts.show_duration
        && let Some(started_at) = transcript.session_started_at
        && now_ms >= started_at
    {
        let elapsed_sec = ((now_ms - started_at) as f64) / 1000.0;
        if elapsed_sec >= 5.0 {
            let dur = format_duration_seconds(elapsed_sec);
            let badge = match gm {
                GlyphMode::Ascii => dur,
                GlyphMode::Nerd => format!("\u{ee9c} {dur}"),
                GlyphMode::Unicode => format!("⏱ {dur}"),
            };
            let styled = paint(&badge, &[DIM]);
            if right_align.iter().any(|s| s == "duration") {
                right_parts.push(styled);
            } else {
                header_parts.push(styled);
            }
        }
    }
    if opts.show_custom_tag && !config.display.custom_tag.is_empty() {
        let tag = sanitize_text(&config.display.custom_tag);
        if !tag.is_empty() {
            let styled = paint(&tag, &[colors::CYAN]);
            if right_align.iter().any(|s| s == "customTag") {
                right_parts.push(styled);
            } else {
                header_parts.push(styled);
            }
        }
    }
    if opts.show_update
        && config.updates.enabled
        && let Some(upd) = update_info
        && is_newer_semver(HUD_VERSION, &upd.latest_version)
        && let Some(norm) = normalize_semver_tag(&upd.latest_version)
    {
        let (icon, mid) = match gm {
            GlyphMode::Ascii => ("^", "."),
            GlyphMode::Nerd => ("\u{f062}", "·"),
            GlyphMode::Unicode => ("↑", "·"),
        };
        header_parts.push(paint(
            &format!("{icon} v{norm} {mid} /agy-hud:hud-update"),
            &[colors::YELLOW, BOLD],
        ));
    }

    let sep_str = sep(gm);
    let left_str = header_parts
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(&sep_str);
    let right_str = right_parts
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(&sep_str);
    join_left_right(left_str, right_str, &sep_str, line_budget)
}

fn build_context_meta_line(
    input: &RenderInput<'_>,
    cost_totals: Option<&CostTotals>,
    labels: &Labels,
    effective_width: Option<usize>,
    line_budget: Option<usize>,
    opts: &MetricsOpts,
) -> String {
    let RenderInput {
        stdin,
        transcript,
        memory,
        config,
        ..
    } = *input;
    let gm = config.display.glyph_mode;
    let sep_str = sep(gm);
    let dot_str = dot(gm);
    let ctx_seg = render_context_segment(stdin, transcript, config, labels, effective_width, opts);
    let meta_items =
        collect_counts_and_meta_items(stdin, transcript, memory, cost_totals, config, labels, opts);
    if meta_items.is_empty() {
        return ctx_seg;
    }

    let right_align = &config.display.right_align;
    let align_all_meta = right_align.iter().any(|s| s == "meta");
    let mut left_meta = Vec::new();
    let mut right_meta = Vec::new();
    for item in meta_items {
        if align_all_meta || right_align.iter().any(|s| s == item.tag) {
            right_meta.push(item.rendered);
        } else {
            left_meta.push(item.rendered);
        }
    }

    let left_meta_str = left_meta.join(&dot_str);
    let left_combined = [ctx_seg, left_meta_str]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(&sep_str);
    let right_meta_str = right_meta.join(&dot_str);
    join_left_right(left_combined, right_meta_str, &sep_str, line_budget)
}

fn build_quota_line(
    input: &RenderInput<'_>,
    labels: &Labels,
    effective_width: Option<usize>,
    opts: &MetricsOpts,
) -> String {
    let RenderInput {
        stdin,
        cached_quota,
        config,
        now_ms,
        ..
    } = *input;
    render_quota_segment(
        stdin,
        cached_quota,
        now_ms,
        config,
        labels,
        effective_width,
        opts,
    )
}

fn build_metrics_line(
    input: &RenderInput<'_>,
    cost_totals: Option<&CostTotals>,
    labels: &Labels,
    effective_width: Option<usize>,
    opts: &MetricsOpts,
) -> String {
    let RenderInput {
        stdin,
        transcript,
        cached_quota,
        memory,
        config,
        now_ms,
        ..
    } = *input;
    let gm = config.display.glyph_mode;
    let sep_str = sep(gm);
    let mut metrics_parts: Vec<String> = Vec::new();
    metrics_parts.push(render_context_segment(
        stdin,
        transcript,
        config,
        labels,
        effective_width,
        opts,
    ));
    let quota_seg = render_quota_segment(
        stdin,
        cached_quota,
        now_ms,
        config,
        labels,
        effective_width,
        opts,
    );
    if !quota_seg.is_empty() {
        metrics_parts.push(quota_seg);
    }
    let meta_seg =
        render_counts_and_meta(stdin, transcript, memory, cost_totals, config, labels, opts);
    if !meta_seg.is_empty() {
        metrics_parts.push(meta_seg);
    }
    metrics_parts
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(&sep_str)
}

fn compose_expanded_rows(
    named_rows: Vec<(&'static str, String)>,
    config: &HudConfig,
    sep_str: &str,
    line_budget: Option<usize>,
) -> Vec<String> {
    if config.display.merge_groups.is_empty() {
        return named_rows
            .into_iter()
            .filter_map(|(_, line)| (!line.is_empty()).then_some(line))
            .collect();
    }

    let mut row_map: std::collections::BTreeMap<&str, String> = named_rows
        .iter()
        .filter(|(_, l)| !l.is_empty())
        .map(|(k, l)| (*k, l.clone()))
        .collect();
    let mut consumed: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut out = Vec::new();

    for (name, _) in &named_rows {
        if consumed.contains(name) {
            continue;
        }
        if let Some(group) = config
            .display
            .merge_groups
            .iter()
            .find(|g| g.iter().any(|member| member == name))
        {
            let mut present: Vec<(&str, String)> = Vec::new();
            for member in group {
                consumed.insert(member.as_str());
                if let Some(val) = row_map.remove(member.as_str()) {
                    present.push((member.as_str(), val));
                }
            }
            if present.is_empty() {
                continue;
            }
            if present.len() == 1 {
                out.push(present.pop().unwrap().1);
                continue;
            }

            let joined = present
                .iter()
                .map(|(_, s)| s.as_str())
                .collect::<Vec<_>>()
                .join(sep_str);
            let fits = line_budget.is_none_or(|budget| text_width(&joined) <= budget);
            if fits {
                let last_is_right = config
                    .display
                    .right_align
                    .iter()
                    .any(|r| r == present[present.len() - 1].0);
                if last_is_right && line_budget.is_some() {
                    let (_, last_line) = present.pop().unwrap();
                    let left = present
                        .into_iter()
                        .map(|(_, s)| s)
                        .collect::<Vec<_>>()
                        .join(sep_str);
                    out.push(join_left_right(left, last_line, sep_str, line_budget));
                } else {
                    out.push(joined);
                }
            } else {
                // Auto-stack fallback: align bar labels if context & quota stacked together
                let ctx_pos = present.iter().position(|(k, _)| *k == "context");
                let quota_pos = present.iter().position(|(k, _)| *k == "quota");
                if let (Some(ci), Some(qi)) = (ctx_pos, quota_pos) {
                    let mut ctx_clone = present[ci].1.clone();
                    let mut quota_clone = present[qi].1.clone();
                    align_bar_prefix_pair(&mut ctx_clone, &mut quota_clone);
                    present[ci].1 = ctx_clone;
                    present[qi].1 = quota_clone;
                }
                for (_, line) in present {
                    out.push(line);
                }
            }
        } else {
            consumed.insert(name);
            if let Some(val) = row_map.remove(name) {
                out.push(val);
            }
        }
    }

    out
}

/// Formats an explicit warning line when `config.json` fails parsing or schema validation,
/// truncating the error description BEFORE the ` · /agy-hud:hud` remediation hint so the
/// recovery command is never clipped off narrow terminals.
pub fn format_config_problem_line(
    problems: &[String],
    config: &HudConfig,
    columns: Option<usize>,
) -> Option<String> {
    let first = problems.first()?;
    let gm = config.display.glyph_mode;
    let clean_msg = sanitize_text(first);
    if clean_msg.is_empty() {
        return None;
    }

    let prefix = match gm {
        GlyphMode::Ascii => "! Config: ",
        GlyphMode::Unicode | GlyphMode::Nerd => "⚠ Config: ",
    };
    let suffix = match gm {
        GlyphMode::Ascii => " . /agy-hud:hud",
        GlyphMode::Unicode | GlyphMode::Nerd => " · /agy-hud:hud",
    };

    let effective_width = match (config.max_width, columns) {
        (Some(mw), Some(lw)) => Some(mw.min(lw)),
        (Some(mw), None) => Some(mw),
        (None, lw) => lw,
    };

    let body = match effective_width {
        Some(budget) if budget > 0 => {
            let fixed_w = text_width(prefix) + text_width(suffix);
            if budget > fixed_w + 4 {
                let clipped_msg =
                    truncate_ansi_with_ellipsis(&clean_msg, budget - fixed_w, ellipsis(gm));
                format!(
                    "{}{}",
                    paint(&format!("{prefix}{clipped_msg}"), &[colors::YELLOW, BOLD]),
                    paint(suffix, &[colors::CYAN, BOLD]),
                )
            } else {
                let minimal = format!("{prefix}/agy-hud:hud");
                let clipped = truncate_ansi_with_ellipsis(&minimal, budget, ellipsis(gm));
                paint(&clipped, &[colors::YELLOW, BOLD])
            }
        }
        _ => format!(
            "{}{}",
            paint(&format!("{prefix}{clean_msg}"), &[colors::YELLOW, BOLD]),
            paint(suffix, &[colors::CYAN, BOLD]),
        ),
    };

    Some(format!("{RESET}{body}{RESET}"))
}

pub fn render_hud_lines(input: RenderInput<'_>) -> Vec<String> {
    render_hud_lines_full(input, None)
}

pub fn render_hud_lines_full(
    input: RenderInput<'_>,
    cost_totals: Option<&CostTotals>,
) -> Vec<String> {
    let RenderInput {
        stdin,
        config,
        transcript,
        columns,
        now_ms,
        ..
    } = input;

    let gm = config.display.glyph_mode;
    let labels = get_labels(config.language);
    let live_width = columns.or_else(|| stdin.and_then(|s| s.terminal_width));
    let effective_width = match (config.max_width, live_width) {
        (Some(mw), Some(lw)) => Some(mw.min(lw)),
        (Some(mw), None) => Some(mw),
        (None, lw) => lw,
    };

    let layout_mode = match config.line_layout {
        LineLayout::Adaptive => {
            if effective_width.is_some_and(|w| w < 76) {
                LineLayout::Compact
            } else {
                LineLayout::Expanded
            }
        }
        explicit => explicit,
    };

    let rail_cols = if layout_mode == LineLayout::Compact {
        0
    } else {
        3
    };
    let line_budget = effective_width.map(|ew| ew.saturating_sub(rail_cols));

    let mut header_opts = HeaderOpts {
        show_title: config.display.show_title,
        show_custom_tag: true,
        show_duration: config.display.show_duration,
        show_update: config.updates.enabled,
        path_levels: if layout_mode == LineLayout::Compact {
            PathLevels::One
        } else {
            config.path_levels
        },
        show_launch_dir: config.display.show_launch_dir && layout_mode != LineLayout::Compact,
        model_format: if layout_mode == LineLayout::Compact {
            ModelFormatMode::Short
        } else {
            config.display.model_format
        },
        show_plan_tier: config.display.show_plan_tier,
        show_speed: config.display.show_speed && layout_mode != LineLayout::Compact,
        show_vcs_extras: true,
        show_folder: config.display.show_project,
        show_vcs: config.vcs.enabled,
        show_mode: config.display.show_mode,
    };

    let mut line1 = build_header_line(&input, labels, &header_opts, line_budget);
    if let Some(budget) = line_budget {
        let header_steps: [fn(&mut HeaderOpts); 7] = [
            |o| o.show_title = false,
            |o| {
                o.show_custom_tag = false;
                o.show_duration = false;
            },
            |o| {
                o.path_levels = PathLevels::One;
                o.show_launch_dir = false;
                if o.model_format == ModelFormatMode::Full {
                    o.model_format = ModelFormatMode::Compact;
                }
                o.show_speed = false;
                o.show_plan_tier = false;
            },
            |o| o.show_update = false,
            |o| {
                o.model_format = ModelFormatMode::Short;
                o.show_vcs_extras = false;
            },
            |o| o.show_folder = false,
            |o| {
                o.show_vcs = false;
                o.show_mode = false;
            },
        ];
        for apply_step in header_steps {
            if text_width(&line1) <= budget {
                break;
            }
            apply_step(&mut header_opts);
            line1 = build_header_line(&input, labels, &header_opts, line_budget);
        }
    }

    let has_any_cost = config.display.show_cost
        || config.display.show_daily_cost
        || config.display.show_weekly_cost;
    let base_metrics_opts = MetricsOpts {
        compact_meta: layout_mode == LineLayout::Compact,
        show_cache_hit_rate: config.display.show_cache_hit_rate,
        quota_bar_enabled: config.display.quota_bar_enabled,
        context_value: config.display.context_value,
        show_200k: true,
        show_compactions: true,
        show_cost: has_any_cost,
        show_context_bar: config.display.show_context_bar,
        show_quota_reset: config.display.show_quota_reset,
        max_quota_buckets: if layout_mode == LineLayout::Compact {
            1
        } else {
            2
        },
        show_meta: true,
    };

    let sep_str = sep(gm);
    let raw_lines = if layout_mode == LineLayout::Compact {
        let mut compact_opts = base_metrics_opts;
        let mut line2 =
            build_metrics_line(&input, cost_totals, labels, effective_width, &compact_opts);
        if let Some(budget) = line_budget {
            let compact_steps: [fn(&mut MetricsOpts); 5] = [
                |o| {
                    o.compact_meta = true;
                    o.show_cache_hit_rate = false;
                },
                |o| {
                    o.quota_bar_enabled = false;
                    if o.context_value == ContextValueMode::Both {
                        o.context_value = ContextValueMode::Percent;
                    }
                },
                |o| {
                    o.show_200k = false;
                    o.show_compactions = false;
                    o.show_cost = false;
                    o.max_quota_buckets = 1;
                },
                |o| {
                    o.show_context_bar = false;
                    o.show_quota_reset = false;
                },
                |o| o.show_meta = false,
            ];
            for apply_step in compact_steps {
                if text_width(&line2) <= budget {
                    break;
                }
                apply_step(&mut compact_opts);
                line2 =
                    build_metrics_line(&input, cost_totals, labels, effective_width, &compact_opts);
            }
        }

        let single_candidate = [&line1, &line2]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(&sep_str);
        let mut out = if let Some(ew) = effective_width
            && text_width(&single_candidate) <= ew
        {
            vec![single_candidate]
        } else {
            [line1, line2]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect()
        };
        if config.display.show_tools
            && let Some(running_tool) = transcript
                .tools
                .iter()
                .find(|t| t.status == ToolStatus::Running)
        {
            let target = running_tool
                .target
                .as_deref()
                .filter(|t| !t.is_empty())
                .map(|t| paint(&format!(": {t}"), &[DIM]))
                .unwrap_or_default();
            let run_icon = if gm == GlyphMode::Ascii { "~" } else { "◐" };
            out.push(format!(
                "{} {}{target}",
                paint(run_icon, &[colors::CYAN, BOLD]),
                paint(&running_tool.name, &[BOLD]),
            ));
        }
        out
    } else {
        let split_meta_row = config
            .display
            .merge_groups
            .iter()
            .any(|g| g.iter().any(|m| m == "meta"));

        let mut ctx_opts = base_metrics_opts;
        if split_meta_row {
            ctx_opts.show_meta = false;
        }
        let mut ctx_line = build_context_meta_line(
            &input,
            cost_totals,
            labels,
            effective_width,
            line_budget,
            &ctx_opts,
        );
        if let Some(budget) = line_budget {
            let ctx_steps: [fn(&mut MetricsOpts); 5] = [
                |o| o.compact_meta = true,
                |o| o.show_cache_hit_rate = false,
                |o| {
                    if o.context_value == ContextValueMode::Both {
                        o.context_value = ContextValueMode::Percent;
                    }
                    o.show_200k = false;
                    o.show_cost = false;
                },
                |o| {
                    o.show_context_bar = false;
                    o.show_compactions = false;
                },
                |o| o.show_meta = false,
            ];
            for apply_step in ctx_steps {
                if text_width(&ctx_line) <= budget {
                    break;
                }
                apply_step(&mut ctx_opts);
                ctx_line = build_context_meta_line(
                    &input,
                    cost_totals,
                    labels,
                    effective_width,
                    line_budget,
                    &ctx_opts,
                );
            }
        }

        let standalone_meta = if split_meta_row {
            render_counts_and_meta(
                stdin,
                transcript,
                input.memory,
                cost_totals,
                config,
                labels,
                &base_metrics_opts,
            )
        } else {
            String::new()
        };

        let mut quota_opts = base_metrics_opts;
        let mut quota_line = build_quota_line(&input, labels, effective_width, &quota_opts);
        if let Some(budget) = line_budget
            && !quota_line.is_empty()
        {
            let quota_steps: [fn(&mut MetricsOpts); 3] = [
                |o| o.quota_bar_enabled = false,
                |o| o.max_quota_buckets = 1,
                |o| o.show_quota_reset = false,
            ];
            for apply_step in quota_steps {
                if text_width(&quota_line) <= budget {
                    break;
                }
                apply_step(&mut quota_opts);
                quota_line = build_quota_line(&input, labels, effective_width, &quota_opts);
            }
        }

        let tools_line = render_tools_line(transcript, config, line_budget).unwrap_or_default();
        let skills_line = render_skills_mcp_line(transcript, config, labels).unwrap_or_default();
        let subagents_line = render_subagents_line(transcript, config, now_ms).unwrap_or_default();
        let todos_line = render_todos_line(transcript, config, labels).unwrap_or_default();

        let has_activity = !tools_line.is_empty()
            || !skills_line.is_empty()
            || !subagents_line.is_empty()
            || !todos_line.is_empty();

        if config.display.merge_groups.is_empty()
            && config.line_layout == LineLayout::Adaptive
            && !has_activity
            && effective_width.is_some_and(|w| w >= 180)
        {
            let merged_metrics = build_metrics_line(
                &input,
                cost_totals,
                labels,
                effective_width,
                &base_metrics_opts,
            );
            let wide_line = [&line1, &merged_metrics]
                .into_iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(&sep_str);
            if effective_width.is_some_and(|w| text_width(&wide_line) <= w) {
                vec![wide_line]
            } else {
                join_with_rail(
                    [line1, ctx_line, quota_line]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect(),
                    true,
                    gm,
                )
            }
        } else {
            let named_rows = vec![
                ("header", line1),
                ("context", ctx_line),
                ("quota", quota_line),
                ("meta", standalone_meta),
                ("tools", tools_line),
                ("skills", skills_line),
                ("subagents", subagents_line),
                ("todos", todos_line),
            ];
            let composed = compose_expanded_rows(named_rows, config, &sep_str, line_budget);
            join_with_rail(composed, true, gm)
        }
    };

    raw_lines
        .into_iter()
        .map(|line| {
            let clipped = match effective_width {
                Some(ew) if ew > 0 => truncate_ansi_with_ellipsis(&line, ew, ellipsis(gm)),
                _ => line,
            };
            format!("{RESET}{clipped}{RESET}")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::strip_ansi;

    #[test]
    fn speed_tier_emoji_covers_all_seven_tiers_and_rounding_boundaries() {
        let cases = [
            (-5.0, "🐢"),
            (0.0, "🐢"),
            (0.4, "🐢"),
            (0.8, "🐢"),
            (49.4, "🐢"),
            (49.5, "🚲"),
            (49.6, "🚲"),
            (50.0, "🚲"),
            (99.4, "🚲"),
            (99.6, "🚜"),
            (100.0, "🚜"),
            (149.4, "🚜"),
            (149.6, "🚗"),
            (150.0, "🚗"),
            (199.4, "🚗"),
            (199.6, "🚄"),
            (200.0, "🚄"),
            (249.4, "🚄"),
            (249.6, "✈️"),
            (250.0, "✈️"),
            (299.4, "✈️"),
            (299.6, "🚀"),
            (300.0, "🚀"),
            (450.0, "🚀"),
        ];
        for (tps, expected) in cases {
            assert_eq!(
                speed_tier_emoji(tps),
                expected,
                "Unexpected tier emoji for {tps}"
            );
        }
        assert_eq!(speed_tier_emoji(f64::NAN), "🐢");
        assert_eq!(speed_tier_emoji(f64::INFINITY), "🐢");
    }

    #[test]
    fn format_speed_badge_handles_range_hiding_glyph_modes_and_ansi_styling() {
        let styled = format_speed_badge(68.3, true, Some(42.1), Some(112.4), GlyphMode::Unicode);
        assert_eq!(strip_ansi(&styled), "🚲 ~68 tok/s (42~112)");
        assert_eq!(text_width(&styled), 21);
        assert!(styled.contains(&paint("~68", &[colors::CYAN, BOLD])));
        assert!(styled.contains(&paint("tok/s", &[DIM])));
        assert!(styled.contains(&paint("(42~112)", &[DIM])));

        let nerd = format_speed_badge(68.3, true, Some(42.1), Some(112.4), GlyphMode::Nerd);
        assert_eq!(nerd, styled);

        let ascii = format_speed_badge(68.3, true, Some(42.1), Some(112.4), GlyphMode::Ascii);
        assert_eq!(strip_ansi(&ascii), "~68 tok/s (42~112)");
        assert_eq!(text_width(&ascii), 18);

        let exact = format_speed_badge(68.3, false, Some(42.1), Some(112.4), GlyphMode::Unicode);
        assert_eq!(strip_ansi(&exact), "🚲 68 tok/s (42~112)");
        assert!(exact.contains(&paint("68", &[colors::CYAN, BOLD])));

        // Hide `(min~max)` when missing, equal after rounding, or inverted
        for (min_tps, max_tps) in [
            (None, None),
            (Some(42.0), None),
            (None, Some(112.0)),
            (Some(68.0), Some(68.0)),
            (Some(68.1), Some(68.4)),
            (Some(80.0), Some(50.0)),
            (Some(f64::NAN), Some(100.0)),
            (Some(-1.0), Some(100.0)),
        ] {
            let hidden = format_speed_badge(68.3, true, min_tps, max_tps, GlyphMode::Unicode);
            assert_eq!(
                strip_ansi(&hidden),
                "🚲 ~68 tok/s",
                "Expected hidden range for min={min_tps:?} max={max_tps:?}"
            );
        }

        // Show `(min~max)` as soon as rounded integers differ
        let narrow_diff =
            format_speed_badge(68.4, true, Some(68.4), Some(68.6), GlyphMode::Unicode);
        assert_eq!(strip_ansi(&narrow_diff), "🚲 ~68 tok/s (68~69)");

        // Verify rounding boundary consistency between number and emoji
        assert_eq!(
            strip_ansi(&format_speed_badge(
                49.4,
                true,
                None,
                None,
                GlyphMode::Unicode
            )),
            "🐢 ~49 tok/s"
        );
        assert_eq!(
            strip_ansi(&format_speed_badge(
                49.6,
                true,
                None,
                None,
                GlyphMode::Unicode
            )),
            "🚲 ~50 tok/s"
        );
        assert_eq!(
            strip_ansi(&format_speed_badge(
                299.6,
                true,
                None,
                None,
                GlyphMode::Unicode
            )),
            "🚀 ~300 tok/s"
        );
        assert_eq!(
            strip_ansi(&format_speed_badge(
                300.0,
                false,
                None,
                None,
                GlyphMode::Unicode
            )),
            "🚀 300 tok/s"
        );
    }
}
