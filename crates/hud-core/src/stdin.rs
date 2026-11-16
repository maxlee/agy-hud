use crate::ansi::sanitize_text;
use crate::transcript::parse_iso_ms;
use crate::types::{
    AgyStdinData, CachedQuotaSnapshot, ContextScaleMode, EffortFormatMode, GlyphMode,
    LearnedCompactionState, ModelFormatMode, ModelStdin, PathLevels, QuotaBucketView, QuotaPace,
    RawQuotaBucket,
};
use std::collections::{BTreeMap, BTreeSet};

const KNOWN_CANONICAL_BUCKETS: &[&str] = &[
    "3p-5h",
    "3p-weekly",
    "gemini-5h",
    "gemini-argon-wtu-weekly",
    "gemini-weekly",
];

#[derive(Debug, Clone, PartialEq)]
pub struct ContextMetrics {
    pub has_data: bool,
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub used_tokens: Option<u64>,
    pub current_usage_tokens: Option<u64>,
    pub window_size: Option<u64>,
    pub cache_hit_percent: Option<u64>,
}

fn effort_symbol_for_mode(effort_lower: &str, glyph_mode: GlyphMode) -> &str {
    match glyph_mode {
        GlyphMode::Ascii => match effort_lower {
            "low" => "*",
            "medium" | "med" => "**",
            "high" => "***",
            "xhigh" | "max" => "****",
            other => other,
        },
        GlyphMode::Unicode | GlyphMode::Nerd => match effort_lower {
            "low" => "⚡",
            "medium" | "med" => "⚡⚡",
            "high" => "⚡⚡⚡",
            "xhigh" | "max" => "⚡⚡⚡⚡",
            other => other,
        },
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

pub(crate) fn is_effort_token(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "low" | "med" | "medium" | "high" | "xhigh" | "x-high" | "max" | "thinking" | "minimal"
    )
}

fn extract_trailing_paren_effort(raw_name: &str) -> Option<&str> {
    let trimmed = raw_name.trim_end();
    let open = trimmed.rfind('(')?;
    if !trimmed.ends_with(')') {
        return None;
    }
    let inside = &trimmed[open + 1..trimmed.len() - 1];
    is_effort_token(inside).then_some(inside.trim())
}

const MAX_MODEL_FAMILY_BYTES: usize = 160;

pub(crate) fn normalize_model_family_str(
    raw_name: &str,
    explicit_effort: Option<&str>,
) -> Option<String> {
    let mut s = sanitize_text(raw_name).trim().to_string();
    if s.is_empty() {
        return None;
    }
    let effort_clean = explicit_effort
        .map(sanitize_text)
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty());

    loop {
        if s.ends_with(')')
            && let Some(open) = s.rfind('(')
        {
            let inside = &s[open + 1..s.len() - 1];
            let matches_effort = is_effort_token(inside)
                || effort_clean
                    .as_deref()
                    .is_some_and(|eff| inside.trim().eq_ignore_ascii_case(eff));
            if !inside.contains('(') && matches_effort {
                s.truncate(open);
                s = s.trim_end().to_string();
                continue;
            }
        }

        if let Some(ref eff) = effort_clean
            && is_effort_token(eff)
            && s.len() > eff.len()
        {
            let split_idx = s.len() - eff.len();
            if s.is_char_boundary(split_idx) && s[split_idx..].eq_ignore_ascii_case(eff) {
                let sep_b = s.as_bytes()[split_idx - 1];
                if (sep_b == b'-' || sep_b.is_ascii_whitespace())
                    && !s[..split_idx - 1].trim().is_empty()
                {
                    s.truncate(split_idx - 1);
                    s = s.trim_end().to_string();
                    continue;
                }
            }
        }

        break;
    }

    let mut normalized = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if normalized.len() > MAX_MODEL_FAMILY_BYTES {
        let mut cut = 0;
        for (idx, ch) in normalized.char_indices() {
            let next = idx + ch.len_utf8();
            if next > MAX_MODEL_FAMILY_BYTES {
                break;
            }
            cut = next;
        }
        normalized.truncate(cut);
        let trimmed_len = normalized.trim_end().len();
        normalized.truncate(trimmed_len);
    }
    (!normalized.is_empty()).then_some(normalized)
}

/// Normalizes a model payload into a canonical lowercase model-family key for compaction learning,
/// stripping effort suffixes while preserving architectural qualifiers such as `(Carbon)` or `(1M)`.
pub fn normalize_model_family(model: Option<&ModelStdin>) -> Option<String> {
    let model = model?;
    let raw_candidate = model
        .id
        .as_deref()
        .filter(|s| !sanitize_text(s).trim().is_empty())
        .or_else(|| {
            model
                .display_name
                .as_deref()
                .filter(|s| !sanitize_text(s).trim().is_empty())
        })?;
    normalize_model_family_str(raw_candidate, model.effort.as_deref())
}

/// Non-empty normalized `model_family` paired with `phys_win > 0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionScope {
    model_family: String,
    phys_win: u64,
}

impl CompactionScope {
    pub fn from_stdin(stdin: Option<&AgyStdinData>) -> Option<Self> {
        let s = stdin?;
        let model_family = normalize_model_family(s.model.as_ref())?;
        let phys_win = s
            .context_window
            .as_ref()?
            .context_window_size
            .filter(|&w| w > 0)?;
        Some(Self {
            model_family,
            phys_win,
        })
    }

    pub fn model_family(&self) -> &str {
        &self.model_family
    }

    pub fn phys_win(&self) -> u64 {
        self.phys_win
    }

    pub fn storage_key(&self) -> String {
        format!("{}\n{}", self.model_family, self.phys_win)
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
    format_model_name_with_glyphs(model, model_format, effort_format, GlyphMode::Unicode)
}

pub fn format_model_name_with_glyphs(
    model: Option<&ModelStdin>,
    model_format: ModelFormatMode,
    effort_format: EffortFormatMode,
    glyph_mode: GlyphMode,
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

    let dot_sep = match glyph_mode {
        GlyphMode::Ascii => ".",
        GlyphMode::Unicode | GlyphMode::Nerd => "·",
    };

    match effort_format {
        EffortFormatMode::Symbol => {
            format!(
                "{base} {}",
                effort_symbol_for_mode(&inferred_effort, glyph_mode)
            )
        }
        EffortFormatMode::Text => format!("{base} {dot_sep} {}", effort_short(&inferred_effort)),
        EffortFormatMode::Hidden => base.to_string(),
    }
}

/// Normalizes verbose plan tier names (e.g. `"Google AI Pro"` -> `"Pro"`).
pub fn normalize_plan_tier(raw: &str) -> String {
    let clean = sanitize_text(raw);
    if clean.is_empty() {
        return String::new();
    }
    let lower = clean.to_ascii_lowercase();
    for prefix in ["google ai ", "antigravity "] {
        if lower.starts_with(prefix) {
            let rest = clean[prefix.len()..].trim();
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    clean
}

/// Formats USD session cost with sub-cent precision (`$0.00`, `<$0.001`, `$0.004`, `$0.38`).
pub fn format_session_cost(usd: f64) -> Option<String> {
    if !usd.is_finite() || usd < 0.0 {
        return None;
    }
    if usd == 0.0 {
        Some("$0.00".to_string())
    } else if usd < 0.001 {
        Some("<$0.001".to_string())
    } else if usd < 0.01 {
        Some(format!("${usd:.3}"))
    } else {
        Some(format!("${usd:.2}"))
    }
}

pub fn extract_context_metrics(stdin: Option<&AgyStdinData>) -> ContextMetrics {
    let Some(cw) = stdin.and_then(|s| s.context_window.as_ref()) else {
        return ContextMetrics {
            has_data: false,
            used_percent: 0.0,
            remaining_percent: 100.0,
            used_tokens: None,
            current_usage_tokens: None,
            window_size: None,
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

    ContextMetrics {
        has_data,
        used_percent,
        remaining_percent,
        used_tokens,
        current_usage_tokens: current_context_tokens,
        window_size,
        cache_hit_percent,
    }
}

pub const DEFAULT_COMPACTION_CEILING_TOKENS: u64 = 256_000;
pub const MIN_COMPACTION_OBSERVATION_TOKENS: u64 = 32_000;
pub const COLD_START_LARGE_WINDOW_MIN_TOKENS: u64 = 900_000;

/// Snaps single-turn checkpoint overshoot (`200k..=320k` -> `256k`, `440k..=620k` -> `512k`)
/// and drops `< 32k` noise so minor turn overshoot does not jitter the denominator across ticks.
pub fn snap_compaction_ceiling(observed_tokens: u64) -> Option<u64> {
    if observed_tokens < MIN_COMPACTION_OBSERVATION_TOKENS {
        None
    } else if (200_000..=320_000).contains(&observed_tokens) {
        Some(256_000)
    } else if (440_000..=620_000).contains(&observed_tokens) {
        Some(512_000)
    } else {
        Some(observed_tokens)
    }
}

pub fn bypass_threshold_for(ceiling_tokens: u64, compaction_count: u32) -> u64 {
    if compaction_count == 0 {
        ceiling_tokens.saturating_mul(115) / 100
    } else {
        ceiling_tokens.saturating_mul(125) / 100
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactionInputs {
    pub compaction_count: u32,
    pub last_compaction_tokens: Option<u64>,
    pub learned: Option<LearnedCompactionState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeilingSource {
    SessionCheckpoint,
    LearnedModelCache,
    ColdStartDefault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveCeiling {
    Scaled {
        ceiling_tokens: u64,
        source: CeilingSource,
    },
    PhysicalWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionResolution {
    pub effective: EffectiveCeiling,
    pub stale_bypass: Option<(u64, Option<u64>)>,
}

const _: () = assert!(COLD_START_LARGE_WINDOW_MIN_TOKENS > DEFAULT_COMPACTION_CEILING_TOKENS);

/// Precedence: `SessionCheckpoint` > `LearnedModelCache` > `Invalidated` > `ColdStartDefault` (`>= 900k`).
/// `stale_bypass` fires only when `authoritative_current_usage_tokens` exceeds the threshold so
/// cumulative session token fallbacks never poison cross-session model caches.
pub fn resolve_compaction_ceiling(
    phys_win: u64,
    compaction_count: u32,
    last_compaction_tokens: Option<u64>,
    learned: Option<LearnedCompactionState>,
    used_tokens: Option<u64>,
    authoritative_current_usage_tokens: Option<u64>,
) -> CompactionResolution {
    let session_ceiling = last_compaction_tokens
        .and_then(snap_compaction_ceiling)
        .filter(|&c| c < phys_win);

    let (cand_ceiling, source, prev_learned_opt) = if let Some(c) = session_ceiling {
        (c, CeilingSource::SessionCheckpoint, None)
    } else if let Some(LearnedCompactionState::Ceiling(c)) = learned
        && c > 0
        && c < phys_win
    {
        (c, CeilingSource::LearnedModelCache, Some(c))
    } else if matches!(learned, Some(LearnedCompactionState::Invalidated)) {
        return CompactionResolution {
            effective: EffectiveCeiling::PhysicalWindow,
            stale_bypass: None,
        };
    } else if phys_win >= COLD_START_LARGE_WINDOW_MIN_TOKENS {
        (
            DEFAULT_COMPACTION_CEILING_TOKENS,
            CeilingSource::ColdStartDefault,
            None,
        )
    } else {
        return CompactionResolution {
            effective: EffectiveCeiling::PhysicalWindow,
            stale_bypass: None,
        };
    };

    let threshold = bypass_threshold_for(cand_ceiling, compaction_count);
    if used_tokens.is_some_and(|u| u > threshold) {
        let stale_bypass = if compaction_count == 0
            && source != CeilingSource::SessionCheckpoint
            && let Some(au) = authoritative_current_usage_tokens
            && au > threshold
        {
            Some((au, prev_learned_opt))
        } else {
            None
        };
        return CompactionResolution {
            effective: EffectiveCeiling::PhysicalWindow,
            stale_bypass,
        };
    }

    CompactionResolution {
        effective: EffectiveCeiling::Scaled {
            ceiling_tokens: cand_ceiling,
            source,
        },
        stale_bypass: None,
    }
}

pub fn extract_context_metrics_scaled(
    stdin: Option<&AgyStdinData>,
    scale: ContextScaleMode,
    compaction: CompactionInputs,
) -> ContextMetrics {
    let raw = extract_context_metrics(stdin);
    if scale == ContextScaleMode::Window {
        return raw;
    }
    let (Some(used_tok), Some(phys_win)) = (raw.used_tokens, raw.window_size) else {
        return raw;
    };

    let resolution = resolve_compaction_ceiling(
        phys_win,
        compaction.compaction_count,
        compaction.last_compaction_tokens,
        compaction.learned,
        Some(used_tok),
        raw.current_usage_tokens,
    );
    let EffectiveCeiling::Scaled { ceiling_tokens, .. } = resolution.effective else {
        return raw;
    };

    let used_percent = ((used_tok as f64 / ceiling_tokens as f64) * 100.0).clamp(0.0, 100.0);
    let remaining_percent = (100.0 - used_percent).max(0.0);

    ContextMetrics {
        used_percent,
        remaining_percent,
        window_size: Some(ceiling_tokens),
        ..raw
    }
}

fn clean_quota_label(bucket_id: &str) -> String {
    let clean = sanitize_text(bucket_id);
    let lower = clean.to_ascii_lowercase();
    if lower == "gemini-argon-wtu-weekly" {
        return "argon-weekly".to_string();
    }
    let rest = if let Some(stripped) = lower.strip_prefix("antigravity") {
        let idx = clean.len() - stripped.len();
        clean[idx..].trim_start_matches(['-', '_'])
    } else if let Some(stripped) = lower.strip_prefix("gemini") {
        let idx = clean.len() - stripped.len();
        clean[idx..].trim_start_matches(['-', '_'])
    } else if let Some(stripped) = lower.strip_prefix("3p") {
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

fn is_bucket_active(raw: &RawQuotaBucket, now_ms: i64) -> bool {
    if raw.disabled.unwrap_or(false) {
        return false;
    }
    if now_ms > 0
        && let Some(rt_str) = raw.reset_time.as_deref()
        && let Some(rt_ms) = parse_iso_ms(rt_str)
        && rt_ms <= now_ms
    {
        return false;
    }
    true
}

fn merge_active_buckets(
    official: Option<&RawQuotaBucket>,
    cached: Option<&RawQuotaBucket>,
    now_ms: i64,
) -> Option<RawQuotaBucket> {
    let off = official.filter(|b| is_bucket_active(b, now_ms));
    let cac = cached.filter(|b| is_bucket_active(b, now_ms));

    match (off, cac) {
        (Some(o), Some(c)) => {
            let o_rt = o.reset_time.as_deref().and_then(parse_iso_ms);
            let c_rt = c.reset_time.as_deref().and_then(parse_iso_ms);
            if let (Some(o_ms), Some(c_ms)) = (o_rt, c_rt) {
                if o_ms > c_ms + 60_000 {
                    return Some(o.clone());
                }
                if c_ms > o_ms + 60_000 {
                    return Some(c.clone());
                }
            }
            let remaining_fraction = match (o.remaining_fraction, c.remaining_fraction) {
                (Some(a), Some(b)) if a.is_finite() && b.is_finite() => Some(a.min(b)),
                (Some(a), _) if a.is_finite() => Some(a),
                (_, Some(b)) if b.is_finite() => Some(b),
                _ => None,
            };
            let remaining_amount = match (o.remaining_amount, c.remaining_amount) {
                (Some(a), Some(b)) if a.is_finite() && b.is_finite() => Some(a.min(b)),
                (Some(a), _) if a.is_finite() => Some(a),
                (_, Some(b)) if b.is_finite() => Some(b),
                _ => None,
            };
            Some(RawQuotaBucket {
                remaining_fraction,
                remaining_amount,
                reset_time: c.reset_time.clone().or_else(|| o.reset_time.clone()),
                reset_in_seconds: o.reset_in_seconds.or(c.reset_in_seconds),
                disabled: Some(false),
            })
        }
        (Some(o), None) => Some(o.clone()),
        (None, Some(c)) => Some(c.clone()),
        (None, None) => None,
    }
}

fn bucket_to_view(id: &str, raw: &RawQuotaBucket, now_ms: i64) -> Option<QuotaBucketView> {
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
    let reset_in_seconds = if now_ms > 0
        && let Some(rt_ms) = raw.reset_time.as_deref().and_then(parse_iso_ms)
        && rt_ms > now_ms
    {
        Some(((rt_ms - now_ms) as f64) / 1000.0)
    } else {
        raw.reset_in_seconds.filter(|v| v.is_finite() && *v >= 0.0)
    };

    if used_percent.is_none() && remaining_amount.is_none() {
        return None;
    }
    Some(QuotaBucketView {
        id: sanitize_text(id),
        label: clean_quota_label(id),
        used_percent,
        remaining_percent,
        remaining_amount,
        reset_in_seconds,
    })
}

pub fn extract_quota_buckets(stdin: Option<&AgyStdinData>) -> Vec<QuotaBucketView> {
    extract_quota_buckets_reconciled(stdin, None, 0)
}

/// Extracts model-appropriate quota buckets while filtering expired windows (`reset_time <= now_ms`)
/// and monotonically reconciling optional cached `/usage` snapshots within the same active window.
pub fn extract_quota_buckets_reconciled(
    stdin: Option<&AgyStdinData>,
    cached: Option<&CachedQuotaSnapshot>,
    now_ms: i64,
) -> Vec<QuotaBucketView> {
    let official_map = stdin.and_then(|s| s.quota.as_ref());
    let cached_map = cached.map(|c| &c.quota);
    if official_map.is_none() && cached_map.is_none() {
        return Vec::new();
    }

    let mut all_ids = BTreeSet::new();
    if let Some(m) = official_map {
        all_ids.extend(m.keys().cloned());
    }
    if let Some(m) = cached_map {
        all_ids.extend(m.keys().cloned());
    }
    if all_ids.is_empty() {
        return Vec::new();
    }

    let has_canonical_source = KNOWN_CANONICAL_BUCKETS.iter().any(|k| all_ids.contains(*k));
    let source_has_argon = all_ids.contains("gemini-argon-wtu-weekly");

    let mut merged: BTreeMap<String, RawQuotaBucket> = BTreeMap::new();
    for id in &all_ids {
        let off = official_map.and_then(|m| m.get(id));
        let cac = cached_map.and_then(|m| m.get(id));
        if let Some(reconciled) = merge_active_buckets(off, cac, now_ms) {
            merged.insert(id.clone(), reconciled);
        }
    }

    if !has_canonical_source {
        return merged
            .iter()
            .filter_map(|(id, raw)| bucket_to_view(id, raw, now_ms))
            .collect();
    }

    let model_haystack = stdin
        .and_then(|s| s.model.as_ref())
        .map(|m| {
            format!(
                "{} {}",
                m.id.as_deref().unwrap_or(""),
                m.display_name.as_deref().unwrap_or("")
            )
            .to_ascii_lowercase()
        })
        .unwrap_or_default();

    let is_3p = [
        "claude",
        "gpt",
        "oss",
        "sonnet",
        "opus",
        "haiku",
        "openai",
        "anthropic",
    ]
    .iter()
    .any(|kw| model_haystack.contains(kw));
    let is_argon = model_haystack.contains("argon");

    let routed_keys: &[&str] = if is_3p {
        &["3p-5h", "3p-weekly"]
    } else if is_argon && source_has_argon {
        &["gemini-argon-wtu-weekly"]
    } else {
        &["gemini-5h", "gemini-weekly"]
    };

    routed_keys
        .iter()
        .filter_map(|&key| {
            merged
                .get(key)
                .and_then(|raw| bucket_to_view(key, raw, now_ms))
        })
        .collect()
}

/// Projects whether the current quota bucket burn rate will exhaust the window before reset.
/// Ignores early low-usage noise (`used_percent < 10%`) to avoid false alarms right after reset.
pub fn compute_quota_pace(bucket: &QuotaBucketView) -> QuotaPace {
    let id_lower = bucket.id.to_ascii_lowercase();
    let label_lower = bucket.label.to_ascii_lowercase();
    let window_sec = if id_lower.contains("5h") || label_lower.contains("5h") {
        18_000.0
    } else if id_lower.contains("weekly")
        || id_lower.contains("7d")
        || label_lower.contains("weekly")
        || label_lower.contains("7d")
    {
        604_800.0
    } else {
        return QuotaPace::Normal;
    };

    let (Some(used_pct), Some(reset_sec)) = (bucket.used_percent, bucket.reset_in_seconds) else {
        return QuotaPace::Normal;
    };
    if !used_pct.is_finite()
        || !reset_sec.is_finite()
        || used_pct < 10.0
        || reset_sec <= 0.0
        || reset_sec >= window_sec
    {
        return QuotaPace::Normal;
    }

    let elapsed_fraction = ((window_sec - reset_sec) / window_sec).clamp(0.0, 1.0);
    if elapsed_fraction <= 0.001 {
        return QuotaPace::Normal;
    }
    let projected_used_pct = used_pct / elapsed_fraction;
    if projected_used_pct > 100.0 {
        QuotaPace::Critical
    } else if projected_used_pct >= 90.0 {
        QuotaPace::Warning
    } else {
        QuotaPace::Normal
    }
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

/// Formats workspace directory with optional launch-directory breadcrumb (`project_dir › current_dir`)
/// when `show_launch_dir` is enabled and `current_dir` is deeper than `pathLevels` or outside `project_dir`.
pub fn format_workspace_breadcrumb(
    project_dir: Option<&str>,
    current_dir: Option<&str>,
    levels: PathLevels,
    show_launch_dir: bool,
    glyph_mode: GlyphMode,
) -> String {
    let active_dir = current_dir.filter(|s| !s.trim().is_empty()).or(project_dir);
    let short_cur = format_workspace_path(active_dir, levels);
    if !show_launch_dir || short_cur.is_empty() {
        return short_cur;
    }

    let Some(keep_count) = levels.as_count() else {
        return short_cur;
    };
    let Some(proj_clean) = project_dir.map(sanitize_text).filter(|s| !s.is_empty()) else {
        return short_cur;
    };
    let Some(cur_clean) = current_dir.map(sanitize_text).filter(|s| !s.is_empty()) else {
        return short_cur;
    };

    let norm_proj = proj_clean.replace('\\', "/");
    let norm_proj = norm_proj.trim_end_matches('/');
    let norm_cur = cur_clean.replace('\\', "/");
    let norm_cur = norm_cur.trim_end_matches('/');
    if norm_proj.is_empty() || norm_cur.is_empty() || norm_proj == norm_cur {
        return short_cur;
    }

    let proj_name = norm_proj.rsplit('/').find(|p| !p.is_empty()).unwrap_or("/");
    let needs_breadcrumb = if let Some(rel) = norm_cur.strip_prefix(norm_proj) {
        if let Some(after_slash) = rel.strip_prefix('/') {
            let depth = after_slash.split('/').filter(|p| !p.is_empty()).count();
            depth >= keep_count
        } else {
            // Sibling directory sharing a string prefix (e.g., `/work/app` vs `/work/app-v2`)
            true
        }
    } else {
        true
    };

    if !needs_breadcrumb {
        return short_cur;
    }

    let sep = match glyph_mode {
        GlyphMode::Ascii => ">",
        GlyphMode::Unicode | GlyphMode::Nerd => "›",
    };
    format!("{proj_name} {sep} {short_cur}")
}
