use crate::ansi::sanitize_text;
use crate::types::{AgyStdinData, MemoryProbeInfo, ProbeAnswers, VcsProbeInfo, VcsType};

/// Merges `stdin.vcs` metadata from `agy` CLI with optional rich VCS probe JSON from `hud-cli`.
pub fn parse_vcs_info(stdin: Option<&AgyStdinData>, answers: &ProbeAnswers) -> Option<VcsProbeInfo> {
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
    let trimmed = stdout.strip_suffix("\r\n").or_else(|| stdout.strip_suffix('\n')).unwrap_or(stdout);
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
