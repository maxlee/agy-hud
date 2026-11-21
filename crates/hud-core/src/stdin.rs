use crate::ansi::sanitize_text;
use crate::transcript::parse_iso_ms;
use crate::types::{
    AgyStdinData, CachedQuotaSnapshot, ContextScaleMode, DB_CEILING_VERSION, DbCeilingInfo,
    EffortFormatMode, GlyphMode, LearnedCompactionState, ModelFormatMode, ModelStdin, PathLevels,
    QuotaBucketView, QuotaPace, RawQuotaBucket,
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
    pub ceiling_known: bool,
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

const EFFORT_SUFFIXES_LONGEST_FIRST: [&str; 9] = [
    "x-high", "xhigh", "medium", "thinking", "minimal", "high", "med", "low", "max",
];

fn strip_effort_suffix_once(s: &str, eff: &str, allow_underscore: bool) -> Option<usize> {
    if s.len() <= eff.len() {
        return None;
    }
    let split_idx = s.len() - eff.len();
    if !s.is_char_boundary(split_idx) || !s[split_idx..].eq_ignore_ascii_case(eff) {
        return None;
    }
    let sep = s.as_bytes()[split_idx - 1];
    let sep_ok = sep == b'-' || sep.is_ascii_whitespace() || (allow_underscore && sep == b'_');
    if sep_ok && !s[..split_idx - 1].trim().is_empty() {
        Some(split_idx - 1)
    } else {
        None
    }
}

fn strip_trailing_efforts(
    mut s: String,
    explicit_effort: Option<&str>,
    strip_any_known: bool,
) -> String {
    loop {
        if s.ends_with(')')
            && let Some(open) = s.rfind('(')
        {
            let inside = &s[open + 1..s.len() - 1];
            let matches_effort = is_effort_token(inside)
                || explicit_effort.is_some_and(|eff| inside.trim().eq_ignore_ascii_case(eff));
            if !inside.contains('(') && matches_effort {
                s.truncate(open);
                s = s.trim_end().to_string();
                continue;
            }
        }

        if let Some(eff) = explicit_effort
            && is_effort_token(eff)
            && let Some(cut) = strip_effort_suffix_once(&s, eff, false)
        {
            s.truncate(cut);
            s = s.trim_end().to_string();
            continue;
        }

        if strip_any_known {
            let mut cut_at = None;
            for eff in EFFORT_SUFFIXES_LONGEST_FIRST {
                if let Some(cut) = strip_effort_suffix_once(&s, eff, true) {
                    cut_at = Some(cut);
                    break;
                }
            }
            if let Some(cut) = cut_at {
                s.truncate(cut);
                s = s.trim_end().to_string();
                continue;
            }
        }

        break;
    }
    s
}

/// Normalizes a raw model identifier or display name into a lowercase model-family string,
/// stripping optional effort suffixes while preserving architectural qualifiers such as `(1M)`.
pub fn normalize_model_family_str(raw_name: &str, explicit_effort: Option<&str>) -> Option<String> {
    let s = sanitize_text(raw_name).trim().to_string();
    if s.is_empty() {
        return None;
    }
    let effort_clean = explicit_effort
        .map(sanitize_text)
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty());

    let s = strip_trailing_efforts(s, effort_clean.as_deref(), false);

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

/// Returns `true` for shadow, utility, or image-generation models (`flash-lite`, `flash_lite`,
/// `banana`, `image`, and non-digit-followed `m50` such as `M50` but not `m500`).
#[must_use]
pub fn is_shadow_or_utility_model(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if lower.contains("flash-lite")
        || lower.contains("flash_lite")
        || lower.contains("banana")
        || lower.contains("image")
    {
        return true;
    }
    let bytes = lower.as_bytes();
    lower.match_indices("m50").any(|(idx, _)| {
        let before_ok = idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric();
        let after_ok = !bytes.get(idx + 3).is_some_and(u8::is_ascii_digit);
        before_ok && after_ok
    })
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
            ceiling_known: true,
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
        ceiling_known: true,
        used_percent,
        remaining_percent,
        used_tokens,
        current_usage_tokens: current_context_tokens,
        window_size,
        cache_hit_percent,
    }
}

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactionInputs {
    pub compaction_count: u32,
    pub last_compaction_tokens: Option<u64>,
    pub learned: Option<LearnedCompactionState>,
    pub db_ceiling: Option<DbCeilingInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeilingSource {
    DbGenMetadata,
    SessionCheckpoint,
    LearnedModelCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveCeiling {
    Scaled {
        ceiling_tokens: u64,
        source: CeilingSource,
    },
    PhysicalWindow,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionResolution {
    pub effective: EffectiveCeiling,
    pub stale_bypass: Option<(u64, Option<u64>)>,
}

/// Normalizes a model identifier or display name into a space-delimited, effort-stripped
/// canonical key for cross-format equality matching (`claude-opus-5-5-medium` == `Claude Opus 5.5 (Medium)`).
pub fn canonical_db_family_key(raw_name: &str) -> Option<String> {
    let raw = sanitize_text(raw_name).trim().to_ascii_lowercase();
    if raw.is_empty() {
        return None;
    }
    let s = strip_trailing_efforts(raw, None, true);

    let mut out = String::with_capacity(s.len());
    let mut prev_space = true;
    for ch in s.chars() {
        if matches!(ch, '-' | '_' | '.') || ch.is_ascii_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    let trimmed = out.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn db_ceiling_matches_family(db: &DbCeilingInfo, active_model_family: Option<&str>) -> bool {
    let active_family = active_model_family.and_then(|f| normalize_model_family_str(f, None));
    let db_family = db
        .model_family
        .as_deref()
        .and_then(|f| normalize_model_family_str(f, None))
        .or_else(|| {
            db.model_id
                .as_deref()
                .and_then(|id| normalize_model_family_str(id, None))
        });
    let (Some(act), Some(dbf)) = (active_family.as_deref(), db_family.as_deref()) else {
        return true;
    };
    if act == dbf {
        return true;
    }
    match (canonical_db_family_key(act), canonical_db_family_key(dbf)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Precedence: `DbGenMetadata`, then `SessionCheckpoint` (`1.25x` bypass), then
/// `PhysWindowSmall` (`< 900k`), then `LearnedModelCache` (`1.15x` bypass to `PhysicalWindow`),
/// and finally `EffectiveCeiling::Unknown`.
/// `stale_bypass` fires only when `authoritative_current_usage_tokens` exceeds the threshold so
/// cumulative session token fallbacks never poison cross-session model caches.
pub fn resolve_compaction_ceiling_with_db(
    phys_win: u64,
    compaction: &CompactionInputs,
    used_tokens: Option<u64>,
    authoritative_current_usage_tokens: Option<u64>,
    active_model_family: Option<&str>,
) -> CompactionResolution {
    if let Some(ref db) = compaction.db_ceiling
        && db.version == DB_CEILING_VERSION
        && (MIN_COMPACTION_OBSERVATION_TOKENS..=phys_win).contains(&db.ceiling_tokens)
        && db_ceiling_matches_family(db, active_model_family)
        && used_tokens.is_none_or(|u| {
            u <= bypass_threshold_for(db.ceiling_tokens, compaction.compaction_count)
        })
    {
        return CompactionResolution {
            effective: EffectiveCeiling::Scaled {
                ceiling_tokens: db.ceiling_tokens,
                source: CeilingSource::DbGenMetadata,
            },
            stale_bypass: None,
        };
    }

    let session_ceiling = compaction
        .last_compaction_tokens
        .and_then(snap_compaction_ceiling)
        .filter(|&c| c < phys_win);

    if let Some(c) = session_ceiling {
        let threshold = bypass_threshold_for(c, compaction.compaction_count);
        if used_tokens.is_some_and(|u| u > threshold) {
            return CompactionResolution {
                effective: EffectiveCeiling::PhysicalWindow,
                stale_bypass: None,
            };
        }
        return CompactionResolution {
            effective: EffectiveCeiling::Scaled {
                ceiling_tokens: c,
                source: CeilingSource::SessionCheckpoint,
            },
            stale_bypass: None,
        };
    }

    if phys_win < COLD_START_LARGE_WINDOW_MIN_TOKENS {
        return CompactionResolution {
            effective: EffectiveCeiling::PhysicalWindow,
            stale_bypass: None,
        };
    }

    match compaction.learned {
        Some(LearnedCompactionState::Ceiling(c)) if c > 0 && c < phys_win => {
            let threshold = bypass_threshold_for(c, compaction.compaction_count);
            if used_tokens.is_some_and(|u| u > threshold) {
                let stale_bypass = if compaction.compaction_count == 0
                    && let Some(au) = authoritative_current_usage_tokens
                    && au > threshold
                {
                    Some((au, Some(c)))
                } else {
                    None
                };
                CompactionResolution {
                    effective: EffectiveCeiling::PhysicalWindow,
                    stale_bypass,
                }
            } else {
                CompactionResolution {
                    effective: EffectiveCeiling::Scaled {
                        ceiling_tokens: c,
                        source: CeilingSource::LearnedModelCache,
                    },
                    stale_bypass: None,
                }
            }
        }
        Some(LearnedCompactionState::Invalidated) => CompactionResolution {
            effective: EffectiveCeiling::PhysicalWindow,
            stale_bypass: None,
        },
        _ => CompactionResolution {
            effective: EffectiveCeiling::Unknown,
            stale_bypass: None,
        },
    }
}

/// Precedence without DB metadata (`SessionCheckpoint`, `PhysWindowSmall`, `LearnedModelCache`, `Unknown`).
pub fn resolve_compaction_ceiling(
    phys_win: u64,
    compaction_count: u32,
    last_compaction_tokens: Option<u64>,
    learned: Option<LearnedCompactionState>,
    used_tokens: Option<u64>,
    authoritative_current_usage_tokens: Option<u64>,
) -> CompactionResolution {
    let inputs = CompactionInputs {
        compaction_count,
        last_compaction_tokens,
        learned,
        db_ceiling: None,
    };
    resolve_compaction_ceiling_with_db(
        phys_win,
        &inputs,
        used_tokens,
        authoritative_current_usage_tokens,
        None,
    )
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

    let active_family = normalize_model_family(stdin.and_then(|s| s.model.as_ref()));
    let resolution = resolve_compaction_ceiling_with_db(
        phys_win,
        &compaction,
        Some(used_tok),
        raw.current_usage_tokens,
        active_family.as_deref(),
    );
    match resolution.effective {
        EffectiveCeiling::Scaled { ceiling_tokens, .. } => {
            let used_percent =
                ((used_tok as f64 / ceiling_tokens as f64) * 100.0).clamp(0.0, 100.0);
            let remaining_percent = (100.0 - used_percent).max(0.0);
            ContextMetrics {
                ceiling_known: true,
                used_percent,
                remaining_percent,
                window_size: Some(ceiling_tokens),
                ..raw
            }
        }
        EffectiveCeiling::PhysicalWindow => raw,
        EffectiveCeiling::Unknown => ContextMetrics {
            ceiling_known: false,
            used_percent: 0.0,
            remaining_percent: 100.0,
            window_size: None,
            ..raw
        },
    }
}

/// Timestamps on or before `2000-01-01T00:00:00Z` (e.g. `1970-01-01`, `0001-01-01`) are
/// sentinel placeholders emitted by uninitialized API fields and treated as invalid.
pub const SENTINEL_RESET_CUTOFF_MS: i64 = 946_684_800_000;

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

fn is_nonneg_finite(v: f64) -> bool {
    v.is_finite() && v >= 0.0
}

/// Normalizes remaining fraction for a quota bucket, inferring `0.0` (100% used) when Proto3
/// omits `remaining_fraction` (`None`) and `remaining_amount` (`None`) on an active non-disabled
/// bucket with a valid non-sentinel future `reset_time`.
pub(crate) fn effective_remaining_fraction(raw: &RawQuotaBucket, now_ms: i64) -> Option<f64> {
    if raw.disabled == Some(true) {
        return None;
    }
    let parsed_rt = raw.reset_time.as_deref().and_then(parse_iso_ms);
    if let Some(rt_ms) = parsed_rt
        && (rt_ms <= SENTINEL_RESET_CUTOFF_MS || (now_ms > 0 && rt_ms <= now_ms))
    {
        return None;
    }
    if let Some(frac) = raw.remaining_fraction {
        return is_nonneg_finite(frac).then(|| frac.clamp(0.0, 1.0));
    }
    if raw.remaining_amount.is_some() {
        return None;
    }
    parsed_rt.map(|_| 0.0)
}

fn is_bucket_active(raw: &RawQuotaBucket, now_ms: i64) -> bool {
    if raw.disabled.unwrap_or(false) {
        return false;
    }
    if let Some(rt_str) = raw.reset_time.as_deref()
        && let Some(rt_ms) = parse_iso_ms(rt_str)
    {
        if rt_ms <= SENTINEL_RESET_CUTOFF_MS {
            return false;
        }
        if now_ms > 0 && rt_ms <= now_ms {
            return false;
        }
    }
    let has_rem_frac = effective_remaining_fraction(raw, now_ms).is_some();
    let has_rem_amt = raw.remaining_amount.is_some_and(is_nonneg_finite);
    has_rem_frac || has_rem_amt
}

fn normalize_active_bucket(raw: &RawQuotaBucket, now_ms: i64) -> RawQuotaBucket {
    RawQuotaBucket {
        remaining_fraction: effective_remaining_fraction(raw, now_ms),
        remaining_amount: raw.remaining_amount.filter(|&v| is_nonneg_finite(v)),
        reset_time: raw.reset_time.clone(),
        reset_in_seconds: raw.reset_in_seconds,
        disabled: raw.disabled,
    }
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
                if o_ms > c_ms.saturating_add(60_000) {
                    return Some(normalize_active_bucket(o, now_ms));
                }
                if c_ms > o_ms.saturating_add(60_000) {
                    return Some(normalize_active_bucket(c, now_ms));
                }
            }
            let o_frac = effective_remaining_fraction(o, now_ms);
            let c_frac = effective_remaining_fraction(c, now_ms);
            let remaining_fraction = match (o_frac, c_frac) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            let remaining_amount = match (o.remaining_amount, c.remaining_amount) {
                (Some(a), Some(b)) if is_nonneg_finite(a) && is_nonneg_finite(b) => Some(a.min(b)),
                (Some(a), _) if is_nonneg_finite(a) => Some(a),
                (_, Some(b)) if is_nonneg_finite(b) => Some(b),
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
        (Some(o), None) => Some(normalize_active_bucket(o, now_ms)),
        (None, Some(c)) => Some(normalize_active_bucket(c, now_ms)),
        (None, None) => None,
    }
}

fn bucket_to_view(id: &str, raw: &RawQuotaBucket, now_ms: i64) -> Option<QuotaBucketView> {
    if !is_bucket_active(raw, now_ms) {
        return None;
    }
    let mut remaining_percent = None;
    let mut used_percent = None;
    if let Some(frac) = effective_remaining_fraction(raw, now_ms) {
        remaining_percent = Some((frac * 1000.0).round() / 10.0);
        used_percent = Some(((1.0 - frac) * 1000.0).round() / 10.0);
    }
    let remaining_amount = raw.remaining_amount.filter(|&v| is_nonneg_finite(v));
    let parsed_rt_ms = raw.reset_time.as_deref().and_then(parse_iso_ms);
    let reset_in_seconds = if now_ms > 0
        && let Some(rt_ms) = parsed_rt_ms
        && rt_ms > SENTINEL_RESET_CUTOFF_MS
        && rt_ms > now_ms
    {
        Some(((rt_ms - now_ms) as f64) / 1000.0)
    } else if parsed_rt_ms.is_some_and(|rt_ms| rt_ms <= SENTINEL_RESET_CUTOFF_MS) {
        None
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

const NON_FAMILY_MODEL_TOKENS: &[&str] = &[
    "google",
    "gemini",
    "claude",
    "anthropic",
    "openai",
    "antigravity",
    "model",
    "preview",
    "latest",
    "default",
    "inherit",
    "auto",
    "low",
    "med",
    "medium",
    "high",
    "xhigh",
    "thinking",
    "minimal",
    "max",
    "weekly",
    "wtu",
    "5h",
    "7d",
    "pro",
    "flash",
    "ultra",
    "mini",
    "nano",
    "oss",
];

fn extract_active_model_family_tokens(model_haystack: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    for tok in model_haystack.split(|c: char| !c.is_ascii_alphanumeric()) {
        if tok.len() < 3 || tok.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if NON_FAMILY_MODEL_TOKENS.contains(&tok) {
            continue;
        }
        if !tokens.contains(&tok) {
            tokens.push(tok);
        }
    }
    tokens
}

pub fn extract_quota_buckets(stdin: Option<&AgyStdinData>) -> Vec<QuotaBucketView> {
    extract_quota_buckets_reconciled(stdin, None, 0)
}

/// Extracts model-appropriate quota buckets while filtering empty, sentinel, or expired windows
/// (`reset_time <= SENTINEL_RESET_CUTOFF_MS` or `reset_time <= now_ms`) and monotonically
/// reconciling optional cached `/usage` snapshots within the same active window.
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

    let mut candidate_ids = BTreeSet::new();
    if let Some(m) = official_map {
        candidate_ids.extend(m.keys().cloned());
    }
    if let Some(m) = cached_map {
        candidate_ids.extend(m.keys().cloned());
    }
    if candidate_ids.is_empty() {
        return Vec::new();
    }

    // Filter out empty, sentinel, disabled, and expired buckets BEFORE canonical/argon routing.
    let mut merged: BTreeMap<String, RawQuotaBucket> = BTreeMap::new();
    for id in &candidate_ids {
        let off = official_map.and_then(|m| m.get(id));
        let cac = cached_map.and_then(|m| m.get(id));
        if let Some(reconciled) = merge_active_buckets(off, cac, now_ms) {
            merged.insert(id.clone(), reconciled);
        }
    }
    if merged.is_empty() {
        return Vec::new();
    }

    let has_canonical_source = KNOWN_CANONICAL_BUCKETS
        .iter()
        .any(|k| merged.contains_key(*k));
    let source_has_argon = merged.contains_key("gemini-argon-wtu-weekly");

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

    let mut views: Vec<QuotaBucketView> = routed_keys
        .iter()
        .filter_map(|&key| {
            merged
                .get(key)
                .and_then(|raw| bucket_to_view(key, raw, now_ms))
        })
        .collect();

    let family_tokens = extract_active_model_family_tokens(&model_haystack);
    let mut family_views = Vec::new();
    let mut credits_views = Vec::new();
    for (id, raw) in &merged {
        let id_lower = id.to_ascii_lowercase();
        if KNOWN_CANONICAL_BUCKETS.contains(&id_lower.as_str())
            || views.iter().any(|v| v.id.eq_ignore_ascii_case(&id_lower))
        {
            continue;
        }
        let matches_family = !family_tokens.is_empty()
            && id_lower
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|seg| family_tokens.contains(&seg));
        let matches_credits = id_lower.contains("credits");
        if matches_family {
            if let Some(view) = bucket_to_view(id, raw, now_ms) {
                family_views.push(view);
            }
        } else if matches_credits && let Some(view) = bucket_to_view(id, raw, now_ms) {
            credits_views.push(view);
        }
    }
    views.extend(family_views);
    views.extend(credits_views);

    views
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContextWindowStdin, CurrentUsageStdin, DbCeilingSource};

    #[test]
    fn sentinel_cutoff_is_exact_2000_01_01_utc() {
        assert_eq!(
            parse_iso_ms("2000-01-01T00:00:00Z"),
            Some(SENTINEL_RESET_CUTOFF_MS)
        );
        assert_eq!(SENTINEL_RESET_CUTOFF_MS, 946_684_800_000);
    }

    #[test]
    fn quota_zero_percent_resilience_and_sentinel_filtering() {
        let now_ms = 1_794_218_400_000i64; // 2026-11-09T10:00:00Z
        let future_reset = "2099-01-01T00:00:00Z".to_string();

        // 1. Proto3 omitted `remaining_fraction` with valid future `reset_time` -> 0.0 remaining (100% used)
        let exhausted = RawQuotaBucket {
            remaining_fraction: None,
            remaining_amount: None,
            reset_time: Some(future_reset.clone()),
            reset_in_seconds: None,
            disabled: None,
        };
        assert_eq!(effective_remaining_fraction(&exhausted, now_ms), Some(0.0));
        assert_eq!(effective_remaining_fraction(&exhausted, 0), Some(0.0));
        assert!(is_bucket_active(&exhausted, now_ms));

        // 2. Sentinel timestamps (<= 2000-01-01) and empty/disabled buckets are dropped
        for bad in [
            RawQuotaBucket::default(),
            RawQuotaBucket {
                remaining_fraction: None,
                remaining_amount: None,
                reset_time: Some("1970-01-01T00:00:00Z".to_string()),
                reset_in_seconds: None,
                disabled: None,
            },
            RawQuotaBucket {
                remaining_fraction: Some(0.8),
                remaining_amount: None,
                reset_time: Some("0001-01-01T00:00:00Z".to_string()),
                reset_in_seconds: None,
                disabled: None,
            },
            RawQuotaBucket {
                remaining_fraction: None,
                remaining_amount: None,
                reset_time: Some("2000-01-01T00:00:00Z".to_string()),
                reset_in_seconds: None,
                disabled: None,
            },
            RawQuotaBucket {
                remaining_fraction: None,
                remaining_amount: None,
                reset_time: Some(future_reset.clone()),
                reset_in_seconds: None,
                disabled: Some(true),
            },
            RawQuotaBucket {
                remaining_fraction: None,
                remaining_amount: None,
                reset_time: Some("2020-01-01T00:00:00Z".to_string()),
                reset_in_seconds: None,
                disabled: None,
            },
        ] {
            assert!(!is_bucket_active(&bad, now_ms), "should drop {bad:?}");
            assert_eq!(effective_remaining_fraction(&bad, now_ms), None);
        }

        // 3. `remaining_amount` present without `remaining_fraction` does NOT synthesize 0.0
        let amt_only = RawQuotaBucket {
            remaining_fraction: None,
            remaining_amount: Some(12.0),
            reset_time: Some(future_reset.clone()),
            reset_in_seconds: None,
            disabled: None,
        };
        assert_eq!(effective_remaining_fraction(&amt_only, now_ms), None);
        assert!(is_bucket_active(&amt_only, now_ms));

        // 4. Live exhausted (`None -> 0.0`) beats stale cached `0.35` in `merge_active_buckets`
        let stale_cached = RawQuotaBucket {
            remaining_fraction: Some(0.35),
            remaining_amount: None,
            reset_time: Some(future_reset.clone()),
            reset_in_seconds: None,
            disabled: None,
        };
        let merged = merge_active_buckets(Some(&exhausted), Some(&stale_cached), now_ms)
            .expect("merged active bucket");
        assert_eq!(merged.remaining_fraction, Some(0.0));

        // 5. Empty argon placeholder is dropped BEFORE `source_has_argon` routing
        let mut quota_map = BTreeMap::new();
        quota_map.insert(
            "gemini-argon-wtu-weekly".to_string(),
            RawQuotaBucket::default(),
        );
        quota_map.insert(
            "gemini-5h".to_string(),
            RawQuotaBucket {
                remaining_fraction: None,
                remaining_amount: None,
                reset_time: Some(future_reset),
                reset_in_seconds: None,
                disabled: None,
            },
        );
        let stdin = AgyStdinData {
            model: Some(ModelStdin {
                id: Some("Gemini 4 Argon".to_string()),
                display_name: Some("Gemini 4 Argon".to_string()),
                effort: None,
            }),
            quota: Some(quota_map),
            ..Default::default()
        };
        let views = extract_quota_buckets_reconciled(Some(&stdin), None, now_ms);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].id, "gemini-5h");
        assert_eq!(views[0].used_percent, Some(100.0));
        assert_eq!(views[0].remaining_percent, Some(0.0));

        // 6. Proto3 string `remaining_amount: "0"` deserializes cleanly and negative fraction/amount are rejected
        let proto3_stdin: AgyStdinData = serde_json::from_str(&format!(
            r#"{{
                "quota": {{
                    "credits-str": {{"remaining_amount": "0", "reset_time": "{}"}},
                    "neg-frac": {{"remaining_fraction": -0.25, "reset_time": "{}"}},
                    "neg-amt": {{"remaining_amount": "-5", "reset_time": "{}"}},
                    "neg-amt-num": {{"remaining_amount": -1.0}}
                }}
            }}"#,
            "2099-01-01T00:00:00Z", "2099-01-01T00:00:00Z", "2099-01-01T00:00:00Z"
        ))
        .expect("deserializes Proto3 string remaining_amount");
        let str_views = extract_quota_buckets_reconciled(Some(&proto3_stdin), None, now_ms);
        assert_eq!(str_views.len(), 1);
        assert_eq!(str_views[0].id, "credits-str");
        assert_eq!(str_views[0].remaining_amount, Some(0.0));
        assert_eq!(str_views[0].used_percent, None);
    }

    #[test]
    fn db_ceiling_precedence_family_validation_and_unknown_cold_start() {
        let argon_db = DbCeilingInfo::new(
            840_000,
            DbCeilingSource::GenMetadata,
            Some("gemini-4-argon".to_string()),
            Some("gemini 4 argon".to_string()),
        );
        let exec_db = DbCeilingInfo::new(512_000, DbCeilingSource::ExecutorMetadata, None, None);

        // 1. DbGenMetadata beats SessionCheckpoint and LearnedModelCache when valid & family matches
        let res_db = resolve_compaction_ceiling_with_db(
            1_048_576,
            &CompactionInputs {
                compaction_count: 1,
                last_compaction_tokens: Some(256_500),
                learned: Some(LearnedCompactionState::Ceiling(256_000)),
                db_ceiling: Some(argon_db.clone()),
            },
            Some(152_000),
            Some(152_000),
            Some("gemini 4 argon"),
        );
        assert_eq!(
            res_db.effective,
            EffectiveCeiling::Scaled {
                ceiling_tokens: 840_000,
                source: CeilingSource::DbGenMetadata,
            }
        );

        // 2. Family mismatch rejects DbGenMetadata and falls through to SessionCheckpoint / Unknown
        let res_mismatch = resolve_compaction_ceiling_with_db(
            1_048_576,
            &CompactionInputs {
                db_ceiling: Some(argon_db),
                ..Default::default()
            },
            Some(152_000),
            Some(152_000),
            Some("gemini 3 flash"),
        );
        assert_eq!(res_mismatch.effective, EffectiveCeiling::Unknown);

        // 3. ExecutorMetadata with unknown DB model family is still accepted
        let res_exec = resolve_compaction_ceiling_with_db(
            1_048_576,
            &CompactionInputs {
                db_ceiling: Some(exec_db),
                ..Default::default()
            },
            Some(152_000),
            Some(152_000),
            Some("gemini 4 argon"),
        );
        assert_eq!(
            res_exec.effective,
            EffectiveCeiling::Scaled {
                ceiling_tokens: 512_000,
                source: CeilingSource::DbGenMetadata,
            }
        );

        // 4. PhysWindowSmall (< 900k) beats LearnedModelCache
        let res_small = resolve_compaction_ceiling_with_db(
            524_288,
            &CompactionInputs {
                learned: Some(LearnedCompactionState::Ceiling(256_000)),
                ..Default::default()
            },
            Some(152_000),
            Some(152_000),
            Some("gemini 4 argon"),
        );
        assert_eq!(res_small.effective, EffectiveCeiling::PhysicalWindow);

        // 5. Out-of-range DbGenMetadata (< 32k or > phys_win) is rejected
        let tiny_db = DbCeilingInfo::new(16_000, DbCeilingSource::GenMetadata, None, None);
        let huge_db = DbCeilingInfo::new(2_000_000, DbCeilingSource::GenMetadata, None, None);
        for bad in [tiny_db, huge_db] {
            let res_bad = resolve_compaction_ceiling_with_db(
                1_048_576,
                &CompactionInputs {
                    db_ceiling: Some(bad),
                    ..Default::default()
                },
                Some(152_000),
                Some(152_000),
                Some("gemini 4 argon"),
            );
            assert_eq!(res_bad.effective, EffectiveCeiling::Unknown);
        }

        // 6. `ceiling_known` is false ONLY for EffectiveCeiling::Unknown
        let cold_stdin = AgyStdinData {
            model: Some(ModelStdin {
                id: Some("Gemini 4 Argon".to_string()),
                display_name: Some("Gemini 4 Argon".to_string()),
                effort: None,
            }),
            context_window: Some(ContextWindowStdin {
                context_window_size: Some(1_048_576),
                used_percentage: Some(14.5),
                current_usage: Some(CurrentUsageStdin {
                    input_tokens: Some(152_000),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let unknown_metrics = extract_context_metrics_scaled(
            Some(&cold_stdin),
            ContextScaleMode::Compaction,
            CompactionInputs::default(),
        );
        assert!(!unknown_metrics.ceiling_known);
        assert_eq!(unknown_metrics.window_size, None);
        assert_eq!(unknown_metrics.used_tokens, Some(152_000));

        let window_mode_metrics = extract_context_metrics_scaled(
            Some(&cold_stdin),
            ContextScaleMode::Window,
            CompactionInputs::default(),
        );
        assert!(window_mode_metrics.ceiling_known);
        assert_eq!(window_mode_metrics.window_size, Some(1_048_576));
    }

    #[test]
    fn shadow_model_filter_matches_expected_tokens_only() {
        for matched in [
            "gemini-2.5-flash-lite",
            "GEMINI_FLASH_LITE",
            "nano-banana",
            "gemini-2.5-flash-image",
            "image-generator",
            "M50",
            "m50",
            "MODEL_PLACEHOLDER_M50",
            "m50-preview",
            "am50-m50",
        ] {
            assert!(
                is_shadow_or_utility_model(matched),
                "expected match: {matched}"
            );
        }
        for unmatched in [
            "",
            "m500",
            "M500",
            "gemini-m500",
            "am50",
            "v2m50",
            "ModelAm50",
            "Gemini 4 Argon",
            "gemini-3-flash",
            "claude-sonnet-4.6",
        ] {
            assert!(
                !is_shadow_or_utility_model(unmatched),
                "unexpected match: {unmatched}"
            );
        }
    }

    #[test]
    fn gemini_4_and_claude_5_5_formatting_compaction_and_quota_passthrough() {
        let mk_model = |id: &str, display: &str, effort: Option<&str>| ModelStdin {
            id: Some(id.to_string()),
            display_name: Some(display.to_string()),
            effort: effort.map(str::to_string),
        };
        assert_eq!(
            format_model_name(
                Some(&mk_model(
                    "gemini-4-neon-xhigh",
                    "Gemini 4 Neon (XHigh)",
                    Some("xhigh")
                )),
                ModelFormatMode::Short,
                EffortFormatMode::Text
            ),
            "Neon · xhigh"
        );
        assert_eq!(
            format_model_name(
                Some(&mk_model(
                    "gemini-4-carbon-high",
                    "Gemini 4 Carbon (High)",
                    Some("high")
                )),
                ModelFormatMode::Short,
                EffortFormatMode::Hidden
            ),
            "Carbon"
        );
        assert_eq!(
            format_model_name(
                Some(&mk_model(
                    "gemini-4-hydrogen-medium",
                    "Gemini 4 Hydrogen (Medium)",
                    Some("medium")
                )),
                ModelFormatMode::Compact,
                EffortFormatMode::Text
            ),
            "Gemini 4 Hydrogen · med"
        );
        assert_eq!(
            format_model_name(
                Some(&mk_model(
                    "claude-opus-5-5-high",
                    "Claude Opus 5.5 (High)",
                    Some("high")
                )),
                ModelFormatMode::Short,
                EffortFormatMode::Text
            ),
            "Opus 5.5 · high"
        );
        assert_eq!(
            format_model_name(
                Some(&mk_model(
                    "claude-sonnet-5-5-medium",
                    "Claude Sonnet 5.5",
                    Some("medium")
                )),
                ModelFormatMode::Short,
                EffortFormatMode::Hidden
            ),
            "Sonnet 5.5"
        );

        assert_eq!(
            normalize_model_family_str("gemini-4-neon-xhigh", Some("xhigh")),
            Some("gemini-4-neon".to_string())
        );
        assert_eq!(
            normalize_model_family_str("gemini-pro-orchestrator-high", Some("high")),
            Some("gemini-pro-orchestrator".to_string())
        );
        assert_eq!(
            canonical_db_family_key("claude-sonnet-5-5-medium"),
            canonical_db_family_key("Claude Sonnet 5.5 (Medium)")
        );

        let now_ms = 1_794_218_400_000i64;
        let future = "2099-01-01T00:00:00Z".to_string();
        let mut quota = BTreeMap::new();
        for (id, frac) in [
            ("gemini-5h", Some(0.80)),
            ("gemini-weekly", Some(0.70)),
            ("3p-5h", Some(0.90)),
            ("3p-weekly", Some(0.85)),
            ("gemini-argon-wtu-weekly", Some(0.60)),
            ("gemini-neon-weekly", Some(0.55)),
            ("gemini-carbon-weekly", Some(0.50)),
            ("gemini-hydrogen-wtu-weekly", Some(0.45)),
            ("gemini-orchestrator-weekly", Some(0.40)),
        ] {
            quota.insert(
                id.to_string(),
                RawQuotaBucket {
                    remaining_fraction: frac,
                    reset_time: Some(future.clone()),
                    ..Default::default()
                },
            );
        }
        quota.insert(
            "ai-credits".to_string(),
            RawQuotaBucket {
                remaining_amount: Some(25.0),
                reset_time: Some(future),
                ..Default::default()
            },
        );

        let ids_for = |id: &str, display: &str| -> Vec<String> {
            let stdin = AgyStdinData {
                model: Some(mk_model(id, display, None)),
                quota: Some(quota.clone()),
                ..Default::default()
            };
            extract_quota_buckets_reconciled(Some(&stdin), None, now_ms)
                .into_iter()
                .map(|b| b.id)
                .collect()
        };

        assert_eq!(
            ids_for("gemini-4-neon-xhigh", "Gemini 4 Neon (XHigh)"),
            vec![
                "gemini-5h",
                "gemini-weekly",
                "gemini-neon-weekly",
                "ai-credits"
            ]
        );
        assert_eq!(
            ids_for("gemini-4-carbon-high", "Gemini 4 Carbon (High)"),
            vec![
                "gemini-5h",
                "gemini-weekly",
                "gemini-carbon-weekly",
                "ai-credits"
            ]
        );
        assert_eq!(
            ids_for("gemini-4-hydrogen-medium", "Gemini 4 Hydrogen (Medium)"),
            vec![
                "gemini-5h",
                "gemini-weekly",
                "gemini-hydrogen-wtu-weekly",
                "ai-credits"
            ]
        );
        assert_eq!(
            ids_for(
                "gemini-pro-orchestrator-high",
                "Gemini Pro Orchestrator (High)"
            ),
            vec![
                "gemini-5h",
                "gemini-weekly",
                "gemini-orchestrator-weekly",
                "ai-credits"
            ]
        );
        assert_eq!(
            ids_for("gemini-4-argon-high", "Gemini 4 Argon (High)"),
            vec!["gemini-argon-wtu-weekly", "ai-credits"]
        );
        assert_eq!(
            ids_for("claude-opus-5-5-high", "Claude Opus 5.5 (High)"),
            vec!["3p-5h", "3p-weekly", "ai-credits"]
        );

        // Mixed-case canonical keys must NOT duplicate or leak, and family buckets outrank `credits`
        let mut mixed_quota = quota;
        mixed_quota.insert(
            "Gemini-Argon-Wtu-Weekly".to_string(),
            RawQuotaBucket {
                remaining_fraction: Some(0.25),
                reset_time: Some("2099-01-01T00:00:00Z".to_string()),
                ..Default::default()
            },
        );
        mixed_quota.insert(
            "3P-Weekly".to_string(),
            RawQuotaBucket {
                remaining_fraction: Some(0.35),
                reset_time: Some("2099-01-01T00:00:00Z".to_string()),
                ..Default::default()
            },
        );
        let mixed_stdin = AgyStdinData {
            model: Some(mk_model(
                "gemini-4-neon-xhigh",
                "Gemini 4 Neon (XHigh)",
                None,
            )),
            quota: Some(mixed_quota),
            ..Default::default()
        };
        let mixed_ids: Vec<String> =
            extract_quota_buckets_reconciled(Some(&mixed_stdin), None, now_ms)
                .into_iter()
                .map(|b| b.id)
                .collect();
        assert_eq!(
            mixed_ids,
            vec![
                "gemini-5h",
                "gemini-weekly",
                "gemini-neon-weekly",
                "ai-credits"
            ]
        );
    }

    #[test]
    fn db_ceiling_matches_slug_and_display_name_while_rejecting_qualifiers_and_switches() {
        let slug_only_db = DbCeilingInfo::new(
            840_000,
            DbCeilingSource::GenMetadata,
            Some("claude-opus-5-5-medium".to_string()),
            None,
        );
        assert!(db_ceiling_matches_family(
            &slug_only_db,
            Some("claude opus 5.5")
        ));
        assert!(!db_ceiling_matches_family(
            &slug_only_db,
            Some("claude sonnet 4.6")
        ));

        let display_db = DbCeilingInfo::new(
            512_000,
            DbCeilingSource::GenMetadata,
            Some("gemini-4-hydrogen".to_string()),
            Some("gemini 4 hydrogen".to_string()),
        );
        assert!(db_ceiling_matches_family(
            &display_db,
            Some("gemini-4-hydrogen-x-high")
        ));
        assert!(!db_ceiling_matches_family(
            &display_db,
            Some("gemini 4 argon (1m)")
        ));

        let qual_db = DbCeilingInfo::new(
            1_048_576,
            DbCeilingSource::GenMetadata,
            Some("Gemini 4 Argon (1M)".to_string()),
            Some("gemini 4 argon (1m)".to_string()),
        );
        assert!(!db_ceiling_matches_family(&qual_db, Some("gemini 4 argon")));
    }
}
