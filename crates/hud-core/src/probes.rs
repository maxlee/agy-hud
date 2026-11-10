use crate::ansi::sanitize_text;
use crate::transcript::{
    adjust_coarse_dt_ms, blend_tps_ewma_x10, compute_tps_x10, estimate_ttft_overhead_ms,
    parse_iso_ms, update_speed_anchors,
};
use crate::types::{
    ActiveSpeedSample, AgyStdinData, CachedQuotaSnapshot, MemoryProbeInfo, ProbeAnswers,
    RawQuotaBucket, SpeedCacheSnapshot, TranscriptSummary, UpdateCacheSnapshot, VcsProbeInfo,
    VcsType,
};
use serde_json::Value;
use std::collections::BTreeMap;

/// Merges `stdin.vcs` metadata from `agy` CLI with optional rich VCS probe JSON from `hud-cli`.
pub fn parse_vcs_info(
    stdin: Option<&AgyStdinData>,
    answers: &ProbeAnswers,
) -> Option<VcsProbeInfo> {
    if let Some(raw_json) = answers.vcs.as_deref()
        && let Ok(mut parsed) = serde_json::from_str::<VcsProbeInfo>(raw_json)
    {
        parsed.branch = sanitize_text(&parsed.branch);
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
    if !(10..=5_000).contains(&parsed.tps_x10) {
        return None;
    }
    Some(parsed)
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

/// Resolves output generation throughput `(tokens_per_sec, is_estimated)` and returns an updated
/// `SpeedCacheSnapshot` whenever live streaming milestones or completed `PLANNER_RESPONSE` steps
/// should be persisted to disk.
pub fn resolve_output_speed(
    stdin: Option<&AgyStdinData>,
    transcript: &TranscriptSummary,
    speed_cache: Option<&SpeedCacheSnapshot>,
    now_ms: i64,
) -> (Option<(f64, bool)>, Option<SpeedCacheSnapshot>) {
    if let Some(exact) = extract_native_stdin_tps(stdin) {
        return (Some((exact, false)), None);
    }

    if speed_cache.is_none()
        && let Some(x10) = transcript
            .resolved_tps_x10
            .filter(|&v| (10..=5_000).contains(&v))
    {
        return (Some((x10 as f64 / 10.0, true)), None);
    }

    let session_key = stdin
        .and_then(|s| s.conversation_id.as_deref().or(s.session_id.as_deref()))
        .map(sanitize_text)
        .unwrap_or_default();
    let model_key = extract_model_key(stdin);

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

    let same_session_cache = (!session_key.is_empty())
        .then_some(speed_cache)
        .flatten()
        .filter(|sc| sc.session_key == session_key);

    let model_switched = same_session_cache.is_some_and(|sc| {
        !sc.model_key.is_empty() && !model_key.is_empty() && sc.model_key != model_key
    });

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
            let reset = SpeedCacheSnapshot {
                session_key,
                model_key,
                step_index: barrier_step,
                output_tokens: 0,
                started_at_ms: now_ms,
                ended_at_ms: -1,
                tps_x10: 0,
                ewma_tps_x10: None,
                short_anchor: None,
                long_anchor: None,
                active: None,
            };
            return (None, Some(reset));
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
                let base = matching_cache
                    .cloned()
                    .unwrap_or_else(|| SpeedCacheSnapshot {
                        session_key: session_key.clone(),
                        model_key: model_key.clone(),
                        step_index: model_switched
                            .then(|| same_session_cache.map(|s| s.step_index))
                            .flatten()
                            .unwrap_or(0),
                        output_tokens: 0,
                        started_at_ms: 0,
                        ended_at_ms: if model_switched { -1 } else { 0 },
                        tps_x10: 0,
                        ewma_tps_x10: None,
                        short_anchor: None,
                        long_anchor: None,
                        active: None,
                    });
                SpeedCacheSnapshot {
                    session_key: session_key.clone(),
                    model_key: if model_key.is_empty() {
                        base.model_key
                    } else {
                        model_key.clone()
                    },
                    active: Some(active),
                    ..base
                }
            })
            .filter(|cand| speed_cache != Some(cand));

        return (display_x10.map(|x| (x as f64 / 10.0, true)), to_persist);
    }

    // 2. Completed planner step resolution & locking
    if let Some(ref lp) = transcript.latest_planner_step
        && lp.is_done
    {
        if let Some(sc) = matching_cache {
            if sc.ended_at_ms == -1 && lp.step_index <= sc.step_index {
                let clear_active = sc.active.is_some().then(|| SpeedCacheSnapshot {
                    active: None,
                    ..sc.clone()
                });
                return (None, clear_active);
            }
            if sc.ended_at_ms > 0
                && sc.step_index == lp.step_index
                && (10..=5_000).contains(&sc.tps_x10)
            {
                let shown = sc
                    .ewma_tps_x10
                    .filter(|&v| (10..=5_000).contains(&v))
                    .unwrap_or(sc.tps_x10);
                let clear_active = sc.active.is_some().then(|| SpeedCacheSnapshot {
                    active: None,
                    ..sc.clone()
                });
                return (Some((shown as f64 / 10.0, true)), clear_active);
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
                    short_anchor,
                    long_anchor,
                    active: None,
                });
                let to_persist = candidate.filter(|cand| speed_cache != Some(cand));
                return (Some((ewma_x10 as f64 / 10.0, true)), to_persist);
            }
        }
    }

    if let Some(x10) = prev_ewma {
        let clear_active =
            matching_cache
                .filter(|sc| sc.active.is_some())
                .map(|sc| SpeedCacheSnapshot {
                    active: None,
                    ..sc.clone()
                });
        return (Some((x10 as f64 / 10.0, true)), clear_active);
    }

    (None, None)
}

pub fn apply_speed_cache(
    stdin: Option<&AgyStdinData>,
    transcript: &mut TranscriptSummary,
    speed_cache: Option<&SpeedCacheSnapshot>,
    now_ms: i64,
) -> Option<SpeedCacheSnapshot> {
    let (resolved, next_cache) = resolve_output_speed(stdin, transcript, speed_cache, now_ms);
    match resolved {
        Some((tps, true)) => {
            let x10 = (tps * 10.0).round() as u32;
            if (10..=5_000).contains(&x10) {
                transcript.resolved_tps_x10 = Some(x10);
                transcript.completed_step_tps_x10 = Some(x10);
            }
        }
        None => {
            transcript.resolved_tps_x10 = None;
            if next_cache.as_ref().is_some_and(|c| c.ended_at_ms == -1)
                || speed_cache.is_some_and(|c| c.ended_at_ms == -1)
            {
                transcript.completed_step_tps_x10 = None;
            }
        }
        Some((_, false)) => {}
    }
    next_cache
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
}
