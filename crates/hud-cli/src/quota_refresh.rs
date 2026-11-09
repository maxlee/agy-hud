use crate::collector::read_bounded_string;
use crate::paths::{AgyPaths, atomic_write_bytes, create_private_dir_all, prepare_data_dir};
use hud_core::probes::parse_usage_command_output;
use hud_core::types::{AgyStdinData, CachedQuotaSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LOCK_STALE_MS: i64 = 120_000;
const ACTIVE_REFRESH_FLOOR_MS: i64 = 60_000;
const IDLE_REFRESH_FLOOR_MS: i64 = 300_000;
const BASE_BACKOFF_MS: i64 = 60_000;
const MAX_BACKOFF_MS: i64 = 600_000;
const USAGE_TIMEOUT_MS: u64 = 32_000;
const USAGE_MAX_STDOUT_BYTES: u64 = 512 * 1024;
const TEMP_SWEEP_AGE_SECS: u64 = 3_600;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Serialize, Deserialize)]
struct QuotaLockInfo {
    token: String,
    at: i64,
    pid: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaBackoffInfo {
    failures: u32,
    next_retry_at: i64,
}

fn make_lock_token(now_ms: i64) -> String {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{pid}-{now_ms}-{nanos}")
}

fn try_acquire_lock(lock_path: &Path, now_ms: i64) -> Option<String> {
    let token = make_lock_token(now_ms);
    let payload = serde_json::to_vec(&QuotaLockInfo {
        token: token.clone(),
        at: now_ms,
        pid: std::process::id(),
    })
    .ok()?;

    let write_exclusive = |p: &Path, bytes: &[u8]| -> std::io::Result<()> {
        let mut f = OpenOptions::new().write(true).create_new(true).open(p)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = f.set_permissions(fs::Permissions::from_mode(0o600));
        }
        f.write_all(bytes)?;
        f.sync_all()
    };

    if write_exclusive(lock_path, &payload).is_ok() {
        return Some(token);
    }

    // Check if existing lock is stale (> 120s)
    let is_stale = if let Some(raw) = read_bounded_string(lock_path, 4096)
        && let Ok(existing) = serde_json::from_str::<QuotaLockInfo>(&raw)
    {
        now_ms.saturating_sub(existing.at) > LOCK_STALE_MS
    } else if let Ok(meta) = fs::symlink_metadata(lock_path)
        && let Ok(modified) = meta.modified()
        && let Ok(age) = SystemTime::now().duration_since(modified)
    {
        (age.as_millis() as i64) > LOCK_STALE_MS
    } else {
        false
    };

    if is_stale {
        let _ = fs::remove_file(lock_path);
        if write_exclusive(lock_path, &payload).is_ok() {
            return Some(token);
        }
    }

    None
}

fn verify_lock_owner(lock_path: &Path, token: &str) -> bool {
    let Some(raw) = read_bounded_string(lock_path, 4096) else {
        return false;
    };
    serde_json::from_str::<QuotaLockInfo>(&raw)
        .map(|info| info.token == token)
        .unwrap_or(false)
}

fn release_lock_if_owner(lock_path: &Path, token: &str) {
    if verify_lock_owner(lock_path, token) {
        let _ = fs::remove_file(lock_path);
    }
}

fn read_backoff(backoff_path: &Path) -> QuotaBackoffInfo {
    read_bounded_string(backoff_path, 4096)
        .and_then(|s| serde_json::from_str::<QuotaBackoffInfo>(&s).ok())
        .unwrap_or_default()
}

fn record_backoff_failure(backoff_path: &Path, now_ms: i64) {
    let prev = read_backoff(backoff_path);
    let failures = prev.failures.saturating_add(1);
    let shift = failures.saturating_sub(1).min(4);
    let delay = (BASE_BACKOFF_MS.saturating_mul(1i64 << shift)).min(MAX_BACKOFF_MS);
    let next = QuotaBackoffInfo {
        failures,
        next_retry_at: now_ms.saturating_add(delay),
    };
    if let Ok(bytes) = serde_json::to_vec(&next) {
        let _ = atomic_write_bytes(backoff_path, &bytes, 0o600);
    }
}

fn sweep_stale_usage_temp_dirs(base_tmp: &Path) {
    let Ok(entries) = fs::read_dir(base_tmp) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.starts_with("agy-hud-usage-") {
            continue;
        }
        let p = entry.path();
        let Ok(meta) = fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
            continue;
        }
        if let Ok(modified) = meta.modified()
            && let Ok(age) = now.duration_since(modified)
            && age.as_secs() >= TEMP_SWEEP_AGE_SECS
        {
            let _ = fs::remove_dir_all(&p);
        }
    }
}

struct ScopedTempDir {
    path: PathBuf,
}

impl ScopedTempDir {
    fn create() -> Option<Self> {
        let base = std::env::temp_dir();
        sweep_stale_usage_temp_dirs(&base);
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = base.join(format!("agy-hud-usage-{pid}-{nanos}"));
        create_private_dir_all(&dir).ok()?;
        Some(Self { path: dir })
    }
}

impl Drop for ScopedTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn run_agy_usage_isolated() -> Option<String> {
    let temp_cwd = ScopedTempDir::create()?;
    let agy_bin = std::env::var("AGY_HUD_AGY_BIN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "agy".to_string());

    let mut command = Command::new(agy_bin);
    command
        .args([
            "-p",
            "/usage",
            "--output-format",
            "json",
            "--print-timeout",
            "30s",
        ])
        .current_dir(&temp_cwd.path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear();

    for key in [
        "PATH",
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_CONFIG_HOME",
        "SYSTEMROOT",
        "WINDIR",
        "TMPDIR",
        "TEMP",
        "TMP",
        "AGY_CLI_DIR",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "NO_PROXY",
    ] {
        if let Ok(val) = std::env::var(key) {
            command.env(key, val);
        }
    }

    command
        .env("AGY_HUD_NESTED", "1")
        .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true");

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command.spawn().ok()?;
    let mut stdout_pipe = child.stdout.take()?;

    let reader = thread::spawn(move || {
        let mut buf = Vec::with_capacity(8192);
        let _ = stdout_pipe
            .by_ref()
            .take(USAGE_MAX_STDOUT_BYTES + 1)
            .read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + Duration::from_millis(USAGE_TIMEOUT_MS);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(Duration::from_millis(15));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    }?;

    if !status.success() {
        return None;
    }
    let buf = reader.join().ok()?;
    if (buf.len() as u64) > USAGE_MAX_STDOUT_BYTES {
        return None;
    }
    String::from_utf8(buf).ok()
}

fn execute_locked_refresh(
    data_dir: &Path,
    now_ms: i64,
) -> Result<CachedQuotaSnapshot, &'static str> {
    let backoff_path = data_dir.join("quota-backoff.json");
    let cache_path = data_dir.join("quota-cache.json");

    let Some(stdout) = run_agy_usage_isolated() else {
        record_backoff_failure(&backoff_path, now_ms);
        return Err(
            "Failed to query `agy -p /usage` (check that `agy` is installed and authenticated).",
        );
    };

    let Some(quota) = parse_usage_command_output(&stdout) else {
        record_backoff_failure(&backoff_path, now_ms);
        return Err("`agy -p /usage` returned an unrecognized or empty quota payload.");
    };

    let snapshot = CachedQuotaSnapshot {
        at: now_ms,
        source: "agy-usage".to_string(),
        quota,
    };
    let bytes = serde_json::to_vec_pretty(&snapshot)
        .map_err(|_| "Failed to serialize quota cache snapshot.")?;
    atomic_write_bytes(&cache_path, &bytes, 0o600)
        .map_err(|_| "Failed to write quota-cache.json.")?;
    let _ = fs::remove_file(&backoff_path);
    Ok(snapshot)
}

/// Runs `agy-hud quota refresh` (either foreground manual or `--background --lock-token <tok>`).
pub fn run_quota_subcommand(
    paths: &AgyPaths,
    args: &[String],
    now_ms: i64,
) -> Result<Option<Value>, String> {
    let sub_action = args.first().map(String::as_str).unwrap_or("");
    if sub_action != "refresh" {
        return Err("Usage: `agy-hud quota refresh`".to_string());
    }

    let is_background = args.iter().any(|a| a == "--background");
    let passed_token = args
        .iter()
        .position(|a| a == "--lock-token")
        .and_then(|idx| args.get(idx + 1))
        .cloned();

    let Some(data_dir) = prepare_data_dir(paths) else {
        if is_background {
            return Ok(None);
        }
        return Err("Unable to prepare secure HUD data directory.".to_string());
    };

    let lock_path = data_dir.join("quota.lock");
    let owned_token = if let Some(tok) = passed_token {
        if !verify_lock_owner(&lock_path, &tok) {
            return Ok(None);
        }
        tok
    } else {
        let Some(tok) = try_acquire_lock(&lock_path, now_ms) else {
            if is_background {
                return Ok(None);
            }
            return Err("Another quota refresh is currently in progress.".to_string());
        };
        tok
    };

    let result = execute_locked_refresh(&data_dir, now_ms);
    release_lock_if_owner(&lock_path, &owned_token);

    match result {
        Ok(snap) => {
            if is_background {
                Ok(None)
            } else {
                let bucket_ids: Vec<&String> = snap.quota.keys().collect();
                Ok(Some(json!({
                    "refreshed": true,
                    "at": snap.at,
                    "source": snap.source,
                    "bucketCount": bucket_ids.len(),
                    "buckets": bucket_ids,
                })))
            }
        }
        Err(static_msg) => {
            if is_background {
                Ok(None)
            } else {
                Err(static_msg.to_string())
            }
        }
    }
}

/// Non-blocking opt-in background quota refresh trigger (`quota.backgroundRefresh == true`).
/// Respects active (`60s`) / idle (`300s`) refresh floors, exponential backoff (`60s..600s`),
/// and single-instance exclusive lock ownership.
pub fn maybe_spawn_background_quota_refresh(
    paths: &AgyPaths,
    current_exe: &Path,
    stdin: &AgyStdinData,
    now_ms: i64,
) {
    let Some(data_dir) = prepare_data_dir(paths) else {
        return;
    };

    let backoff = read_backoff(&data_dir.join("quota-backoff.json"));
    if now_ms < backoff.next_retry_at {
        return;
    }

    let is_idle = stdin
        .agent_state
        .as_deref()
        .unwrap_or("idle")
        .eq_ignore_ascii_case("idle")
        && !stdin.tool_confirmation_pending.unwrap_or(false);
    let floor_ms = if is_idle {
        IDLE_REFRESH_FLOOR_MS
    } else {
        ACTIVE_REFRESH_FLOOR_MS
    };

    let cache_path = data_dir.join("quota-cache.json");
    if let Some(raw_cache) = read_bounded_string(&cache_path, 64 * 1024)
        && let Ok(cached) = serde_json::from_str::<CachedQuotaSnapshot>(&raw_cache)
        && now_ms.saturating_sub(cached.at) < floor_ms
    {
        return;
    }

    let lock_path = data_dir.join("quota.lock");
    let Some(token) = try_acquire_lock(&lock_path, now_ms) else {
        return;
    };

    let mut cmd = Command::new(current_exe);
    cmd.args([
        "quota",
        "refresh",
        "--background",
        "--lock-token",
        token.as_str(),
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    if cmd.spawn().is_err() {
        release_lock_if_owner(&lock_path, &token);
    }
}
