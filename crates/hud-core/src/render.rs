use crate::ansi::{
    BOLD, ITALIC, RESET, colors, file_link, paint, render_bar, sanitize_text, text_width,
    truncate_ansi,
};
use crate::config::HudConfig;
use crate::i18n::{Labels, get_labels};
use crate::stdin::{
    extract_context_metrics, extract_quota_buckets, format_compact_tokens,
    format_duration_seconds, format_model_name, format_workspace_path,
};
use crate::types::{
    AgyStdinData, ContextValueMode, LineLayout, MemoryProbeInfo, ModelFormatMode, PathLevels,
    QuotaValueMode, SubagentStatus, TodoStatus, ToolStatus, TranscriptSummary, VcsProbeInfo,
};

pub struct RenderInput<'a> {
    pub stdin: Option<&'a AgyStdinData>,
    pub config: &'a HudConfig,
    pub transcript: &'a TranscriptSummary,
    pub vcs: Option<&'a VcsProbeInfo>,
    pub memory: Option<&'a MemoryProbeInfo>,
    pub columns: Option<usize>,
    pub now_ms: i64,
}

fn sep() -> String {
    format!("{} │ {RESET}", colors::GRAY)
}

fn dot() -> String {
    format!("{} · {RESET}", colors::GRAY)
}

fn render_state_badge(stdin: Option<&AgyStdinData>, labels: &Labels) -> String {
    if stdin.and_then(|s| s.tool_confirmation_pending).unwrap_or(false) {
        return paint(&format!("⚠ {}", labels.state.confirm), &[colors::BRIGHT_YELLOW, BOLD]);
    }
    let state = stdin
        .and_then(|s| s.agent_state.as_deref())
        .unwrap_or("idle")
        .to_ascii_lowercase();

    match state.as_str() {
        "idle" => paint(&format!("● {}", labels.state.idle), &[colors::BRIGHT_GREEN, BOLD]),
        "thinking" => paint(
            &format!("◆ {}", labels.state.thinking),
            &[colors::BRIGHT_YELLOW, BOLD],
        ),
        "working" => paint(
            &format!("⚙ {}", labels.state.working),
            &[colors::BRIGHT_CYAN, BOLD],
        ),
        "tool_use" => paint(
            &format!("🔧 {}", labels.state.tool_use),
            &[colors::BRIGHT_MAGENTA, BOLD],
        ),
        "reviewing" => paint(
            &format!("👁 {}", labels.state.reviewing),
            &[colors::BRIGHT_BLUE, BOLD],
        ),
        "authenticating" => paint(
            &format!("🔑 {}", labels.state.authenticating),
            &[colors::YELLOW, BOLD],
        ),
        "initializing" => paint(
            &format!("◌ {}", labels.state.initializing),
            &[colors::CYAN, BOLD],
        ),
        "error" => paint(&format!("✖ {}", labels.state.error), &[colors::BRIGHT_RED, BOLD]),
        other => {
            let clean = sanitize_text(other).to_ascii_uppercase();
            let text = if clean.is_empty() {
                labels.state.idle
            } else {
                clean.as_str()
            };
            paint(&format!("● {text}"), &[colors::WHITE, BOLD])
        }
    }
}

fn render_mode_badges(stdin: Option<&AgyStdinData>, labels: &Labels) -> String {
    let mut parts = Vec::new();
    let cycle = sanitize_text(stdin.and_then(|s| s.cycle_mode.as_deref()).unwrap_or(""))
        .to_ascii_lowercase();
    match cycle.as_str() {
        "plan" => parts.push(paint(
            &format!("[{}]", labels.mode.plan),
            &[colors::BRIGHT_CYAN, BOLD],
        )),
        "auto" => parts.push(paint(
            &format!("[{}]", labels.mode.auto),
            &[colors::BRIGHT_GREEN, BOLD],
        )),
        "accept-edits" => parts.push(paint(
            &format!("[{}]", labels.mode.accept_edits),
            &[colors::BRIGHT_YELLOW],
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
            colors::BRIGHT_GREEN
        } else if vim_mode.starts_with("VISUAL") {
            colors::BRIGHT_MAGENTA
        } else {
            colors::BRIGHT_BLUE
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

fn render_vcs_segment(vcs: Option<&VcsProbeInfo>, config: &HudConfig) -> String {
    let Some(vcs) = vcs else {
        return String::new();
    };
    if !config.vcs.enabled || vcs.branch.is_empty() {
        return String::new();
    }

    let prefix = vcs.vcs_type.as_str();
    let dirty_mark = if config.vcs.show_dirty && vcs.dirty {
        paint("*", &[colors::BRIGHT_YELLOW, BOLD])
    } else {
        String::new()
    };
    let branch_color = if vcs.dirty && config.vcs.show_dirty {
        colors::BRIGHT_RED
    } else {
        colors::BRIGHT_BLUE
    };

    let mut extras = Vec::new();
    if config.vcs.show_ahead_behind {
        let mut ab = String::new();
        if vcs.ahead > 0 {
            ab.push_str(&format!("↑{}", vcs.ahead));
        }
        if vcs.behind > 0 {
            ab.push_str(&format!("↓{}", vcs.behind));
        }
        if !ab.is_empty() {
            extras.push(paint(&ab, &[colors::CYAN]));
        }
    }
    if vcs.conflicts > 0 {
        extras.push(paint(&format!("!{}", vcs.conflicts), &[colors::BRIGHT_RED, BOLD]));
    }
    if config.vcs.show_file_stats {
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

    format!(
        "{}{}{dirty_mark}{extra_text}{}",
        paint(&format!("{prefix}:("), &[colors::GRAY]),
        paint(&vcs.branch, &[branch_color]),
        paint(")", &[colors::GRAY]),
    )
}

fn render_context_segment(
    stdin: Option<&AgyStdinData>,
    config: &HudConfig,
    labels: &Labels,
    effective_width: Option<usize>,
) -> String {
    let metrics = extract_context_metrics(stdin);
    let pct_str = format!("{:.1}%", metrics.used_percent);
    let rem_str = format!("{:.1}% {}", metrics.remaining_percent, labels.metrics.remaining);
    let tok_str = match (metrics.used_tokens, metrics.window_size) {
        (Some(u), Some(w)) => Some(format!(
            "{}/{}",
            format_compact_tokens(u),
            format_compact_tokens(w)
        )),
        (Some(u), None) => Some(format_compact_tokens(u)),
        _ => None,
    };

    let value_text = match config.display.context_value {
        ContextValueMode::Tokens => tok_str.unwrap_or(pct_str),
        ContextValueMode::Remaining => rem_str,
        ContextValueMode::Both => match tok_str {
            Some(tok) => format!("{pct_str} {}", paint(&format!("({tok})"), &[colors::GRAY])),
            None => pct_str,
        },
        ContextValueMode::Percent => pct_str,
    };

    let bar_len = if config.line_layout == LineLayout::Adaptive
        && effective_width.is_some_and(|w| w < 120)
    {
        config.display.bar_length.min(8)
    } else {
        config.display.bar_length
    };

    let warn_at = config.display.context_warning_threshold as f64;
    let crit_at = config.display.context_critical_threshold as f64;

    let bar = if config.display.show_context_bar {
        format!(
            "{} ",
            render_bar(
                metrics.used_percent,
                bar_len,
                warn_at,
                crit_at,
                colors::BRIGHT_GREEN
            )
        )
    } else {
        String::new()
    };

    let num_color = if metrics.used_percent >= crit_at {
        colors::BRIGHT_RED
    } else if metrics.used_percent >= warn_at {
        colors::BRIGHT_YELLOW
    } else {
        colors::BRIGHT_WHITE
    };

    let over_200k = if metrics.exceeds_200k {
        format!(" {}", paint("200k+", &[colors::BRIGHT_YELLOW]))
    } else {
        String::new()
    };

    let cache_part = match (config.display.show_cache_hit_rate, metrics.cache_hit_percent) {
        (true, Some(hit)) => format!(
            "{}{}",
            dot(),
            paint(
                &format!("⚡{hit}% {}", labels.metrics.cache),
                &[colors::CYAN]
            )
        ),
        _ => String::new(),
    };

    format!(
        "{} {bar}{}{over_200k}{cache_part}",
        paint(labels.metrics.context, &[colors::GRAY]),
        paint(&value_text, &[num_color, BOLD]),
    )
}

fn render_quota_segment(
    stdin: Option<&AgyStdinData>,
    config: &HudConfig,
    labels: &Labels,
    effective_width: Option<usize>,
    max_buckets: usize,
) -> String {
    if !config.display.show_quota {
        return String::new();
    }
    let buckets = extract_quota_buckets(stdin);
    if buckets.is_empty() {
        return String::new();
    }

    let mini_len = if config.line_layout == LineLayout::Adaptive
        && effective_width.is_some_and(|w| w < 120)
    {
        5
    } else {
        ((config.display.bar_length as f64 * 0.65).floor() as usize).max(5)
    };

    let rendered: Vec<String> = buckets
        .into_iter()
        .take(max_buckets)
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
                let mini_bar = if config.display.quota_bar_enabled {
                    format!("{} ", render_bar(used, mini_len, 70.0, 90.0, colors::BRIGHT_CYAN))
                } else {
                    String::new()
                };
                let reset = match (config.display.show_quota_reset, b.reset_in_seconds) {
                    (true, Some(sec)) => format!(
                        " {}",
                        paint(&format!("({})", format_duration_seconds(sec)), &[colors::GRAY])
                    ),
                    _ => String::new(),
                };
                format!(
                    "{} {mini_bar}{}{reset}",
                    paint(&label, &[colors::GRAY]),
                    paint(&display_pct, &[colors::BRIGHT_WHITE, BOLD]),
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
                format!(
                    "{} {}",
                    paint(&label, &[colors::GRAY]),
                    paint(&amt, &[colors::BRIGHT_WHITE, BOLD]),
                )
            }
        })
        .collect();

    rendered.join(&dot())
}

fn render_counts_and_meta(
    stdin: Option<&AgyStdinData>,
    memory: Option<&MemoryProbeInfo>,
    config: &HudConfig,
    labels: &Labels,
    compact: bool,
) -> String {
    let mut items = Vec::new();

    if config.display.show_cost
        && let Some(cost) = stdin.and_then(|s| s.cost.as_ref())
        && let Some(usd) = cost.total_usd
        && usd.is_finite()
    {
        let prefix = if cost.estimated.unwrap_or(false) { "~" } else { "" };
        items.push(paint(&format!("{prefix}${usd:.2}"), &[colors::YELLOW]));
    }

    if config.display.show_counts {
        if let Some(subagents) = stdin.and_then(|s| s.subagents.as_ref())
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
            let color = if running > 0 {
                colors::BRIGHT_CYAN
            } else {
                colors::BRIGHT_WHITE
            };
            items.push(format!(
                "{} {}",
                paint(labels.metrics.subagents, &[colors::GRAY]),
                paint(&agent_label, &[color, BOLD]),
            ));
        }

        if let Some(tasks) = stdin.and_then(|s| s.task_count)
            && tasks > 0
        {
            items.push(format!(
                "{} {}",
                paint(labels.metrics.tasks, &[colors::GRAY]),
                paint(&tasks.to_string(), &[colors::BRIGHT_CYAN, BOLD]),
            ));
        }

        if !compact
            && let Some(arts) = stdin.and_then(|s| s.artifact_count)
            && arts > 0
        {
            items.push(format!(
                "{} {}",
                paint(labels.metrics.artifacts, &[colors::GRAY]),
                paint(&arts.to_string(), &[colors::BRIGHT_WHITE, BOLD]),
            ));
        }

        if let Some(queued) = stdin.and_then(|s| s.pending_input_count)
            && queued > 0
        {
            items.push(paint(
                &format!("+{queued} {}", labels.metrics.queued),
                &[colors::BRIGHT_YELLOW, BOLD],
            ));
        }
    }

    if config.display.show_memory
        && let Some(mem) = memory
    {
        let mem_color = if mem.used_percent >= 90 {
            colors::BRIGHT_RED
        } else if mem.used_percent >= 75 {
            colors::BRIGHT_YELLOW
        } else {
            colors::BRIGHT_WHITE
        };
        items.push(format!(
            "{} {}",
            paint(labels.metrics.memory, &[colors::GRAY]),
            paint(&format!("{}%", mem.used_percent), &[mem_color]),
        ));
    }

    if config.display.show_sandbox
        && let Some(enabled) = stdin.and_then(|s| s.sandbox.as_ref()).and_then(|sb| sb.enabled)
    {
        if enabled {
            items.push(paint(
                &format!("🛡 {}", labels.metrics.sandbox_on),
                &[colors::BRIGHT_GREEN, BOLD],
            ));
        } else if !compact {
            items.push(paint(labels.metrics.sandbox_off, &[colors::GRAY]));
        }
    }

    items.join(&dot())
}

fn render_tools_activity_line(
    transcript: &TranscriptSummary,
    config: &HudConfig,
    labels: &Labels,
) -> Option<String> {
    let mut parts = Vec::new();

    if config.display.show_tools && !transcript.tools.is_empty() {
        let formatted: Vec<String> = transcript
            .tools
            .iter()
            .take(config.display.tools_max_visible)
            .map(|t| {
                let (icon, name_color) = match t.status {
                    ToolStatus::Running => (
                        paint("◐", &[colors::BRIGHT_CYAN, BOLD]),
                        colors::BRIGHT_WHITE,
                    ),
                    ToolStatus::Error => {
                        (paint("✗", &[colors::BRIGHT_RED, BOLD]), colors::BRIGHT_RED)
                    }
                    ToolStatus::Completed => (paint("✓", &[colors::BRIGHT_GREEN]), colors::WHITE),
                };
                let count_part = if t.count > 1 {
                    paint(&format!(" ×{}", t.count), &[colors::CYAN])
                } else {
                    String::new()
                };
                let target_part = match t.target.as_deref() {
                    Some(tgt) if !tgt.is_empty() => paint(&format!(": {tgt}"), &[colors::GRAY]),
                    _ => String::new(),
                };
                format!(
                    "{icon} {}{count_part}{target_part}",
                    paint(&t.name, &[name_color])
                )
            })
            .collect();
        parts.push(formatted.join(&dot()));
    }

    if config.display.show_skills_and_mcp {
        if !transcript.skills.is_empty() {
            let start = transcript.skills.len().saturating_sub(3);
            let shown = transcript.skills[start..].join(", ");
            parts.push(format!(
                "{} {}",
                paint(&format!("{}:", labels.metrics.skills), &[colors::GRAY]),
                paint(&shown, &[colors::MAGENTA]),
            ));
        }
        if !transcript.mcp_servers.is_empty() {
            let start = transcript.mcp_servers.len().saturating_sub(3);
            let mcp_items: Vec<String> = transcript.mcp_servers[start..]
                .iter()
                .map(|srv| {
                    if transcript.mcp_errors.contains(srv) {
                        paint(&format!("{srv}!"), &[colors::BRIGHT_RED, BOLD])
                    } else {
                        paint(srv, &[colors::CYAN])
                    }
                })
                .collect();
            parts.push(format!(
                "{} {}",
                paint(&format!("{}:", labels.metrics.mcp), &[colors::GRAY]),
                mcp_items.join(", "),
            ));
        }
    }

    (!parts.is_empty()).then(|| parts.join(&sep()))
}

fn render_subagents_line(
    transcript: &TranscriptSummary,
    config: &HudConfig,
    now_ms: i64,
) -> Option<String> {
    if !config.display.show_subagents || transcript.subagents.is_empty() {
        return None;
    }

    let formatted: Vec<String> = transcript
        .subagents
        .iter()
        .take(4)
        .map(|ag| {
            let running = ag.status == SubagentStatus::Running;
            let icon = if running {
                paint("◐", &[colors::BRIGHT_CYAN, BOLD])
            } else {
                paint("✓", &[colors::BRIGHT_GREEN])
            };
            let title = match ag.role.as_deref() {
                Some(role) if !role.is_empty() => {
                    format!("{role} {}", paint(&format!("[{}]", ag.name), &[colors::GRAY]))
                }
                _ => ag.name.clone(),
            };
            let elapsed = if let Some(start) = ag.started_at {
                let end = ag.ended_at.unwrap_or(now_ms);
                if end >= start {
                    format!(
                        " {}",
                        paint(
                            &format!("({})", format_duration_seconds(((end - start) as f64) / 1000.0)),
                            &[colors::GRAY],
                        )
                    )
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            let styled_title = if running {
                paint(&title, &[colors::BRIGHT_WHITE])
            } else {
                title
            };
            format!("{icon} {styled_title}{elapsed}")
        })
        .collect();

    Some(formatted.join(&sep()))
}

fn render_todos_line(
    transcript: &TranscriptSummary,
    config: &HudConfig,
    labels: &Labels,
) -> Option<String> {
    if !config.display.show_todos || transcript.todos.is_empty() {
        return None;
    }

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
        .or_else(|| transcript.todos.iter().find(|t| t.status == TodoStatus::Pending));

    let Some(active) = active else {
        return Some(format!(
            "{} {}",
            paint("✓", &[colors::BRIGHT_GREEN, BOLD]),
            paint(
                &format!("{}: {completed}/{total} (100%)", labels.metrics.todos),
                &[colors::BRIGHT_GREEN]
            ),
        ));
    };

    let pct = ((completed as f64 / total as f64) * 100.0).round() as usize;
    let icon = if active.status == TodoStatus::InProgress {
        paint("▸", &[colors::BRIGHT_CYAN, BOLD])
    } else {
        paint("◦", &[colors::GRAY])
    };
    Some(format!(
        "{icon} {} {}",
        paint(&active.text, &[colors::BRIGHT_WHITE]),
        paint(&format!("({completed}/{total} · {pct}%)"), &[colors::CYAN]),
    ))
}

fn join_with_rail(lines: Vec<String>, use_box_rail: bool) -> Vec<String> {
    if !use_box_rail || lines.len() <= 1 {
        return lines;
    }
    let last = lines.len() - 1;
    lines
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            if idx == 0 {
                format!("{}╭─{RESET} {line}", colors::GRAY)
            } else if idx == last {
                format!("{}╰─{RESET} {line}", colors::GRAY)
            } else {
                format!("{}├─{RESET} {line}", colors::GRAY)
            }
        })
        .collect()
}

pub fn render_hud_lines(input: RenderInput<'_>) -> Vec<String> {
    let RenderInput {
        stdin,
        config,
        transcript,
        vcs,
        memory,
        columns,
        now_ms,
    } = input;

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

    // Header segments
    let mut header_parts: Vec<String> = Vec::new();
    if config.display.show_state {
        header_parts.push(render_state_badge(stdin, labels));
    }
    if config.display.show_mode {
        let modes = render_mode_badges(stdin, labels);
        if !modes.is_empty() {
            header_parts.push(modes);
        }
    }
    if config.display.show_model {
        let mf = if layout_mode == LineLayout::Compact {
            ModelFormatMode::Short
        } else {
            config.display.model_format
        };
        let model_str = format_model_name(
            stdin.and_then(|s| s.model.as_ref()),
            mf,
            config.display.effort_format,
        );
        if !model_str.is_empty() {
            let tier = match (
                config.display.show_plan_tier,
                stdin.and_then(|s| s.plan_tier.as_deref()),
            ) {
                (true, Some(pt)) if !sanitize_text(pt).is_empty() => {
                    format!(" {}", paint(&sanitize_text(pt), &[colors::YELLOW]))
                }
                _ => String::new(),
            };
            header_parts.push(format!(
                "{}{tier}",
                paint(&format!("[{model_str}]"), &[colors::BRIGHT_MAGENTA, ITALIC])
            ));
        }
    }
    if config.display.show_project {
        let raw_dir = stdin.and_then(|s| {
            s.workspace
                .as_ref()
                .and_then(|w| w.project_dir.as_deref().or(w.current_dir.as_deref()))
                .or(s.cwd.as_deref())
        });
        let levels = if layout_mode == LineLayout::Compact {
            PathLevels::One
        } else {
            config.path_levels
        };
        let short_dir = format_workspace_path(raw_dir, levels);
        let linked_dir = match raw_dir {
            Some(dir) if !short_dir.is_empty() => file_link(
                dir,
                &paint(&short_dir, &[colors::BRIGHT_WHITE, BOLD]),
                config.hyperlinks,
            ),
            _ => String::new(),
        };
        let vcs_seg = render_vcs_segment(vcs, config);
        let proj_vcs: Vec<&str> = [linked_dir.as_str(), vcs_seg.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        if !proj_vcs.is_empty() {
            header_parts.push(proj_vcs.join(" "));
        }
    }
    if config.display.show_duration
        && let Some(started_at) = transcript.session_started_at
        && now_ms >= started_at
    {
        let elapsed_sec = ((now_ms - started_at) as f64) / 1000.0;
        if elapsed_sec >= 5.0 {
            header_parts.push(paint(
                &format!("⏱ {}", format_duration_seconds(elapsed_sec)),
                &[colors::GRAY],
            ));
        }
    }
    if !config.display.custom_tag.is_empty() {
        let tag = sanitize_text(&config.display.custom_tag);
        if !tag.is_empty() {
            header_parts.push(paint(&tag, &[colors::CYAN]));
        }
    }

    let sep_str = sep();
    let line1 = header_parts
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(&sep_str);

    // Metrics segments
    let mut metrics_parts: Vec<String> = Vec::new();
    metrics_parts.push(render_context_segment(stdin, config, labels, effective_width));
    let quota_seg = render_quota_segment(
        stdin,
        config,
        labels,
        effective_width,
        if layout_mode == LineLayout::Compact { 1 } else { 2 },
    );
    if !quota_seg.is_empty() {
        metrics_parts.push(quota_seg);
    }
    let meta_seg = render_counts_and_meta(
        stdin,
        memory,
        config,
        labels,
        layout_mode == LineLayout::Compact,
    );
    if !meta_seg.is_empty() {
        metrics_parts.push(meta_seg);
    }

    let line2 = metrics_parts
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(&sep_str);

    let raw_lines = if layout_mode == LineLayout::Compact {
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
                .map(|t| paint(&format!(": {t}"), &[colors::GRAY]))
                .unwrap_or_default();
            out.push(format!(
                "{} {}{target}",
                paint("◐", &[colors::BRIGHT_CYAN, BOLD]),
                running_tool.name,
            ));
        }
        out
    } else {
        let mut activity_lines = Vec::new();
        if let Some(tl) = render_tools_activity_line(transcript, config, labels) {
            activity_lines.push(tl);
        }
        if let Some(sl) = render_subagents_line(transcript, config, now_ms) {
            activity_lines.push(sl);
        }
        if let Some(tdl) = render_todos_line(transcript, config, labels) {
            activity_lines.push(tdl);
        }

        if config.line_layout == LineLayout::Adaptive
            && activity_lines.is_empty()
            && effective_width.is_some_and(|w| w >= 132)
        {
            let wide_line = [&line1, &line2]
                .into_iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(&sep_str);
            if effective_width.is_some_and(|w| text_width(&wide_line) <= w) {
                vec![wide_line]
            } else {
                join_with_rail(
                    [line1, line2].into_iter().filter(|s| !s.is_empty()).collect(),
                    true,
                )
            }
        } else {
            let mut all = Vec::with_capacity(2 + activity_lines.len());
            if !line1.is_empty() {
                all.push(line1);
            }
            if !line2.is_empty() {
                all.push(line2);
            }
            all.extend(activity_lines);
            join_with_rail(all, true)
        }
    };

    raw_lines
        .into_iter()
        .map(|line| {
            let clipped = match effective_width {
                Some(ew) if ew > 0 => truncate_ansi(&line, ew),
                _ => line,
            };
            format!("{RESET}{clipped}{RESET}")
        })
        .collect()
}
