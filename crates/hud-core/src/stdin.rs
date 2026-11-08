use crate::ansi::sanitize_text;
use crate::types::{
    AgyStdinData, EffortFormatMode, ModelFormatMode, ModelStdin, PathLevels, QuotaBucketView,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ContextMetrics {
    pub has_data: bool,
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub used_tokens: Option<u64>,
    pub window_size: Option<u64>,
    pub exceeds_200k: bool,
    pub cache_hit_percent: Option<u64>,
}

fn effort_symbol(effort_lower: &str) -> &str {
    match effort_lower {
        "low" => "⚡",
        "medium" | "med" => "⚡⚡",
        "high" => "⚡⚡⚡",
        "xhigh" | "max" => "⚡⚡⚡⚡",
        other => other,
    }
}

fn effort_short(effort_lower: &str) -> &str {
    match effort_lower {
        "low" => "low",
        "medium" => "med",
        "high" => "high",
        "xhigh" => "xhigh",
        "max" => "max",
        other => other,
    }
}

fn extract_trailing_paren_effort(raw_name: &str) -> Option<&str> {
    let trimmed = raw_name.trim_end();
    let open = trimmed.rfind('(')?;
    if !trimmed.ends_with(')') {
        return None;
    }
    let inside = &trimmed[open + 1..trimmed.len() - 1];
    if matches!(
        inside.to_ascii_lowercase().as_str(),
        "low" | "medium" | "high" | "xhigh" | "max" | "thinking"
    ) {
        Some(inside)
    } else {
        None
    }
}

fn strip_trailing_paren(name: &str) -> &str {
    let trimmed = name.trim_end();
    if trimmed.ends_with(')')
        && let Some(open) = trimmed.rfind('(')
        && !trimmed[open + 1..trimmed.len() - 1].contains('(')
    {
        return trimmed[..open].trim_end();
    }
    trimmed
}

fn strip_short_model_prefixes(name: &str) -> &str {
    let mut s = name.trim();
    if s.len() >= 7 && s[..7].eq_ignore_ascii_case("google ") {
        s = s[7..].trim_start();
    }
    if s.len() >= 7 && s[..7].eq_ignore_ascii_case("gemini ") {
        let rest = s[7..].trim_start();
        // Optional version token e.g. "4 " or "2.5 "
        if let Some(space_idx) = rest.find(' ') {
            let ver = &rest[..space_idx];
            if !ver.is_empty() && ver.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
                s = rest[space_idx + 1..].trim_start();
            }
        }
    } else if s.len() >= 7 && s[..7].eq_ignore_ascii_case("claude ") {
        s = s[7..].trim_start();
    }
    s
}

pub fn format_model_name(
    model: Option<&ModelStdin>,
    model_format: ModelFormatMode,
    effort_format: EffortFormatMode,
) -> String {
    let Some(model) = model else {
        return String::new();
    };
    let raw_candidate = model
        .display_name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or(model.id.as_deref())
        .unwrap_or("");
    let raw_name = sanitize_text(raw_candidate);
    if raw_name.is_empty() {
        return String::new();
    }

    let paren_effort = extract_trailing_paren_effort(&raw_name);
    let inferred_effort = sanitize_text(
        model
            .effort
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or(paren_effort)
            .unwrap_or(""),
    )
    .to_ascii_lowercase();

    let mut base = raw_name.as_str();
    if matches!(
        model_format,
        ModelFormatMode::Compact | ModelFormatMode::Short
    ) {
        base = strip_trailing_paren(base);
    }
    if model_format == ModelFormatMode::Short {
        base = strip_short_model_prefixes(base);
    }

    if inferred_effort.is_empty() || effort_format == EffortFormatMode::Hidden {
        return base.to_string();
    }
    if model_format == ModelFormatMode::Full
        && paren_effort.is_some()
        && effort_format == EffortFormatMode::Text
    {
        return base.to_string();
    }

    match effort_format {
        EffortFormatMode::Symbol => format!("{base} {}", effort_symbol(&inferred_effort)),
        EffortFormatMode::Text => format!("{base} · {}", effort_short(&inferred_effort)),
        EffortFormatMode::Hidden => base.to_string(),
    }
}

pub fn extract_context_metrics(stdin: Option<&AgyStdinData>) -> ContextMetrics {
    let exceeds_flag = stdin.and_then(|s| s.exceeds_200k_tokens).unwrap_or(false);
    let Some(cw) = stdin.and_then(|s| s.context_window.as_ref()) else {
        return ContextMetrics {
            has_data: false,
            used_percent: 0.0,
            remaining_percent: 100.0,
            used_tokens: None,
            window_size: None,
            exceeds_200k: exceeds_flag,
            cache_hit_percent: None,
        };
    };

    let window_size = cw.context_window_size.filter(|&sz| sz > 0);
    let mut current_context_tokens: Option<u64> = None;
    let mut cache_hit_percent: Option<u64> = None;

    if let Some(cu) = cw.current_usage.as_ref() {
        let in_tok = cu.input_tokens.unwrap_or(0);
        let out_tok = cu.output_tokens.unwrap_or(0);
        let cache_create = cu.cache_creation_input_tokens.unwrap_or(0);
        let cache_read = cu.cache_read_input_tokens.unwrap_or(0);
        let prompt_total = in_tok
            .saturating_add(cache_create)
            .saturating_add(cache_read);
        if prompt_total.saturating_add(out_tok) > 0 {
            current_context_tokens = Some(prompt_total.saturating_add(out_tok));
        }
        if cache_read > 0 && prompt_total > 0 {
            cache_hit_percent =
                Some(((cache_read as f64 / prompt_total as f64) * 100.0).round() as u64);
        }
    }

    let total_tokens_fallback =
        if cw.total_input_tokens.is_some() || cw.total_output_tokens.is_some() {
            Some(
                cw.total_input_tokens
                    .unwrap_or(0)
                    .saturating_add(cw.total_output_tokens.unwrap_or(0)),
            )
        } else {
            None
        };

    let used_tokens = current_context_tokens.or(total_tokens_fallback);

    let raw_used_pct = cw
        .used_percentage
        .filter(|v| v.is_finite())
        .or_else(|| match (used_tokens, window_size) {
            (Some(u), Some(w)) if w > 0 => Some((u as f64 / w as f64) * 100.0),
            _ => None,
        })
        .unwrap_or(0.0);
    let used_percent = raw_used_pct.clamp(0.0, 100.0);

    let remaining_percent = cw
        .remaining_percentage
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(0.0, 100.0))
        .unwrap_or_else(|| (100.0 - used_percent).max(0.0));

    let has_data = cw.used_percentage.is_some() || used_tokens.is_some() || window_size.is_some();
    let exceeds_200k = exceeds_flag || used_tokens.is_some_and(|u| u > 200_000);

    ContextMetrics {
        has_data,
        used_percent,
        remaining_percent,
        used_tokens,
        window_size,
        exceeds_200k,
        cache_hit_percent,
    }
}

fn clean_quota_label(bucket_id: &str) -> String {
    let clean = sanitize_text(bucket_id);
    let lower = clean.to_ascii_lowercase();
    let rest = if let Some(stripped) = lower.strip_prefix("antigravity") {
        let idx = clean.len() - stripped.len();
        clean[idx..].trim_start_matches(['-', '_'])
    } else if let Some(stripped) = lower.strip_prefix("gemini") {
        let idx = clean.len() - stripped.len();
        clean[idx..].trim_start_matches(['-', '_'])
    } else {
        clean.as_str()
    };
    if rest.is_empty() {
        clean
    } else {
        rest.to_string()
    }
}

pub fn extract_quota_buckets(stdin: Option<&AgyStdinData>) -> Vec<QuotaBucketView> {
    let Some(quota) = stdin.and_then(|s| s.quota.as_ref()) else {
        return Vec::new();
    };
    let mut buckets = Vec::new();
    for (id, raw) in quota {
        if raw.disabled.unwrap_or(false) {
            continue;
        }
        let mut remaining_percent = None;
        let mut used_percent = None;
        if let Some(frac_raw) = raw.remaining_fraction
            && frac_raw.is_finite()
        {
            let frac = frac_raw.clamp(0.0, 1.0);
            remaining_percent = Some((frac * 1000.0).round() / 10.0);
            used_percent = Some(((1.0 - frac) * 1000.0).round() / 10.0);
        }
        let remaining_amount = raw.remaining_amount.filter(|v| v.is_finite());
        let reset_in_seconds = raw.reset_in_seconds.filter(|v| v.is_finite() && *v >= 0.0);

        if used_percent.is_none() && remaining_amount.is_none() {
            continue;
        }
        buckets.push(QuotaBucketView {
            id: sanitize_text(id),
            label: clean_quota_label(id),
            used_percent,
            remaining_percent,
            remaining_amount,
            reset_in_seconds,
        });
    }
    buckets
}

pub fn format_compact_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", (count as f64) / 1_000_000.0)
    } else if count >= 10_000 {
        format!("{}k", ((count as f64) / 1_000.0).round() as u64)
    } else if count >= 1_000 {
        format!("{:.1}k", (count as f64) / 1_000.0)
    } else {
        count.to_string()
    }
}

pub fn format_duration_seconds(total_seconds: f64) -> String {
    if !total_seconds.is_finite() || total_seconds < 0.0 {
        return "0s".to_string();
    }
    let sec = total_seconds.floor() as u64;
    if sec < 60 {
        return format!("{sec}s");
    }
    let days = sec / 86_400;
    let hours = (sec % 86_400) / 3_600;
    let mins = (sec % 3_600) / 60;
    let rem_sec = sec % 60;

    if days > 0 {
        if hours > 0 {
            format!("{days}d {hours}h")
        } else {
            format!("{days}d")
        }
    } else if hours > 0 {
        if mins > 0 {
            format!("{hours}h {mins}m")
        } else {
            format!("{hours}h")
        }
    } else if rem_sec > 0 && mins < 10 {
        format!("{mins}m {rem_sec}s")
    } else {
        format!("{mins}m")
    }
}

pub fn format_workspace_path(raw_path: Option<&str>, levels: PathLevels) -> String {
    let clean = sanitize_text(raw_path.unwrap_or(""));
    if clean.is_empty() {
        return String::new();
    }
    let Some(keep_count) = levels.as_count() else {
        return clean;
    };
    let normalized = clean.replace('\\', "/");
    let trimmed = normalized.trim_end_matches('/');
    let parts: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return "/".to_string();
    }
    if parts.len() <= keep_count {
        return parts.join("/");
    }
    parts[parts.len() - keep_count..].join("/")
}
