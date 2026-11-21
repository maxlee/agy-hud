use crate::collector::{exec_file_safe, read_bounded_string};
use crate::paths::{
    AgyPaths, atomic_write_bytes, atomic_write_public_bytes, canonical_or_parent,
    canonicalize_with_tail, create_private_dir_all,
};
use hud_core::types::UnprefixedSkillsFacts;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Formats `statusLine.command` safely per OS:
/// - On Windows, omits surrounding double quotes (`"`) because Antigravity CLI's `cmd.exe`
///   invocation strips/mishandles leading quotes (`tightship#89`), and returns an actionable
///   8.3 short-path warning if the binary path contains spaces.
/// - On POSIX, wraps in single quotes with embedded single-quote escaping.
pub fn format_statusline_command_for_platform(
    bin_path: &str,
    is_windows: bool,
) -> (String, Option<String>) {
    if is_windows {
        let warning = if bin_path.contains(' ') {
            Some(
                "Windows binary path contains spaces; because Antigravity CLI statusLine.command cannot use surrounding double quotes on Windows (tightship#89), move AGY_HUD_HOME to a space-free path or configure its 8.3 short path."
                    .to_string(),
            )
        } else {
            None
        };
        (bin_path.to_string(), warning)
    } else {
        (format!("'{}'", bin_path.replace('\'', "'\\''")), None)
    }
}

fn format_statusline_command(bin_path: &str) -> (String, Option<String>) {
    format_statusline_command_for_platform(bin_path, cfg!(windows))
}

pub fn format_title_command_for_platform(
    bin_path: &str,
    is_windows: bool,
) -> (String, Option<String>) {
    let (cmd, warning) = format_statusline_command_for_platform(bin_path, is_windows);
    (format!("{cmd} title"), warning)
}

fn format_title_command(bin_path: &str) -> (String, Option<String>) {
    format_title_command_for_platform(bin_path, cfg!(windows))
}

fn read_json_object_or_empty(file_path: &Path) -> Result<Map<String, Value>, String> {
    if !file_path.exists() {
        return Ok(Map::new());
    }
    let raw = fs::read_to_string(file_path).map_err(|e| e.to_string())?;
    let stripped = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    if stripped.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(stripped).map_err(|e| e.to_string())? {
        Value::Object(map) => Ok(map),
        _ => Err(format!(
            "{} does not contain a JSON object.",
            file_path.display()
        )),
    }
}

fn classify_statusline_command(cmd: &str) -> &'static str {
    if cmd.is_empty() {
        "none"
    } else if cmd.contains("agy-hud") || cmd.contains("hud.mjs") || cmd.contains("statusline.mjs") {
        "agy-hud"
    } else {
        "other"
    }
}

fn utc_compact_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let hour = sod / 3_600;
    let min = (sod % 3_600) / 60;
    let sec = sod % 60;

    // Howard Hinnant civil-from-days
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + i64::from(m <= 2);

    format!("{year:04}{m:02}{d:02}T{hour:02}{min:02}{sec:02}Z")
}

fn file_mode_or_default(path: &Path, default_mode: u32) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(st) = fs::metadata(path) {
            return st.permissions().mode() & 0o777;
        }
    }
    let _ = path;
    default_mode
}

const INSTALLER_VERSION: &str = env!("CARGO_PKG_VERSION");

const MANAGED_SKILL_NAMES: [&str; 2] = ["hud", "hud-update"];
const MANAGED_MARKER_FILE: &str = ".agy-hud-managed.json";
const MAX_SKILLS_JSON_BYTES: u64 = 256 * 1024;
const MAX_INHERITS_DEPTH: usize = 8;

const EMBEDDED_HUD_SKILL_MD: &str = include_str!("../../../skills/hud/SKILL.md");
const EMBEDDED_HUD_CLI_SH: &str = include_str!("../../../skills/hud/scripts/hud-cli.sh");
const EMBEDDED_HUD_UPDATE_SKILL_MD: &str = include_str!("../../../skills/hud-update/SKILL.md");
const EMBEDDED_HUD_UPDATE_CLI_SH: &str =
    include_str!("../../../skills/hud-update/scripts/hud-cli.sh");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DestState {
    Absent,
    Managed,
    Conflict,
}

#[derive(Debug, Default, Deserialize)]
struct SkillsJsonFile {
    #[serde(default)]
    entries: Vec<SkillsJsonEntry>,
    #[serde(default)]
    inherits: Vec<SkillsJsonEntry>,
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SkillsJsonEntry {
    #[serde(default)]
    path: String,
    #[serde(default)]
    include_only: Option<Vec<String>>,
    #[serde(default)]
    exclude: Vec<String>,
}

fn embedded_skill_files(skill_name: &str) -> (&'static str, &'static str) {
    match skill_name {
        "hud" => (EMBEDDED_HUD_SKILL_MD, EMBEDDED_HUD_CLI_SH),
        "hud-update" => (EMBEDDED_HUD_UPDATE_SKILL_MD, EMBEDDED_HUD_UPDATE_CLI_SH),
        _ => ("", ""),
    }
}

fn load_skill_source_files(
    paths: &AgyPaths,
    plugin_root: &Path,
    skill_name: &str,
) -> (Vec<u8>, Vec<u8>) {
    let candidates = [
        plugin_root.join("skills"),
        paths
            .gemini_config_dir
            .join("plugins")
            .join("agy-hud")
            .join("skills"),
        paths.hud_home.join("plugin").join("skills"),
    ];
    for root in candidates {
        let skill_dir = root.join(skill_name);
        let md_path = skill_dir.join("SKILL.md");
        let sh_path = skill_dir.join("scripts").join("hud-cli.sh");
        if let (Some(md), Some(sh)) = (
            read_bounded_string(&md_path, 256 * 1024),
            read_bounded_string(&sh_path, 64 * 1024),
        ) {
            return (md.into_bytes(), sh.into_bytes());
        }
    }
    let (md, sh) = embedded_skill_files(skill_name);
    (md.as_bytes().to_vec(), sh.as_bytes().to_vec())
}

fn inspect_dest_state(dest: &Path, skill_name: &str) -> DestState {
    let meta = match fs::symlink_metadata(dest) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return DestState::Absent,
        Err(_) => return DestState::Conflict,
    };
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        return DestState::Conflict;
    }
    let marker_path = dest.join(MANAGED_MARKER_FILE);
    let Ok(marker_meta) = fs::symlink_metadata(&marker_path) else {
        return DestState::Conflict;
    };
    if marker_meta.file_type().is_symlink() || !marker_meta.file_type().is_file() {
        return DestState::Conflict;
    }
    let Some(raw) = read_bounded_string(&marker_path, 16 * 1024) else {
        return DestState::Conflict;
    };
    let Ok(val) = serde_json::from_str::<Value>(&raw) else {
        return DestState::Conflict;
    };
    let is_ours = val.get("managedBy").and_then(Value::as_str) == Some("agy-hud")
        && val.get("skill").and_then(Value::as_str) == Some(skill_name);
    if is_ours {
        DestState::Managed
    } else {
        DestState::Conflict
    }
}

fn read_regular_or_symlink_bounded(path: &Path, max_bytes: u64) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    let f = File::open(path).ok()?;
    let mut buf = Vec::with_capacity(meta.len() as usize);
    f.take(max_bytes + 1).read_to_end(&mut buf).ok()?;
    if (buf.len() as u64) > max_bytes {
        return None;
    }
    String::from_utf8(buf).ok()
}

fn strip_jsonc_comments_and_trailing_commas(raw: &str) -> String {
    let src = raw.strip_prefix('\u{feff}').unwrap_or(raw).as_bytes();
    let mut no_comments = Vec::with_capacity(src.len());
    let mut i = 0;
    let mut in_string = false;
    while i < src.len() {
        let b = src[i];
        if in_string {
            no_comments.push(b);
            if b == b'\\' && i + 1 < src.len() {
                i += 1;
                no_comments.push(src[i]);
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            no_comments.push(b);
            i += 1;
        } else if b == b'/' && i + 1 < src.len() && src[i + 1] == b'/' {
            i += 2;
            while i < src.len() && src[i] != b'\n' {
                i += 1;
            }
        } else if b == b'/' && i + 1 < src.len() && src[i + 1] == b'*' {
            i += 2;
            while i + 1 < src.len() && !(src[i] == b'*' && src[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(src.len());
        } else {
            no_comments.push(b);
            i += 1;
        }
    }

    let mut clean = Vec::with_capacity(no_comments.len());
    in_string = false;
    let mut j = 0;
    while j < no_comments.len() {
        let b = no_comments[j];
        if in_string {
            clean.push(b);
            if b == b'\\' && j + 1 < no_comments.len() {
                j += 1;
                clean.push(no_comments[j]);
            } else if b == b'"' {
                in_string = false;
            }
            j += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            clean.push(b);
            j += 1;
            continue;
        }
        if b == b',' {
            let mut k = j + 1;
            while k < no_comments.len() && no_comments[k].is_ascii_whitespace() {
                k += 1;
            }
            if k < no_comments.len() && matches!(no_comments[k], b']' | b'}') {
                j += 1;
                continue;
            }
        }
        clean.push(b);
        j += 1;
    }

    String::from_utf8_lossy(&clean).into_owned()
}

fn split_top_level_alternates(pat: &str) -> Vec<&str> {
    let bytes = pat.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut in_class = false;
    let mut escaped = false;
    let mut start = 0usize;
    for (idx, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if b == b'\\' {
            escaped = true;
            continue;
        }
        if in_class {
            if b == b']' {
                in_class = false;
            }
            continue;
        }
        match b {
            b'[' => in_class = true,
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b'|' if depth == 0 => {
                out.push(&pat[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }
    out.push(&pat[start..]);
    out
}

fn find_matching_paren(bytes: &[u8], open_idx: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_class = false;
    let mut escaped = false;
    for (idx, &b) in bytes.iter().enumerate().skip(open_idx) {
        if escaped {
            escaped = false;
            continue;
        }
        if b == b'\\' {
            escaped = true;
            continue;
        }
        if in_class {
            if b == b']' {
                in_class = false;
            }
            continue;
        }
        match b {
            b'[' => in_class = true,
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Clone, Copy)]
enum AtomMatcher<'a> {
    Any,
    Lit(u8),
    Class { body: &'a [u8], neg: bool },
}

impl AtomMatcher<'_> {
    fn matches(self, c: u8) -> bool {
        match self {
            Self::Any => true,
            Self::Lit(lit) => c == lit,
            Self::Class { body, neg } => {
                let mut found = false;
                let mut idx = 0;
                while idx < body.len() {
                    if idx + 2 < body.len() && body[idx + 1] == b'-' {
                        if c >= body[idx] && c <= body[idx + 2] {
                            found = true;
                            break;
                        }
                        idx += 3;
                    } else {
                        if c == body[idx] {
                            found = true;
                            break;
                        }
                        idx += 1;
                    }
                }
                if neg { !found } else { found }
            }
        }
    }
}

fn regex_match_here(pat: &[u8], text: &[u8], anchor_end: bool, budget: &mut usize) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;

    if pat.is_empty() {
        return !anchor_end || text.is_empty();
    }

    // Parenthesized group `(a|b)` with optional `?`/`*`/`+`
    if pat[0] == b'('
        && let Some(close_idx) = find_matching_paren(pat, 0)
    {
        let inner = std::str::from_utf8(&pat[1..close_idx]).unwrap_or("");
        let rest = &pat[close_idx + 1..];
        let (quant, after_quant) = match rest.first().copied() {
            Some(q @ (b'?' | b'*' | b'+')) => (Some(q), &rest[1..]),
            _ => (None, rest),
        };
        let alts = split_top_level_alternates(inner);
        match quant {
            Some(b'?') => {
                if regex_match_here(after_quant, text, anchor_end, budget) {
                    return true;
                }
                for alt in alts {
                    let mut combined = alt.as_bytes().to_vec();
                    combined.extend_from_slice(after_quant);
                    if regex_match_here(&combined, text, anchor_end, budget) {
                        return true;
                    }
                }
                return false;
            }
            Some(b'*') => {
                if regex_match_here(after_quant, text, anchor_end, budget) {
                    return true;
                }
                for split in 1..=text.len() {
                    for alt in &alts {
                        if regex_match_here(alt.as_bytes(), &text[..split], true, budget)
                            && regex_match_here(pat, &text[split..], anchor_end, budget)
                        {
                            return true;
                        }
                    }
                }
                return false;
            }
            Some(b'+') => {
                for split in 1..=text.len() {
                    for alt in &alts {
                        if regex_match_here(alt.as_bytes(), &text[..split], true, budget)
                            && (regex_match_here(after_quant, &text[split..], anchor_end, budget)
                                || regex_match_here(pat, &text[split..], anchor_end, budget))
                        {
                            return true;
                        }
                    }
                }
                return false;
            }
            _ => {
                for alt in alts {
                    let mut combined = alt.as_bytes().to_vec();
                    combined.extend_from_slice(after_quant);
                    if regex_match_here(&combined, text, anchor_end, budget) {
                        return true;
                    }
                }
                return false;
            }
        }
    }

    // Parse single atom: `\x`, `[...]`, `.`, or literal byte
    let (atom_end, atom) = if pat[0] == b'\\' && pat.len() >= 2 {
        (2, AtomMatcher::Lit(pat[1]))
    } else if pat[0] == b'['
        && let Some(rel_close) = pat[1..].iter().position(|&b| b == b']')
    {
        let class_slice = &pat[1..1 + rel_close];
        let (neg, body) = if let Some(rest) = class_slice.strip_prefix(b"^") {
            (true, rest)
        } else {
            (false, class_slice)
        };
        (2 + rel_close, AtomMatcher::Class { body, neg })
    } else if pat[0] == b'.' {
        (1, AtomMatcher::Any)
    } else {
        (1, AtomMatcher::Lit(pat[0]))
    };

    let after_atom = &pat[atom_end..];
    match after_atom.first().copied() {
        Some(b'*') => {
            let rest = &after_atom[1..];
            let mut idx = 0;
            loop {
                if regex_match_here(rest, &text[idx..], anchor_end, budget) {
                    return true;
                }
                if idx >= text.len() || !atom.matches(text[idx]) {
                    break;
                }
                idx += 1;
            }
            false
        }
        Some(b'+') => {
            let rest = &after_atom[1..];
            let mut idx = 0;
            while idx < text.len() && atom.matches(text[idx]) {
                idx += 1;
                if regex_match_here(rest, &text[idx..], anchor_end, budget) {
                    return true;
                }
            }
            false
        }
        Some(b'?') => {
            let rest = &after_atom[1..];
            if !text.is_empty()
                && atom.matches(text[0])
                && regex_match_here(rest, &text[1..], anchor_end, budget)
            {
                return true;
            }
            regex_match_here(rest, text, anchor_end, budget)
        }
        _ => {
            !text.is_empty()
                && atom.matches(text[0])
                && regex_match_here(after_atom, &text[1..], anchor_end, budget)
        }
    }
}

fn pattern_matches_skill(pattern: &str, skill_name: &str) -> bool {
    let pat = pattern.trim();
    if pat.is_empty() {
        return false;
    }
    let pat = pat
        .strip_suffix("/SKILL.md")
        .unwrap_or(pat)
        .trim_end_matches('/');
    if pat == "*" {
        return true;
    }

    let has_meta = pat.bytes().any(|b| {
        matches!(
            b,
            b'^' | b'$' | b'.' | b'*' | b'+' | b'?' | b'(' | b')' | b'[' | b']' | b'|' | b'\\'
        )
    });
    if !has_meta {
        return pat == skill_name;
    }

    for alt in split_top_level_alternates(pat) {
        let (anchored_start, body) = match alt.strip_prefix('^') {
            Some(rest) => (true, rest),
            None => (false, alt),
        };
        let (anchored_end, body) = match body.strip_suffix('$') {
            Some(rest) => (true, rest),
            None => (false, body),
        };
        let pat_bytes = body.as_bytes();
        let text_bytes = skill_name.as_bytes();
        let mut budget = 1024usize;
        if anchored_start {
            if regex_match_here(pat_bytes, text_bytes, anchored_end, &mut budget) {
                return true;
            }
        } else {
            for start in 0..=text_bytes.len() {
                if regex_match_here(pat_bytes, &text_bytes[start..], anchored_end, &mut budget) {
                    return true;
                }
            }
        }
    }
    false
}

fn resolve_skills_json_path(raw_path: &str, base_dir: &Path, home: &Path) -> PathBuf {
    let trimmed = raw_path.trim();
    if trimmed == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
    {
        return home.join(rest);
    }
    let p = Path::new(trimmed);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    }
}

fn build_excluded_self_skill_roots(paths: &AgyPaths, plugin_root: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![
        canonicalize_with_tail(
            &paths
                .gemini_config_dir
                .join("plugins")
                .join("agy-hud")
                .join("skills"),
        ),
        canonicalize_with_tail(&paths.hud_home.join("plugin").join("skills")),
        canonicalize_with_tail(&paths.gemini_config_dir.join("skills")),
    ];
    if let Some(pr) = plugin_root {
        roots.push(canonicalize_with_tail(&pr.join("skills")));
    }
    roots
}

fn is_excluded_self_dir(dir: &Path, excluded_roots: &[PathBuf]) -> bool {
    let canon = canonicalize_with_tail(dir);
    excluded_roots
        .iter()
        .any(|root| canon == *root || canon.starts_with(root))
}

fn dir_exposes_foreign_skill(dir: &Path, skill_name: &str, excluded_roots: &[PathBuf]) -> bool {
    if is_excluded_self_dir(dir, excluded_roots) {
        return false;
    }
    // 1. Entry points directly to a `<skill_name>` directory containing `SKILL.md`
    if dir.file_name().and_then(|s| s.to_str()) == Some(skill_name)
        && dir.join("SKILL.md").is_file()
        && inspect_dest_state(dir, skill_name) != DestState::Managed
    {
        return true;
    }
    // 2. Standard 1-level scan: `<dir>/<skill_name>/SKILL.md`
    let child = dir.join(skill_name);
    !is_excluded_self_dir(&child, excluded_roots)
        && child.join("SKILL.md").is_file()
        && inspect_dest_state(&child, skill_name) != DestState::Managed
}

fn entry_allows_skill(
    skill_name: &str,
    entry_include_only: Option<&[String]>,
    entry_exclude: &[String],
    inherited_includes: &[&[String]],
    inherited_excludes: &[&[String]],
) -> bool {
    if entry_exclude
        .iter()
        .any(|p| pattern_matches_skill(p, skill_name))
    {
        return false;
    }
    for group in inherited_excludes {
        if group.iter().any(|p| pattern_matches_skill(p, skill_name)) {
            return false;
        }
    }
    if let Some(inc) = entry_include_only
        && !inc.iter().any(|p| pattern_matches_skill(p, skill_name))
    {
        return false;
    }
    for group in inherited_includes {
        if !group.iter().any(|p| pattern_matches_skill(p, skill_name)) {
            return false;
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn check_skills_json_file_conflict(
    cfg_path: &Path,
    paths: &AgyPaths,
    excluded_roots: &[PathBuf],
    skill_name: &str,
    inherited_includes: &[&[String]],
    inherited_excludes: &[&[String]],
    depth: usize,
    visited: &mut BTreeSet<PathBuf>,
) -> bool {
    if depth > MAX_INHERITS_DEPTH {
        return false;
    }
    let canon_cfg = canonicalize_with_tail(cfg_path);
    if !visited.insert(canon_cfg) {
        return false;
    }

    let Some(raw) = read_regular_or_symlink_bounded(cfg_path, MAX_SKILLS_JSON_BYTES) else {
        return false;
    };
    let sanitized = strip_jsonc_comments_and_trailing_commas(&raw);
    let Ok(parsed) = serde_json::from_str::<SkillsJsonFile>(&sanitized) else {
        return false;
    };

    let base_dir = cfg_path
        .parent()
        .unwrap_or(paths.gemini_config_dir.as_path());
    let mut next_excludes: Vec<&[String]> = inherited_excludes.to_vec();
    if !parsed.exclude.is_empty() {
        next_excludes.push(&parsed.exclude);
    }

    for entry in &parsed.entries {
        let raw_entry = entry.path.trim();
        if raw_entry.is_empty() {
            continue;
        }
        let resolved_dir = resolve_skills_json_path(raw_entry, base_dir, &paths.home);
        if is_excluded_self_dir(&resolved_dir, excluded_roots) {
            continue;
        }

        // Check nested `include_only` relative paths (e.g. `nested/hud`)
        if let Some(ref inc_list) = entry.include_only
            && inc_list.iter().any(|item| {
                let trimmed = item.trim().trim_end_matches('/');
                if !trimmed.contains(['/', '\\']) {
                    return false;
                }
                let candidate = resolved_dir.join(trimmed);
                candidate.file_name().and_then(|s| s.to_str()) == Some(skill_name)
                    && !is_excluded_self_dir(&candidate, excluded_roots)
                    && candidate.join("SKILL.md").is_file()
                    && inspect_dest_state(&candidate, skill_name) != DestState::Managed
                    && entry_allows_skill(
                        skill_name,
                        None,
                        &entry.exclude,
                        inherited_includes,
                        &next_excludes,
                    )
            })
        {
            return true;
        }

        if entry_allows_skill(
            skill_name,
            entry.include_only.as_deref(),
            &entry.exclude,
            inherited_includes,
            &next_excludes,
        ) && dir_exposes_foreign_skill(&resolved_dir, skill_name, excluded_roots)
        {
            return true;
        }
    }

    for inh in &parsed.inherits {
        let raw_inh = inh.path.trim();
        if raw_inh.is_empty() {
            continue;
        }
        let inh_path = resolve_skills_json_path(raw_inh, base_dir, &paths.home);
        let mut child_includes = inherited_includes.to_vec();
        if let Some(ref inc) = inh.include_only {
            child_includes.push(inc);
        }
        let mut child_excludes = next_excludes.clone();
        if !inh.exclude.is_empty() {
            child_excludes.push(&inh.exclude);
        }
        if check_skills_json_file_conflict(
            &inh_path,
            paths,
            excluded_roots,
            skill_name,
            &child_includes,
            &child_excludes,
            depth + 1,
            visited,
        ) {
            return true;
        }
    }

    false
}

fn has_skills_json_conflict(
    paths: &AgyPaths,
    plugin_root: Option<&Path>,
    skill_name: &str,
) -> bool {
    let skills_json_path = paths.gemini_config_dir.join("skills.json");
    if fs::metadata(&skills_json_path).is_err() {
        return false;
    }
    let excluded_roots = build_excluded_self_skill_roots(paths, plugin_root);
    let mut visited = BTreeSet::new();
    check_skills_json_file_conflict(
        &skills_json_path,
        paths,
        &excluded_roots,
        skill_name,
        &[],
        &[],
        0,
        &mut visited,
    )
}

/// Read-only inspection of unprefixed `/hud` and `/hud-update` shortcut skill states under
/// `paths.gemini_config_dir/skills` (preserving symlink/foreign/skills.json conflicts).
pub fn inspect_unprefixed_skills(
    paths: &AgyPaths,
    plugin_root: Option<&Path>,
) -> UnprefixedSkillsFacts {
    let skills_dir = paths.gemini_config_dir.join("skills");
    let detect_one = |skill_name: &str| -> String {
        let dest = skills_dir.join(skill_name);
        match inspect_dest_state(&dest, skill_name) {
            DestState::Conflict => "conflict".to_string(),
            DestState::Managed => {
                if has_skills_json_conflict(paths, plugin_root, skill_name) {
                    "conflict".to_string()
                } else {
                    "installed".to_string()
                }
            }
            DestState::Absent => {
                if has_skills_json_conflict(paths, plugin_root, skill_name) {
                    "conflict".to_string()
                } else {
                    "not-installed".to_string()
                }
            }
        }
    };

    UnprefixedSkillsFacts {
        hud: detect_one("hud"),
        hud_update: detect_one("hud-update"),
        skills_dir: canonicalize_with_tail(&skills_dir)
            .to_string_lossy()
            .into_owned(),
    }
}

fn install_unprefixed_skills(paths: &AgyPaths, plugin_root: &Path) -> Result<Value, String> {
    let skills_dir = paths.gemini_config_dir.join("skills");
    let parent_blocked = fs::symlink_metadata(&skills_dir).is_ok() && !skills_dir.is_dir();

    let mut statuses: [&'static str; 2] = ["skipped-conflict", "skipped-conflict"];
    for (idx, skill_name) in MANAGED_SKILL_NAMES.iter().enumerate() {
        if parent_blocked {
            statuses[idx] = "skipped-conflict";
            continue;
        }
        let dest = skills_dir.join(skill_name);
        let dest_state = inspect_dest_state(&dest, skill_name);
        let json_conflict = has_skills_json_conflict(paths, Some(plugin_root), skill_name);
        if dest_state == DestState::Conflict || json_conflict {
            statuses[idx] = "skipped-conflict";
            continue;
        }

        // Never `chmod 0700` existing `gemini_config_dir` or `skills/`
        fs::create_dir_all(&dest).map_err(|e| {
            format!(
                "Failed to prepare unprefixed skill directory {}: {e}",
                dest.display()
            )
        })?;

        let scripts_dir = dest.join("scripts");
        if let Ok(meta) = fs::symlink_metadata(&scripts_dir)
            && (meta.file_type().is_symlink() || !meta.file_type().is_dir())
        {
            let _ = fs::remove_file(&scripts_dir);
        }
        fs::create_dir_all(&scripts_dir).map_err(|e| {
            format!(
                "Failed to prepare skill scripts directory {}: {e}",
                scripts_dir.display()
            )
        })?;

        let (skill_md_bytes, hud_cli_sh_bytes) =
            load_skill_source_files(paths, plugin_root, skill_name);
        let mut marker_bytes = serde_json::to_vec_pretty(&json!({
            "managedBy": "agy-hud",
            "skill": skill_name,
            "version": INSTALLER_VERSION,
        }))
        .map_err(|e| e.to_string())?;
        marker_bytes.push(b'\n');

        atomic_write_public_bytes(&dest.join("SKILL.md"), &skill_md_bytes, 0o644)
            .map_err(|e| e.to_string())?;
        atomic_write_public_bytes(&scripts_dir.join("hud-cli.sh"), &hud_cli_sh_bytes, 0o755)
            .map_err(|e| e.to_string())?;
        atomic_write_public_bytes(&dest.join(MANAGED_MARKER_FILE), &marker_bytes, 0o644)
            .map_err(|e| e.to_string())?;

        statuses[idx] = "installed";
    }

    Ok(json!({
        "skillsDir": canonicalize_with_tail(&skills_dir).to_string_lossy(),
        "hud": statuses[0],
        "hudUpdate": statuses[1],
    }))
}

pub fn detect_installed_binary_version(paths: &AgyPaths) -> Option<String> {
    let meta_path = paths.hud_home.join("install-meta.json");
    if let Some(raw_meta) = read_bounded_string(&meta_path, 16 * 1024)
        && let Ok(val) = serde_json::from_str::<Value>(&raw_meta)
        && let Some(ver) = val.get("version").and_then(Value::as_str)
        && !ver.trim().is_empty()
    {
        return Some(ver.trim().to_string());
    }
    if paths.installed_bin_path.is_file() {
        let args = vec!["--version".to_string()];
        if let Some(out) = exec_file_safe(&paths.installed_bin_path.to_string_lossy(), &args, None)
            && let Some(ver) = out.split_whitespace().nth(1)
            && !ver.is_empty()
        {
            return Some(ver.to_string());
        }
    }
    None
}

pub fn install_status_line(
    paths: &AgyPaths,
    current_exe: &Path,
    plugin_root: &Path,
    argv: &[String],
) -> Result<Value, String> {
    let replace = argv.iter().any(|a| a == "--replace");
    let explicit_stack = argv.iter().any(|a| a == "--stack");
    let explicit_no_stack = argv.iter().any(|a| a == "--no-stack");
    let explicit_with_title = argv.iter().any(|a| a == "--with-title");
    let explicit_no_title = argv.iter().any(|a| a == "--no-title");
    if explicit_with_title && explicit_no_title {
        return Err("Cannot specify both --with-title and --no-title.".to_string());
    }
    let plugin_flag = argv.iter().any(|a| a == "--plugin-installed");
    let explicit_padding = argv
        .iter()
        .position(|a| a == "--padding")
        .and_then(|idx| argv.get(idx + 1))
        .and_then(|v| v.parse::<u64>().ok());

    let previous_version = detect_installed_binary_version(paths);

    create_private_dir_all(&paths.cli_dir).map_err(|e| e.to_string())?;
    create_private_dir_all(&paths.hud_home).map_err(|e| e.to_string())?;
    create_private_dir_all(&paths.bin_dir).map_err(|e| e.to_string())?;

    let mut settings = read_json_object_or_empty(&paths.cli_settings_path)?;
    let existing_sl = settings
        .get("statusLine")
        .and_then(Value::as_object)
        .cloned();
    let current_cmd = existing_sl
        .as_ref()
        .and_then(|m| m.get("command"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let current_kind = classify_statusline_command(current_cmd);

    let existing_title = settings.get("title").and_then(Value::as_object).cloned();
    let current_title_cmd = existing_title
        .as_ref()
        .and_then(|m| m.get("command"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let current_title_kind = classify_statusline_command(current_title_cmd);

    let enable_title = if explicit_with_title {
        true
    } else if explicit_no_title {
        false
    } else {
        current_title_kind == "agy-hud"
    };

    let stack = if explicit_stack {
        true
    } else if explicit_no_stack {
        false
    } else if current_kind == "agy-hud" {
        existing_sl
            .as_ref()
            .and_then(|m| m.get("stack_with_default"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    } else {
        false
    };

    let padding = if let Some(p) = explicit_padding {
        p
    } else if current_kind == "agy-hud" {
        existing_sl
            .as_ref()
            .and_then(|m| m.get("padding"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    } else {
        0
    };

    if current_kind == "other" && !replace {
        return Err(
            "settings.json already configures another statusLine script. Ask the user first, then re-run `agy-hud install --replace` (a backup is created automatically)."
                .to_string(),
        );
    }
    if enable_title && current_title_kind == "other" && !replace {
        return Err(
            "settings.json already configures another title command. Ask the user first, then re-run `agy-hud install --with-title --replace` (a backup is created automatically)."
                .to_string(),
        );
    }

    // Deploy native Rust binary into `<hudHome>/bin/agy-hud` atomically (`0755`)
    let exe_real = canonical_or_parent(current_exe);
    let target_bin = canonical_or_parent(&paths.installed_bin_path);
    if exe_real != target_bin {
        let exe_bytes = fs::read(&exe_real)
            .map_err(|e| format!("Failed to read binary {}: {e}", exe_real.display()))?;
        atomic_write_bytes(&target_bin, &exe_bytes, 0o755)
            .map_err(|e| format!("Failed to deploy binary to {}: {e}", target_bin.display()))?;
    }

    // Load any prior install-meta.json so we preserve `pluginInstalled` and tracked backup paths
    let meta_path = paths.hud_home.join("install-meta.json");
    let prev_meta = read_bounded_string(&meta_path, 16 * 1024)
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    let prev_plugin_installed = prev_meta
        .as_ref()
        .and_then(|m| m.get("pluginInstalled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut tracked_backups: BTreeSet<String> = prev_meta
        .as_ref()
        .and_then(|m| m.get("backups"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let mut backup_path: Option<String> = None;
    if paths.cli_settings_path.exists() {
        let stamp = utc_compact_timestamp();
        let bak = Path::new(&format!(
            "{}.bak.{stamp}",
            paths.cli_settings_path.display()
        ))
        .to_path_buf();
        fs::copy(&paths.cli_settings_path, &bak).map_err(|e| e.to_string())?;
        let bak_str = canonical_or_parent(&bak).to_string_lossy().into_owned();
        tracked_backups.insert(bak_str);
        backup_path = Some(bak.to_string_lossy().into_owned());
    }

    // Record version, plugin root, pluginInstalled state, and tracked backups for clean `uninstall`
    let meta_bytes = serde_json::to_vec_pretty(&json!({
        "version": INSTALLER_VERSION,
        "pluginRoot": canonical_or_parent(plugin_root).to_string_lossy(),
        "pluginInstalled": prev_plugin_installed || plugin_flag,
        "backups": tracked_backups,
    }))
    .unwrap_or_default();
    let _ = atomic_write_bytes(&meta_path, &meta_bytes, 0o600);

    if current_kind == "other"
        && let Some(prev_val) = settings.get("statusLine")
    {
        let prev_bytes = serde_json::to_vec_pretty(prev_val).map_err(|e| e.to_string())?;
        atomic_write_bytes(&paths.previous_statusline_path, &prev_bytes, 0o600)
            .map_err(|e| e.to_string())?;
    }
    if enable_title
        && current_title_kind == "other"
        && let Some(prev_title_val) = settings.get("title")
    {
        let prev_bytes = serde_json::to_vec_pretty(prev_title_val).map_err(|e| e.to_string())?;
        atomic_write_bytes(&paths.previous_title_path, &prev_bytes, 0o600)
            .map_err(|e| e.to_string())?;
    }

    let (command_str, command_warning) =
        format_statusline_command(&paths.installed_bin_path.to_string_lossy());
    let mut next_sl = if current_kind == "agy-hud" {
        existing_sl.unwrap_or_default()
    } else {
        Map::new()
    };
    next_sl.insert("type".to_string(), Value::String("command".to_string()));
    next_sl.insert("command".to_string(), Value::String(command_str.clone()));
    next_sl.insert("enabled".to_string(), Value::Bool(true));
    next_sl.insert("padding".to_string(), Value::Number(padding.into()));
    next_sl.insert("stack_with_default".to_string(), Value::Bool(stack));

    settings.insert("statusLine".to_string(), Value::Object(next_sl));

    let title_command_report: Option<String> = if enable_title {
        let (title_cmd, _) = format_title_command(&paths.installed_bin_path.to_string_lossy());
        let mut next_title = if current_title_kind == "agy-hud" {
            existing_title.unwrap_or_default()
        } else {
            Map::new()
        };
        next_title.insert("type".to_string(), Value::String("command".to_string()));
        next_title.insert("command".to_string(), Value::String(title_cmd.clone()));
        next_title.insert("enabled".to_string(), Value::Bool(true));
        settings.insert("title".to_string(), Value::Object(next_title));
        Some(title_cmd)
    } else {
        if explicit_no_title && current_title_kind == "agy-hud" {
            if let Some(raw) = read_bounded_string(&paths.previous_title_path, 64 * 1024)
                && let Ok(prev_val) = serde_json::from_str::<Value>(&raw)
            {
                settings.insert("title".to_string(), prev_val);
            } else {
                settings.remove("title");
            }
            let _ = fs::remove_file(&paths.previous_title_path);
        }
        None
    };

    let target_settings = canonical_or_parent(&paths.cli_settings_path);
    let mode = file_mode_or_default(&target_settings, 0o600);
    let mut out_bytes = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
    out_bytes.push(b'\n');
    atomic_write_bytes(&target_settings, &out_bytes, mode).map_err(|e| e.to_string())?;

    let unprefixed_skills = install_unprefixed_skills(paths, plugin_root)?;

    let mut report = json!({
        "installed": true,
        "version": INSTALLER_VERSION,
        "previousVersion": previous_version,
        "engine": "rust-native",
        "settingsPath": paths.cli_settings_path.to_string_lossy(),
        "binaryPath": paths.installed_bin_path.to_string_lossy(),
        "command": command_str,
        "stackWithDefault": stack,
        "replaced": current_kind,
        "titleConfigured": enable_title,
        "titleCommand": title_command_report,
        "backupPath": backup_path,
        "unprefixedSkills": unprefixed_skills,
    });
    if let Some(warn_msg) = command_warning
        && let Some(obj) = report.as_object_mut()
    {
        obj.insert("warning".to_string(), Value::String(warn_msg));
    }
    Ok(report)
}

pub fn uninstall_status_line(paths: &AgyPaths, argv: &[String]) -> Result<Value, String> {
    let keep_config = argv.iter().any(|a| a == "--keep-config");
    let reset_default = argv.iter().any(|a| a == "--reset-default");
    let force_plugin_uninstall = argv.iter().any(|a| a == "--plugin");

    let meta_path = paths.hud_home.join("install-meta.json");
    let meta = read_bounded_string(&meta_path, 16 * 1024)
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    let plugin_installed = meta
        .as_ref()
        .and_then(|m| m.get("pluginInstalled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tracked_backups: Vec<String> = meta
        .as_ref()
        .and_then(|m| m.get("backups"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    // 1. Restore `~/.gemini/antigravity-cli/settings.json` cleanly to official default (or prior third-party bar/title)
    let mut restored_previous = false;
    let status_line_action: &'static str;
    let title_action: &'static str;

    if paths.cli_settings_path.exists() {
        let mut settings = read_json_object_or_empty(&paths.cli_settings_path)?;
        let mut settings_changed = false;

        let current_cmd = settings
            .get("statusLine")
            .and_then(Value::as_object)
            .and_then(|m| m.get("command"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let current_kind = classify_statusline_command(current_cmd);

        if current_kind == "agy-hud" {
            if !reset_default
                && paths.previous_statusline_path.exists()
                && let Ok(raw) = fs::read_to_string(&paths.previous_statusline_path)
                && let Ok(prev_val) = serde_json::from_str::<Value>(&raw)
            {
                settings.insert("statusLine".to_string(), prev_val);
                restored_previous = true;
                status_line_action = "restored-previous";
            } else {
                // Removing `statusLine` completely restores Antigravity CLI's official built-in default status bar
                settings.remove("statusLine");
                status_line_action = "restored-official-default";
            }
            settings_changed = true;
        } else if current_kind == "none" {
            status_line_action = "already-official-default";
        } else {
            // Foreign statusLine configured manually by user — never clobber!
            status_line_action = "kept-foreign";
        }

        let current_title_cmd = settings
            .get("title")
            .and_then(Value::as_object)
            .and_then(|m| m.get("command"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let current_title_kind = classify_statusline_command(current_title_cmd);

        if current_title_kind == "agy-hud" {
            if !reset_default
                && let Some(raw) = read_bounded_string(&paths.previous_title_path, 64 * 1024)
                && let Ok(prev_val) = serde_json::from_str::<Value>(&raw)
            {
                settings.insert("title".to_string(), prev_val);
                title_action = "restored-previous";
            } else {
                settings.remove("title");
                title_action = "restored-official-default";
            }
            settings_changed = true;
        } else if current_title_kind == "none" {
            title_action = "already-official-default";
        } else {
            // Foreign title configured manually by user — never clobber!
            title_action = "kept-foreign";
        }

        if settings_changed {
            let target_settings = canonical_or_parent(&paths.cli_settings_path);
            let mode = file_mode_or_default(&target_settings, 0o600);
            let mut out_bytes = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
            out_bytes.push(b'\n');
            atomic_write_bytes(&target_settings, &out_bytes, mode).map_err(|e| e.to_string())?;
        }
    } else {
        status_line_action = "settings-not-present";
        title_action = "settings-not-present";
    }

    // 2. Remove `.bak.<timestamp>` files created by `agy-hud install` (only tracked ones inside cli_dir)
    let canon_cli_dir = canonical_or_parent(&paths.cli_dir);
    let mut removed_backups = 0usize;
    for bak_str in &tracked_backups {
        let bak_path = Path::new(bak_str);
        if bak_path.parent() == Some(canon_cli_dir.as_path())
            && bak_path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("settings.json.bak."))
            && fs::remove_file(bak_path).is_ok()
        {
            removed_backups += 1;
        }
    }

    // 3. Unregister Antigravity plugin (`agy plugin uninstall agy-hud`) if registered
    let skip_plugin = std::env::var("AGY_SKIP_PLUGIN_INSTALL").as_deref() == Ok("1");
    let mut plugin_uninstalled = false;
    if (plugin_installed || force_plugin_uninstall) && !skip_plugin {
        let args = vec![
            "plugin".to_string(),
            "uninstall".to_string(),
            "agy-hud".to_string(),
        ];
        plugin_uninstalled = exec_file_safe("agy", &args, None).is_some();
    }

    // 4. Clean up `<hudHome>` (`~/.gemini/antigravity-cli/agy-hud`)
    let canon_hud_home = canonical_or_parent(&paths.hud_home);
    let canon_home = canonical_or_parent(&paths.home);
    let safe_distinct_hud_home = canon_hud_home != canon_cli_dir && canon_hud_home != canon_home;

    let mut removed_hud_home = false;
    if paths.hud_home.exists() && safe_distinct_hud_home {
        for subdir in ["bin", "plugin", "probe-cache", "data"] {
            let _ = fs::remove_dir_all(paths.hud_home.join(subdir));
        }
        for file in [
            "heartbeat.json",
            "install-meta.json",
            "previous-statusline.json",
            "previous-title.json",
            "config.undo.json",
        ] {
            let _ = fs::remove_file(paths.hud_home.join(file));
        }
        if !keep_config {
            let _ = fs::remove_file(&paths.config_path);
            removed_hud_home = fs::remove_dir_all(&paths.hud_home).is_ok();
        }
    }

    // 5. Clean up managed unprefixed shortcut skills under `<gemini_config_dir>/skills/{hud,hud-update}`
    let skills_dir = paths.gemini_config_dir.join("skills");
    let mut unprefixed_skills_removed: Vec<&'static str> = Vec::new();
    for skill_name in MANAGED_SKILL_NAMES {
        let skill_dir = skills_dir.join(skill_name);
        if inspect_dest_state(&skill_dir, skill_name) == DestState::Managed
            && fs::remove_dir_all(&skill_dir).is_ok()
        {
            unprefixed_skills_removed.push(skill_name);
        }
    }
    if let Ok(meta) = fs::symlink_metadata(&skills_dir)
        && !meta.file_type().is_symlink()
        && meta.file_type().is_dir()
    {
        let _ = fs::remove_dir(&skills_dir);
    }

    Ok(json!({
        "uninstalled": true,
        "settingsPath": paths.cli_settings_path.to_string_lossy(),
        "statusLineAction": status_line_action,
        "titleAction": title_action,
        "restoredPrevious": restored_previous,
        "removedBackups": removed_backups,
        "pluginUninstalled": plugin_uninstalled,
        "removedHudHome": removed_hud_home,
        "keptConfig": keep_config,
        "unprefixedSkillsRemoved": unprefixed_skills_removed,
    }))
}
