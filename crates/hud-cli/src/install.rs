use crate::paths::{AgyPaths, atomic_write_bytes, canonical_or_parent, create_private_dir_all};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn shell_quote(s: &str) -> String {
    if cfg!(windows) {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
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

pub fn install_status_line(
    paths: &AgyPaths,
    current_exe: &Path,
    plugin_root: &Path,
    argv: &[String],
) -> Result<Value, String> {
    let replace = argv.iter().any(|a| a == "--replace");
    let stack = argv.iter().any(|a| a == "--stack");
    let padding = argv
        .iter()
        .position(|a| a == "--padding")
        .and_then(|idx| argv.get(idx + 1))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

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

    let current_kind = if current_cmd.is_empty() {
        "none"
    } else if current_cmd.contains("agy-hud")
        || current_cmd.contains("hud.mjs")
        || current_cmd.contains("statusline.mjs")
    {
        "agy-hud"
    } else {
        "other"
    };

    if current_kind == "other" && !replace {
        return Err(
            "settings.json already configures another statusLine script. Ask the user first, then re-run `agy-hud install --replace` (a backup is created automatically)."
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

    // Record plugin root metadata so `doctor` knows plugin origin even when run via deployed binary
    let meta_bytes = serde_json::to_vec_pretty(&json!({
        "pluginRoot": canonical_or_parent(plugin_root).to_string_lossy(),
    }))
    .unwrap_or_default();
    let _ = atomic_write_bytes(
        &paths.hud_home.join("install-meta.json"),
        &meta_bytes,
        0o600,
    );

    let mut backup_path: Option<String> = None;
    if paths.cli_settings_path.exists() {
        let stamp = utc_compact_timestamp();
        let bak = Path::new(&format!(
            "{}.bak.{stamp}",
            paths.cli_settings_path.display()
        ))
        .to_path_buf();
        fs::copy(&paths.cli_settings_path, &bak).map_err(|e| e.to_string())?;
        backup_path = Some(bak.to_string_lossy().into_owned());
    }

    if current_kind == "other"
        && let Some(prev_val) = settings.get("statusLine")
    {
        let prev_bytes = serde_json::to_vec_pretty(prev_val).map_err(|e| e.to_string())?;
        atomic_write_bytes(&paths.previous_statusline_path, &prev_bytes, 0o600)
            .map_err(|e| e.to_string())?;
    }

    let command_str = shell_quote(&paths.installed_bin_path.to_string_lossy());
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

    let target_settings = canonical_or_parent(&paths.cli_settings_path);
    let mode = file_mode_or_default(&target_settings, 0o600);
    let mut out_bytes = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
    out_bytes.push(b'\n');
    atomic_write_bytes(&target_settings, &out_bytes, mode).map_err(|e| e.to_string())?;

    Ok(json!({
        "installed": true,
        "engine": "rust-native",
        "settingsPath": paths.cli_settings_path.to_string_lossy(),
        "binaryPath": paths.installed_bin_path.to_string_lossy(),
        "command": command_str,
        "replaced": current_kind,
        "backupPath": backup_path,
    }))
}

pub fn uninstall_status_line(paths: &AgyPaths) -> Result<Value, String> {
    if !paths.cli_settings_path.exists() {
        return Ok(json!({
            "uninstalled": false,
            "reason": "settings.json does not exist.",
        }));
    }

    let mut settings = read_json_object_or_empty(&paths.cli_settings_path)?;
    let mut restored_previous = false;

    if paths.previous_statusline_path.exists() {
        if let Ok(raw) = fs::read_to_string(&paths.previous_statusline_path)
            && let Ok(prev_val) = serde_json::from_str::<Value>(&raw)
        {
            settings.insert("statusLine".to_string(), prev_val);
            let _ = fs::remove_file(&paths.previous_statusline_path);
            restored_previous = true;
        } else {
            settings.remove("statusLine");
        }
    } else {
        settings.remove("statusLine");
    }

    let target_settings = canonical_or_parent(&paths.cli_settings_path);
    let mode = file_mode_or_default(&target_settings, 0o600);
    let mut out_bytes = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
    out_bytes.push(b'\n');
    atomic_write_bytes(&target_settings, &out_bytes, mode).map_err(|e| e.to_string())?;

    Ok(json!({
        "uninstalled": true,
        "settingsPath": paths.cli_settings_path.to_string_lossy(),
        "restoredPrevious": restored_previous,
    }))
}
