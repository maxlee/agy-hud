use hud_core::types::{AgyStdinData, LogicPaths};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct AgyPaths {
    pub home: PathBuf,
    pub cli_dir: PathBuf,
    pub hud_home: PathBuf,
    pub cli_settings_path: PathBuf,
    pub bin_dir: PathBuf,
    pub installed_bin_path: PathBuf,
    pub data_dir: PathBuf,
    pub quota_cache_path: PathBuf,
    pub update_cache_path: PathBuf,
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
            data_dir,
            probe_cache_dir: hud_home.join("probe-cache"),
            config_path: hud_home.join("config.json"),
            undo_path: hud_home.join("config.undo.json"),
            heartbeat_path: hud_home.join("heartbeat.json"),
            previous_statusline_path: hud_home.join("previous-statusline.json"),
            cli_dir,
            hud_home,
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

/// Ensures `<hudHome>/data` is a real directory free of planted symlinks.
pub fn prepare_data_dir(paths: &AgyPaths) -> Option<PathBuf> {
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
    sweep_symlinks(&paths.data_dir);
    fs::canonicalize(&paths.data_dir).ok()
}

/// Read-only resolution of `<hudHome>/data` without creating directories on disk.
pub fn existing_data_dir(paths: &AgyPaths) -> Option<PathBuf> {
    let meta = fs::symlink_metadata(&paths.data_dir).ok()?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return None;
    }
    fs::canonicalize(&paths.data_dir).ok()
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

    let transcript_buf = abs_norm(stdin.and_then(|s| s.transcript_path.as_deref()));
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
        cli_settings: canonical_or_parent(&paths.cli_settings_path)
            .to_string_lossy()
            .into_owned(),
        transcript: transcript_buf.map(|p| p.to_string_lossy().into_owned()),
        task_md: task_md_buf.map(|p| p.to_string_lossy().into_owned()),
        cwd: cwd_buf.map(|p| p.to_string_lossy().into_owned()),
    }
}

pub fn atomic_write_bytes(target: &Path, bytes: &[u8], mode_octal: u32) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        create_private_dir_all(parent)?;
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
