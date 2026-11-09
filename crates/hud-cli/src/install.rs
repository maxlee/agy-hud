use crate::collector::{exec_file_safe, read_bounded_string};
use crate::paths::{AgyPaths, atomic_write_bytes, canonical_or_parent, create_private_dir_all};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
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

pub fn install_status_line(
    paths: &AgyPaths,
    current_exe: &Path,
    plugin_root: &Path,
    argv: &[String],
) -> Result<Value, String> {
    let replace = argv.iter().any(|a| a == "--replace");
    let stack = argv.iter().any(|a| a == "--stack");
    let plugin_flag = argv.iter().any(|a| a == "--plugin-installed");
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
    let current_kind = classify_statusline_command(current_cmd);

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

    // Record plugin root, pluginInstalled state, and tracked backups for clean `uninstall`
    let meta_bytes = serde_json::to_vec_pretty(&json!({
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

    let target_settings = canonical_or_parent(&paths.cli_settings_path);
    let mode = file_mode_or_default(&target_settings, 0o600);
    let mut out_bytes = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
    out_bytes.push(b'\n');
    atomic_write_bytes(&target_settings, &out_bytes, mode).map_err(|e| e.to_string())?;

    let mut report = json!({
        "installed": true,
        "engine": "rust-native",
        "settingsPath": paths.cli_settings_path.to_string_lossy(),
        "binaryPath": paths.installed_bin_path.to_string_lossy(),
        "command": command_str,
        "replaced": current_kind,
        "backupPath": backup_path,
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

    // 1. Restore `~/.gemini/antigravity-cli/settings.json` cleanly to official default (or prior third-party bar)
    let mut restored_previous = false;
    let status_line_action: &'static str;

    if paths.cli_settings_path.exists() {
        let mut settings = read_json_object_or_empty(&paths.cli_settings_path)?;
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
            let target_settings = canonical_or_parent(&paths.cli_settings_path);
            let mode = file_mode_or_default(&target_settings, 0o600);
            let mut out_bytes = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
            out_bytes.push(b'\n');
            atomic_write_bytes(&target_settings, &out_bytes, mode).map_err(|e| e.to_string())?;
        } else if current_kind == "none" {
            status_line_action = "already-official-default";
        } else {
            // Foreign statusLine configured manually by user — never clobber!
            status_line_action = "kept-foreign";
        }
    } else {
        status_line_action = "settings-not-present";
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
            "config.undo.json",
        ] {
            let _ = fs::remove_file(paths.hud_home.join(file));
        }
        if !keep_config {
            let _ = fs::remove_file(&paths.config_path);
            removed_hud_home = fs::remove_dir_all(&paths.hud_home).is_ok();
        }
    }

    Ok(json!({
        "uninstalled": true,
        "settingsPath": paths.cli_settings_path.to_string_lossy(),
        "statusLineAction": status_line_action,
        "restoredPrevious": restored_previous,
        "removedBackups": removed_backups,
        "pluginUninstalled": plugin_uninstalled,
        "removedHudHome": removed_hud_home,
        "keptConfig": keep_config,
    }))
}
