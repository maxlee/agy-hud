use crate::ansi::{
    BOLD, DIM, ITALIC, RESET, colors, file_link, paint, render_bar_with_mode, sanitize_text,
    sanitize_title_text, text_width, truncate_ansi_with_ellipsis,
};
use crate::config::HudConfig;
use crate::i18n::{Labels, get_labels};
use crate::probes::{is_newer_semver, normalize_semver_tag, resolve_output_speed};
use crate::stdin::{
    extract_context_metrics, extract_quota_buckets_reconciled, format_compact_tokens,
    format_duration_seconds, format_model_name_with_glyphs, format_session_cost,
    format_workspace_path, normalize_plan_tier,
};
use crate::types::{
    AgyStdinData, CachedQuotaSnapshot, ContextValueMode, GlyphMode, HUD_VERSION, LineLayout,
    MemoryProbeInfo, ModelFormatMode, PathLevels, QuotaValueMode, SubagentStatus, TodoStatus,
    ToolStatus, TranscriptSummary, UpdateCacheSnapshot, VcsProbeInfo,
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

    format!(
        "{nerd_prefix}{}{}{dirty_mark}{extra_text}{}",
        paint(&format!("{prefix}:("), &[DIM]),
        paint(&vcs.branch, &[branch_color, BOLD]),
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
    show_cost: bool,
    show_context_bar: bool,
    show_quota_reset: bool,
    max_quota_buckets: usize,
    show_meta: bool,
}

fn render_context_segment(
    stdin: Option<&AgyStdinData>,
    config: &HudConfig,
    labels: &Labels,
    effective_width: Option<usize>,
    opts: &MetricsOpts,
) -> String {
    let gm = config.display.glyph_mode;
    let metrics = extract_context_metrics(stdin);
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

    let value_text = match opts.context_value {
        ContextValueMode::Tokens => paint(&tok_str.unwrap_or(pct_str), &[BOLD]),
        ContextValueMode::Remaining => paint(&rem_str, &[BOLD]),
        ContextValueMode::Both => {
            let num_styles: &[&str] = if metrics.used_percent
                >= (config.display.context_critical_threshold as f64)
            {
                &[colors::RED, BOLD]
            } else if metrics.used_percent >= (config.display.context_warning_threshold as f64) {
                &[colors::YELLOW, BOLD]
            } else {
                &[BOLD]
            };
            match tok_str {
                Some(tok) => format!(
                    "{} {}",
                    paint(&pct_str, num_styles),
                    paint(&format!("({tok})"), &[DIM])
                ),
                None => paint(&pct_str, num_styles),
            }
        }
        ContextValueMode::Percent => {
            let num_styles: &[&str] = if metrics.used_percent
                >= (config.display.context_critical_threshold as f64)
            {
                &[colors::RED, BOLD]
            } else if metrics.used_percent >= (config.display.context_warning_threshold as f64) {
                &[colors::YELLOW, BOLD]
            } else {
                &[BOLD]
            };
            paint(&pct_str, num_styles)
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
        .take(opts.max_quota_buckets)
        .map(|b| {
            let label = if b.label.is_empty() {
                labels.metrics.quota.to_string()
            } else {
                format!("{}({})", labels.metrics.quota, b.label)
            };
            if let (Some(used), Some(rem)) = (b.used_percent, b.remaining_percent) {
                let display_pct = match config.display.quota_value {
                    QuotaValueMode::Remaining => {
                        format!("{:.0}% {}", rem, labels.metrics.remaining)
                    }
                    QuotaValueMode::Used => format!("{:.0}%", used),
                };
                let pct_styles: &[&str] = if used >= 90.0 {
                    &[colors::RED, BOLD]
                } else if used >= 70.0 {
                    &[colors::YELLOW, BOLD]
                } else {
                    &[BOLD]
                };
                let mini_bar = if opts.quota_bar_enabled {
                    format!(
                        "{} ",
                        render_bar_with_mode(used, mini_len, 70.0, 90.0, colors::CYAN, gm)
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
                    "{} {mini_bar}{}{reset}",
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

fn render_counts_and_meta(
    stdin: Option<&AgyStdinData>,
    transcript: &TranscriptSummary,
    memory: Option<&MemoryProbeInfo>,
    config: &HudConfig,
    labels: &Labels,
    opts: &MetricsOpts,
) -> String {
    if !opts.show_meta {
        return String::new();
    }
    let gm = config.display.glyph_mode;
    let mut items = Vec::new();

    if opts.show_cost
        && let Some(cost) = stdin.and_then(|s| s.cost.as_ref())
        && let Some(usd) = cost.total_usd
        && let Some(formatted_usd) = format_session_cost(usd)
    {
        let prefix = if cost.estimated.unwrap_or(false) {
            "~"
        } else {
            ""
        };
        items.push(paint(
            &format!("{prefix}{formatted_usd}"),
            &[colors::YELLOW, BOLD],
        ));
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
            items.push(format!(
                "{} {}",
                paint(labels.metrics.subagents, &[DIM]),
                paint(&agent_label, val_styles),
            ));
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
            items.push(format!(
                "{} {}",
                paint(labels.metrics.subagents, &[DIM]),
                paint(&agent_label, val_styles),
            ));
        }

        if let Some(tasks) = stdin.and_then(|s| s.task_count)
            && tasks > 0
        {
            items.push(format!(
                "{} {}",
                paint(labels.metrics.tasks, &[DIM]),
                paint(&tasks.to_string(), &[colors::CYAN, BOLD]),
            ));
        }

        if !opts.compact_meta
            && let Some(arts) = stdin.and_then(|s| s.artifact_count)
            && arts > 0
        {
            items.push(format!(
                "{} {}",
                paint(labels.metrics.artifacts, &[DIM]),
                paint(&arts.to_string(), &[BOLD]),
            ));
        }

        if let Some(queued) = stdin.and_then(|s| s.pending_input_count)
            && queued > 0
        {
            items.push(paint(
                &format!("+{queued} {}", labels.metrics.queued),
                &[colors::YELLOW, BOLD],
            ));
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
        items.push(format!(
            "{} {}",
            paint(labels.metrics.memory, &[DIM]),
            paint(&format!("{}%", mem.used_percent), mem_styles),
        ));
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
        items.push(paint(&badge, &[colors::GREEN, BOLD]));
    }

    items.join(&dot(gm))
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

    let formatted: Vec<String> = transcript
        .subagents
        .iter()
        .take(4)
        .map(|ag| {
            let running = ag.status == SubagentStatus::Running;
            let icon = if running {
                paint(if ascii { "~" } else { "◐" }, &[colors::CYAN, BOLD])
            } else {
                paint(if ascii { "+" } else { "✓" }, &[colors::GREEN])
            };
            let title = match ag.role.as_deref() {
                Some(role) if !role.is_empty() => {
                    let styled_role = if running {
                        paint(role, &[BOLD])
                    } else {
                        role.to_string()
                    };
                    format!("{styled_role} {}", paint(&format!("[{}]", ag.name), &[DIM]))
                }
                _ => {
                    if running {
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

fn build_header_line(input: &RenderInput<'_>, labels: &Labels, opts: &HeaderOpts) -> String {
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
                match resolve_output_speed(stdin, transcript, None, now_ms).0 {
                    Some((tps, estimated)) => {
                        let prefix = if estimated { "~" } else { "" };
                        format!(
                            "{}{} {}",
                            dot(gm),
                            paint(&format!("{prefix}{tps:.0}"), &[colors::CYAN, BOLD]),
                            paint("tok/s", &[DIM]),
                        )
                    }
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
        let raw_dir = stdin.and_then(|s| {
            s.workspace
                .as_ref()
                .and_then(|w| w.current_dir.as_deref())
                .filter(|d| !d.trim().is_empty())
                .or_else(|| s.cwd.as_deref().filter(|d| !d.trim().is_empty()))
                .or_else(|| {
                    s.workspace
                        .as_ref()
                        .and_then(|w| w.project_dir.as_deref())
                        .filter(|d| !d.trim().is_empty())
                })
        });
        let linked_dir = if opts.show_folder {
            let short_dir = format_workspace_path(raw_dir, opts.path_levels);
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
            header_parts.push(paint(&badge, &[DIM]));
        }
    }
    if opts.show_custom_tag && !config.display.custom_tag.is_empty() {
        let tag = sanitize_text(&config.display.custom_tag);
        if !tag.is_empty() {
            header_parts.push(paint(&tag, &[colors::CYAN]));
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
    header_parts
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(&sep_str)
}

fn build_context_meta_line(
    input: &RenderInput<'_>,
    labels: &Labels,
    effective_width: Option<usize>,
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
    let ctx_seg = render_context_segment(stdin, config, labels, effective_width, opts);
    let meta_seg = render_counts_and_meta(stdin, transcript, memory, config, labels, opts);
    [ctx_seg, meta_seg]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(&sep_str)
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
    let meta_seg = render_counts_and_meta(stdin, transcript, memory, config, labels, opts);
    if !meta_seg.is_empty() {
        metrics_parts.push(meta_seg);
    }
    metrics_parts
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(&sep_str)
}

pub fn render_hud_lines(input: RenderInput<'_>) -> Vec<String> {
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
        model_format: if layout_mode == LineLayout::Compact {
            ModelFormatMode::Short
        } else {
            config.display.model_format
        },
        show_plan_tier: config.display.show_plan_tier,
        show_speed: config.display.show_speed && layout_mode != LineLayout::Compact,
        show_vcs_extras: true,
        show_folder: config.display.show_project,
        show_vcs: config.display.show_project,
        show_mode: config.display.show_mode,
    };

    let mut line1 = build_header_line(&input, labels, &header_opts);
    if let Some(budget) = line_budget {
        let header_steps: [fn(&mut HeaderOpts); 7] = [
            |o| o.show_title = false,
            |o| {
                o.show_custom_tag = false;
                o.show_duration = false;
            },
            |o| {
                o.path_levels = PathLevels::One;
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
            line1 = build_header_line(&input, labels, &header_opts);
        }
    }

    let base_metrics_opts = MetricsOpts {
        compact_meta: layout_mode == LineLayout::Compact,
        show_cache_hit_rate: config.display.show_cache_hit_rate,
        quota_bar_enabled: config.display.quota_bar_enabled,
        context_value: config.display.context_value,
        show_200k: true,
        show_cost: config.display.show_cost,
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
        let mut line2 = build_metrics_line(&input, labels, effective_width, &compact_opts);
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
                line2 = build_metrics_line(&input, labels, effective_width, &compact_opts);
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
        let mut ctx_opts = base_metrics_opts;
        let mut ctx_line = build_context_meta_line(&input, labels, effective_width, &ctx_opts);
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
                |o| o.show_context_bar = false,
                |o| o.show_meta = false,
            ];
            for apply_step in ctx_steps {
                if text_width(&ctx_line) <= budget {
                    break;
                }
                apply_step(&mut ctx_opts);
                ctx_line = build_context_meta_line(&input, labels, effective_width, &ctx_opts);
            }
        }

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

        let mut activity_lines = Vec::new();
        if let Some(tl) = render_tools_line(transcript, config, line_budget) {
            activity_lines.push(tl);
        }
        if let Some(sml) = render_skills_mcp_line(transcript, config, labels) {
            activity_lines.push(sml);
        }
        if let Some(sl) = render_subagents_line(transcript, config, now_ms) {
            activity_lines.push(sl);
        }
        if let Some(tdl) = render_todos_line(transcript, config, labels) {
            activity_lines.push(tdl);
        }

        if config.line_layout == LineLayout::Adaptive
            && activity_lines.is_empty()
            && effective_width.is_some_and(|w| w >= 180)
        {
            let merged_metrics =
                build_metrics_line(&input, labels, effective_width, &base_metrics_opts);
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
            let mut all = Vec::with_capacity(3 + activity_lines.len());
            if !line1.is_empty() {
                all.push(line1);
            }
            if !ctx_line.is_empty() {
                all.push(ctx_line);
            }
            if !quota_line.is_empty() {
                all.push(quota_line);
            }
            all.extend(activity_lines);
            join_with_rail(all, true, gm)
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
