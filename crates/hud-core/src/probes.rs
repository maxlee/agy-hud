use crate::ansi::sanitize_text;
use crate::transcript::{
    MAX_VALID_TPS_X10, MIN_VALID_TPS_X10, adjust_coarse_dt_ms, blend_tps_ewma_x10, compute_tps_x10,
    estimate_ttft_overhead_ms, merge_min_max_pair_x10, parse_iso_ms, update_min_max_x10,
    update_speed_anchors,
};
use crate::types::{
    ActiveSpeedSample, AgyStdinData, CachedQuotaSnapshot, CostLedgerSnapshot, CostTotals,
    MemoryProbeInfo, ProbeAnswers, RawQuotaBucket, SessionCostEntry, SpeedCacheSnapshot,
    TranscriptSummary, UpdateCacheSnapshot, VcsProbeInfo, VcsType,
};
use serde_json::Value;
use std::collections::BTreeMap;

/// Normalizes a Git remote origin URL (`https://github.com/owner/repo(.git)` or `git@github.com:owner/repo(.git)`)
/// strictly to `https://github.com/<owner>/<repo>`, rejecting embedded credentials, non-GitHub hosts, or path traversal.
pub fn normalize_github_repo_url(raw: &str) -> Option<String> {
    let clean = sanitize_text(raw);
    let trimmed = clean.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let rest = if lower.starts_with("https://github.com/") {
        &trimmed["https://github.com/".len()..]
    } else if lower.starts_with("http://github.com/") {
        &trimmed["http://github.com/".len()..]
    } else if lower.starts_with("git@github.com:") {
        &trimmed["git@github.com:".len()..]
    } else if lower.starts_with("ssh://git@github.com/") {
        &trimmed["ssh://git@github.com/".len()..]
    } else {
        return None;
    };

    let without_slash = rest.trim_end_matches('/');
    let without_git = without_slash
        .strip_suffix(".git")
        .or_else(|| without_slash.strip_suffix(".GIT"))
        .unwrap_or(without_slash);
    let mut parts = without_git.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let valid_seg = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    if !valid_seg(owner) || !valid_seg(repo) {
        return None;
    }
    Some(format!("https://github.com/{owner}/{repo}"))
}

/// Merges `stdin.vcs` metadata from `agy` CLI with optional rich VCS probe JSON from `hud-cli`.
pub fn parse_vcs_info(
    stdin: Option<&AgyStdinData>,
    answers: &ProbeAnswers,
) -> Option<VcsProbeInfo> {
    if let Some(raw_json) = answers.vcs.as_deref()
        && let Ok(mut parsed) = serde_json::from_str::<VcsProbeInfo>(raw_json)
    {
        parsed.branch = sanitize_text(&parsed.branch);
        parsed.repo_url = parsed
            .repo_url
            .as_deref()
            .and_then(normalize_github_repo_url);
        if !parsed.branch.is_empty() {
            return Some(parsed);
        }
    }

    let sv = stdin.and_then(|s| s.vcs.as_ref())?;
    let branch_raw = sv
        .branch
        .as_deref()
        .filter(|b| !b.trim().is_empty())
        .or_else(|| sv.client.as_deref().filter(|c| !c.trim().is_empty()))?;
    let branch = sanitize_text(branch_raw);
    if branch.is_empty() {
        return None;
    }

    Some(VcsProbeInfo {
        vcs_type: VcsType::from_loose(sv.vcs_type.as_deref().unwrap_or("git")),
        branch,
        dirty: sv.dirty.unwrap_or(false),
        ahead: 0,
        behind: 0,
        modified: 0,
        added: 0,
        deleted: 0,
        untracked: 0,
        conflicts: 0,
        repo_url: None,
    })
}

pub fn parse_memory_info(answers: &ProbeAnswers) -> Option<MemoryProbeInfo> {
    let raw_json = answers.memory.as_deref()?;
    let parsed = serde_json::from_str::<MemoryProbeInfo>(raw_json).ok()?;
    if parsed.total_bytes == 0 {
        return None;
    }
    let used_percent = (((parsed.used_bytes as f64) / (parsed.total_bytes as f64)) * 100.0)
        .round()
        .clamp(0.0, 100.0) as u64;
    Some(MemoryProbeInfo {
        total_bytes: parsed.total_bytes,
        used_bytes: parsed.used_bytes,
        used_percent,
    })
}

/// Parses `git status --porcelain=v2 --branch` output into `VcsProbeInfo`.
/// If HEAD is `(detached)`, `detached_rev` supplies the short SHA fallback.
pub fn parse_git_porcelain_v2(stdout: &str, detached_rev: Option<&str>) -> Option<VcsProbeInfo> {
    let mut branch = String::new();
    let mut ahead = 0u64;
    let mut behind = 0u64;
    let mut modified = 0u64;
    let mut added = 0u64;
    let mut deleted = 0u64;
    let mut untracked = 0u64;
    let mut conflicts = 0u64;

    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some(head) = line.strip_prefix("# branch.head ") {
            branch = head.trim().to_string();
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            for tok in ab.split_whitespace() {
                if let Some(a) = tok.strip_prefix('+') {
                    ahead = a.parse::<u64>().unwrap_or(0);
                } else if let Some(b) = tok.strip_prefix('-') {
                    behind = b.parse::<u64>().unwrap_or(0);
                }
            }
        } else if line.starts_with("u ") {
            conflicts += 1;
        } else if line.starts_with("? ") {
            untracked += 1;
        } else if (line.starts_with("1 ") || line.starts_with("2 ")) && line.len() >= 4 {
            let xy = &line[2..4];
            if xy.contains('A') {
                added += 1;
            } else if xy.contains('D') {
                deleted += 1;
            } else {
                modified += 1;
            }
        }
    }

    if branch == "(detached)" {
        branch = match detached_rev.map(str::trim).filter(|s| !s.is_empty()) {
            Some(rev) => format!("@{rev}"),
            None => "detached".to_string(),
        };
    }
    let branch = sanitize_text(&branch);
    if branch.is_empty() {
        return None;
    }

    let dirty = modified + added + deleted + untracked + conflicts > 0;
    Some(VcsProbeInfo {
        vcs_type: VcsType::Git,
        branch,
        dirty,
        ahead,
        behind,
        modified,
        added,
        deleted,
        untracked,
        conflicts,
        repo_url: None,
    })
}

/// Parses strictly read-only Jujutsu (`jj log -r @`) delimiter-framed output.
pub fn parse_jj_probe_output(stdout: &str) -> Option<VcsProbeInfo> {
    let trimmed = stdout
        .strip_suffix("\r\n")
        .or_else(|| stdout.strip_suffix('\n'))
        .unwrap_or(stdout);
    if trimmed.is_empty() || trimmed.contains('\n') || trimmed.contains('\r') {
        return None;
    }

    let parts: Vec<&str> = trimmed.split('\x1f').collect();
    if parts.len() != 4 {
        return None;
    }
    let change_id = parts[0].trim();
    let raw_bookmarks = parts[1];
    let non_empty_flag = parts[2];
    let conflict_flag = parts[3];

    if change_id.is_empty()
        || !matches!(non_empty_flag, "0" | "1")
        || !matches!(conflict_flag, "0" | "1")
    {
        return None;
    }

    let bookmarks: Vec<&str> = raw_bookmarks
        .split('\x1e')
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .collect();

    let raw_branch = if bookmarks.len() > 1 {
        format!("{}+{}", bookmarks[0], bookmarks.len() - 1)
    } else if bookmarks.len() == 1 {
        bookmarks[0].to_string()
    } else {
        format!("@{change_id}")
    };

    Some(VcsProbeInfo {
        vcs_type: VcsType::Jj,
        branch: sanitize_text(&raw_branch),
        dirty: non_empty_flag == "1",
        ahead: 0,
        behind: 0,
        modified: 0,
        added: 0,
        deleted: 0,
        untracked: 0,
        conflicts: u64::from(conflict_flag == "1"),
        repo_url: None,
    })
}

/// Parses Mercurial (`hg branch` + `hg status -mardu`) output.
pub fn parse_hg_probe_output(branch_out: &str, status_out: Option<&str>) -> Option<VcsProbeInfo> {
    let branch_line = branch_out.trim().lines().next()?.trim();
    let branch = sanitize_text(branch_line);
    if branch.is_empty() {
        return None;
    }

    let mut modified = 0u64;
    let mut added = 0u64;
    let mut deleted = 0u64;
    let mut untracked = 0u64;

    if let Some(st) = status_out {
        for raw_line in st.lines() {
            let bytes = raw_line.as_bytes();
            if bytes.len() < 2 || bytes[1] != b' ' {
                continue;
            }
            match bytes[0] {
                b'M' => modified += 1,
                b'A' => added += 1,
                b'R' | b'!' => deleted += 1,
                b'?' => untracked += 1,
                _ => {}
            }
        }
    }

    let dirty = modified + added + deleted + untracked > 0;
    Some(VcsProbeInfo {
        vcs_type: VcsType::Hg,
        branch,
        dirty,
        ahead: 0,
        behind: 0,
        modified,
        added,
        deleted,
        untracked,
        conflicts: 0,
        repo_url: None,
    })
}

/// Parses raw `.git/HEAD` text (`ref: refs/heads/...` or detached SHA) without spawning `git`.
pub fn parse_git_head_text(head_raw: &str) -> Option<VcsProbeInfo> {
    let trimmed = head_raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let raw_branch = if let Some(after_ref) = trimmed.strip_prefix("ref:") {
        let r = after_ref.trim();
        if let Some(b) = r.strip_prefix("refs/heads/") {
            b.to_string()
        } else {
            r.rsplit('/').next().unwrap_or(r).to_string()
        }
    } else if trimmed.len() >= 7 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!("@{}", &trimmed[..7])
    } else {
        trimmed.to_string()
    };

    let branch = sanitize_text(&raw_branch);
    if branch.is_empty() {
        return None;
    }
    Some(VcsProbeInfo {
        vcs_type: VcsType::Git,
        branch,
        dirty: false,
        ahead: 0,
        behind: 0,
        modified: 0,
        added: 0,
        deleted: 0,
        untracked: 0,
        conflicts: 0,
        repo_url: None,
    })
}

fn is_valid_bucket_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 40 {
        return false;
    }
    if !(bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit()) {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// Parses `agy -p /usage --output-format json` stdout into sanitized quota buckets.
pub fn parse_usage_command_output(stdout: &str) -> Option<BTreeMap<String, RawQuotaBucket>> {
    let parsed: Value = serde_json::from_str(stdout.trim()).ok()?;
    let groups = parsed
        .get("command")
        .and_then(|c| c.get("data"))
        .and_then(|d| d.get("groups"))
        .and_then(Value::as_array)?;

    let mut quota = BTreeMap::new();
    for group in groups {
        let Some(buckets) = group.get("buckets").and_then(Value::as_array) else {
            continue;
        };
        for item in buckets {
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !is_valid_bucket_id(id) {
                continue;
            }
            let Some(fraction) = item.get("remaining_fraction").and_then(Value::as_f64) else {
                continue;
            };
            if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
                continue;
            }
            let Some(reset_str) = item.get("reset_time").and_then(Value::as_str) else {
                continue;
            };
            if parse_iso_ms(reset_str).is_none() {
                continue;
            }
            quota.insert(
                id.to_string(),
                RawQuotaBucket {
                    remaining_fraction: Some(fraction),
                    remaining_amount: None,
                    reset_time: Some(reset_str.to_string()),
                    reset_in_seconds: None,
                    disabled: None,
                },
            );
        }
    }

    (!quota.is_empty()).then_some(quota)
}

pub fn parse_cached_quota_info(answers: &ProbeAnswers) -> Option<CachedQuotaSnapshot> {
    let raw_json = answers.quota_cache.as_deref()?;
    serde_json::from_str::<CachedQuotaSnapshot>(raw_json).ok()
}

fn parse_semver_core(raw: &str, allow_prerelease_strip: bool) -> Option<(u64, u64, u64)> {
    let trimmed = raw.trim();
    let without_v = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    let without_build = without_v.split('+').next().unwrap_or(without_v);
    let core = if allow_prerelease_strip {
        without_build.split('-').next().unwrap_or(without_build)
    } else if without_build.contains('-') {
        return None;
    } else {
        without_build
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Normalizes a stable release tag such as `"v1.2.0"` or `"1.2.0"` into `"1.2.0"`,
/// rejecting pre-release tags (`-alpha`, `-rc.1`) and non-semver strings.
pub fn normalize_semver_tag(raw: &str) -> Option<String> {
    let (maj, min, pat) = parse_semver_core(raw, false)?;
    Some(format!("{maj}.{min}.{pat}"))
}

/// Returns `true` strictly when `candidate` is a valid stable semver newer than `current`.
pub fn is_newer_semver(current: &str, candidate: &str) -> bool {
    let Some(cur) = parse_semver_core(current, true) else {
        return false;
    };
    let Some(cand) = parse_semver_core(candidate, false) else {
        return false;
    };
    cand > cur
}

/// Parses GitHub Releases API `/releases/latest` JSON into `(normalized_version, optional_html_url)`,
/// skipping drafts and pre-releases.
pub fn parse_github_latest_release(raw_json: &str) -> Option<(String, Option<String>)> {
    let val: Value = serde_json::from_str(raw_json.trim()).ok()?;
    if val.get("draft").and_then(Value::as_bool) == Some(true)
        || val.get("prerelease").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let tag = val.get("tag_name").and_then(Value::as_str)?;
    let version = normalize_semver_tag(tag)?;
    let url = val
        .get("html_url")
        .and_then(Value::as_str)
        .map(sanitize_text)
        .filter(|u| u.starts_with("https://github.com/"));
    Some((version, url))
}

pub fn parse_update_cache_info(answers: &ProbeAnswers) -> Option<UpdateCacheSnapshot> {
    let raw_json = answers.update_cache.as_deref()?;
    let mut parsed = serde_json::from_str::<UpdateCacheSnapshot>(raw_json).ok()?;
    parsed.latest_version = normalize_semver_tag(&parsed.latest_version)?;
    parsed.release_url = parsed
        .release_url
        .as_deref()
        .map(sanitize_text)
        .filter(|u| u.starts_with("https://github.com/"));
    Some(parsed)
}

pub fn parse_speed_cache_info(answers: &ProbeAnswers) -> Option<SpeedCacheSnapshot> {
    let raw_json = answers.speed_cache.as_deref()?;
    let mut parsed = serde_json::from_str::<SpeedCacheSnapshot>(raw_json).ok()?;
    parsed.session_key = sanitize_text(&parsed.session_key);
    parsed.model_key = sanitize_text(&parsed.model_key);

    let mut norm_min = None;
    let mut norm_max = None;
    merge_min_max_pair_x10(
        &mut norm_min,
        &mut norm_max,
        parsed.session_min_tps_x10,
        parsed.session_max_tps_x10,
    );
    parsed.session_min_tps_x10 = norm_min;
    parsed.session_max_tps_x10 = norm_max;

    let has_valid_tps = (MIN_VALID_TPS_X10..=MAX_VALID_TPS_X10).contains(&parsed.tps_x10);
    let has_session_range = norm_min.is_some() && norm_max.is_some();
    let is_barrier = parsed.ended_at_ms == -1 && parsed.tps_x10 == 0;
    let has_active = parsed.active.is_some() && parsed.tps_x10 == 0;

    if has_valid_tps || (parsed.tps_x10 == 0 && (is_barrier || has_active || has_session_range)) {
        Some(parsed)
    } else {
        None
    }
}

fn extract_native_stdin_tps(stdin: Option<&AgyStdinData>) -> Option<f64> {
    let extra = &stdin?.extra;
    for top_key in ["output_tokens_per_sec", "tokens_per_second"] {
        if let Some(v) = extra.get(top_key).and_then(Value::as_f64)
            && v.is_finite()
            && (0.1..=2_000.0).contains(&v)
        {
            return Some(v);
        }
    }
    if let Some(tel) = extra.get("telemetry").and_then(Value::as_object) {
        for k in [
            "output_tokens_per_sec",
            "tokens_per_second",
            "outputTokensPerSec",
        ] {
            if let Some(v) = tel.get(k).and_then(Value::as_f64)
                && v.is_finite()
                && (0.1..=2_000.0).contains(&v)
            {
                return Some(v);
            }
        }
    }
    None
}

fn extract_model_key(stdin: Option<&AgyStdinData>) -> String {
    stdin
        .and_then(|s| s.model.as_ref())
        .and_then(|m| m.id.as_deref().or(m.display_name.as_deref()))
        .map(sanitize_text)
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputSpeedDisplay {
    pub current_tps: f64,
    pub estimated: bool,
    pub min_tps: Option<f64>,
    pub max_tps: Option<f64>,
}

struct ResolvedSpeedOutput {
    current: Option<(f64, bool)>,
    session_min_x10: Option<u32>,
    session_max_x10: Option<u32>,
    next_cache: Option<SpeedCacheSnapshot>,
}

fn empty_speed_cache(
    session_key: String,
    model_key: String,
    step_index: usize,
    started_at_ms: i64,
    ended_at_ms: i64,
    session_min_tps_x10: Option<u32>,
    session_max_tps_x10: Option<u32>,
) -> SpeedCacheSnapshot {
    SpeedCacheSnapshot {
        session_key,
        model_key,
        step_index,
        output_tokens: 0,
        started_at_ms,
        ended_at_ms,
        tps_x10: 0,
        ewma_tps_x10: None,
        session_min_tps_x10,
        session_max_tps_x10,
        short_anchor: None,
        long_anchor: None,
        active: None,
    }
}

fn clear_active_snapshot(
    sc: &SpeedCacheSnapshot,
    session_min_x10: Option<u32>,
    session_max_x10: Option<u32>,
) -> Option<SpeedCacheSnapshot> {
    sc.active.is_some().then(|| SpeedCacheSnapshot {
        active: None,
        session_min_tps_x10: session_min_x10,
        session_max_tps_x10: session_max_x10,
        ..sc.clone()
    })
}

fn resolve_output_speed_inner(
    stdin: Option<&AgyStdinData>,
    transcript: &TranscriptSummary,
    speed_cache: Option<&SpeedCacheSnapshot>,
    now_ms: i64,
) -> ResolvedSpeedOutput {
    let session_key = stdin
        .and_then(|s| s.conversation_id.as_deref().or(s.session_id.as_deref()))
        .map(sanitize_text)
        .unwrap_or_default();
    let model_key = extract_model_key(stdin);

    let same_session_cache = (!session_key.is_empty())
        .then_some(speed_cache)
        .flatten()
        .filter(|sc| sc.session_key == session_key);

    let model_switched = same_session_cache.is_some_and(|sc| {
        !sc.model_key.is_empty() && !model_key.is_empty() && sc.model_key != model_key
    });

    // Session min/max span model switches within the same session; only completed steps and
    // native exact stdin tps update them.
    let mut session_min_x10 = None;
    let mut session_max_x10 = None;
    merge_min_max_pair_x10(
        &mut session_min_x10,
        &mut session_max_x10,
        transcript.session_min_tps_x10,
        transcript.session_max_tps_x10,
    );
    if let Some(sc) = same_session_cache {
        if sc.session_min_tps_x10.is_none()
            && sc.session_max_tps_x10.is_none()
            && sc.ended_at_ms > 0
            && (MIN_VALID_TPS_X10..=MAX_VALID_TPS_X10).contains(&sc.tps_x10)
        {
            update_min_max_x10(&mut session_min_x10, &mut session_max_x10, sc.tps_x10);
        } else {
            merge_min_max_pair_x10(
                &mut session_min_x10,
                &mut session_max_x10,
                sc.session_min_tps_x10,
                sc.session_max_tps_x10,
            );
        }
    }

    if let Some(exact) = extract_native_stdin_tps(stdin) {
        let exact_x10 = (exact * 10.0).round() as u32;
        update_min_max_x10(&mut session_min_x10, &mut session_max_x10, exact_x10);
        let next_cache = (!session_key.is_empty())
            .then(|| {
                let mut cand = match same_session_cache {
                    Some(sc) if !model_switched => sc.clone(),
                    Some(sc) => {
                        let barrier_step = transcript
                            .latest_planner_step
                            .as_ref()
                            .map_or(sc.step_index, |lp| lp.step_index.max(sc.step_index));
                        empty_speed_cache(
                            session_key.clone(),
                            model_key.clone(),
                            barrier_step,
                            now_ms,
                            -1,
                            session_min_x10,
                            session_max_x10,
                        )
                    }
                    None => empty_speed_cache(
                        session_key.clone(),
                        model_key.clone(),
                        0,
                        0,
                        0,
                        session_min_x10,
                        session_max_x10,
                    ),
                };
                if !model_key.is_empty() {
                    cand.model_key = model_key.clone();
                }
                cand.session_min_tps_x10 = session_min_x10;
                cand.session_max_tps_x10 = session_max_x10;
                cand
            })
            .filter(|cand| speed_cache != Some(cand));
        return ResolvedSpeedOutput {
            current: Some((exact, false)),
            session_min_x10,
            session_max_x10,
            next_cache,
        };
    }

    if speed_cache.is_none()
        && let Some(x10) = transcript
            .resolved_tps_x10
            .filter(|&v| (10..=5_000).contains(&v))
    {
        return ResolvedSpeedOutput {
            current: Some((x10 as f64 / 10.0, true)),
            session_min_x10,
            session_max_x10,
            next_cache: None,
        };
    }

    let agent_state = stdin
        .and_then(|s| s.agent_state.as_deref())
        .unwrap_or("")
        .trim();
    let is_idle = agent_state.eq_ignore_ascii_case("idle");
    let is_tool_use = agent_state.eq_ignore_ascii_case("tool_use");
    let is_streaming_state = agent_state.eq_ignore_ascii_case("streaming");
    let is_thinking_state =
        agent_state.eq_ignore_ascii_case("thinking") || agent_state.eq_ignore_ascii_case("working");
    let confirm_pending = stdin
        .and_then(|s| s.tool_confirmation_pending)
        .unwrap_or(false);

    if model_switched && let Some(old_sc) = same_session_cache {
        let lp_needs_fresh_eval = transcript
            .latest_planner_step
            .as_ref()
            .is_some_and(|lp| !lp.is_done || lp.step_index > old_sc.step_index);
        if !lp_needs_fresh_eval && !is_streaming_state && !is_thinking_state {
            let barrier_step = transcript
                .latest_planner_step
                .as_ref()
                .map_or(old_sc.step_index, |lp| lp.step_index.max(old_sc.step_index));
            let reset = empty_speed_cache(
                session_key,
                model_key,
                barrier_step,
                now_ms,
                -1,
                session_min_x10,
                session_max_x10,
            );
            return ResolvedSpeedOutput {
                current: None,
                session_min_x10,
                session_max_x10,
                next_cache: Some(reset),
            };
        }
    }

    let matching_cache = same_session_cache.filter(|_| !model_switched);
    let has_model_barrier =
        model_switched || matching_cache.is_some_and(|sc| sc.ended_at_ms == -1 && sc.tps_x10 == 0);

    let prev_ewma = matching_cache
        .and_then(|sc| {
            sc.ewma_tps_x10
                .or_else(|| (sc.ended_at_ms > 0).then_some(sc.tps_x10))
                .filter(|&v| (10..=5_000).contains(&v))
        })
        .or_else(|| {
            (!has_model_barrier)
                .then_some(transcript.completed_step_tps_x10)
                .flatten()
                .filter(|&v| (10..=5_000).contains(&v))
        });

    let lp_in_flight = transcript
        .latest_planner_step
        .as_ref()
        .filter(|lp| !lp.is_done);

    let completed_lp_already_handled = transcript.latest_planner_step.as_ref().is_none_or(|lp| {
        !lp.is_done
            || lp.has_trailing_user_input
            || matching_cache.is_some_and(|sc| {
                (sc.ended_at_ms > 0 && sc.step_index == lp.step_index && sc.tps_x10 > 0)
                    || (sc.ended_at_ms == -1 && lp.step_index <= sc.step_index)
            })
    });

    // 1. Live 300ms tick tracking while model is thinking or streaming
    if lp_in_flight.is_some()
        || ((is_streaming_state || is_thinking_state) && completed_lp_already_handled)
    {
        let mut active = matching_cache
            .and_then(|sc| sc.active.clone())
            .filter(|a| {
                now_ms >= a.turn_started_at_ms
                    && now_ms - a.turn_started_at_ms <= 300_000
                    && match (a.step_index, lp_in_flight.map(|l| l.step_index)) {
                        (Some(a_idx), Some(l_idx)) => a_idx == l_idx,
                        _ => true,
                    }
            })
            .unwrap_or_else(|| ActiveSpeedSample {
                step_index: lp_in_flight.map(|l| l.step_index),
                turn_started_at_ms: lp_in_flight
                    .map(|l| l.started_at_ms.min(now_ms))
                    .unwrap_or(now_ms),
                decode_started_at_ms: None,
                first_est_tokens: 0,
                peak_est_tokens: 0,
                live_tps_x10: None,
                confirm_seen: false,
            });

        if let Some(lp) = lp_in_flight {
            active.step_index = Some(lp.step_index);
        }
        active.confirm_seen |= confirm_pending;

        let est_tokens = lp_in_flight.map_or(0, |l| l.estimated_tokens);
        if active.decode_started_at_ms.is_none() && (is_streaming_state || est_tokens > 0) {
            active.decode_started_at_ms = Some(now_ms);
            active.first_est_tokens = est_tokens;
        }
        active.peak_est_tokens = active.peak_est_tokens.max(est_tokens);

        if let Some(dec_ms) = active.decode_started_at_ms
            && !active.confirm_seen
        {
            let delta_tok = active
                .peak_est_tokens
                .saturating_sub(active.first_est_tokens);
            let stream_dt = now_ms - dec_ms;
            if delta_tok >= 16
                && (700..=180_000).contains(&stream_dt)
                && let Some(live_x10) = compute_tps_x10(delta_tok, stream_dt)
            {
                active.live_tps_x10 = Some(match active.live_tps_x10 {
                    Some(prev) => ((prev as u64 * 4 + live_x10 as u64 * 6) / 10) as u32,
                    None => live_x10,
                });
            }
        }

        let display_x10 = active.live_tps_x10.or(prev_ewma);
        let to_persist = (!session_key.is_empty())
            .then(|| {
                let base = matching_cache.cloned().unwrap_or_else(|| {
                    empty_speed_cache(
                        session_key.clone(),
                        model_key.clone(),
                        model_switched
                            .then(|| same_session_cache.map(|s| s.step_index))
                            .flatten()
                            .unwrap_or(0),
                        0,
                        if model_switched { -1 } else { 0 },
                        session_min_x10,
                        session_max_x10,
                    )
                });
                SpeedCacheSnapshot {
                    session_key: session_key.clone(),
                    model_key: if model_key.is_empty() {
                        base.model_key
                    } else {
                        model_key.clone()
                    },
                    session_min_tps_x10: session_min_x10,
                    session_max_tps_x10: session_max_x10,
                    active: Some(active),
                    ..base
                }
            })
            .filter(|cand| speed_cache != Some(cand));

        return ResolvedSpeedOutput {
            current: display_x10.map(|x| (x as f64 / 10.0, true)),
            session_min_x10,
            session_max_x10,
            next_cache: to_persist,
        };
    }

    // 2. Completed planner step resolution & locking
    if let Some(ref lp) = transcript.latest_planner_step
        && lp.is_done
    {
        if let Some(sc) = matching_cache {
            if sc.ended_at_ms == -1 && lp.step_index <= sc.step_index {
                return ResolvedSpeedOutput {
                    current: None,
                    session_min_x10,
                    session_max_x10,
                    next_cache: clear_active_snapshot(sc, session_min_x10, session_max_x10),
                };
            }
            if sc.ended_at_ms > 0
                && sc.step_index == lp.step_index
                && (10..=5_000).contains(&sc.tps_x10)
            {
                let shown = sc
                    .ewma_tps_x10
                    .filter(|&v| (10..=5_000).contains(&v))
                    .unwrap_or(sc.tps_x10);
                return ResolvedSpeedOutput {
                    current: Some((shown as f64 / 10.0, true)),
                    session_min_x10,
                    session_max_x10,
                    next_cache: clear_active_snapshot(sc, session_min_x10, session_max_x10),
                };
            }
        }

        let matched_active = matching_cache
            .and_then(|sc| sc.active.as_ref())
            .filter(|a| {
                a.step_index == Some(lp.step_index)
                    || (a.step_index.is_none()
                        && (lp.started_at_ms - a.turn_started_at_ms).abs() <= 15_000
                        && now_ms >= lp.started_at_ms)
            });

        let mut short_anchor = matching_cache.and_then(|sc| sc.short_anchor).or_else(|| {
            (!has_model_barrier)
                .then_some(transcript.short_anchor)
                .flatten()
        });
        let mut long_anchor = matching_cache.and_then(|sc| sc.long_anchor).or_else(|| {
            (!has_model_barrier)
                .then_some(transcript.long_anchor)
                .flatten()
        });

        let step_confirm_contaminated =
            confirm_pending || matched_active.is_some_and(|a| a.confirm_seen);

        if !step_confirm_contaminated {
            // 2a. TTFT-stripped millisecond decode window from live 300ms ticks
            let live_measured = matched_active.and_then(|active| {
                let end_ms = match lp.ended_at_ms {
                    Some(gen_end) => now_ms.min(gen_end + 999),
                    None => now_ms,
                };
                if let Some(dec_ms) = active.decode_started_at_ms {
                    let lead_in = if active.first_est_tokens > 4 {
                        150.min((dec_ms - active.turn_started_at_ms).max(0))
                    } else {
                        0
                    };
                    let decode_dt = (end_ms - dec_ms) + lead_in;
                    if let Some(x10) = compute_tps_x10(lp.output_tokens, decode_dt) {
                        return Some((x10, end_ms, true));
                    }
                }
                // Sub-tick fast Flash burst fallback using turn_started_at_ms
                let turn_dt = end_ms - active.turn_started_at_ms;
                compute_tps_x10(lp.output_tokens, turn_dt).map(|x10| (x10, end_ms, false))
            });

            // 2b. Coarse transcript / idle-lock timing with 2-point TTFT calibration
            let resolved_sample = live_measured.or_else(|| {
                let end_ms = if let Some(gen_end) = lp.ended_at_ms {
                    Some(gen_end)
                } else if !lp.has_trailing_user_input && (is_idle || is_tool_use) {
                    let raw_dt = now_ms - lp.started_at_ms;
                    let raw_tps = if raw_dt > 0 {
                        (lp.output_tokens as f64) / ((raw_dt as f64) / 1000.0)
                    } else {
                        0.0
                    };
                    // Guard against late/cold idle renders minutes after generation finished
                    if matched_active.is_none() && raw_dt > 45_000 && raw_tps < 12.0 {
                        None
                    } else {
                        Some(now_ms)
                    }
                } else {
                    None
                }?;

                let raw_dt = end_ms - lp.started_at_ms;
                update_speed_anchors(
                    &mut short_anchor,
                    &mut long_anchor,
                    lp.output_tokens,
                    raw_dt,
                );
                let overhead_ms = estimate_ttft_overhead_ms(short_anchor, long_anchor);
                let adj_dt = adjust_coarse_dt_ms(raw_dt, overhead_ms);
                compute_tps_x10(lp.output_tokens, adj_dt).map(|x10| (x10, end_ms, false))
            });

            if let Some((sample_x10, end_ms, ttft_stripped)) = resolved_sample {
                update_min_max_x10(&mut session_min_x10, &mut session_max_x10, sample_x10);
                let ewma_x10 = if matching_cache.is_none()
                    && !ttft_stripped
                    && lp.output_tokens >= 1_500
                    && prev_ewma.is_some_and(|prev| sample_x10 >= prev)
                {
                    sample_x10
                } else {
                    blend_tps_ewma_x10(prev_ewma, sample_x10, lp.output_tokens, ttft_stripped)
                };

                let candidate = (!session_key.is_empty()).then(|| SpeedCacheSnapshot {
                    session_key: session_key.clone(),
                    model_key: model_key.clone(),
                    step_index: lp.step_index,
                    output_tokens: lp.output_tokens,
                    started_at_ms: lp.started_at_ms,
                    ended_at_ms: end_ms,
                    tps_x10: sample_x10,
                    ewma_tps_x10: Some(ewma_x10),
                    session_min_tps_x10: session_min_x10,
                    session_max_tps_x10: session_max_x10,
                    short_anchor,
                    long_anchor,
                    active: None,
                });
                let to_persist = candidate.filter(|cand| speed_cache != Some(cand));
                return ResolvedSpeedOutput {
                    current: Some((ewma_x10 as f64 / 10.0, true)),
                    session_min_x10,
                    session_max_x10,
                    next_cache: to_persist,
                };
            }
        }
    }

    if let Some(x10) = prev_ewma {
        let clear_active = matching_cache
            .and_then(|sc| clear_active_snapshot(sc, session_min_x10, session_max_x10));
        return ResolvedSpeedOutput {
            current: Some((x10 as f64 / 10.0, true)),
            session_min_x10,
            session_max_x10,
            next_cache: clear_active,
        };
    }

    ResolvedSpeedOutput {
        current: None,
        session_min_x10,
        session_max_x10,
        next_cache: None,
    }
}

/// Resolves output generation throughput `(tokens_per_sec, is_estimated)` and returns an updated
/// `SpeedCacheSnapshot` whenever live streaming milestones or completed `PLANNER_RESPONSE` steps
/// should be persisted to disk.
pub fn resolve_output_speed(
    stdin: Option<&AgyStdinData>,
    transcript: &TranscriptSummary,
    speed_cache: Option<&SpeedCacheSnapshot>,
    now_ms: i64,
) -> (Option<(f64, bool)>, Option<SpeedCacheSnapshot>) {
    let resolved = resolve_output_speed_inner(stdin, transcript, speed_cache, now_ms);
    (resolved.current, resolved.next_cache)
}

/// Resolves output generation throughput together with session-wide min/max bounds derived from
/// completed `PLANNER_RESPONSE` steps and native exact `stdin` telemetry.
pub fn resolve_output_speed_display(
    stdin: Option<&AgyStdinData>,
    transcript: &TranscriptSummary,
    speed_cache: Option<&SpeedCacheSnapshot>,
    now_ms: i64,
) -> Option<OutputSpeedDisplay> {
    let resolved = resolve_output_speed_inner(stdin, transcript, speed_cache, now_ms);
    resolved
        .current
        .map(|(current_tps, estimated)| OutputSpeedDisplay {
            current_tps,
            estimated,
            min_tps: resolved.session_min_x10.map(|x| x as f64 / 10.0),
            max_tps: resolved.session_max_x10.map(|x| x as f64 / 10.0),
        })
}

pub fn apply_speed_cache(
    stdin: Option<&AgyStdinData>,
    transcript: &mut TranscriptSummary,
    speed_cache: Option<&SpeedCacheSnapshot>,
    now_ms: i64,
) -> Option<SpeedCacheSnapshot> {
    let resolved = resolve_output_speed_inner(stdin, transcript, speed_cache, now_ms);
    transcript.session_min_tps_x10 = resolved.session_min_x10;
    transcript.session_max_tps_x10 = resolved.session_max_x10;
    match resolved.current {
        Some((tps, true)) => {
            let x10 = (tps * 10.0).round() as u32;
            if (10..=5_000).contains(&x10) {
                transcript.resolved_tps_x10 = Some(x10);
                transcript.completed_step_tps_x10 = Some(x10);
            }
        }
        None => {
            transcript.resolved_tps_x10 = None;
            if resolved
                .next_cache
                .as_ref()
                .is_some_and(|c| c.ended_at_ms == -1)
                || speed_cache.is_some_and(|c| c.ended_at_ms == -1)
            {
                transcript.completed_step_tps_x10 = None;
            }
        }
        Some((_, false)) => {}
    }
    resolved.next_cache
}

const COST_LEDGER_VERSION: u8 = 1;
const MAX_LEDGER_SESSIONS: usize = 128;
const STALE_LEDGER_SESSION_MS: i64 = 14 * 86_400_000;

/// Pure Howard Hinnant epoch-days-to-civil-date (`YYYY, MM, DD`) conversion.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + i64::from(m <= 2), m as u32, d as u32)
}

fn format_ymd(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn utc_day_and_monday_keys(now_ms: i64) -> (String, String) {
    let epoch_days = now_ms.div_euclid(86_400_000);
    let weekday_from_mon = (epoch_days + 3).rem_euclid(7);
    let monday_days = epoch_days - weekday_from_mon;
    (format_ymd(epoch_days), format_ymd(monday_days))
}

fn quantize_usd(v: f64) -> f64 {
    if !v.is_finite() || v <= 0.0 {
        0.0
    } else {
        (v * 100_000.0).round() / 100_000.0
    }
}

fn extract_weekly_quota_reset_ms(stdin: Option<&AgyStdinData>, now_ms: i64) -> Option<i64> {
    let quota = stdin?.quota.as_ref()?;
    for key in ["gemini-weekly", "gemini-argon-wtu-weekly", "3p-weekly"] {
        let Some(bucket) = quota.get(key) else {
            continue;
        };
        if bucket.disabled.unwrap_or(false) {
            continue;
        }
        if let Some(rt_ms) = bucket.reset_time.as_deref().and_then(parse_iso_ms)
            && rt_ms > now_ms
        {
            return Some(rt_ms);
        }
        if let Some(sec) = bucket.reset_in_seconds
            && sec.is_finite()
            && sec > 0.0
        {
            return Some(now_ms + (sec * 1000.0).round() as i64);
        }
    }
    None
}

pub fn parse_cost_ledger_info(answers: &ProbeAnswers) -> Option<CostLedgerSnapshot> {
    let raw_json = answers.cost_ledger.as_deref()?;
    let parsed = serde_json::from_str::<CostLedgerSnapshot>(raw_json).ok()?;
    (parsed.version == COST_LEDGER_VERSION && !parsed.day_key.is_empty()).then_some(parsed)
}

/// Pure watermark-based cross-session cost ledger reconciler.
/// Tracks monotonic `total_usd` per `conversation_id` / `session_id` so 300ms statusLine ticks
/// never double-count spend, and returns `None` for the snapshot output when no disk write is needed.
pub fn reconcile_cost_ledger(
    existing: Option<&CostLedgerSnapshot>,
    stdin: Option<&AgyStdinData>,
    now_ms: i64,
) -> (Option<CostTotals>, Option<CostLedgerSnapshot>) {
    if now_ms <= 0 {
        return (None, None);
    }
    let live_cost = stdin
        .and_then(|s| s.cost.as_ref())
        .and_then(|c| c.total_usd)
        .filter(|v| v.is_finite() && *v >= 0.0);

    if existing.is_none() && live_cost.is_none() {
        return (None, None);
    }

    let (day_key, calendar_week_key) = utc_day_and_monday_keys(now_ms);
    let observed_week_reset_ms = extract_weekly_quota_reset_ms(stdin, now_ms);

    let mut ledger = match existing.filter(|e| e.version == COST_LEDGER_VERSION) {
        Some(prev) => prev.clone(),
        None => CostLedgerSnapshot {
            version: COST_LEDGER_VERSION,
            day_key: day_key.clone(),
            calendar_week_key: calendar_week_key.clone(),
            week_reset_ms: observed_week_reset_ms,
            day_carry_usd: 0.0,
            week_carry_usd: 0.0,
            sessions: BTreeMap::new(),
        },
    };

    if ledger.day_key != day_key {
        ledger.day_key = day_key;
        ledger.day_carry_usd = 0.0;
        for entry in ledger.sessions.values_mut() {
            entry.day_baseline = entry.total;
        }
    }

    let week_rolled = match (ledger.week_reset_ms, observed_week_reset_ms) {
        (Some(prev_reset), Some(next_reset)) => {
            now_ms >= prev_reset || next_reset > prev_reset + 3_600_000
        }
        (Some(prev_reset), None) => now_ms >= prev_reset,
        (None, _) => ledger.calendar_week_key != calendar_week_key,
    };
    if week_rolled {
        ledger.calendar_week_key = calendar_week_key;
        ledger.week_reset_ms = observed_week_reset_ms;
        ledger.week_carry_usd = 0.0;
        for entry in ledger.sessions.values_mut() {
            entry.week_baseline = entry.total;
        }
    } else if ledger.week_reset_ms.is_none() && observed_week_reset_ms.is_some() {
        ledger.week_reset_ms = observed_week_reset_ms;
    }

    if let Some(raw_usd) = live_cost {
        let usd = quantize_usd(raw_usd);
        let session_key = stdin
            .and_then(|s| s.conversation_id.as_deref().or(s.session_id.as_deref()))
            .map(sanitize_text)
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| "_ephemeral".to_string());

        if let Some(entry) = ledger.sessions.get_mut(&session_key) {
            if usd + 1e-5 < entry.total {
                ledger.day_carry_usd = quantize_usd(
                    ledger.day_carry_usd + (entry.total - entry.day_baseline).max(0.0),
                );
                ledger.week_carry_usd = quantize_usd(
                    ledger.week_carry_usd + (entry.total - entry.week_baseline).max(0.0),
                );
                entry.day_baseline = 0.0;
                entry.week_baseline = 0.0;
                entry.total = usd;
                entry.updated_at = now_ms;
            } else if usd > entry.total + 1e-6 {
                entry.total = usd;
                entry.updated_at = now_ms;
            }
        } else {
            ledger.sessions.insert(
                session_key,
                SessionCostEntry {
                    day_baseline: 0.0,
                    week_baseline: 0.0,
                    total: usd,
                    updated_at: now_ms,
                },
            );
        }
    }

    // Prune ancient sessions (>14d) or cap session map size while folding active deltas into carry
    let stale_keys: Vec<String> = ledger
        .sessions
        .iter()
        .filter(|(_, e)| now_ms.saturating_sub(e.updated_at) > STALE_LEDGER_SESSION_MS)
        .map(|(k, _)| k.clone())
        .collect();
    for k in stale_keys {
        if let Some(removed) = ledger.sessions.remove(&k) {
            ledger.day_carry_usd = quantize_usd(
                ledger.day_carry_usd + (removed.total - removed.day_baseline).max(0.0),
            );
            ledger.week_carry_usd = quantize_usd(
                ledger.week_carry_usd + (removed.total - removed.week_baseline).max(0.0),
            );
        }
    }
    while ledger.sessions.len() > MAX_LEDGER_SESSIONS {
        let Some(oldest_key) = ledger
            .sessions
            .iter()
            .min_by_key(|(_, e)| e.updated_at)
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        if let Some(removed) = ledger.sessions.remove(&oldest_key) {
            ledger.day_carry_usd = quantize_usd(
                ledger.day_carry_usd + (removed.total - removed.day_baseline).max(0.0),
            );
            ledger.week_carry_usd = quantize_usd(
                ledger.week_carry_usd + (removed.total - removed.week_baseline).max(0.0),
            );
        }
    }

    let today_sum: f64 = ledger
        .sessions
        .values()
        .map(|e| (e.total - e.day_baseline).max(0.0))
        .sum();
    let week_sum: f64 = ledger
        .sessions
        .values()
        .map(|e| (e.total - e.week_baseline).max(0.0))
        .sum();

    let totals = CostTotals {
        today_usd: quantize_usd(ledger.day_carry_usd + today_sum),
        week_usd: quantize_usd(ledger.week_carry_usd + week_sum),
        estimated: stdin
            .and_then(|s| s.cost.as_ref())
            .and_then(|c| c.estimated)
            .unwrap_or(true),
    };
    let changed = existing != Some(&ledger);
    (Some(totals), changed.then_some(ledger))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jj_bookmarks_anonymous_change_id_and_conflicts() {
        let anon = parse_jj_probe_output("kpqxywon\x1f\x1f0\x1f0\n").unwrap();
        assert_eq!(anon.vcs_type, VcsType::Jj);
        assert_eq!(anon.branch, "@kpqxywon");
        assert!(!anon.dirty);
        assert_eq!(anon.conflicts, 0);

        let conflicted = parse_jj_probe_output("zvtrplmn\x1ffeat/hud\x1emain\x1f1\x1f1\n").unwrap();
        assert_eq!(conflicted.branch, "feat/hud+1");
        assert!(conflicted.dirty);
        assert_eq!(conflicted.conflicts, 1);
    }

    #[test]
    fn parses_hg_branch_and_status() {
        let hg = parse_hg_probe_output(
            "feature-hg\n",
            Some("M src/app.rs\nA src/new.rs\nR src/old.rs\n! src/missing.rs\n? scratch.txt\n"),
        )
        .unwrap();
        assert_eq!(hg.vcs_type, VcsType::Hg);
        assert_eq!(hg.branch, "feature-hg");
        assert!(hg.dirty);
        assert_eq!(hg.modified, 1);
        assert_eq!(hg.added, 1);
        assert_eq!(hg.deleted, 2);
        assert_eq!(hg.untracked, 1);
        assert!(parse_hg_probe_output("", Some("")).is_none());
    }

    fn base_speed_stdin(conv_id: &str, model_id: &str, state: &str) -> AgyStdinData {
        AgyStdinData {
            conversation_id: Some(conv_id.to_string()),
            agent_state: Some(state.to_string()),
            model: Some(crate::types::ModelStdin {
                id: Some(model_id.to_string()),
                display_name: Some(model_id.to_string()),
                effort: None,
            }),
            ..Default::default()
        }
    }

    fn planner_timing(
        step_index: usize,
        output_tokens: u64,
        estimated_tokens: u64,
        started_at_ms: i64,
        is_done: bool,
    ) -> crate::types::PlannerStepTiming {
        crate::types::PlannerStepTiming {
            step_index,
            output_tokens,
            estimated_tokens,
            started_at_ms,
            ended_at_ms: None,
            has_trailing_user_input: false,
            is_done,
        }
    }

    #[test]
    fn live_streaming_ticks_do_not_pollute_session_min_max() {
        let base_ms = 1_794_200_000_000i64;
        let mut stdin = base_speed_stdin("conv-live", "Gemini 4 Argon", "streaming");

        // Step 1 streaming at ~200 tok/s live must not record min/max before completion
        let mut tx1_stream = TranscriptSummary {
            latest_planner_step: Some(planner_timing(1, 0, 3, base_ms, false)),
            ..Default::default()
        };
        let c0 = apply_speed_cache(Some(&stdin), &mut tx1_stream, None, base_ms + 500)
            .expect("records active decode start");
        assert_eq!(tx1_stream.session_min_tps_x10, None);
        assert_eq!(tx1_stream.session_max_tps_x10, None);

        tx1_stream.latest_planner_step = Some(planner_timing(1, 0, 203, base_ms, false));
        let c1 = apply_speed_cache(Some(&stdin), &mut tx1_stream, Some(&c0), base_ms + 1_500)
            .expect("records live tps");
        assert_eq!(c1.session_min_tps_x10, None);
        assert_eq!(c1.session_max_tps_x10, None);
        let disp_live1 =
            resolve_output_speed_display(Some(&stdin), &tx1_stream, Some(&c1), base_ms + 1_500)
                .expect("live speed available");
        assert!(disp_live1.estimated);
        assert!(disp_live1.current_tps >= 190.0);
        assert_eq!(disp_live1.min_tps, None);
        assert_eq!(disp_live1.max_tps, None);

        // Step 1 completes at 100.0 tok/s (200 tokens over 2000 ms decode window)
        stdin.agent_state = Some("idle".to_string());
        let mut tx1_done = TranscriptSummary {
            latest_planner_step: Some(planner_timing(1, 200, 200, base_ms, true)),
            ..Default::default()
        };
        let locked1 = apply_speed_cache(Some(&stdin), &mut tx1_done, Some(&c1), base_ms + 2_500)
            .expect("locks step 1");
        assert_eq!(locked1.tps_x10, 1000);
        assert_eq!(locked1.session_min_tps_x10, Some(1000));
        assert_eq!(locked1.session_max_tps_x10, Some(1000));
        assert_eq!(tx1_done.session_min_tps_x10, Some(1000));
        assert_eq!(tx1_done.session_max_tps_x10, Some(1000));

        // Step 2 streams with a huge live spike (~300 tok/s): min/max must remain locked at 100.0
        stdin.agent_state = Some("streaming".to_string());
        let mut tx2_stream = TranscriptSummary {
            latest_planner_step: Some(planner_timing(2, 0, 3, base_ms + 10_000, false)),
            ..Default::default()
        };
        let c2 = apply_speed_cache(
            Some(&stdin),
            &mut tx2_stream,
            Some(&locked1),
            base_ms + 10_200,
        )
        .expect("starts step 2 active sample");
        tx2_stream.latest_planner_step = Some(planner_timing(2, 0, 303, base_ms + 10_000, false));
        let c3 = apply_speed_cache(Some(&stdin), &mut tx2_stream, Some(&c2), base_ms + 11_200)
            .expect("updates step 2 live tps");
        assert_eq!(c3.session_min_tps_x10, Some(1000));
        assert_eq!(c3.session_max_tps_x10, Some(1000));
        let disp_live2 =
            resolve_output_speed_display(Some(&stdin), &tx2_stream, None, base_ms + 11_200)
                .expect("uses synced transcript state");
        assert!(disp_live2.current_tps >= 250.0);
        assert_eq!(disp_live2.min_tps, Some(100.0));
        assert_eq!(disp_live2.max_tps, Some(100.0));

        // Step 2 completes at 50.0 tok/s (150 tokens over 3000 ms decode): raw sample updates min to 50.0,
        // even though EWMA stays above 50.0
        stdin.agent_state = Some("idle".to_string());
        let mut tx2_done = TranscriptSummary {
            latest_planner_step: Some(planner_timing(2, 150, 150, base_ms + 10_000, true)),
            ..Default::default()
        };
        let locked2 = apply_speed_cache(Some(&stdin), &mut tx2_done, Some(&c3), base_ms + 13_200)
            .expect("locks step 2");
        assert_eq!(locked2.tps_x10, 500);
        assert!(locked2.ewma_tps_x10.unwrap() > 500);
        assert_eq!(locked2.session_min_tps_x10, Some(500));
        assert_eq!(locked2.session_max_tps_x10, Some(1000));

        let disp_done2 =
            resolve_output_speed_display(Some(&stdin), &tx2_done, None, base_ms + 13_200)
                .expect("resolved display");
        assert!(!disp_done2.current_tps.is_nan());
        assert_eq!(disp_done2.min_tps, Some(50.0));
        assert_eq!(disp_done2.max_tps, Some(100.0));
    }

    #[test]
    fn native_stdin_exact_tps_updates_and_persists_session_min_max() {
        let base_ms = 1_794_200_100_000i64;
        let mut stdin = base_speed_stdin("conv-native", "Gemini 4 Argon", "idle");
        stdin
            .extra
            .insert("output_tokens_per_sec".to_string(), serde_json::json!(45.0));

        let mut tx = TranscriptSummary::default();
        let sc1 = apply_speed_cache(Some(&stdin), &mut tx, None, base_ms)
            .expect("persists native exact min/max");
        assert_eq!(tx.session_min_tps_x10, Some(450));
        assert_eq!(tx.session_max_tps_x10, Some(450));

        // Same native reading does not rewrite cache
        assert!(apply_speed_cache(Some(&stdin), &mut tx, Some(&sc1), base_ms + 300).is_none());

        // Persisted native-only snapshot survives `parse_speed_cache_info`
        let parsed1 = parse_speed_cache_info(&ProbeAnswers {
            speed_cache: Some(serde_json::to_string(&sc1).unwrap()),
            ..Default::default()
        })
        .expect("native cache survives deserialization");
        assert_eq!(parsed1, sc1);

        // Higher native speed via nested telemetry updates max (even > 500 tok/s)
        stdin.extra.clear();
        stdin.extra.insert(
            "telemetry".to_string(),
            serde_json::json!({ "outputTokensPerSec": 820.0 }),
        );
        let mut tx2 = TranscriptSummary::default();
        let sc2 = apply_speed_cache(Some(&stdin), &mut tx2, Some(&parsed1), base_ms + 1_000)
            .expect("updates max from native telemetry");
        assert_eq!(tx2.session_min_tps_x10, Some(450));
        assert_eq!(tx2.session_max_tps_x10, Some(8_200));

        let disp = resolve_output_speed_display(Some(&stdin), &tx2, None, base_ms + 1_000)
            .expect("native exact display");
        assert_eq!(
            disp,
            OutputSpeedDisplay {
                current_tps: 820.0,
                estimated: false,
                min_tps: Some(45.0),
                max_tps: Some(820.0),
            }
        );

        let parsed2 = parse_speed_cache_info(&ProbeAnswers {
            speed_cache: Some(serde_json::to_string(&sc2).unwrap()),
            ..Default::default()
        })
        .expect("high-rate native cache survives deserialization");
        assert_eq!(parsed2.session_min_tps_x10, Some(450));
        assert_eq!(parsed2.session_max_tps_x10, Some(8_200));

        // Different session must not inherit `conv-native` bounds
        let mut other_stdin = base_speed_stdin("conv-other", "Gemini 4 Argon", "idle");
        other_stdin
            .extra
            .insert("output_tokens_per_sec".to_string(), serde_json::json!(60.0));
        let mut other_tx = TranscriptSummary::default();
        let _ = apply_speed_cache(
            Some(&other_stdin),
            &mut other_tx,
            Some(&parsed2),
            base_ms + 2_000,
        );
        assert_eq!(other_tx.session_min_tps_x10, Some(600));
        assert_eq!(other_tx.session_max_tps_x10, Some(600));
    }

    #[test]
    fn model_switch_preserves_session_min_max_while_resetting_ewma() {
        let base_ms = 1_794_200_200_000i64;
        let argon_stdin = base_speed_stdin("conv-switch", "Gemini 4 Argon", "idle");

        // Turn 1 on Argon: 42.0 tok/s (from prior transcript step) and 112.0 tok/s (latest step)
        let mut tx_argon = TranscriptSummary {
            completed_step_tps_x10: Some(420),
            session_min_tps_x10: Some(420),
            session_max_tps_x10: Some(420),
            latest_planner_step: Some(planner_timing(2, 1120, 1120, base_ms, true)),
            ..Default::default()
        };
        let locked_argon =
            apply_speed_cache(Some(&argon_stdin), &mut tx_argon, None, base_ms + 10_000)
                .expect("locks Argon step 2 at 112.0 tok/s");
        assert_eq!(locked_argon.tps_x10, 1120);
        assert_eq!(locked_argon.session_min_tps_x10, Some(420));
        assert_eq!(locked_argon.session_max_tps_x10, Some(1120));

        // Mid-session switch to Flash while idle emits a barrier, clears EWMA/current speed,
        // and preserves session min/max
        let mut flash_stdin = base_speed_stdin("conv-switch", "Gemini 3 Flash", "idle");
        let mut tx_switch = tx_argon.clone();
        let barrier = apply_speed_cache(
            Some(&flash_stdin),
            &mut tx_switch,
            Some(&locked_argon),
            base_ms + 12_000,
        )
        .expect("writes model-switch barrier");
        assert_eq!(barrier.ended_at_ms, -1);
        assert_eq!(barrier.tps_x10, 0);
        assert_eq!(barrier.ewma_tps_x10, None);
        assert_eq!(barrier.session_min_tps_x10, Some(420));
        assert_eq!(barrier.session_max_tps_x10, Some(1120));
        assert_eq!(tx_switch.resolved_tps_x10, None);
        assert_eq!(tx_switch.completed_step_tps_x10, None);
        assert_eq!(tx_switch.session_min_tps_x10, Some(420));
        assert_eq!(tx_switch.session_max_tps_x10, Some(1120));
        assert!(
            resolve_output_speed_display(
                Some(&flash_stdin),
                &tx_switch,
                Some(&barrier),
                base_ms + 12_000,
            )
            .is_none()
        );

        // Model-switch barrier survives `parse_speed_cache_info`
        let parsed_barrier = parse_speed_cache_info(&ProbeAnswers {
            speed_cache: Some(serde_json::to_string(&barrier).unwrap()),
            ..Default::default()
        })
        .expect("model-switch barrier survives deserialization");
        assert_eq!(parsed_barrier, barrier);

        // Flash starts streaming with a fast live tick (~250 tok/s): live tick does not update max
        flash_stdin.agent_state = Some("streaming".to_string());
        let mut tx_flash_stream = TranscriptSummary {
            latest_planner_step: Some(planner_timing(3, 0, 3, base_ms + 13_000, false)),
            ..Default::default()
        };
        let flash_c0 = apply_speed_cache(
            Some(&flash_stdin),
            &mut tx_flash_stream,
            Some(&parsed_barrier),
            base_ms + 13_100,
        )
        .expect("records Flash decode start");
        tx_flash_stream.latest_planner_step =
            Some(planner_timing(3, 0, 253, base_ms + 13_000, false));
        let flash_c1 = apply_speed_cache(
            Some(&flash_stdin),
            &mut tx_flash_stream,
            Some(&flash_c0),
            base_ms + 14_100,
        )
        .expect("records Flash live tick");
        let disp_flash_live = resolve_output_speed_display(
            Some(&flash_stdin),
            &tx_flash_stream,
            None,
            base_ms + 14_100,
        )
        .expect("Flash live display");
        assert!(disp_flash_live.current_tps >= 240.0);
        assert_eq!(disp_flash_live.min_tps, Some(42.0));
        assert_eq!(disp_flash_live.max_tps, Some(112.0));

        // Flash step 3 finishes at 180.0 tok/s -> EWMA starts fresh at 180.0 and max expands to 180.0
        flash_stdin.agent_state = Some("idle".to_string());
        let mut tx_flash_done = TranscriptSummary {
            latest_planner_step: Some(planner_timing(3, 180, 180, base_ms + 13_000, true)),
            ..Default::default()
        };
        let locked_flash = apply_speed_cache(
            Some(&flash_stdin),
            &mut tx_flash_done,
            Some(&flash_c1),
            base_ms + 14_100,
        )
        .expect("locks Flash step 3");
        assert_eq!(locked_flash.tps_x10, 1800);
        assert_eq!(locked_flash.ewma_tps_x10, Some(1800));
        assert_eq!(locked_flash.session_min_tps_x10, Some(420));
        assert_eq!(locked_flash.session_max_tps_x10, Some(1800));

        let disp_flash_done = resolve_output_speed_display(
            Some(&flash_stdin),
            &tx_flash_done,
            None,
            base_ms + 14_100,
        )
        .expect("Flash completed display");
        assert_eq!(
            disp_flash_done,
            OutputSpeedDisplay {
                current_tps: 180.0,
                estimated: true,
                min_tps: Some(42.0),
                max_tps: Some(180.0),
            }
        );
    }

    #[test]
    fn legacy_speed_cache_json_backward_compatibility() {
        let legacy_json = r#"{
            "sessionKey": "conv-legacy",
            "modelKey": "Gemini 4 Argon",
            "stepIndex": 4,
            "outputTokens": 500,
            "startedAtMs": 1000,
            "endedAtMs": 6000,
            "tpsX10": 1000,
            "ewmaTpsX10": 950
        }"#;
        let parsed = parse_speed_cache_info(&ProbeAnswers {
            speed_cache: Some(legacy_json.to_string()),
            ..Default::default()
        })
        .expect("legacy cache parses cleanly");
        assert_eq!(parsed.session_min_tps_x10, None);
        assert_eq!(parsed.session_max_tps_x10, None);

        // Upgrading mid-session with legacy cache (`session_min/max == None`, `ended_at_ms > 0`)
        // seeds session min/max from `sc.tps_x10` (100.0), not `ewma_tps_x10` (95.0).
        let base_ms = 1_794_200_300_000i64;
        let legacy_stdin = base_speed_stdin("conv-legacy", "Gemini 4 Argon", "idle");
        let mut legacy_tx = TranscriptSummary {
            latest_planner_step: Some(planner_timing(4, 500, 500, 1_000, true)),
            ..Default::default()
        };
        let disp_upgraded =
            resolve_output_speed_display(Some(&legacy_stdin), &legacy_tx, Some(&parsed), base_ms)
                .expect("resolved display from legacy cache");
        assert_eq!(
            disp_upgraded,
            OutputSpeedDisplay {
                current_tps: 95.0,
                estimated: true,
                min_tps: Some(100.0),
                max_tps: Some(100.0),
            }
        );
        assert!(
            apply_speed_cache(Some(&legacy_stdin), &mut legacy_tx, Some(&parsed), base_ms)
                .is_none(),
            "idle legacy locked step does not trigger spurious cache rewrite"
        );
        assert_eq!(legacy_tx.session_min_tps_x10, Some(1000));
        assert_eq!(legacy_tx.session_max_tps_x10, Some(1000));

        // Subsequent step (50.0 tok/s) merges with the seeded legacy step to persist (50.0~100.0)
        let mut next_tx = TranscriptSummary {
            latest_planner_step: Some(planner_timing(5, 50, 50, base_ms - 1_000, true)),
            ..Default::default()
        };
        let upgraded_next =
            apply_speed_cache(Some(&legacy_stdin), &mut next_tx, Some(&parsed), base_ms)
                .expect("locks step 5 with seeded legacy min/max");
        assert_eq!(upgraded_next.tps_x10, 500);
        assert_eq!(upgraded_next.session_min_tps_x10, Some(500));
        assert_eq!(upgraded_next.session_max_tps_x10, Some(1000));
        assert_eq!(next_tx.session_min_tps_x10, Some(500));
        assert_eq!(next_tx.session_max_tps_x10, Some(1000));

        // Mid-session model switch straight after upgrade preserves seeded `sc.tps_x10` in barrier
        let switch_stdin = base_speed_stdin("conv-legacy", "Gemini 3 Flash", "idle");
        let mut switch_tx = TranscriptSummary {
            latest_planner_step: Some(planner_timing(4, 500, 500, 1_000, true)),
            ..Default::default()
        };
        let barrier = apply_speed_cache(
            Some(&switch_stdin),
            &mut switch_tx,
            Some(&parsed),
            base_ms + 1_000,
        )
        .expect("emits barrier preserving seeded legacy min/max");
        assert_eq!(barrier.ended_at_ms, -1);
        assert_eq!(barrier.session_min_tps_x10, Some(1000));
        assert_eq!(barrier.session_max_tps_x10, Some(1000));

        // Empty/zero non-barrier snapshot without active or range must still be rejected
        let empty_zero = r#"{
            "sessionKey": "conv-legacy",
            "modelKey": "",
            "stepIndex": 0,
            "outputTokens": 0,
            "startedAtMs": 0,
            "endedAtMs": 0,
            "tpsX10": 0
        }"#;
        assert!(
            parse_speed_cache_info(&ProbeAnswers {
                speed_cache: Some(empty_zero.to_string()),
                ..Default::default()
            })
            .is_none()
        );
    }
}
