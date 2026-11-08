use crate::paths::{AgyPaths, atomic_write_bytes, canonical_or_parent, create_private_dir_all};
use hud_core::config::HudConfig;
use hud_core::probes::{
    parse_git_porcelain_v2, parse_hg_probe_output, parse_jj_probe_output,
};
use hud_core::transcript::parse_transcript_and_tasks;
use hud_core::types::{
    AgyStdinData, LogicPaths, MemoryProbeInfo, ProbeAnswers, TranscriptSummary, VcsProbeInfo,
    VcsStdin, VcsType,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{IsTerminal, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

pub const MAX_STDIN_BYTES: usize = 256 * 1024;
pub const HEAD_READ_BYTES: usize = 8 * 1024;
pub const TAIL_READ_BYTES: u64 = 256 * 1024;
pub const MAX_TASK_MD_BYTES: u64 = 64 * 1024;
pub const PROBE_TIMEOUT_MS: u64 = 1_200;
pub const PROBE_MAX_BUFFER: u64 = 512 * 1024;

pub const GIT_SAFE_PREFIX: &[&str] = &[
    "-c",
    "core.quotePath=false",
    "--no-optional-locks",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "protocol.allow=never",
];

pub const JJ_PROBE_ARGS: &[&str] = &[
    "--ignore-working-copy",
    "--at-operation=@",
    "--no-pager",
    "log",
    "-r",
    "@",
    "--no-graph",
    "--color",
    "never",
    "-T",
    "change_id.shortest(8) ++ \"\\x1f\" ++ self.local_bookmarks().map(|b| b.name()).join(\"\\x1e\") ++ \"\\x1f\" ++ if(self.empty(), \"0\", \"1\") ++ \"\\x1f\" ++ if(self.conflict(), \"1\", \"0\")",
];

pub const HG_SAFE_PREFIX: &[&str] = &[
    "--config",
    "ui.interactive=False",
    "--config",
    "extensions.fsmonitor=!",
];

pub trait ProbeRunner: Sync {
    fn run(&self, cmd: &str, args: &[String], cwd: Option<&Path>) -> Option<String>;
}

pub struct SystemProbeRunner;

impl ProbeRunner for SystemProbeRunner {
    fn run(&self, cmd: &str, args: &[String], cwd: Option<&Path>) -> Option<String> {
        exec_file_safe(cmd, args, cwd)
    }
}

impl<F> ProbeRunner for F
where
    F: Fn(&str, &[String], Option<&Path>) -> Option<String> + Sync,
{
    fn run(&self, cmd: &str, args: &[String], cwd: Option<&Path>) -> Option<String> {
        self(cmd, args, cwd)
    }
}

/// Reads bounded JSON from `stdin` when piped (`!is_terminal()`).
pub fn read_stdin_payload() -> Option<AgyStdinData> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut buf = Vec::with_capacity(4096);
    if stdin
        .lock()
        .take((MAX_STDIN_BYTES + 1) as u64)
        .read_to_end(&mut buf)
        .is_err()
    {
        return None;
    }
    if buf.is_empty() || buf.len() > MAX_STDIN_BYTES {
        return None;
    }
    serde_json::from_slice::<AgyStdinData>(&buf).ok()
}

pub fn read_bounded_string(path: &Path, max_bytes: u64) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > max_bytes {
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

/// Bounded `O(1)` head (`8 KB`) + tail (`256 KB`) reader for multi-megabyte `transcript.jsonl`.
pub fn read_transcript_slices(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(meta) = fs::metadata(path) else {
        return (None, None);
    };
    if !meta.is_file() || meta.len() == 0 {
        return (None, None);
    }
    let Ok(mut f) = File::open(path) else {
        return (None, None);
    };

    let head_len = (meta.len() as usize).min(HEAD_READ_BYTES);
    let mut head_buf = vec![0u8; head_len];
    let head_line = if let Ok(n) = f.read(&mut head_buf) {
        let slice = &head_buf[..n];
        let end = slice.iter().position(|&b| b == b'\n').unwrap_or(n);
        Some(String::from_utf8_lossy(&slice[..end]).into_owned())
    } else {
        None
    };

    let offset = meta.len().saturating_sub(TAIL_READ_BYTES);
    if f.seek(SeekFrom::Start(offset)).is_err() {
        return (head_line, None);
    }
    let tail_cap = (meta.len() - offset) as usize;
    let mut tail_buf = Vec::with_capacity(tail_cap);
    if f.take(TAIL_READ_BYTES + 1).read_to_end(&mut tail_buf).is_err() {
        return (head_line, None);
    }
    let lossy = String::from_utf8_lossy(&tail_buf);
    let tail_str = if offset > 0 {
        match lossy.find('\n') {
            Some(nl) => lossy[nl + 1..].to_string(),
            None => String::new(),
        }
    } else {
        lossy.into_owned()
    };

    (head_line, Some(tail_str))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectedRepo {
    Git {
        root: PathBuf,
        git_dir: PathBuf,
        common_dir: PathBuf,
    },
    Jj {
        root: PathBuf,
        meta_dir: PathBuf,
    },
    Hg {
        root: PathBuf,
        meta_dir: PathBuf,
    },
}

impl DetectedRepo {
    pub fn root(&self) -> &Path {
        match self {
            Self::Git { root, .. } | Self::Jj { root, .. } | Self::Hg { root, .. } => root,
        }
    }

    pub fn vcs_type(&self) -> VcsType {
        match self {
            Self::Git { .. } => VcsType::Git,
            Self::Jj { .. } => VcsType::Jj,
            Self::Hg { .. } => VcsType::Hg,
        }
    }
}

fn resolve_git_worktree_dirs(repo_dir: &Path, dot_git_file: &Path) -> Option<(PathBuf, PathBuf)> {
    let raw = read_bounded_string(dot_git_file, 4096)?;
    let trimmed = raw.trim();
    let rest = if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("gitdir:") {
        trimmed[7..].trim()
    } else {
        return None;
    };
    if rest.is_empty() {
        return None;
    }
    let candidate = Path::new(rest);
    let joined_git = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        repo_dir.join(candidate)
    };
    let git_dir = canonical_or_parent(&joined_git);

    let commondir_file = git_dir.join("commondir");
    let common_dir = if let Some(rel_common) = read_bounded_string(&commondir_file, 4096) {
        let rel = rel_common.trim();
        if rel.is_empty() {
            git_dir.clone()
        } else {
            let cp = Path::new(rel);
            let joined = if cp.is_absolute() {
                cp.to_path_buf()
            } else {
                git_dir.join(cp)
            };
            canonical_or_parent(&joined)
        }
    } else {
        git_dir.clone()
    };

    Some((git_dir, common_dir))
}

/// Walks upward from `cwd` (up to 64 levels) to locate the nearest enclosing VCS root.
/// Checks `.jj` before `.git` at the same level so colocated Jujutsu repos resolve as `jj`,
/// and ignores symlinked VCS markers via `symlink_metadata`.
pub fn detect_vcs_repo(cwd: &Path) -> Option<DetectedRepo> {
    let resolved = fs::canonicalize(cwd).ok()?;
    let meta = fs::symlink_metadata(&resolved).ok()?;
    if !meta.file_type().is_dir() {
        return None;
    }

    let mut cur: Option<&Path> = Some(resolved.as_path());
    for _ in 0..64 {
        let dir = cur?;

        let dot_jj = dir.join(".jj");
        if let Ok(st) = fs::symlink_metadata(&dot_jj)
            && st.file_type().is_dir()
        {
            return Some(DetectedRepo::Jj {
                root: dir.to_path_buf(),
                meta_dir: dot_jj,
            });
        }

        let dot_git = dir.join(".git");
        if let Ok(st) = fs::symlink_metadata(&dot_git) {
            if st.file_type().is_dir() {
                return Some(DetectedRepo::Git {
                    root: dir.to_path_buf(),
                    git_dir: dot_git.clone(),
                    common_dir: dot_git,
                });
            }
            if st.file_type().is_file()
                && let Some((git_dir, common_dir)) = resolve_git_worktree_dirs(dir, &dot_git)
            {
                return Some(DetectedRepo::Git {
                    root: dir.to_path_buf(),
                    git_dir,
                    common_dir,
                });
            }
        }

        let dot_hg = dir.join(".hg");
        if let Ok(st) = fs::symlink_metadata(&dot_hg)
            && st.file_type().is_dir()
        {
            return Some(DetectedRepo::Hg {
                root: dir.to_path_buf(),
                meta_dir: dot_hg,
            });
        }

        cur = dir.parent().filter(|p| *p != dir);
    }
    None
}

fn stat_token(target: &Path) -> String {
    let Ok(st) = fs::metadata(target) else {
        return "-".to_string();
    };
    let mtime_ns = st
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{mtime_ns}:{}", st.len())
}

/// Computes a cheap `O(1)` stat fingerprint of VCS state files so commits, checkouts, staging,
/// `jj` operations, or `hg` updates immediately invalidate cached VCS status.
pub fn compute_vcs_signature(repo: &DetectedRepo) -> String {
    match repo {
        DetectedRepo::Git {
            git_dir,
            common_dir,
            ..
        } => {
            let head_path = git_dir.join("HEAD");
            let mut ref_token = "-".to_string();
            if let Some(head_text) = read_bounded_string(&head_path, 4096)
                && let Some(ref_rel) = head_text.trim().strip_prefix("ref:")
            {
                let rel = ref_rel.trim();
                if !rel.is_empty() {
                    ref_token = stat_token(&common_dir.join(rel));
                }
            }
            format!(
                "git|{}|{}|{}|{}",
                stat_token(&head_path),
                stat_token(&git_dir.join("index")),
                stat_token(&common_dir.join("packed-refs")),
                ref_token,
            )
        }
        DetectedRepo::Jj { meta_dir, .. } => format!(
            "jj|{}|{}",
            stat_token(&meta_dir.join("working_copy").join("checkout")),
            stat_token(&meta_dir.join("repo").join("op_heads").join("heads")),
        ),
        DetectedRepo::Hg { meta_dir, .. } => format!(
            "hg|{}|{}|{}",
            stat_token(&meta_dir.join("dirstate")),
            stat_token(&meta_dir.join("branch")),
            stat_token(&meta_dir.join("bookmarks")),
        ),
    }
}

/// Extracts `[filter "<name>"]` / `[filter.<name>]` driver names from `.git/config` so `probe_git`
/// can neutralize untrusted repo-local clean/smudge/process filters via `-c` overrides.
pub fn extract_git_filter_names(config_text: &str) -> Vec<String> {
    let mut names = BTreeSet::new();
    for raw_line in config_text.lines() {
        let line = raw_line.trim();
        if !line.starts_with('[') {
            continue;
        }
        let Some(close) = line.find(']') else {
            continue;
        };
        let header = line[1..close].trim();
        if header.len() < 6 || !header[..6].eq_ignore_ascii_case("filter") {
            continue;
        }
        let rest = header[6..].trim();
        let extracted = if let Some(after_quote) = rest.strip_prefix('"') {
            after_quote.strip_suffix('"').map(str::trim)
        } else {
            rest.strip_prefix('.').map(str::trim)
        };
        if let Some(name) = extracted
            && !name.is_empty()
            && !name.chars().any(|c| c.is_control() || c == '\0')
        {
            names.insert(name.to_string());
        }
    }
    names.into_iter().collect()
}

pub fn build_git_filter_neutralizers(git_dir: &Path, common_dir: &Path) -> Vec<String> {
    let mut names = BTreeSet::new();
    for cfg_path in [git_dir.join("config"), common_dir.join("config")] {
        if let Some(text) = read_bounded_string(&cfg_path, 128 * 1024) {
            for n in extract_git_filter_names(&text) {
                names.insert(n);
            }
        }
    }

    let mut args = Vec::with_capacity(names.len() * 8);
    for name in names {
        for suffix in ["clean=", "smudge=", "process=", "required=false"] {
            args.push("-c".to_string());
            args.push(format!("filter.{name}.{suffix}"));
        }
    }
    args
}

/// Timeout-bounded, shell-free subprocess runner (`1200ms` hard cap, `512 KB` stdout cap).
pub fn exec_file_safe(cmd: &str, args: &[String], cwd: Option<&Path>) -> Option<String> {
    let mut command = Command::new(cmd);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear();

    for key in ["PATH", "HOME", "USERPROFILE", "SYSTEMROOT", "WINDIR"] {
        if let Ok(val) = std::env::var(key) {
            command.env(key, val);
        }
    }
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("HGPLAIN", "1")
        .env("LC_ALL", "C");

    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    let mut child = command.spawn().ok()?;
    let mut stdout_pipe = child.stdout.take()?;

    let reader_handle = thread::spawn(move || {
        let mut buf = Vec::with_capacity(4096);
        let _ = stdout_pipe
            .by_ref()
            .take(PROBE_MAX_BUFFER + 1)
            .read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + Duration::from_millis(PROBE_TIMEOUT_MS);
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(Duration::from_millis(4));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };

    let status = exit_status?;
    if !status.success() {
        return None;
    }
    let buf = reader_handle.join().ok()?;
    if (buf.len() as u64) > PROBE_MAX_BUFFER {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

pub fn probe_git(
    root: &Path,
    git_dir: &Path,
    common_dir: &Path,
    runner: &dyn ProbeRunner,
) -> Option<VcsProbeInfo> {
    let mut status_args: Vec<String> = GIT_SAFE_PREFIX.iter().map(|s| (*s).to_string()).collect();
    status_args.extend(build_git_filter_neutralizers(git_dir, common_dir));
    status_args.extend([
        "status".to_string(),
        "--porcelain=v2".to_string(),
        "--branch".to_string(),
        "--ignore-submodules=dirty".to_string(),
    ]);

    let out = runner.run("git", &status_args, Some(root))?;
    let detached_rev = if out.lines().any(|l| l.trim() == "# branch.head (detached)") {
        let mut rev_args: Vec<String> = GIT_SAFE_PREFIX.iter().map(|s| (*s).to_string()).collect();
        rev_args.extend([
            "rev-parse".to_string(),
            "--short".to_string(),
            "HEAD".to_string(),
        ]);
        runner.run("git", &rev_args, Some(root))
    } else {
        None
    };

    parse_git_porcelain_v2(&out, detached_rev.as_deref())
}

pub fn probe_jj(root: &Path, runner: &dyn ProbeRunner) -> Option<VcsProbeInfo> {
    let args: Vec<String> = JJ_PROBE_ARGS.iter().map(|s| (*s).to_string()).collect();
    let out = runner.run("jj", &args, Some(root))?;
    parse_jj_probe_output(&out)
}

pub fn probe_hg(root: &Path, runner: &dyn ProbeRunner) -> Option<VcsProbeInfo> {
    let mut branch_args: Vec<String> = HG_SAFE_PREFIX.iter().map(|s| (*s).to_string()).collect();
    branch_args.push("branch".to_string());

    let mut status_args: Vec<String> = HG_SAFE_PREFIX.iter().map(|s| (*s).to_string()).collect();
    status_args.extend(["status".to_string(), "-mardu".to_string()]);

    let (branch_out, status_out) = thread::scope(|s| {
        let b = s.spawn(|| runner.run("hg", &branch_args, Some(root)));
        let st = s.spawn(|| runner.run("hg", &status_args, Some(root)));
        (b.join().ok().flatten(), st.join().ok().flatten())
    });

    parse_hg_probe_output(branch_out.as_deref()?, status_out.as_deref())
}

pub fn probe_vcs_uncached(repo: &DetectedRepo, runner: &dyn ProbeRunner) -> Option<VcsProbeInfo> {
    match repo {
        DetectedRepo::Jj { root, .. } => probe_jj(root, runner),
        DetectedRepo::Hg { root, .. } => probe_hg(root, runner),
        DetectedRepo::Git {
            root,
            git_dir,
            common_dir,
        } => probe_git(root, git_dir, common_dir, runner),
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShardedCacheEntry {
    repo_root: String,
    sig: String,
    at: i64,
    expires_at: i64,
    branch: String,
    dirty: bool,
    value: String,
}

/// Sharded per-workspace VCS cache (`<hudHome>/probe-cache/<fnv64>.json`) with `O(1)` stat signature
/// invalidation, `stdin.vcs` cross-checking, and `ttl_ms` expiration.
pub fn probe_vcs_cached(
    cwd: &Path,
    paths: &AgyPaths,
    ttl_ms: u64,
    stdin_vcs: Option<&VcsStdin>,
    now_ms: i64,
    runner: &dyn ProbeRunner,
) -> Option<String> {
    let repo = detect_vcs_repo(cwd)?;
    let sig = compute_vcs_signature(&repo);
    let cache_file = paths.probe_cache_file_for(repo.root());
    let repo_root_str = repo.root().to_string_lossy().into_owned();

    if ttl_ms > 0
        && let Some(raw_cache) = read_bounded_string(&cache_file, 64 * 1024)
        && let Ok(entry) = serde_json::from_str::<ShardedCacheEntry>(&raw_cache)
        && entry.repo_root == repo_root_str
        && entry.sig == sig
        && now_ms < entry.expires_at
    {
        let branch_matches = match stdin_vcs.and_then(|v| v.branch.as_deref()) {
            Some(sb) if !sb.is_empty() && !entry.branch.is_empty() => sb == entry.branch,
            _ => true,
        };
        let dirty_matches = match stdin_vcs.and_then(|v| v.dirty) {
            Some(sd) => sd == entry.dirty,
            None => true,
        };
        if branch_matches && dirty_matches {
            return Some(entry.value);
        }
    }

    let fresh = probe_vcs_uncached(&repo, runner)?;
    let fresh_json = serde_json::to_string(&fresh).ok()?;

    if ttl_ms > 0 {
        let entry = ShardedCacheEntry {
            repo_root: repo_root_str,
            sig,
            at: now_ms,
            expires_at: now_ms.saturating_add(ttl_ms as i64),
            branch: fresh.branch,
            dirty: fresh.dirty,
            value: fresh_json.clone(),
        };
        if let Ok(bytes) = serde_json::to_vec(&entry) {
            let _ = create_private_dir_all(&paths.probe_cache_dir);
            let _ = atomic_write_bytes(&cache_file, &bytes, 0o600);
        }
    }

    Some(fresh_json)
}

pub fn probe_memory(runner: &dyn ProbeRunner) -> Option<String> {
    if cfg!(target_os = "linux")
        && let Some(meminfo) = read_bounded_string(Path::new("/proc/meminfo"), 64 * 1024)
    {
        let mut total_kb: Option<u64> = None;
        let mut avail_kb: Option<u64> = None;
        for line in meminfo.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                total_kb = rest.split_whitespace().next().and_then(|s| s.parse().ok());
            } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                avail_kb = rest.split_whitespace().next().and_then(|s| s.parse().ok());
            }
        }
        if let (Some(tot), Some(avail)) = (total_kb, avail_kb)
            && tot > 0
        {
            let total_bytes = tot * 1024;
            let used_bytes = total_bytes.saturating_sub(avail * 1024);
            let used_percent = (((used_bytes as f64) / (total_bytes as f64)) * 100.0)
                .round()
                .clamp(0.0, 100.0) as u64;
            return serde_json::to_string(&MemoryProbeInfo {
                total_bytes,
                used_bytes,
                used_percent,
            })
            .ok();
        }
    }

    if cfg!(target_os = "macos") {
        let sysctl_args = vec!["-n".to_string(), "hw.memsize".to_string()];
        let total_bytes = runner
            .run("sysctl", &sysctl_args, None)
            .and_then(|s| s.trim().parse::<u64>().ok())?;
        if total_bytes == 0 {
            return None;
        }
        let vm_out = runner.run("vm_stat", &[], None)?;
        let mut page_size = 16_384u64;
        let mut active = 0u64;
        let mut wired = 0u64;
        let mut compressed = 0u64;

        for line in vm_out.lines() {
            if let Some(idx) = line.find("page size of ") {
                let after = &line[idx + "page size of ".len()..];
                if let Some(num_str) = after.split_whitespace().next()
                    && let Ok(ps) = num_str.parse::<u64>()
                {
                    page_size = ps;
                }
            } else if let Some((label, raw_val)) = line.split_once(':') {
                let num = raw_val
                    .trim()
                    .trim_end_matches('.')
                    .parse::<u64>()
                    .unwrap_or(0);
                match label.trim() {
                    "Pages active" => active = num,
                    "Pages wired down" => wired = num,
                    "Pages occupied by compressor" => compressed = num,
                    _ => {}
                }
            }
        }
        let used_pages = active.saturating_add(wired).saturating_add(compressed);
        if used_pages > 0 {
            let used_bytes = total_bytes.min(used_pages.saturating_mul(page_size));
            let used_percent = (((used_bytes as f64) / (total_bytes as f64)) * 100.0)
                .round()
                .clamp(0.0, 100.0) as u64;
            return serde_json::to_string(&MemoryProbeInfo {
                total_bytes,
                used_bytes,
                used_percent,
            })
            .ok();
        }
    }

    None
}

pub struct CollectedRenderData {
    pub transcript: TranscriptSummary,
    pub probes: ProbeAnswers,
}

/// Collects `transcript.jsonl`, `task.md`, VCS status, and host RAM concurrently via `std::thread::scope`.
pub fn collect_render_inputs(
    paths: &AgyPaths,
    logic_paths: &LogicPaths,
    config: &HudConfig,
    stdin: &AgyStdinData,
    now_ms: i64,
    runner: &dyn ProbeRunner,
) -> CollectedRenderData {
    let cwd = logic_paths.cwd.as_deref().map(Path::new);
    let transcript_path = logic_paths.transcript.as_deref().map(Path::new);
    let task_md_path = logic_paths.task_md.as_deref().map(Path::new);
    thread::scope(|s| {
        let tx_handle = s.spawn(|| {
            let (head, tail) = transcript_path
                .map(read_transcript_slices)
                .unwrap_or((None, None));
            let task_md = task_md_path.and_then(|p| read_bounded_string(p, MAX_TASK_MD_BYTES));
            parse_transcript_and_tasks(
                head.as_deref(),
                tail.as_deref(),
                task_md.as_deref(),
                Some(stdin),
            )
        });

        let vcs_handle = s.spawn(|| {
            if !config.vcs.enabled {
                return None;
            }
            let dir = cwd?;
            probe_vcs_cached(
                dir,
                paths,
                config.vcs.cache_ttl_ms,
                stdin.vcs.as_ref(),
                now_ms,
                runner,
            )
        });

        let mem_handle = s.spawn(|| {
            if !config.display.show_memory {
                return None;
            }
            probe_memory(runner)
        });

        CollectedRenderData {
            transcript: tx_handle.join().unwrap_or_default(),
            probes: ProbeAnswers {
                vcs: vcs_handle.join().ok().flatten(),
                memory: mem_handle.join().ok().flatten(),
            },
        }
    })
}
