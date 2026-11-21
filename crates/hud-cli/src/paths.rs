use hud_core::stdin::CompactionScope;
use hud_core::types::{AgyStdinData, LogicPaths};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct AgyPaths {
    pub home: PathBuf,
    pub cli_dir: PathBuf,
    pub hud_home: PathBuf,
    pub gemini_config_dir: PathBuf,
    pub cli_settings_path: PathBuf,
    pub bin_dir: PathBuf,
    pub installed_bin_path: PathBuf,
    pub data_dir: PathBuf,
    pub quota_cache_path: PathBuf,
    pub update_cache_path: PathBuf,
    pub cost_ledger_path: PathBuf,
    pub speed_cache_dir: PathBuf,
    pub transcript_cursor_dir: PathBuf,
    pub compaction_ceilings_dir: PathBuf,
    pub db_ceilings_dir: PathBuf,
    pub probe_cache_dir: PathBuf,
    pub config_path: PathBuf,
    pub undo_path: PathBuf,
    pub heartbeat_path: PathBuf,
    pub previous_statusline_path: PathBuf,
}

fn expand_tilde(raw: &str, home: &Path) -> PathBuf {
    if raw == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return home.join(rest);
    }
    PathBuf::from(raw)
}

fn resolve_abs(p: PathBuf) -> PathBuf {
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    }
}

impl AgyPaths {
    pub fn from_env_map(env: &HashMap<String, String>) -> Self {
        let home_str = if cfg!(windows) {
            env.get("USERPROFILE").or_else(|| env.get("HOME"))
        } else {
            env.get("HOME").or_else(|| env.get("USERPROFILE"))
        }
        .cloned()
        .unwrap_or_else(|| ".".to_string());
        let home = PathBuf::from(home_str);

        let cli_dir = env
            .get("AGY_CLI_DIR")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| resolve_abs(expand_tilde(s, &home)))
            .unwrap_or_else(|| home.join(".gemini").join("antigravity-cli"));

        let hud_home = env
            .get("AGY_HUD_HOME")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| resolve_abs(expand_tilde(s, &home)))
            .unwrap_or_else(|| cli_dir.join("agy-hud"));

        let gemini_config_dir = env
            .get("AGY_CONFIG_DIR")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| resolve_abs(expand_tilde(s, &home)))
            .unwrap_or_else(|| home.join(".gemini").join("config"));

        let bin_name = if cfg!(windows) {
            "agy-hud.exe"
        } else {
            "agy-hud"
        };
        let bin_dir = hud_home.join("bin");

        let data_dir = hud_home.join("data");

        Self {
            home,
            cli_settings_path: cli_dir.join("settings.json"),
            installed_bin_path: bin_dir.join(bin_name),
            bin_dir,
            quota_cache_path: data_dir.join("quota-cache.json"),
            update_cache_path: data_dir.join("update-cache.json"),
            cost_ledger_path: data_dir.join("cost-ledger.json"),
            speed_cache_dir: data_dir.join("speed-cache"),
            transcript_cursor_dir: data_dir.join("transcript-cursors"),
            compaction_ceilings_dir: data_dir.join("compaction-ceilings"),
            db_ceilings_dir: data_dir.join("db-ceilings"),
            data_dir,
            probe_cache_dir: hud_home.join("probe-cache"),
            config_path: hud_home.join("config.json"),
            undo_path: hud_home.join("config.undo.json"),
            heartbeat_path: hud_home.join("heartbeat.json"),
            previous_statusline_path: hud_home.join("previous-statusline.json"),
            cli_dir,
            hud_home,
            gemini_config_dir,
        }
    }

    pub fn from_process_env() -> Self {
        let map: HashMap<String, String> = std::env::vars().collect();
        Self::from_env_map(&map)
    }

    pub fn probe_cache_file_for(&self, repo_root: &Path) -> PathBuf {
        let key = fnv1a_64_hex(&repo_root.to_string_lossy());
        self.probe_cache_dir.join(format!("{key}.json"))
    }

    pub fn speed_cache_file_for(&self, session_key: &str) -> PathBuf {
        let key = fnv1a_64_hex(session_key);
        self.speed_cache_dir.join(format!("{key}.json"))
    }

    pub fn transcript_cursor_file_for(&self, transcript_path: &Path) -> PathBuf {
        let key = fnv1a_64_hex(&transcript_path.to_string_lossy());
        self.transcript_cursor_dir.join(format!("{key}.json"))
    }

    pub fn compaction_ceiling_file_for(&self, scope: &CompactionScope) -> PathBuf {
        let key = fnv1a_64_hex(&scope.storage_key());
        self.compaction_ceilings_dir.join(format!("{key}.json"))
    }

    pub fn db_ceiling_file_for(&self, db_path: &Path) -> PathBuf {
        db_ceiling_cache_path(&self.data_dir, db_path)
    }
}

/// Returns the cache file path `<data_real_dir>/db-ceilings/<fnv64_hex(db_path)>.json`.
pub fn db_ceiling_cache_path(data_real_dir: &Path, db_path: &Path) -> PathBuf {
    let key = fnv1a_64_hex(&db_path.to_string_lossy());
    data_real_dir
        .join("db-ceilings")
        .join(format!("{key}.json"))
}

/// Deterministic 64-bit FNV-1a hash formatted as 16 lowercase hex digits.
pub fn fnv1a_64_hex(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in input.as_bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Canonicalizes `path` if it exists, or canonicalizes its existing parent directory and joins
/// `file_name` (essential on macOS where `/var/folders` and `/tmp` symlink into `/private/...`).
pub fn canonical_or_parent(path: &Path) -> PathBuf {
    if let Ok(real) = fs::canonicalize(path) {
        return real;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Ok(real_parent) = fs::canonicalize(parent)
    {
        return real_parent.join(name);
    }
    path.to_path_buf()
}

/// Canonicalizes the deepest existing ancestor of `path` and re-attaches any non-existent
/// trailing components (ensuring home abbreviation works before nested dirs exist on macOS).
pub fn canonicalize_with_tail(path: &Path) -> PathBuf {
    if let Ok(real) = fs::canonicalize(path) {
        return real;
    }
    let mut tail = Vec::new();
    let mut cur = Some(path);
    while let Some(p) = cur {
        if let Ok(real) = fs::canonicalize(p) {
            let mut out = real;
            for part in tail.into_iter().rev() {
                out.push(part);
            }
            return out;
        }
        if let Some(name) = p.file_name() {
            tail.push(name.to_os_string());
        } else {
            break;
        }
        cur = p.parent();
    }
    path.to_path_buf()
}

pub fn derive_task_md_path(transcript_path: &Path) -> Option<PathBuf> {
    if !transcript_path.is_absolute() {
        return None;
    }
    let logs_dir = transcript_path.parent()?;
    if logs_dir.file_name().and_then(|s| s.to_str()) == Some("logs")
        && let Some(sys_gen_dir) = logs_dir.parent()
        && sys_gen_dir.file_name().and_then(|s| s.to_str()) == Some(".system_generated")
        && let Some(conv_dir) = sys_gen_dir.parent()
    {
        return Some(conv_dir.join("task.md"));
    }
    Some(logs_dir.join("task.md"))
}

fn is_safe_conv_id(raw: &str) -> bool {
    let s = raw.trim();
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.contains(['/', '\\', '\0'])
        && !s.chars().any(char::is_control)
}

fn extract_conv_id_from_transcript_path(raw_path: &str) -> Option<&str> {
    let norm = raw_path.trim();
    let suffix = "/.system_generated/logs/transcript.jsonl";
    let win_suffix = "\\.system_generated\\logs\\transcript.jsonl";
    let before = norm
        .strip_suffix(suffix)
        .or_else(|| norm.strip_suffix(win_suffix))?;
    let (_, conv_id) = before.rsplit_once(['/', '\\'])?;
    is_safe_conv_id(conv_id).then_some(conv_id)
}

fn extract_transcript_root(raw_path: &str) -> Option<PathBuf> {
    let norm = raw_path.trim();
    let suffix = "/.system_generated/logs/transcript.jsonl";
    let win_suffix = "\\.system_generated\\logs\\transcript.jsonl";
    if let Some(before) = norm
        .strip_suffix(suffix)
        .or_else(|| norm.strip_suffix(win_suffix))
        && let Some((brain_dir, conv_id)) = before.rsplit_once(['/', '\\'])
        && is_safe_conv_id(conv_id)
        && let Some((root_str, last)) = brain_dir.rsplit_once(['/', '\\'])
        && last == "brain"
        && !root_str.is_empty()
    {
        let root_path = PathBuf::from(root_str);
        if root_path.is_absolute() {
            return Some(root_path);
        }
    }

    let p = Path::new(norm);
    if !p.is_absolute() {
        return None;
    }
    for ancestor in p.ancestors().take(6) {
        if ancestor.file_name().and_then(|s| s.to_str()) == Some("brain")
            && let Some(parent) = ancestor.parent()
            && !parent.as_os_str().is_empty()
        {
            return Some(parent.to_path_buf());
        }
    }
    None
}

fn resolve_conv_id(stdin: Option<&AgyStdinData>) -> Option<&str> {
    let raw_tp = stdin
        .and_then(|s| s.transcript_path.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    stdin
        .and_then(|s| s.conversation_id.as_deref())
        .map(str::trim)
        .filter(|id| is_safe_conv_id(id))
        .or_else(|| {
            stdin
                .and_then(|s| s.session_id.as_deref())
                .map(str::trim)
                .filter(|id| is_safe_conv_id(id))
        })
        .or_else(|| raw_tp.and_then(extract_conv_id_from_transcript_path))
}

/// Resolves the live `transcript.jsonl` path, automatically recovering when `agy` CLI reports an
/// IDE-scoped `~/.gemini/antigravity/brain/<id>` path while storing CLI logs under `<cli_dir>/brain/<id>`.
pub fn resolve_transcript_path(paths: &AgyPaths, stdin: Option<&AgyStdinData>) -> Option<PathBuf> {
    let raw_tp = stdin
        .and_then(|s| s.transcript_path.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if let Some(tp_str) = raw_tp {
        let p = Path::new(tp_str);
        if p.is_absolute() && fs::metadata(p).is_ok_and(|m| m.is_file()) {
            return Some(canonical_or_parent(p));
        }
    }

    let conv_id = resolve_conv_id(stdin);

    if let Some(id) = conv_id {
        let rel_tail = Path::new("brain")
            .join(id)
            .join(".system_generated")
            .join("logs")
            .join("transcript.jsonl");
        let candidate_bases = [
            paths.cli_dir.clone(),
            paths.home.join(".gemini").join("antigravity-cli"),
            paths.home.join(".gemini").join("antigravity"),
        ];
        for base in candidate_bases {
            let candidate = base.join(&rel_tail);
            if fs::metadata(&candidate).is_ok_and(|m| m.is_file()) {
                return Some(canonical_or_parent(&candidate));
            }
        }
    }

    if let Some(tp_str) = raw_tp {
        let p = Path::new(tp_str);
        if p.is_absolute() {
            return Some(canonical_or_parent(p));
        }
    }

    conv_id.map(|id| {
        canonical_or_parent(
            &paths
                .cli_dir
                .join("brain")
                .join(id)
                .join(".system_generated")
                .join("logs")
                .join("transcript.jsonl"),
        )
    })
}

/// Resolves the existing SQLite conversation DB `<root>/conversations/<id>.db`, probing
/// `<cli_dir>`, `<transcript_root>`, `~/.gemini/antigravity-cli`, and `~/.gemini/antigravity`.
pub fn resolve_conversation_db_path(
    paths: &AgyPaths,
    stdin: Option<&AgyStdinData>,
) -> Option<PathBuf> {
    let id = resolve_conv_id(stdin)?;
    let db_filename = format!("{id}.db");
    let transcript_root = stdin
        .and_then(|s| s.transcript_path.as_deref())
        .and_then(extract_transcript_root);

    let candidate_bases = [
        Some(paths.cli_dir.clone()),
        transcript_root,
        Some(paths.home.join(".gemini").join("antigravity-cli")),
        Some(paths.home.join(".gemini").join("antigravity")),
    ];

    let mut checked: Vec<PathBuf> = Vec::with_capacity(4);
    for base in candidate_bases.into_iter().flatten() {
        let candidate = base.join("conversations").join(&db_filename);
        if checked.iter().any(|p| p == &candidate) {
            continue;
        }
        checked.push(candidate.clone());
        if fs::metadata(&candidate).is_ok_and(|m| m.is_file()) {
            return Some(canonical_or_parent(&candidate));
        }
    }
    None
}

pub fn create_private_dir_all(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn sweep_symlinks(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if let Ok(meta) = fs::symlink_metadata(&p) {
            if meta.file_type().is_symlink() {
                let _ = fs::remove_file(&p);
            } else if meta.is_dir() {
                sweep_symlinks(&p);
            }
        }
    }
}

const MAINTENANCE_INTERVAL_MS: i64 = 3_600_000;
const CACHE_MAX_AGE_SECS: u64 = 14 * 86_400;
const CACHE_MAX_FILES_PER_DIR: usize = 256;
const MAINTENANCE_STAMP_FILE: &str = ".maintenance-at";

/// Fast `O(1)` hot-path variant of [`prepare_data_dir`] that checks `hud_home` and `data_dir`
/// for planted symlinks and creates private directories without recursive `sweep_symlinks`.
pub fn prepare_data_dir_fast(paths: &AgyPaths) -> Option<PathBuf> {
    if let Ok(meta) = fs::symlink_metadata(&paths.hud_home)
        && meta.file_type().is_symlink()
    {
        return None;
    }
    create_private_dir_all(&paths.hud_home).ok()?;

    if let Ok(meta) = fs::symlink_metadata(&paths.data_dir)
        && meta.file_type().is_symlink()
    {
        let _ = fs::remove_file(&paths.data_dir);
    }
    create_private_dir_all(&paths.data_dir).ok()?;
    fs::canonicalize(&paths.data_dir).ok()
}

/// Ensures `<hudHome>/data` is a real directory free of planted symlinks.
pub fn prepare_data_dir(paths: &AgyPaths) -> Option<PathBuf> {
    let real = prepare_data_dir_fast(paths)?;
    sweep_symlinks(&real);
    Some(real)
}

fn prune_cache_dir(dir: &Path, now_sys: SystemTime) {
    let Ok(dir_meta) = fs::symlink_metadata(dir) else {
        return;
    };
    if dir_meta.file_type().is_symlink() || !dir_meta.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let max_age = std::time::Duration::from_secs(CACHE_MAX_AGE_SECS);
    let mut evictable: Vec<(SystemTime, PathBuf)> = Vec::new();
    let mut lock_count: usize = 0;

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) == Some("lock") {
            lock_count += 1;
            continue;
        }
        let modified = meta.modified().unwrap_or(now_sys);
        if now_sys
            .duration_since(modified)
            .is_ok_and(|age| age > max_age)
        {
            let _ = fs::remove_file(&path);
        } else {
            evictable.push((modified, path));
        }
    }

    let total_regular = evictable.len().saturating_add(lock_count);
    if total_regular > CACHE_MAX_FILES_PER_DIR {
        evictable.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let excess = total_regular - CACHE_MAX_FILES_PER_DIR;
        for (_, path) in evictable.into_iter().take(excess) {
            let _ = fs::remove_file(path);
        }
    }
}

/// Performs hourly symlink sweeping and bounded cache eviction (`> 14d` or `> 256` entries)
/// across `data_dir` subdirectories and `probe_cache_dir`, gated by `data_dir/.maintenance-at`.
pub fn maybe_maintain_data_dir(paths: &AgyPaths, data_dir: &Path, now_ms: i64) {
    let Ok(data_meta) = fs::symlink_metadata(data_dir) else {
        return;
    };
    if data_meta.file_type().is_symlink() || !data_meta.is_dir() {
        return;
    }

    let stamp_path = data_dir.join(MAINTENANCE_STAMP_FILE);
    if let Some(raw) = read_bounded_string(&stamp_path, 64)
        && let Ok(last_ms) = raw.trim().parse::<i64>()
        && now_ms >= last_ms
        && now_ms.saturating_sub(last_ms) < MAINTENANCE_INTERVAL_MS
    {
        return;
    }

    if atomic_write_bytes(&stamp_path, format!("{now_ms}\n").as_bytes(), 0o600).is_err() {
        return;
    }

    let now_sys = if now_ms > 0 {
        UNIX_EPOCH + std::time::Duration::from_millis(now_ms as u64)
    } else {
        SystemTime::now()
    };

    sweep_symlinks(data_dir);
    for sub in [
        "transcript-cursors",
        "db-ceilings",
        "compaction-ceilings",
        "speed-cache",
    ] {
        prune_cache_dir(&data_dir.join(sub), now_sys);
    }

    if !fs::symlink_metadata(&paths.hud_home).is_ok_and(|m| m.file_type().is_symlink())
        && let Ok(probe_meta) = fs::symlink_metadata(&paths.probe_cache_dir)
    {
        if probe_meta.file_type().is_symlink() {
            let _ = fs::remove_file(&paths.probe_cache_dir);
        } else if probe_meta.is_dir() {
            sweep_symlinks(&paths.probe_cache_dir);
            prune_cache_dir(&paths.probe_cache_dir, now_sys);
        }
    }
}

/// Read-only resolution of `<hudHome>/data` without creating directories on disk.
pub fn existing_data_dir(paths: &AgyPaths) -> Option<PathBuf> {
    let meta = fs::symlink_metadata(&paths.data_dir).ok()?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return None;
    }
    fs::canonicalize(&paths.data_dir).ok()
}

fn resolve_speed_cache_session_key(stdin: Option<&AgyStdinData>) -> Option<String> {
    let s = stdin?;
    s.conversation_id
        .as_deref()
        .map(hud_core::ansi::sanitize_text)
        .filter(|k| !k.is_empty())
        .or_else(|| {
            s.session_id
                .as_deref()
                .map(hud_core::ansi::sanitize_text)
                .filter(|k| !k.is_empty())
        })
}

pub fn build_logic_paths(
    paths: &AgyPaths,
    stdin: Option<&AgyStdinData>,
    data_real_dir: Option<&Path>,
) -> LogicPaths {
    let abs_norm = |raw: Option<&str>| -> Option<PathBuf> {
        let s = raw?.trim();
        if s.is_empty() {
            return None;
        }
        let p = Path::new(s);
        if !p.is_absolute() {
            return None;
        }
        Some(canonical_or_parent(p))
    };

    let cwd_buf = abs_norm(
        stdin
            .and_then(|s| s.workspace.as_ref())
            .and_then(|w| w.current_dir.as_deref()),
    )
    .or_else(|| abs_norm(stdin.and_then(|s| s.cwd.as_deref())))
    .or_else(|| {
        abs_norm(
            stdin
                .and_then(|s| s.workspace.as_ref())
                .and_then(|w| w.project_dir.as_deref()),
        )
    })
    .or_else(|| {
        let s = stdin?;
        let env_cwd = std::env::current_dir().ok()?;
        if let Some(raw_rel) = s.cwd.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            let rel_name = Path::new(raw_rel).file_name();
            if rel_name.is_none() || rel_name == env_cwd.file_name() {
                return Some(canonical_or_parent(&env_cwd));
            }
            return None;
        }
        Some(canonical_or_parent(&env_cwd))
    });

    let transcript_buf = resolve_transcript_path(paths, stdin);
    let task_md_buf = transcript_buf
        .as_deref()
        .and_then(derive_task_md_path)
        .map(|p| canonical_or_parent(&p));

    LogicPaths {
        data: data_real_dir.map(|d| d.to_string_lossy().into_owned()),
        config: canonical_or_parent(&paths.config_path)
            .to_string_lossy()
            .into_owned(),
        undo: canonical_or_parent(&paths.undo_path)
            .to_string_lossy()
            .into_owned(),
        heartbeat: canonical_or_parent(&paths.heartbeat_path)
            .to_string_lossy()
            .into_owned(),
        last_snapshot: data_real_dir
            .map(|d| d.join("last-snapshot.json").to_string_lossy().into_owned()),
        quota_cache: data_real_dir
            .map(|d| d.join("quota-cache.json").to_string_lossy().into_owned()),
        update_cache: data_real_dir
            .map(|d| d.join("update-cache.json").to_string_lossy().into_owned()),
        speed_cache: data_real_dir.map(|d| match resolve_speed_cache_session_key(stdin) {
            Some(session_key) => {
                let key = fnv1a_64_hex(&session_key);
                d.join("speed-cache")
                    .join(format!("{key}.json"))
                    .to_string_lossy()
                    .into_owned()
            }
            None => d.join("speed-cache.json").to_string_lossy().into_owned(),
        }),
        cost_ledger: data_real_dir
            .map(|d| d.join("cost-ledger.json").to_string_lossy().into_owned()),
        compaction_ceiling: match (data_real_dir, CompactionScope::from_stdin(stdin)) {
            (Some(d), Some(scope)) => {
                let key = fnv1a_64_hex(&scope.storage_key());
                Some(
                    d.join("compaction-ceilings")
                        .join(format!("{key}.json"))
                        .to_string_lossy()
                        .into_owned(),
                )
            }
            _ => None,
        },
        transcript_cursor: match (data_real_dir, transcript_buf.as_deref()) {
            (Some(d), Some(tp)) => {
                let key = fnv1a_64_hex(&tp.to_string_lossy());
                Some(
                    d.join("transcript-cursors")
                        .join(format!("{key}.json"))
                        .to_string_lossy()
                        .into_owned(),
                )
            }
            _ => None,
        },
        cli_settings: canonical_or_parent(&paths.cli_settings_path)
            .to_string_lossy()
            .into_owned(),
        transcript: transcript_buf.map(|p| p.to_string_lossy().into_owned()),
        task_md: task_md_buf.map(|p| p.to_string_lossy().into_owned()),
        cwd: cwd_buf.map(|p| p.to_string_lossy().into_owned()),
    }
}

fn atomic_write_bytes_inner(
    target: &Path,
    bytes: &[u8],
    mode_octal: u32,
    private_parent: bool,
) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        if private_parent {
            create_private_dir_all(parent)?;
        } else {
            fs::create_dir_all(parent)?;
        }
    }
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let file_name = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let tmp_path = target.with_file_name(format!(".{file_name}.{pid}.{nanos}.tmp"));

    {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(mode_octal));
    }
    let _ = mode_octal;
    if let Err(err) = fs::rename(&tmp_path, target) {
        #[cfg(windows)]
        if target.exists() {
            let old_path = target.with_file_name(format!("{file_name}.old"));
            let _ = fs::remove_file(&old_path);
            if fs::rename(target, &old_path).is_ok() && fs::rename(&tmp_path, target).is_ok() {
                let _ = fs::remove_file(&old_path);
                return Ok(());
            }
        }
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

pub fn atomic_write_bytes(target: &Path, bytes: &[u8], mode_octal: u32) -> std::io::Result<()> {
    atomic_write_bytes_inner(target, bytes, mode_octal, true)
}

/// Atomically writes `bytes` to `target` without forcing `0700` permissions on parent directories.
pub fn atomic_write_public_bytes(
    target: &Path,
    bytes: &[u8],
    mode_octal: u32,
) -> std::io::Result<()> {
    atomic_write_bytes_inner(target, bytes, mode_octal, false)
}

/// Reads a regular non-symlink UTF-8 file strictly bounded by `max_bytes`.
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

pub fn write_config_with_undo(
    paths: &AgyPaths,
    next_raw_config: &Map<String, Value>,
) -> Result<(), String> {
    create_private_dir_all(&paths.hud_home).map_err(|e| e.to_string())?;
    if paths.config_path.exists() {
        fs::copy(&paths.config_path, &paths.undo_path).map_err(|e| e.to_string())?;
    } else {
        atomic_write_bytes(&paths.undo_path, b"{}\n", 0o600).map_err(|e| e.to_string())?;
    }
    let mut json_bytes = serde_json::to_vec_pretty(next_raw_config).map_err(|e| e.to_string())?;
    json_bytes.push(b'\n');
    atomic_write_bytes(&paths.config_path, &json_bytes, 0o600).map_err(|e| e.to_string())
}

pub fn undo_config(paths: &AgyPaths) -> Result<(), String> {
    if !paths.undo_path.exists() {
        return Err("Nothing to undo (no previous config backup found).".to_string());
    }
    let prev = fs::read(&paths.undo_path).map_err(|e| e.to_string())?;
    atomic_write_bytes(&paths.config_path, &prev, 0o600).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&paths.undo_path);
    Ok(())
}

struct ExclusiveFileLock<'a>(&'a Path);

impl Drop for ExclusiveFileLock<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

/// Executes `f` while holding an exclusive `create_new(true)` lockfile at `lock_path`,
/// automatically recovering stale lockfiles older than `stale_ms` so crashed renders never wedge.
pub fn with_exclusive_lock_file<T, F: FnOnce() -> T>(
    lock_path: &Path,
    stale_ms: u64,
    f: F,
) -> Option<T> {
    if let Some(parent) = lock_path.parent() {
        create_private_dir_all(parent).ok()?;
    }

    const LOCK_BACKOFF_MS: [u64; 6] = [1, 2, 4, 8, 16, 32];
    let mut acquired = false;
    for attempt in 0..=LOCK_BACKOFF_MS.len() {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(mut file) => {
                let _ = writeln!(file, "{}", std::process::id());
                acquired = true;
                break;
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Ok(meta) = fs::symlink_metadata(lock_path) {
                    if meta.file_type().is_symlink() {
                        let _ = fs::remove_file(lock_path);
                        continue;
                    }
                    let is_stale = meta
                        .modified()
                        .ok()
                        .and_then(|m| SystemTime::now().duration_since(m).ok())
                        .is_some_and(|age| age.as_millis() as u64 >= stale_ms);
                    if is_stale {
                        let _ = fs::remove_file(lock_path);
                        continue;
                    }
                }
                if let Some(&delay_ms) = LOCK_BACKOFF_MS.get(attempt) {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                }
            }
            Err(_) => return None,
        }
    }

    if !acquired {
        return None;
    }
    let _guard = ExclusiveFileLock(lock_path);
    Some(f())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn new(label: &str) -> Self {
            let pid = std::process::id();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!("agy-hud-paths-{label}-{pid}-{nanos}"));
            fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn paths_for(home: &Path, cli_dir: &Path) -> AgyPaths {
        let mut env = HashMap::new();
        env.insert("HOME".to_string(), home.to_string_lossy().into_owned());
        env.insert(
            "USERPROFILE".to_string(),
            home.to_string_lossy().into_owned(),
        );
        env.insert(
            "AGY_CLI_DIR".to_string(),
            cli_dir.to_string_lossy().into_owned(),
        );
        AgyPaths::from_env_map(&env)
    }

    #[test]
    fn resolve_conversation_db_path_probes_in_priority_order() {
        let tmp = TempDirGuard::new("priority");
        let home = tmp.0.join("home");
        let custom_cli = tmp.0.join("custom-cli");
        let tx_root = tmp.0.join("tx-root");
        let def_cli = home.join(".gemini").join("antigravity-cli");
        let def_ide = home.join(".gemini").join("antigravity");

        let paths = paths_for(&home, &custom_cli);
        let id = "4db069b0-b004-4f77-97d5-39bd836acc12";
        let stdin = AgyStdinData {
            conversation_id: Some(id.to_string()),
            transcript_path: Some(
                tx_root
                    .join("brain")
                    .join(id)
                    .join(".system_generated")
                    .join("logs")
                    .join("transcript.jsonl")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..Default::default()
        };

        assert_eq!(resolve_conversation_db_path(&paths, Some(&stdin)), None);

        // 4. Fallback: ~/.gemini/antigravity/conversations/<id>.db
        let ide_db = def_ide.join("conversations").join(format!("{id}.db"));
        fs::create_dir_all(ide_db.parent().unwrap()).unwrap();
        fs::write(&ide_db, b"ide").unwrap();
        assert_eq!(
            resolve_conversation_db_path(&paths, Some(&stdin)),
            Some(canonical_or_parent(&ide_db))
        );

        // 3. Overrides IDE: ~/.gemini/antigravity-cli/conversations/<id>.db
        let def_cli_db = def_cli.join("conversations").join(format!("{id}.db"));
        fs::create_dir_all(def_cli_db.parent().unwrap()).unwrap();
        fs::write(&def_cli_db, b"def-cli").unwrap();
        assert_eq!(
            resolve_conversation_db_path(&paths, Some(&stdin)),
            Some(canonical_or_parent(&def_cli_db))
        );

        // 2. Overrides defaults: <transcript_root>/conversations/<id>.db
        let tx_db = tx_root.join("conversations").join(format!("{id}.db"));
        fs::create_dir_all(tx_db.parent().unwrap()).unwrap();
        fs::write(&tx_db, b"tx").unwrap();
        assert_eq!(
            resolve_conversation_db_path(&paths, Some(&stdin)),
            Some(canonical_or_parent(&tx_db))
        );

        // 1. Highest priority: <cli_dir>/conversations/<id>.db
        let cli_db = custom_cli.join("conversations").join(format!("{id}.db"));
        fs::create_dir_all(cli_db.parent().unwrap()).unwrap();
        fs::write(&cli_db, b"cli").unwrap();
        assert_eq!(
            resolve_conversation_db_path(&paths, Some(&stdin)),
            Some(canonical_or_parent(&cli_db))
        );
    }

    #[test]
    fn resolve_conversation_db_path_rejects_unsafe_ids_and_recovers_from_session_or_transcript() {
        let tmp = TempDirGuard::new("ids");
        let home = tmp.0.join("home");
        let cli_dir = home.join(".gemini").join("antigravity-cli");
        let paths = paths_for(&home, &cli_dir);

        let safe_id = "c5ae584c-f578-47cf-96fb-3485852537e4";
        let db = cli_dir.join("conversations").join(format!("{safe_id}.db"));
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        fs::write(&db, b"db").unwrap();

        // Unsafe conversation_id falls back to session_id
        let via_session = AgyStdinData {
            conversation_id: Some("../escape".to_string()),
            session_id: Some(safe_id.to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_conversation_db_path(&paths, Some(&via_session)),
            Some(canonical_or_parent(&db))
        );

        // Extracted strictly from transcript_path when conversation_id and session_id are absent
        let via_tx = AgyStdinData {
            transcript_path: Some(
                home.join(".gemini")
                    .join("antigravity")
                    .join("brain")
                    .join(safe_id)
                    .join(".system_generated")
                    .join("logs")
                    .join("transcript.jsonl")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..Default::default()
        };
        assert_eq!(
            resolve_conversation_db_path(&paths, Some(&via_tx)),
            Some(canonical_or_parent(&db))
        );

        // Unsafe everywhere yields None
        let unsafe_only = AgyStdinData {
            conversation_id: Some("..".to_string()),
            session_id: Some("a/b".to_string()),
            transcript_path: Some(
                "/tmp/brain/../.system_generated/logs/transcript.jsonl".to_string(),
            ),
            ..Default::default()
        };
        assert_eq!(
            resolve_conversation_db_path(&paths, Some(&unsafe_only)),
            None
        );
    }

    #[test]
    fn db_ceiling_cache_path_uses_fnv64_hex_under_data_dir() {
        let data_real = Path::new("/tmp/agy-hud-data");
        let db_path = Path::new("/Users/alice/.gemini/antigravity-cli/conversations/abc.db");
        let expected_hash = fnv1a_64_hex(&db_path.to_string_lossy());
        let cache_path = db_ceiling_cache_path(data_real, db_path);
        assert_eq!(
            cache_path,
            data_real
                .join("db-ceilings")
                .join(format!("{expected_hash}.json"))
        );

        let paths = paths_for(Path::new("/Users/alice"), Path::new("/Users/alice/.cli"));
        assert_eq!(
            paths.db_ceiling_file_for(db_path),
            paths
                .data_dir
                .join("db-ceilings")
                .join(format!("{expected_hash}.json"))
        );
    }

    #[test]
    fn build_logic_paths_shards_speed_cache_by_session_with_legacy_fallback() {
        let tmp = TempDirGuard::new("speed-shard");
        let home = tmp.0.join("home");
        let cli_dir = tmp.0.join("cli");
        let paths = paths_for(&home, &cli_dir);
        assert_eq!(paths.speed_cache_dir, paths.data_dir.join("speed-cache"));

        let data_real = prepare_data_dir_fast(&paths).expect("prepare data dir fast");

        // 1. conversation_id takes precedence and sanitizes/trims whitespace
        let stdin_conv = AgyStdinData {
            conversation_id: Some("  conv-abc-123  ".to_string()),
            session_id: Some("sess-ignored".to_string()),
            ..Default::default()
        };
        let lp_conv = build_logic_paths(&paths, Some(&stdin_conv), Some(&data_real));
        let expected_conv = data_real
            .join("speed-cache")
            .join(format!("{}.json", fnv1a_64_hex("conv-abc-123")));
        assert_eq!(
            lp_conv.speed_cache.as_deref(),
            Some(expected_conv.to_string_lossy().as_ref())
        );

        // 2. Empty/whitespace conversation_id falls back to session_id
        let stdin_sess = AgyStdinData {
            conversation_id: Some("   ".to_string()),
            session_id: Some("sess-xyz".to_string()),
            ..Default::default()
        };
        let lp_sess = build_logic_paths(&paths, Some(&stdin_sess), Some(&data_real));
        let expected_sess = data_real
            .join("speed-cache")
            .join(format!("{}.json", fnv1a_64_hex("sess-xyz")));
        assert_eq!(
            lp_sess.speed_cache.as_deref(),
            Some(expected_sess.to_string_lossy().as_ref())
        );

        // 3. Neither ID present -> legacy <data>/speed-cache.json fallback
        let stdin_none = AgyStdinData::default();
        let lp_none = build_logic_paths(&paths, Some(&stdin_none), Some(&data_real));
        assert_eq!(
            lp_none.speed_cache.as_deref(),
            Some(
                data_real
                    .join("speed-cache.json")
                    .to_string_lossy()
                    .as_ref()
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn prepare_data_dir_fast_is_non_recursive_while_prepare_data_dir_sweeps_nested_symlinks() {
        let tmp = TempDirGuard::new("fast-prep");
        let home = tmp.0.join("home");
        let cli_dir = tmp.0.join("cli");
        let paths = paths_for(&home, &cli_dir);

        let real_data = prepare_data_dir_fast(&paths).expect("initial fast prepare");
        let sub = real_data.join("speed-cache");
        fs::create_dir_all(&sub).unwrap();
        let nested_link = sub.join("evil.json");
        std::os::unix::fs::symlink(tmp.0.join("missing"), &nested_link).unwrap();

        // Fast prepare does O(1) top-level checks and skips recursive symlink sweeping
        assert!(prepare_data_dir_fast(&paths).is_some());
        assert!(
            fs::symlink_metadata(&nested_link).is_ok(),
            "prepare_data_dir_fast must not traverse subdirectories"
        );

        // Full prepare sweeps nested symlinks
        assert!(prepare_data_dir(&paths).is_some());
        assert!(
            fs::symlink_metadata(&nested_link).is_err(),
            "prepare_data_dir must sweep nested symlinks"
        );

        // Symlinked hud_home is rejected by both
        let sym_cli = tmp.0.join("sym-cli");
        let mut sym_paths = paths_for(&home, &sym_cli);
        sym_paths.hud_home = tmp.0.join("hud-symlink");
        std::os::unix::fs::symlink(&paths.hud_home, &sym_paths.hud_home).unwrap();
        assert_eq!(prepare_data_dir_fast(&sym_paths), None);
    }

    #[test]
    fn maybe_maintain_data_dir_gates_hourly_and_prunes_14d_and_256_cap() {
        let tmp = TempDirGuard::new("maintain");
        let home = tmp.0.join("home");
        let cli_dir = tmp.0.join("cli");
        let paths = paths_for(&home, &cli_dir);
        let data_real = prepare_data_dir_fast(&paths).expect("data dir");

        let now_ms = 1_750_000_000_000i64;
        let now_sys = UNIX_EPOCH + std::time::Duration::from_millis(now_ms as u64);
        let stale_sys = now_sys - std::time::Duration::from_secs(14 * 86_400 + 60);
        let fresh_sys = now_sys - std::time::Duration::from_secs(60);

        let write_with_mtime = |path: &Path, mtime: SystemTime| {
            if let Some(p) = path.parent() {
                fs::create_dir_all(p).unwrap();
            }
            let f = File::create(path).unwrap();
            f.set_modified(mtime).unwrap();
        };

        // 1. Seed old (>14d) and fresh files across all 5 pruned cache directories
        for dir in [
            data_real.join("transcript-cursors"),
            data_real.join("db-ceilings"),
            data_real.join("compaction-ceilings"),
            data_real.join("speed-cache"),
            paths.probe_cache_dir.clone(),
        ] {
            write_with_mtime(&dir.join("stale.json"), stale_sys);
            write_with_mtime(&dir.join("fresh.json"), fresh_sys);
        }

        // 2. Seed >256 files in speed-cache/ with distinct ordered mtimes (000 oldest .. 269 newest)
        let sc_dir = data_real.join("speed-cache");
        for i in 0..270u64 {
            let mtime = now_sys - std::time::Duration::from_secs(1_000 - i);
            write_with_mtime(&sc_dir.join(format!("item-{i:03}.json")), mtime);
        }

        // 3. Seed an active .lock file with the OLDEST mtime + a subdirectory (non-regular entry)
        //    in compaction-ceilings/ alongside 260 files: neither .lock nor subdir may be deleted!
        let cc_dir = data_real.join("compaction-ceilings");
        let active_lock = cc_dir.join("active.lock");
        write_with_mtime(
            &active_lock,
            now_sys - std::time::Duration::from_secs(10_000),
        );
        let non_regular_dir = cc_dir.join("keep-subdir");
        fs::create_dir_all(&non_regular_dir).unwrap();
        for i in 0..260u64 {
            let mtime = now_sys - std::time::Duration::from_secs(500 - i);
            write_with_mtime(&cc_dir.join(format!("cc-{i:03}.json")), mtime);
        }

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                tmp.0.join("missing"),
                paths.probe_cache_dir.join("bad-link"),
            )
            .unwrap();
        }

        maybe_maintain_data_dir(&paths, &data_real, now_ms);

        assert!(data_real.join(".maintenance-at").is_file());
        for dir in [
            data_real.join("transcript-cursors"),
            data_real.join("db-ceilings"),
            data_real.join("compaction-ceilings"),
            data_real.join("speed-cache"),
            paths.probe_cache_dir.clone(),
        ] {
            assert!(
                !dir.join("stale.json").exists(),
                "stale.json must be pruned from {}",
                dir.display()
            );
            assert!(
                dir.join("fresh.json").exists(),
                "fresh.json must be kept in {}",
                dir.display()
            );
        }

        // Oldest items 000..014 (15 items out of 270 + fresh.json = 271 -> 256 kept) were evicted
        let sc_files: Vec<_> = fs::read_dir(&sc_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|ft| ft.is_file()))
            .collect();
        assert_eq!(sc_files.len(), CACHE_MAX_FILES_PER_DIR);
        assert!(!sc_dir.join("item-000.json").exists());
        assert!(sc_dir.join("item-269.json").exists());

        // Active .lock and non-regular subdir survived in compaction-ceilings/
        assert!(active_lock.is_file(), ".lock file must never be deleted");
        assert!(non_regular_dir.is_dir(), "non-regular dir must survive");

        #[cfg(unix)]
        assert!(
            fs::symlink_metadata(paths.probe_cache_dir.join("bad-link")).is_err(),
            "symlink inside probe_cache_dir must be swept"
        );

        // 4. Within 1 hour (now_ms + 3_599_999): gated, newly planted stale file is NOT pruned!
        let late_stale = data_real.join("db-ceilings/new-stale.json");
        write_with_mtime(&late_stale, stale_sys);
        maybe_maintain_data_dir(&paths, &data_real, now_ms + MAINTENANCE_INTERVAL_MS - 1);
        assert!(
            late_stale.exists(),
            "hourly gate must skip maintenance before 3,600,000ms"
        );

        // 5. At exactly +1 hour (now_ms + 3_600_000): maintenance runs again and prunes it!
        maybe_maintain_data_dir(&paths, &data_real, now_ms + MAINTENANCE_INTERVAL_MS);
        assert!(
            !late_stale.exists(),
            "maintenance must run after 3,600,000ms"
        );
    }

    #[test]
    fn with_exclusive_lock_file_exponential_backoff_recovers_after_contended_hold() {
        let tmp = TempDirGuard::new("lock-backoff");
        let lock_path = tmp.0.join("contended.lock");
        let holder = File::create(&lock_path).unwrap();
        drop(holder);

        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(28));
                let _ = fs::remove_file(&lock_path);
            });
            let acquired = with_exclusive_lock_file(&lock_path, 5_000, || 42usize);
            assert_eq!(
                acquired,
                Some(42),
                "exponential backoff (~63ms) must outlast 28ms lock hold"
            );
        });
        assert!(!lock_path.exists());
    }
}
