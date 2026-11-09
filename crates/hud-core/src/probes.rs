use crate::ansi::sanitize_text;
use crate::transcript::parse_iso_ms;
use crate::types::{
    AgyStdinData, CachedQuotaSnapshot, MemoryProbeInfo, ProbeAnswers, RawQuotaBucket,
    UpdateCacheSnapshot, VcsProbeInfo, VcsType,
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
        .or(sv.vcs_type.as_deref())?;
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
