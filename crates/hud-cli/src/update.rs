use crate::collector::{exec_file_safe, read_bounded_string};
use crate::install::detect_installed_binary_version;
use crate::paths::{
    AgyPaths, atomic_write_bytes, create_private_dir_all, existing_data_dir, prepare_data_dir,
};
use hud_core::probes::{is_newer_semver, normalize_semver_tag, parse_github_latest_release};
use hud_core::types::{HUD_VERSION, UpdateCacheSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const UPDATE_CHECK_FLOOR_MS: i64 = 6 * 3_600_000;
const LOCK_STALE_MS: i64 = 120_000;
const BASE_BACKOFF_MS: i64 = 900_000;
const MAX_BACKOFF_MS: i64 = 21_600_000;
const API_TIMEOUT_MS: u64 = 10_000;
const DOWNLOAD_TIMEOUT_MS: u64 = 60_000;
const API_MAX_STDOUT_BYTES: u64 = 256 * 1024;
const TEMP_SWEEP_AGE_SECS: u64 = 3_600;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Serialize, Deserialize)]
struct UpdateLockInfo {
    token: String,
    at: i64,
    pid: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateBackoffInfo {
    failures: u32,
    next_retry_at: i64,
}

fn is_safe_repo_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg.len() <= 64
        && seg != "."
        && seg != ".."
        && seg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

pub fn resolve_github_repo() -> String {
    if let Ok(raw) = std::env::var("AGY_HUD_REPO") {
        let trimmed = raw.trim();
        let mut parts = trimmed.split('/');
        if let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next())
            && is_safe_repo_segment(owner)
            && is_safe_repo_segment(name)
        {
            return format!("{owner}/{name}");
        }
    }
    "maxlee/agy-hud".to_string()
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
    let payload = serde_json::to_vec(&UpdateLockInfo {
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

    let is_stale = if let Some(raw) = read_bounded_string(lock_path, 4096)
        && let Ok(existing) = serde_json::from_str::<UpdateLockInfo>(&raw)
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
    serde_json::from_str::<UpdateLockInfo>(&raw)
        .map(|info| info.token == token)
        .unwrap_or(false)
}

fn release_lock_if_owner(lock_path: &Path, token: &str) {
    if verify_lock_owner(lock_path, token) {
        let _ = fs::remove_file(lock_path);
    }
}

fn read_backoff(backoff_path: &Path) -> UpdateBackoffInfo {
    read_bounded_string(backoff_path, 4096)
        .and_then(|s| serde_json::from_str::<UpdateBackoffInfo>(&s).ok())
        .unwrap_or_default()
}

fn record_backoff_failure(backoff_path: &Path, now_ms: i64) {
    let prev = read_backoff(backoff_path);
    let failures = prev.failures.saturating_add(1);
    let shift = failures.saturating_sub(1).min(5);
    let delay = (BASE_BACKOFF_MS.saturating_mul(1i64 << shift)).min(MAX_BACKOFF_MS);
    let next = UpdateBackoffInfo {
        failures,
        next_retry_at: now_ms.saturating_add(delay),
    };
    if let Ok(bytes) = serde_json::to_vec(&next) {
        let _ = atomic_write_bytes(backoff_path, &bytes, 0o600);
    }
}

fn run_bounded_network_cmd(
    program: &str,
    args: &[&str],
    timeout_ms: u64,
    max_stdout: u64,
) -> Option<String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear();

    for key in [
        "PATH",
        "HOME",
        "USERPROFILE",
        "SYSTEMROOT",
        "WINDIR",
        "TMPDIR",
        "TEMP",
        "TMP",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "https_proxy",
        "http_proxy",
        "all_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ] {
        if let Ok(val) = std::env::var(key) {
            command.env(key, val);
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command.spawn().ok()?;
    let mut stdout_pipe = child.stdout.take()?;

    let reader = thread::spawn(move || {
        let mut buf = Vec::with_capacity(4096);
        let _ = stdout_pipe
            .by_ref()
            .take(max_stdout + 1)
            .read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
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
    if (buf.len() as u64) > max_stdout {
        return None;
    }
    String::from_utf8(buf).ok()
}

fn fetch_latest_release_json(repo: &str) -> Option<String> {
    if let Ok(mock) = std::env::var("AGY_HUD_UPDATE_CHECK_JSON") {
        let trimmed = mock.trim();
        if !trimmed.is_empty() {
            if trimmed.starts_with('{') {
                return Some(trimmed.to_string());
            }
            return read_bounded_string(Path::new(trimmed), 64 * 1024);
        }
    }

    let api_url = std::env::var("AGY_HUD_RELEASE_API_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with("https://") || s.starts_with("http://127.0.0.1"))
        .unwrap_or_else(|| format!("https://api.github.com/repos/{repo}/releases/latest"));

    let curl_bin = if cfg!(windows) { "curl.exe" } else { "curl" };
    if let Some(out) = run_bounded_network_cmd(
        curl_bin,
        &[
            "-fsSL",
            "-m",
            "10",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: agy-hud",
            &api_url,
        ],
        API_TIMEOUT_MS,
        API_MAX_STDOUT_BYTES,
    ) {
        return Some(out);
    }

    #[cfg(not(windows))]
    if let Some(out) = run_bounded_network_cmd(
        "wget",
        &[
            "-qO-",
            "--timeout=10",
            "--header=Accept: application/vnd.github+json",
            "--header=User-Agent: agy-hud",
            &api_url,
        ],
        API_TIMEOUT_MS,
        API_MAX_STDOUT_BYTES,
    ) {
        return Some(out);
    }

    #[cfg(windows)]
    {
        let ps_url = api_url.replace('\'', "''");
        let ps_cmd = format!(
            "(Invoke-WebRequest -UseBasicParsing -TimeoutSec 10 -Headers @{{'Accept'='application/vnd.github+json';'User-Agent'='agy-hud'}} -Uri '{ps_url}').Content"
        );
        if let Some(out) = run_bounded_network_cmd(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", &ps_cmd],
            API_TIMEOUT_MS,
            API_MAX_STDOUT_BYTES,
        ) {
            return Some(out);
        }
    }

    None
}

fn download_url_to_file(url: &str, dest: &Path) -> Result<(), String> {
    let dest_str = dest.to_string_lossy();
    let curl_bin = if cfg!(windows) { "curl.exe" } else { "curl" };
    if run_bounded_network_cmd(
        curl_bin,
        &[
            "-fsSL",
            "-m",
            "55",
            "-H",
            "User-Agent: agy-hud",
            url,
            "-o",
            &dest_str,
        ],
        DOWNLOAD_TIMEOUT_MS,
        4096,
    )
    .is_some()
        && dest.is_file()
    {
        return Ok(());
    }

    #[cfg(not(windows))]
    if run_bounded_network_cmd(
        "wget",
        &["-qO", &dest_str, "--timeout=55", url],
        DOWNLOAD_TIMEOUT_MS,
        4096,
    )
    .is_some()
        && dest.is_file()
    {
        return Ok(());
    }

    #[cfg(windows)]
    {
        let ps_url = url.replace('\'', "''");
        let ps_dest = dest_str.replace('\'', "''");
        let ps_cmd = format!(
            "Invoke-WebRequest -UseBasicParsing -TimeoutSec 55 -Uri '{ps_url}' -OutFile '{ps_dest}'"
        );
        if run_bounded_network_cmd(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", &ps_cmd],
            DOWNLOAD_TIMEOUT_MS,
            4096,
        )
        .is_some()
            && dest.is_file()
        {
            return Ok(());
        }
    }

    Err(format!("Failed to download {url}"))
}

pub fn parse_expected_sha256(sums_text: &str, archive_name: &str) -> Option<String> {
    let starred = format!("*{archive_name}");
    for line in sums_text.lines() {
        let mut cols = line.split_whitespace();
        let Some(hash) = cols.next() else {
            continue;
        };
        let Some(file_col) = cols.next() else {
            continue;
        };
        if (file_col == archive_name || file_col == starred)
            && hash.len() == 64
            && hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

fn compute_file_sha256(file_path: &Path) -> Result<String, String> {
    let file_str = file_path.to_string_lossy().into_owned();

    #[cfg(not(windows))]
    {
        if let Some(out) = exec_file_safe("sha256sum", std::slice::from_ref(&file_str), None)
            && let Some(h) = out.split_whitespace().next()
            && h.len() == 64
            && h.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Ok(h.to_ascii_lowercase());
        }
        let shasum_args = vec!["-a".to_string(), "256".to_string(), file_str.clone()];
        if let Some(out) = exec_file_safe("shasum", &shasum_args, None)
            && let Some(h) = out.split_whitespace().next()
            && h.len() == 64
            && h.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Ok(h.to_ascii_lowercase());
        }
        let ossl_args = vec!["dgst".to_string(), "-sha256".to_string(), file_str];
        if let Some(out) = exec_file_safe("openssl", &ossl_args, None)
            && let Some(h) = out.split_whitespace().last()
            && h.len() == 64
            && h.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Ok(h.to_ascii_lowercase());
        }
        Err(
            "No SHA-256 verification tool found (expected sha256sum, shasum, or openssl)."
                .to_string(),
        )
    }

    #[cfg(windows)]
    {
        let ps_file = file_str.replace('\'', "''");
        let ps_args = vec![
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            format!("(Get-FileHash -Algorithm SHA256 -LiteralPath '{ps_file}').Hash"),
        ];
        if let Some(out) = exec_file_safe("powershell", &ps_args, None)
            && let Some(h) = out.split_whitespace().next()
            && h.len() == 64
            && h.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Ok(h.to_ascii_lowercase());
        }
        Err("Failed to compute SHA-256 via PowerShell Get-FileHash.".to_string())
    }
}

pub fn detect_release_platform() -> Result<(&'static str, &'static str, &'static str), String> {
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        Ok(("darwin-arm64", "agy-hud-darwin-arm64.tar.gz", "agy-hud"))
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
        Ok(("darwin-x64", "agy-hud-darwin-x64.tar.gz", "agy-hud"))
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        Ok(("linux-x64", "agy-hud-linux-x64.tar.gz", "agy-hud"))
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
        Ok(("linux-arm64", "agy-hud-linux-arm64.tar.gz", "agy-hud"))
    } else if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
        Ok(("windows-x64", "agy-hud-windows-x64.zip", "agy-hud.exe"))
    } else {
        Err(format!(
            "Unsupported prebuilt platform ({}-{}). Build from source via `cargo build --release`.",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    }
}

fn sweep_stale_update_temp_dirs(base_tmp: &Path) {
    let Ok(entries) = fs::read_dir(base_tmp) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.starts_with("agy-hud-update-") {
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

struct ScopedUpdateTempDir {
    path: PathBuf,
}

impl ScopedUpdateTempDir {
    fn create() -> Result<Self, String> {
        let base = std::env::temp_dir();
        sweep_stale_update_temp_dirs(&base);
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = base.join(format!("agy-hud-update-{pid}-{nanos}"));
        create_private_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(Self { path: dir })
    }
}

impl Drop for ScopedUpdateTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn extract_release_archive(archive_path: &Path, extract_dir: &Path) -> Result<(), String> {
    create_private_dir_all(extract_dir).map_err(|e| e.to_string())?;
    let archive_str = archive_path.to_string_lossy().into_owned();
    let extract_str = extract_dir.to_string_lossy().into_owned();

    if archive_str.ends_with(".tar.gz") {
        let status = Command::new("tar")
            .args(["-xzf", &archive_str, "-C", &extract_str])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("Failed to invoke `tar`: {e}"))?;
        if !status.success() {
            return Err("`tar -xzf` failed to extract release archive.".to_string());
        }
        return Ok(());
    }

    #[cfg(windows)]
    {
        let ps_arc = archive_str.replace('\'', "''");
        let ps_dst = extract_str.replace('\'', "''");
        let ps_cmd =
            format!("Expand-Archive -LiteralPath '{ps_arc}' -DestinationPath '{ps_dst}' -Force");
        let status = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps_cmd])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("Failed to invoke PowerShell Expand-Archive: {e}"))?;
        if !status.success() {
            return Err("Expand-Archive failed to unpack Windows release zip.".to_string());
        }
        return Ok(());
    }

    #[cfg(not(windows))]
    Err(format!("Unsupported archive format: {archive_str}"))
}

fn write_update_cache_snapshot(paths: &AgyPaths, snap: &UpdateCacheSnapshot) {
    if let Some(data_dir) = prepare_data_dir(paths)
        && let Ok(bytes) = serde_json::to_vec_pretty(snap)
    {
        let _ = atomic_write_bytes(&data_dir.join("update-cache.json"), &bytes, 0o600);
        let _ = fs::remove_file(data_dir.join("update-backoff.json"));
    }
}

fn perform_release_upgrade(
    paths: &AgyPaths,
    repo: &str,
    target_version: Option<&str>,
    release_url: Option<String>,
    passthrough_flags: &[String],
    now_ms: i64,
) -> Result<Value, String> {
    let (platform, archive_name, bin_name) = detect_release_platform()?;
    let previous_version =
        detect_installed_binary_version(paths).or_else(|| Some(HUD_VERSION.to_string()));
    let tmp = ScopedUpdateTempDir::create()?;
    let archive_dest = tmp.path.join(archive_name);
    let sums_dest = tmp.path.join("SHA256SUMS");

    if let Ok(local_dir) = std::env::var("AGY_HUD_ARCHIVE_DIR")
        && !local_dir.trim().is_empty()
    {
        let src_dir = Path::new(local_dir.trim());
        fs::copy(src_dir.join(archive_name), &archive_dest).map_err(|e| {
            format!(
                "Failed to copy {}: {e}",
                src_dir.join(archive_name).display()
            )
        })?;
        fs::copy(src_dir.join("SHA256SUMS"), &sums_dest)
            .map_err(|e| format!("Failed to copy SHA256SUMS: {e}"))?;
    } else {
        let base_url = match target_version {
            Some(ver) => {
                let tag = if ver.starts_with('v') {
                    ver.to_string()
                } else {
                    format!("v{ver}")
                };
                format!("https://github.com/{repo}/releases/download/{tag}")
            }
            None => format!("https://github.com/{repo}/releases/latest/download"),
        };
        download_url_to_file(&format!("{base_url}/{archive_name}"), &archive_dest)?;
        download_url_to_file(&format!("{base_url}/SHA256SUMS"), &sums_dest)?;
    }

    let sums_text = read_bounded_string(&sums_dest, 64 * 1024)
        .ok_or_else(|| "SHA256SUMS is missing or unreadable.".to_string())?;
    let expected_hash = parse_expected_sha256(&sums_text, archive_name)
        .ok_or_else(|| format!("Checksum entry for {archive_name} not found in SHA256SUMS."))?;
    let actual_hash = compute_file_sha256(&archive_dest)?;
    if expected_hash != actual_hash {
        return Err(format!(
            "SHA-256 mismatch for {archive_name} (expected {expected_hash}, got {actual_hash}). Aborting update."
        ));
    }

    let extract_dir = tmp.path.join("extract");
    extract_release_archive(&archive_dest, &extract_dir)?;

    let extracted_plugin = extract_dir.join("agy-hud");
    let bundled_bin_rel = Path::new("scripts")
        .join("bin")
        .join(platform)
        .join(bin_name);
    if !extracted_plugin.join("plugin.json").is_file()
        || !extracted_plugin.join(&bundled_bin_rel).is_file()
    {
        return Err(format!(
            "Extracted release archive {archive_name} is missing expected plugin files."
        ));
    }

    let bundle_version = read_bounded_string(&extracted_plugin.join("plugin.json"), 32 * 1024)
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("version").and_then(Value::as_str).map(str::to_string))
        .and_then(|v| normalize_semver_tag(&v))
        .or_else(|| target_version.and_then(normalize_semver_tag))
        .unwrap_or_else(|| HUD_VERSION.to_string());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for rel in [
            bundled_bin_rel.as_path(),
            Path::new("scripts/agy-hud"),
            Path::new("install.sh"),
            Path::new("uninstall.sh"),
            Path::new("skills/hud/scripts/hud-cli.sh"),
            Path::new("skills/hud-update/scripts/hud-cli.sh"),
        ] {
            let p = extracted_plugin.join(rel);
            if p.is_file() {
                let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o755));
            }
        }
    }

    create_private_dir_all(&paths.cli_dir).map_err(|e| e.to_string())?;
    create_private_dir_all(&paths.hud_home).map_err(|e| e.to_string())?;

    let plugin_stage = paths.hud_home.join("plugin");
    let plugin_next = paths.hud_home.join("plugin.next");
    let _ = fs::remove_dir_all(&plugin_next);
    fs::rename(&extracted_plugin, &plugin_next)
        .map_err(|e| format!("Failed to stage plugin update: {e}"))?;
    let _ = fs::remove_dir_all(&plugin_stage);
    fs::rename(&plugin_next, &plugin_stage)
        .map_err(|e| format!("Failed to activate staged plugin: {e}"))?;

    let skip_plugin = std::env::var("AGY_SKIP_PLUGIN_INSTALL").as_deref() == Ok("1");
    let mut plugin_updated = false;
    if !skip_plugin {
        let args = vec![
            "plugin".to_string(),
            "install".to_string(),
            plugin_stage.to_string_lossy().into_owned(),
        ];
        plugin_updated = exec_file_safe("agy", &args, None).is_some();
    }

    let staged_bin = plugin_stage.join(&bundled_bin_rel);
    let mut install_args = vec!["install".to_string()];
    if plugin_updated {
        install_args.push("--plugin-installed".to_string());
    }
    install_args.extend(passthrough_flags.iter().cloned());

    let mut inst_cmd = Command::new(&staged_bin);
    inst_cmd
        .args(&install_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let inst_out = inst_cmd
        .output()
        .map_err(|e| format!("Failed to execute staged binary installer: {e}"))?;
    if !inst_out.status.success() {
        let stdout_err = String::from_utf8_lossy(&inst_out.stdout);
        let stderr_err = String::from_utf8_lossy(&inst_out.stderr);
        return Err(format!(
            "Staged binary `install` failed: {}{}",
            stdout_err.trim(),
            stderr_err.trim()
        ));
    }
    let install_report: Value =
        serde_json::from_slice(&inst_out.stdout).unwrap_or_else(|_| json!({}));

    let final_release_url = release_url.or_else(|| {
        Some(format!(
            "https://github.com/{repo}/releases/tag/v{bundle_version}"
        ))
    });
    write_update_cache_snapshot(
        paths,
        &UpdateCacheSnapshot {
            checked_at: now_ms,
            latest_version: bundle_version.clone(),
            release_url: final_release_url.clone(),
        },
    );

    Ok(json!({
        "updated": true,
        "previousVersion": previous_version,
        "version": bundle_version,
        "sha256": actual_hash,
        "releaseUrl": final_release_url,
        "pluginUpdated": plugin_updated,
        "binaryPath": paths.installed_bin_path.to_string_lossy(),
        "stackWithDefault": install_report.get("stackWithDefault").cloned().unwrap_or(Value::Bool(false)),
    }))
}

/// Executes `agy-hud update [--check] [--force] [--background-check --lock-token <tok>]`.
pub fn run_update_subcommand(
    paths: &AgyPaths,
    args: &[String],
    now_ms: i64,
) -> Result<Option<Value>, String> {
    let is_bg = args.iter().any(|a| a == "--background-check");
    let check_only = args.iter().any(|a| a == "--check");
    let force = args.iter().any(|a| a == "--force");
    let repo = resolve_github_repo();

    if is_bg {
        let Some(token) = args
            .iter()
            .position(|a| a == "--lock-token")
            .and_then(|idx| args.get(idx + 1))
            .cloned()
        else {
            return Ok(None);
        };
        let Some(data_dir) = existing_data_dir(paths) else {
            return Ok(None);
        };
        let lock_path = data_dir.join("update.lock");
        if !verify_lock_owner(&lock_path, &token) {
            return Ok(None);
        }

        let fetched =
            fetch_latest_release_json(&repo).and_then(|raw| parse_github_latest_release(&raw));

        // Re-verify lock ownership AND `hud_home` existence after network I/O so concurrent `uninstall` leaves zero files!
        if paths.hud_home.exists() && verify_lock_owner(&lock_path, &token) {
            let backoff_path = data_dir.join("update-backoff.json");
            let cache_path = data_dir.join("update-cache.json");
            if let Some((latest_version, release_url)) = fetched {
                let snap = UpdateCacheSnapshot {
                    checked_at: now_ms,
                    latest_version,
                    release_url,
                };
                if let Ok(bytes) = serde_json::to_vec_pretty(&snap) {
                    let _ = atomic_write_bytes(&cache_path, &bytes, 0o600);
                    let _ = fs::remove_file(&backoff_path);
                }
            } else {
                record_backoff_failure(&backoff_path, now_ms);
            }
        }
        release_lock_if_owner(&lock_path, &token);
        return Ok(None);
    }

    if check_only {
        let raw_json = fetch_latest_release_json(&repo).ok_or_else(|| {
            format!(
                "Failed to query latest GitHub release for {repo} (check network connectivity)."
            )
        })?;
        let (latest_version, release_url) =
            parse_github_latest_release(&raw_json).ok_or_else(|| {
                "GitHub Releases response did not contain a valid stable release tag.".to_string()
            })?;

        if paths.hud_home.exists() {
            write_update_cache_snapshot(
                paths,
                &UpdateCacheSnapshot {
                    checked_at: now_ms,
                    latest_version: latest_version.clone(),
                    release_url: release_url.clone(),
                },
            );
        }

        let current_installed =
            detect_installed_binary_version(paths).unwrap_or_else(|| HUD_VERSION.to_string());
        let update_available = is_newer_semver(&current_installed, &latest_version);

        return Ok(Some(json!({
            "currentVersion": current_installed,
            "latestVersion": latest_version,
            "updateAvailable": update_available,
            "releaseUrl": release_url,
            "checkedAt": now_ms,
        })));
    }

    // Foreground `agy-hud update`
    let passthrough_flags: Vec<String> = args
        .iter()
        .filter(|a| !matches!(a.as_str(), "--force" | "--check"))
        .cloned()
        .collect();

    let has_local_archive = std::env::var("AGY_HUD_ARCHIVE_DIR")
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let explicit_env_ver = std::env::var("AGY_HUD_VERSION")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "latest");

    if let Some(pin_ver) = explicit_env_ver {
        let norm = normalize_semver_tag(&pin_ver).ok_or_else(|| {
            format!("Invalid AGY_HUD_VERSION \"{pin_ver}\" (expected semver e.g. 1.2.0).")
        })?;
        let report =
            perform_release_upgrade(paths, &repo, Some(&norm), None, &passthrough_flags, now_ms)?;
        return Ok(Some(report));
    }

    if has_local_archive && std::env::var("AGY_HUD_UPDATE_CHECK_JSON").is_err() {
        let report = perform_release_upgrade(paths, &repo, None, None, &passthrough_flags, now_ms)?;
        return Ok(Some(report));
    }

    let raw_json = fetch_latest_release_json(&repo).ok_or_else(|| {
        format!("Failed to query latest GitHub release for {repo} (check network connectivity).")
    })?;
    let (latest_version, release_url) =
        parse_github_latest_release(&raw_json).ok_or_else(|| {
            "GitHub Releases response did not contain a valid stable release tag.".to_string()
        })?;

    let current_installed =
        detect_installed_binary_version(paths).unwrap_or_else(|| HUD_VERSION.to_string());
    if !force && !is_newer_semver(&current_installed, &latest_version) {
        if paths.hud_home.exists() {
            write_update_cache_snapshot(
                paths,
                &UpdateCacheSnapshot {
                    checked_at: now_ms,
                    latest_version: latest_version.clone(),
                    release_url: release_url.clone(),
                },
            );
        }
        return Ok(Some(json!({
            "updated": false,
            "alreadyUpToDate": true,
            "currentVersion": current_installed,
            "latestVersion": latest_version,
            "releaseUrl": release_url,
            "message": format!("agy-hud v{current_installed} is already the latest version."),
        })));
    }

    let report = perform_release_upgrade(
        paths,
        &repo,
        Some(&latest_version),
        release_url,
        &passthrough_flags,
        now_ms,
    )?;
    Ok(Some(report))
}

/// Non-blocking 6-hour background GitHub Releases check (`updates.enabled == true`).
/// Never blocks synchronous `statusLine` rendering (`< 0.1ms` local stat/read).
pub fn maybe_spawn_background_update_check(paths: &AgyPaths, current_exe: &Path, now_ms: i64) {
    if std::env::var("AGY_HUD_DISABLE_BG_UPDATE").as_deref() == Ok("1") {
        return;
    }
    let Some(data_dir) = prepare_data_dir(paths) else {
        return;
    };

    let backoff = read_backoff(&data_dir.join("update-backoff.json"));
    if now_ms < backoff.next_retry_at {
        return;
    }

    let cache_path = data_dir.join("update-cache.json");
    if let Some(raw_cache) = read_bounded_string(&cache_path, 16 * 1024)
        && let Ok(cached) = serde_json::from_str::<UpdateCacheSnapshot>(&raw_cache)
        && now_ms.saturating_sub(cached.checked_at) < UPDATE_CHECK_FLOOR_MS
    {
        return;
    }

    let lock_path = data_dir.join("update.lock");
    let Some(token) = try_acquire_lock(&lock_path, now_ms) else {
        return;
    };

    let mut cmd = Command::new(current_exe);
    cmd.args([
        "update",
        "--background-check",
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
