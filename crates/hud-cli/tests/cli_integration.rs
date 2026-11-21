use hud_cli::collector::{
    DetectedRepo, GIT_SAFE_PREFIX, HG_SAFE_PREFIX, JJ_PROBE_ARGS, ProbeRunner, SystemProbeRunner,
    collect_render_inputs, compute_vcs_signature, detect_vcs_repo, extract_git_filter_names,
    probe_git, probe_hg, probe_jj, probe_vcs_cached, probe_vcs_uncached,
    read_and_parse_transcript_incremental, read_git_head_fallback, resolve_executable_in_path,
};
use hud_cli::install::{format_statusline_command_for_platform, format_title_command_for_platform};
use hud_cli::paths::{
    AgyPaths, build_logic_paths, db_ceiling_cache_path, derive_task_md_path, prepare_data_dir,
};
use hud_cli::session_db;
use hud_core::ansi::strip_ansi;
use hud_core::config::HudConfig;
use hud_core::probes::parse_db_ceiling_info;
use hud_core::types::{
    AgyStdinData, ContextWindowStdin, CurrentUsageStdin, DbCeilingSource, ModelStdin,
    TRANSCRIPT_CURSOR_VERSION, TranscriptCursor, VcsStdin, VcsType, WorkspaceStdin,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("agy-hud-{prefix}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        Self { path: dir }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn run_bin(
    args: &[&str],
    env_overrides: &[(&str, &Path)],
    stdin_data: Option<&str>,
) -> (bool, String) {
    let bin = env!("CARGO_BIN_EXE_agy-hud");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env("AGY_HUD_DISABLE_BG_UPDATE", "1")
        .env("AGY_SKIP_PLUGIN_INSTALL", "1")
        .env("LC_ALL", "en_US.UTF-8")
        .env("LANG", "en_US.UTF-8")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env_overrides {
        cmd.env(k, v);
    }
    if stdin_data.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }

    let mut child = cmd.spawn().expect("spawn agy-hud");
    if let Some(input) = stdin_data
        && let Some(mut pipe) = child.stdin.take()
    {
        pipe.write_all(input.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), stdout)
}

#[test]
fn derive_task_md_path_resolves_conversation_root_from_system_generated_logs() {
    let base = std::env::temp_dir().join("brain").join("1234-5678");
    let tp = base
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl");
    assert_eq!(
        derive_task_md_path(&tp).as_deref(),
        Some(base.join("task.md").as_path())
    );
}

#[test]
fn detect_vcs_repo_handles_colocated_jj_nested_git_worktrees_hg_and_rejects_symlinks() {
    let tmp = TempDir::new("vcs-detect");

    // 1. Colocated .jj + .git prefers jj
    let colocated = tmp.path.join("colocated");
    fs::create_dir_all(colocated.join(".jj")).unwrap();
    fs::create_dir_all(colocated.join(".git")).unwrap();
    assert_eq!(
        detect_vcs_repo(&colocated).map(|r| r.vcs_type()),
        Some(VcsType::Jj)
    );

    // 2. Nested .git inside outer .jj stops at inner .git boundary
    let nested_git = colocated.join("packages").join("sub-git");
    fs::create_dir_all(nested_git.join(".git")).unwrap();
    let det_nested = detect_vcs_repo(&nested_git).unwrap();
    assert_eq!(det_nested.vcs_type(), VcsType::Git);
    assert_eq!(det_nested.root(), fs::canonicalize(&nested_git).unwrap());

    // 3. Git worktree (.git file pointing to gitdir + commondir)
    let main_git_dir = tmp.path.join("main-repo").join(".git");
    let wt_git_dir = main_git_dir.join("worktrees").join("wt-feature");
    fs::create_dir_all(&wt_git_dir).unwrap();
    fs::write(wt_git_dir.join("HEAD"), "ref: refs/heads/wt-feature\n").unwrap();
    fs::write(wt_git_dir.join("commondir"), "../..\n").unwrap();

    let worktree_dir = tmp.path.join("wt-checkout");
    fs::create_dir_all(&worktree_dir).unwrap();
    fs::write(
        worktree_dir.join(".git"),
        format!("gitdir: {}\n", wt_git_dir.display()),
    )
    .unwrap();

    let det_wt = detect_vcs_repo(&worktree_dir).unwrap();
    match &det_wt {
        DetectedRepo::Git {
            git_dir,
            common_dir,
            ..
        } => {
            assert_eq!(git_dir, &fs::canonicalize(&wt_git_dir).unwrap());
            assert_eq!(common_dir, &fs::canonicalize(&main_git_dir).unwrap());
        }
        other => panic!("Expected Git worktree, got {other:?}"),
    }
    assert!(compute_vcs_signature(&det_wt).starts_with("git|"));

    // 4. Mercurial (.hg)
    let hg_repo = tmp.path.join("hg-repo");
    fs::create_dir_all(hg_repo.join(".hg")).unwrap();
    assert_eq!(
        detect_vcs_repo(&hg_repo).map(|r| r.vcs_type()),
        Some(VcsType::Hg)
    );

    // 5. Symlinked .jj marker is ignored for safety
    #[cfg(unix)]
    {
        let fake_symlink_repo = tmp.path.join("symlink-repo");
        fs::create_dir_all(&fake_symlink_repo).unwrap();
        std::os::unix::fs::symlink(colocated.join(".jj"), fake_symlink_repo.join(".jj")).unwrap();
        assert!(detect_vcs_repo(&fake_symlink_repo).is_none());
    }
}

#[test]
fn jj_hg_and_git_probes_enforce_read_only_flags_and_neutralize_repo_local_git_filters() {
    assert!(JJ_PROBE_ARGS.contains(&"--ignore-working-copy"));
    assert!(JJ_PROBE_ARGS.contains(&"--at-operation=@"));
    assert!(JJ_PROBE_ARGS.contains(&"--no-pager"));

    let seen_jj: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
    let jj_res = probe_jj(
        Path::new("/tmp"),
        &|cmd: &str, args: &[String], _cwd: Option<&Path>| {
            assert_eq!(cmd, "jj");
            seen_jj.lock().unwrap().push(args.to_vec());
            Some("abcd1234\x1ffeat/jj-probe\x1f1\x1f1\n".to_string())
        },
    )
    .unwrap();
    assert_eq!(jj_res.branch, "feat/jj-probe");
    assert!(jj_res.dirty);
    assert_eq!(jj_res.conflicts, 1);

    let hg_res = probe_hg(
        Path::new("/tmp"),
        &|cmd: &str, args: &[String], _cwd: Option<&Path>| {
            assert_eq!(cmd, "hg");
            for flag in HG_SAFE_PREFIX {
                assert!(args.iter().any(|a| a == flag));
            }
            if args.iter().any(|a| a == "branch") {
                Some("feature-hg\n".to_string())
            } else {
                Some("M a.rs\n? b.txt\n".to_string())
            }
        },
    )
    .unwrap();
    assert_eq!(hg_res.branch, "feature-hg");
    assert_eq!(hg_res.modified, 1);
    assert_eq!(hg_res.untracked, 1);

    // Verify extract_git_filter_names + real git filter neutralization!
    assert_eq!(
        extract_git_filter_names("[filter \"evil\"]\n\tclean = touch /tmp/pwn\n[filter.legacy]\n"),
        vec!["evil".to_string(), "legacy".to_string()]
    );

    let tmp = TempDir::new("git-filter-canary");
    let repo_dir = tmp.path.join("untrusted-repo");
    let canary_file = tmp.path.join("canary-pwned");
    fs::create_dir_all(&repo_dir).unwrap();

    let init_ok = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo_dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if init_ok {
        // Plant a repo-local filter driver + .gitattributes that would create `canary_file` if executed
        let canary_posix = canary_file.to_string_lossy().replace('\\', "/");
        let mut cfg_text = fs::read_to_string(repo_dir.join(".git/config")).unwrap_or_default();
        cfg_text.push_str(&format!(
            "\n[filter \"evil\"]\n\tclean = touch '{canary_posix}'\n\tsmudge = touch '{canary_posix}'\n\trequired = true\n",
        ));
        fs::write(repo_dir.join(".git/config"), cfg_text).unwrap();
        fs::write(repo_dir.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
        fs::write(repo_dir.join("payload.txt"), "hello world\n").unwrap();

        let captured_args: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let _ = probe_git(
            &repo_dir,
            &repo_dir.join(".git"),
            &repo_dir.join(".git"),
            &|cmd: &str, args: &[String], cwd: Option<&Path>| {
                *captured_args.lock().unwrap() = args.to_vec();
                SystemProbeRunner.run(cmd, args, cwd)
            },
        );

        let args = captured_args.lock().unwrap().clone();
        for flag in GIT_SAFE_PREFIX {
            assert!(args.iter().any(|a| a == flag));
        }
        assert!(args.contains(&"filter.evil.clean=".to_string()));
        assert!(args.contains(&"filter.evil.required=false".to_string()));
        assert!(
            !canary_file.exists(),
            "Malicious repo-local Git filter must NEVER execute during HUD status probe"
        );
    }
}

#[test]
fn sharded_vcs_cache_isolates_workspaces_and_invalidates_on_mtime_stdin_flip_and_ttl() {
    let tmp = TempDir::new("sharded-cache");
    let repo_a = tmp.path.join("repo-a");
    let repo_b = tmp.path.join("repo-b");

    for r in [&repo_a, &repo_b] {
        fs::create_dir_all(r.join(".git/refs/heads")).unwrap();
        fs::write(r.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(r.join(".git/index"), "v1").unwrap();
    }

    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let mut env_map = HashMap::new();
    env_map.insert("HOME".to_string(), fake_home.to_string_lossy().into_owned());
    env_map.insert(
        "USERPROFILE".to_string(),
        fake_home.to_string_lossy().into_owned(),
    );
    env_map.insert(
        "AGY_CONFIG_DIR".to_string(),
        gemini_cfg.to_string_lossy().into_owned(),
    );
    env_map.insert(
        "AGY_CLI_DIR".to_string(),
        tmp.path.join("cli").to_string_lossy().into_owned(),
    );
    env_map.insert(
        "AGY_HUD_HOME".to_string(),
        tmp.path.join("hud").to_string_lossy().into_owned(),
    );
    let paths = AgyPaths::from_env_map(&env_map);
    assert_eq!(paths.gemini_config_dir, gemini_cfg);

    let spawns = AtomicUsize::new(0);
    let runner = |_cmd: &str, _args: &[String], _cwd: Option<&Path>| -> Option<String> {
        let n = spawns.fetch_add(1, Ordering::SeqCst) + 1;
        Some(format!("# branch.head main\n# branch.ab +{n} -0\n"))
    };

    let t0 = 1_700_000_000_000i64;

    // 1. Cold miss on repo_a -> 1 spawn
    let r1: Value =
        serde_json::from_str(&probe_vcs_cached(&repo_a, &paths, 1500, None, t0, &runner).unwrap())
            .unwrap();
    assert_eq!(spawns.load(Ordering::SeqCst), 1);
    assert_eq!(r1["ahead"], 1);

    // 2. Warm hit on repo_a at t0 + 300ms -> 0 new spawns!
    let r2: Value = serde_json::from_str(
        &probe_vcs_cached(&repo_a, &paths, 1500, None, t0 + 300, &runner).unwrap(),
    )
    .unwrap();
    assert_eq!(spawns.load(Ordering::SeqCst), 1);
    assert_eq!(r2["ahead"], 1);

    // 3. Concurrent repo_b writes a distinct sharded file without disturbing repo_a's cache!
    let _ = probe_vcs_cached(&repo_b, &paths, 1500, None, t0 + 350, &runner).unwrap();
    assert_eq!(spawns.load(Ordering::SeqCst), 2);
    let shard_files: Vec<_> = fs::read_dir(&paths.probe_cache_dir)
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(
        shard_files.len(),
        2,
        "Expected 2 independent sharded cache files"
    );

    // repo_a is still a warm hit!
    let _ = probe_vcs_cached(&repo_a, &paths, 1500, None, t0 + 380, &runner).unwrap();
    assert_eq!(spawns.load(Ordering::SeqCst), 2);

    // 4. Mutate repo_a/.git/index size -> immediate invalidation!
    fs::write(repo_a.join(".git/index"), "v2-staged-change").unwrap();
    let r3: Value = serde_json::from_str(
        &probe_vcs_cached(&repo_a, &paths, 1500, None, t0 + 400, &runner).unwrap(),
    )
    .unwrap();
    assert_eq!(spawns.load(Ordering::SeqCst), 3);
    assert_eq!(r3["ahead"], 3);

    // 5. stdin.vcs.dirty flip -> immediate invalidation!
    let dirty_stdin = VcsStdin {
        vcs_type: Some("git".to_string()),
        branch: Some("main".to_string()),
        dirty: Some(true),
        client: None,
    };
    let _ = probe_vcs_cached(&repo_a, &paths, 1500, Some(&dirty_stdin), t0 + 450, &runner).unwrap();
    assert_eq!(spawns.load(Ordering::SeqCst), 4);

    // 6. Advance past TTL -> refreshes
    let _ = probe_vcs_cached(&repo_a, &paths, 1500, None, t0 + 2500, &runner).unwrap();
    assert_eq!(spawns.load(Ordering::SeqCst), 5);
}

#[test]
fn full_binary_cli_lifecycle_install_render_doctor_set_undo_report_uninstall() {
    let tmp = TempDir::new("cli-e2e");
    let cli_dir = tmp.path.join("antigravity-cli");
    let hud_home = cli_dir.join("agy-hud");
    let settings_path = cli_dir.join("settings.json");
    fs::create_dir_all(&cli_dir).unwrap();

    // Seed foreign statusLine with secret token to test redaction & --replace guard
    fs::write(
        &settings_path,
        serde_json::to_string_pretty(&json!({
            "colorScheme": "dark",
            "statusLine": {
                "type": "command",
                "command": "/usr/local/bin/custom-bar.sh --token=secret123"
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    fs::create_dir_all(&fake_home).unwrap();
    let env_vars: [(&str, &Path); 5] = [
        ("HOME", fake_home.as_path()),
        ("USERPROFILE", fake_home.as_path()),
        ("AGY_CONFIG_DIR", gemini_cfg.as_path()),
        ("AGY_CLI_DIR", cli_dir.as_path()),
        ("AGY_HUD_HOME", hud_home.as_path()),
    ];

    // 1. doctor detects foreign statusLine and redacts secret123
    let (ok_doc, doc_out) = run_bin(&["doctor"], &env_vars, None);
    assert!(ok_doc);
    let doc_json: Value = serde_json::from_str(&doc_out).unwrap();
    assert_eq!(doc_json["statusLine"]["kind"], "other");
    assert_eq!(doc_json["runtime"]["engine"], "rust-native");
    assert_eq!(doc_json["config"]["problems"], json!([]));
    let cmd_redacted = doc_json["statusLine"]["command"].as_str().unwrap();
    assert!(!cmd_redacted.contains("secret123"));
    assert!(cmd_redacted.contains("[REDACTED]"));

    // 2. install without --replace refuses to overwrite foreign statusLine
    let (ok_inst_fail, _) = run_bin(&["install"], &env_vars, None);
    assert!(!ok_inst_fail);

    // 3. install --replace --stack deploys native binary and updates settings.json
    let (ok_inst, inst_out) = run_bin(&["install", "--replace", "--stack"], &env_vars, None);
    assert!(ok_inst);
    let inst_json: Value = serde_json::from_str(&inst_out).unwrap();
    assert_eq!(inst_json["installed"], true);
    assert_eq!(inst_json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(inst_json["previousVersion"], Value::Null);
    assert_eq!(inst_json["stackWithDefault"], true);
    assert_eq!(inst_json["engine"], "rust-native");
    assert_eq!(inst_json["replaced"], "other");
    assert_eq!(inst_json["unprefixedSkills"]["hud"], "installed");
    assert_eq!(inst_json["unprefixedSkills"]["hudUpdate"], "installed");
    assert!(
        gemini_cfg
            .join("skills/hud/.agy-hud-managed.json")
            .is_file()
    );
    assert!(
        gemini_cfg
            .join("skills/hud-update/.agy-hud-managed.json")
            .is_file()
    );
    assert!(Path::new(inst_json["backupPath"].as_str().unwrap()).exists());
    let expected_bin = if cfg!(windows) {
        "agy-hud.exe"
    } else {
        "agy-hud"
    };
    assert!(hud_home.join("bin").join(expected_bin).exists());

    // Re-running `install` without `--stack` preserves `stack_with_default: true` and reports `previousVersion`!
    let (ok_reinst, reinst_out) = run_bin(&["install"], &env_vars, None);
    assert!(ok_reinst);
    let reinst_json: Value = serde_json::from_str(&reinst_out).unwrap();
    assert_eq!(reinst_json["previousVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(reinst_json["stackWithDefault"], true);

    let updated_settings: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(updated_settings["colorScheme"], "dark");
    assert_eq!(updated_settings["statusLine"]["stack_with_default"], true);
    assert!(
        updated_settings["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains(expected_bin)
    );

    // 4. Pipe live stdin payload to render statusLine and populate heartbeat + last-snapshot
    let live_payload = json!({
        "cwd": "/Users/alice/secret-repo",
        "conversation_id": "abcd-1234",
        "conversation_title": "Top secret algorithm",
        "email": "alice@corp.internal",
        "model": { "id": "Gemini 4 Argon", "display_name": "Gemini 4 Argon", "effort": "high" },
        "context_window": { "used_percentage": 33.3, "context_window_size": 1_000_000 },
        "agent_state": "working",
        "terminal_width": 100
    })
    .to_string();
    let (ok_live, live_out) = run_bin(&[], &env_vars, Some(&live_payload));
    assert!(ok_live);
    assert!(live_out.contains("WORKING"));

    // 5. preview --last replays the recorded snapshot
    let (ok_replay, replay_out) = run_bin(&["preview", "--last"], &env_vars, None);
    assert!(ok_replay);
    assert!(replay_out.contains("WORKING"));

    // 6. report scrubs PII completely
    let (ok_rep, rep_out) = run_bin(&["report", "--note", "rust e2e"], &env_vars, None);
    assert!(ok_rep);
    assert!(!rep_out.contains("alice@corp.internal"));
    assert!(!rep_out.contains("secret-repo"));
    assert!(!rep_out.contains("Top secret algorithm"));
    assert!(!rep_out.contains("abcd-1234"));
    let rep_json: Value = serde_json::from_str(&rep_out).unwrap();
    assert_eq!(rep_json["scrubbedStdin"]["email"], "user@example.com");

    // 7. set & set --undo
    let (ok_set, set_out) = run_bin(
        &["set", "language=zh-Hans", "display.barLength=16"],
        &env_vars,
        None,
    );
    assert!(ok_set);
    assert!(set_out.contains("language = \"zh-Hans\""));
    let cfg_after_set: Value =
        serde_json::from_str(&fs::read_to_string(hud_home.join("config.json")).unwrap()).unwrap();
    assert_eq!(cfg_after_set["language"], "zh-Hans");
    assert_eq!(cfg_after_set["display"]["barLength"], 16);

    let (ok_undo, _) = run_bin(&["set", "--undo"], &env_vars, None);
    assert!(ok_undo);
    let cfg_after_undo: Value =
        serde_json::from_str(&fs::read_to_string(hud_home.join("config.json")).unwrap()).unwrap();
    assert!(cfg_after_undo.get("language").is_none());

    // 8. uninstall restores previous foreign statusLine AND removes hud_home + tracked backups cleanly!
    let backup_from_install = PathBuf::from(inst_json["backupPath"].as_str().unwrap());
    assert!(backup_from_install.exists());
    let (ok_uninst, uninst_out) = run_bin(&["uninstall"], &env_vars, None);
    assert!(ok_uninst);
    let uninst_json: Value = serde_json::from_str(&uninst_out).unwrap();
    assert_eq!(uninst_json["restoredPrevious"], true);
    assert_eq!(uninst_json["statusLineAction"], "restored-previous");
    assert!(uninst_json["removedBackups"].as_u64().unwrap() >= 1);
    assert_eq!(
        uninst_json["unprefixedSkillsRemoved"],
        json!(["hud", "hud-update"])
    );
    assert!(!gemini_cfg.join("skills").exists());
    assert!(!backup_from_install.exists());
    assert!(
        !hud_home.exists(),
        "hud_home must be completely removed on default clean uninstall"
    );
    let restored_settings: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(
        restored_settings["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("custom-bar.sh")
    );

    // 9. Running `doctor` after clean uninstall must NOT recreate `hud_home`!
    let (ok_doc_after, doc_after_out) = run_bin(&["doctor"], &env_vars, None);
    assert!(ok_doc_after);
    assert!(
        !hud_home.exists(),
        "read-only `doctor` must not recreate hud_home after uninstall"
    );
    let doc_after_json: Value = serde_json::from_str(&doc_after_out).unwrap();
    assert_eq!(doc_after_json["statusLine"]["kind"], "other");

    // 10. Official-default lifecycle: install `--stack` on clean settings.json -> uninstall restores pure official default with zero leftover files!
    let clean_cli_dir = tmp.path.join("clean-official-cli");
    let clean_hud_home = clean_cli_dir.join("agy-hud");
    let clean_settings_path = clean_cli_dir.join("settings.json");
    fs::create_dir_all(&clean_cli_dir).unwrap();
    fs::write(
        &clean_settings_path,
        serde_json::to_string_pretty(&json!({
            "colorScheme": "solarized",
            "vimMode": true
        }))
        .unwrap(),
    )
    .unwrap();
    let clean_home = tmp.path.join("clean-home");
    let clean_cfg = clean_home.join(".gemini/config");
    fs::create_dir_all(&clean_home).unwrap();
    let clean_env: [(&str, &Path); 5] = [
        ("HOME", clean_home.as_path()),
        ("USERPROFILE", clean_home.as_path()),
        ("AGY_CONFIG_DIR", clean_cfg.as_path()),
        ("AGY_CLI_DIR", clean_cli_dir.as_path()),
        ("AGY_HUD_HOME", clean_hud_home.as_path()),
    ];
    let (ok_clean_inst, _) = run_bin(&["install", "--stack"], &clean_env, None);
    assert!(ok_clean_inst);
    assert!(clean_hud_home.exists());

    // Test `--keep-config` first, then full clean `uninstall`
    let _ = run_bin(&["set", "language=zh-Hans"], &clean_env, None);
    let (ok_keep_uninst, keep_out) = run_bin(&["uninstall", "--keep-config"], &clean_env, None);
    assert!(ok_keep_uninst);
    let keep_json: Value = serde_json::from_str(&keep_out).unwrap();
    assert_eq!(keep_json["statusLineAction"], "restored-official-default");
    assert_eq!(keep_json["keptConfig"], true);
    assert!(clean_hud_home.join("config.json").exists());
    assert!(!clean_hud_home.join("bin").exists());

    // Now run full clean uninstall: `settings.json` has zero `statusLine` key, all user keys intact, and `clean_cli_dir` has ONLY `settings.json`!
    let (ok_full_uninst, _) = run_bin(&["uninstall"], &clean_env, None);
    assert!(ok_full_uninst);
    assert!(!clean_hud_home.exists());
    let final_official_settings: Value =
        serde_json::from_str(&fs::read_to_string(&clean_settings_path).unwrap()).unwrap();
    assert_eq!(
        final_official_settings,
        json!({
            "colorScheme": "solarized",
            "vimMode": true
        })
    );
    let remaining_entries: Vec<String> = fs::read_dir(&clean_cli_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        remaining_entries,
        vec!["settings.json".to_string()],
        "cli_dir must have zero leftover backups or agy-hud directories after clean uninstall"
    );

    // 11. AGY_HUD_DISABLE=1 suppresses render output cleanly
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agy-hud"));
    cmd.env("HOME", &fake_home)
        .env("USERPROFILE", &fake_home)
        .env("AGY_CONFIG_DIR", &gemini_cfg)
        .env("AGY_CLI_DIR", &cli_dir)
        .env("AGY_HUD_HOME", &hud_home)
        .env("AGY_HUD_DISABLE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(live_payload.as_bytes())
        .unwrap();
    let disabled_out = child.wait_with_output().unwrap();
    assert!(disabled_out.status.success());
    assert!(disabled_out.stdout.is_empty());
}

#[test]
fn open_pipe_stdin_exits_early_without_eof_and_nested_mode_is_inert() {
    let tmp = TempDir::new("open-pipe");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");

    let payload = json!({
        "cwd": "/tmp/demo",
        "model": { "id": "Gemini 4 Argon", "display_name": "Gemini 4 Argon" },
        "context_window": { "used_percentage": 18.0 },
        "agent_state": "thinking",
        "terminal_width": 96
    })
    .to_string();

    // 1. Open pipe WITHOUT closing `child_stdin` (`antigravity-cli#698` regression guard)
    let started = std::time::Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agy-hud"))
        .env("HOME", &fake_home)
        .env("USERPROFILE", &fake_home)
        .env("AGY_CONFIG_DIR", &gemini_cfg)
        .env("AGY_CLI_DIR", &cli_dir)
        .env("AGY_HUD_HOME", &hud_home)
        .env("LC_ALL", "en_US.UTF-8")
        .env("LANG", "en_US.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut open_stdin = child.stdin.take().unwrap();
    open_stdin.write_all(payload.as_bytes()).unwrap();
    open_stdin.flush().unwrap();
    // Intentionally keep `open_stdin` alive while waiting for child to exit!
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_millis(3000),
            "agy-hud blocked waiting for EOF on an open stdin pipe!"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert!(status.success());
    let mut rendered = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut rendered).unwrap();
    drop(open_stdin);
    assert!(rendered.contains("THINKING"));

    // 2. AGY_HUD_NESTED=1 exits 0 with zero output and zero disk writes
    let nested_hud_home = tmp.path.join("nested-hud-home");
    let mut nested = Command::new(env!("CARGO_BIN_EXE_agy-hud"))
        .env("HOME", &fake_home)
        .env("USERPROFILE", &fake_home)
        .env("AGY_CONFIG_DIR", &gemini_cfg)
        .env("AGY_CLI_DIR", &cli_dir)
        .env("AGY_HUD_HOME", &nested_hud_home)
        .env("AGY_HUD_NESTED", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    nested
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let nested_out = nested.wait_with_output().unwrap();
    assert!(nested_out.status.success());
    assert!(nested_out.stdout.is_empty());
    assert!(
        !nested_hud_home.exists(),
        "AGY_HUD_NESTED=1 must never create files or directories"
    );
}

#[test]
fn git_worktree_current_dir_priority_and_zero_subprocess_git_head_fallback() {
    let tmp = TempDir::new("worktree-head-fallback");
    let main_repo = tmp.path.join("main-repo");
    let main_git_dir = main_repo.join(".git");
    let wt_git_dir = main_git_dir.join("worktrees").join("wt-hotfix");
    fs::create_dir_all(&wt_git_dir).unwrap();
    fs::write(wt_git_dir.join("HEAD"), "ref: refs/heads/hotfix/pipe-eof\n").unwrap();
    fs::write(wt_git_dir.join("commondir"), "../..\n").unwrap();

    let wt_dir = tmp.path.join("wt-hotfix-checkout");
    fs::create_dir_all(&wt_dir).unwrap();
    fs::write(
        wt_dir.join(".git"),
        format!("gitdir: {}\n", wt_git_dir.display()),
    )
    .unwrap();

    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let mut env_map = HashMap::new();
    env_map.insert("HOME".to_string(), fake_home.to_string_lossy().into_owned());
    env_map.insert(
        "USERPROFILE".to_string(),
        fake_home.to_string_lossy().into_owned(),
    );
    env_map.insert(
        "AGY_CONFIG_DIR".to_string(),
        gemini_cfg.to_string_lossy().into_owned(),
    );
    env_map.insert(
        "AGY_CLI_DIR".to_string(),
        tmp.path.join("cli").to_string_lossy().into_owned(),
    );
    env_map.insert(
        "AGY_HUD_HOME".to_string(),
        tmp.path.join("hud").to_string_lossy().into_owned(),
    );
    let paths = AgyPaths::from_env_map(&env_map);
    assert_eq!(paths.gemini_config_dir, gemini_cfg);

    // 1. `workspace.current_dir` (worktree) takes priority over `workspace.project_dir` (main repo)
    let stdin = AgyStdinData {
        cwd: Some(main_repo.to_string_lossy().into_owned()),
        workspace: Some(WorkspaceStdin {
            current_dir: Some(wt_dir.to_string_lossy().into_owned()),
            project_dir: Some(main_repo.to_string_lossy().into_owned()),
        }),
        ..Default::default()
    };
    let logic_paths = build_logic_paths(&paths, Some(&stdin), None);
    assert_eq!(
        logic_paths.cwd.as_deref(),
        Some(
            fs::canonicalize(&wt_dir)
                .unwrap()
                .to_string_lossy()
                .as_ref()
        )
    );

    // 2. When `git` times out / is absent (`runner` returns `None`), `.git/HEAD` fallback resolves worktree branch & caches it!
    let spawns = AtomicUsize::new(0);
    let failing_runner = |_cmd: &str, _args: &[String], _cwd: Option<&Path>| -> Option<String> {
        spawns.fetch_add(1, Ordering::SeqCst);
        None
    };

    let t0 = 1_700_000_000_000i64;
    let first: Value = serde_json::from_str(
        &probe_vcs_cached(&wt_dir, &paths, 1500, None, t0, &failing_runner).unwrap(),
    )
    .unwrap();
    assert_eq!(first["branch"], "hotfix/pipe-eof");
    assert_eq!(spawns.load(Ordering::SeqCst), 1);

    // Second render within TTL hits cache -> 0 additional failing `git` spawns!
    let second: Value = serde_json::from_str(
        &probe_vcs_cached(&wt_dir, &paths, 1500, None, t0 + 200, &failing_runner).unwrap(),
    )
    .unwrap();
    assert_eq!(second["branch"], "hotfix/pipe-eof");
    assert_eq!(spawns.load(Ordering::SeqCst), 1);
}

#[test]
fn windows_statusline_quote_fix_and_isolated_quota_refresh_e2e() {
    // 1. Windows `statusLine.command` quote trap check (`tightship#89`)
    let (win_cmd_clean, warn_clean) = format_statusline_command_for_platform(
        r"C:\Users\max\.gemini\antigravity-cli\agy-hud\bin\agy-hud.exe",
        true,
    );
    assert!(!win_cmd_clean.contains('"'));
    assert!(warn_clean.is_none());

    let (win_cmd_spaced, warn_spaced) = format_statusline_command_for_platform(
        r"C:\Users\Max Lee\.gemini\antigravity-cli\agy-hud\bin\agy-hud.exe",
        true,
    );
    assert!(!win_cmd_spaced.contains('"'));
    assert!(warn_spaced.unwrap().contains("8.3 short path"));

    // 2. Hermetic `agy-hud quota refresh` E2E with fake `agy` binary verifying env & isolated temp cwd
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new("quota-refresh");
        let fake_home = tmp.path.join("home");
        let gemini_cfg = fake_home.join(".gemini/config");
        let cli_dir = tmp.path.join("cli");
        let hud_home = cli_dir.join("agy-hud");
        let fake_agy = tmp.path.join("fake-agy.sh");

        let script = r#"#!/bin/sh
if [ "$AGY_HUD_NESTED" != "1" ]; then
  exit 2
fi
if [ "$AGY_CLI_DISABLE_AUTO_UPDATE" != "true" ]; then
  exit 3
fi
case "$(pwd)" in
  *agy-hud-usage-*) ;;
  *) exit 4 ;;
esac
cat <<'EOF'
{"command":{"data":{"groups":[{"buckets":[{"id":"gemini-argon-wtu-weekly","remaining_fraction":0.35,"reset_time":"2099-01-01T00:00:00Z"}]}]}}}
EOF
"#;
        fs::write(&fake_agy, script).unwrap();
        fs::set_permissions(&fake_agy, fs::Permissions::from_mode(0o755)).unwrap();

        let env_vars: [(&str, &Path); 6] = [
            ("HOME", fake_home.as_path()),
            ("USERPROFILE", fake_home.as_path()),
            ("AGY_CONFIG_DIR", gemini_cfg.as_path()),
            ("AGY_CLI_DIR", cli_dir.as_path()),
            ("AGY_HUD_HOME", hud_home.as_path()),
            ("AGY_HUD_AGY_BIN", fake_agy.as_path()),
        ];

        let (ok_ref, ref_out) = run_bin(&["quota", "refresh"], &env_vars, None);
        assert!(ok_ref, "quota refresh failed: {ref_out}");
        let ref_json: Value = serde_json::from_str(&ref_out).unwrap();
        assert_eq!(ref_json["refreshed"], true);
        assert_eq!(ref_json["buckets"], json!(["gemini-argon-wtu-weekly"]));
        assert!(hud_home.join("data/quota-cache.json").exists());

        // Verify live render reconciles stale official 0.80 down to cached 0.35 (65% used)!
        let live_payload = json!({
            "cwd": "/tmp/demo",
            "model": { "id": "Gemini 4 Argon", "display_name": "Gemini 4 Argon" },
            "context_window": { "used_percentage": 20.0 },
            "quota": {
                "gemini-argon-wtu-weekly": {
                    "remaining_fraction": 0.80,
                    "reset_time": "2099-01-01T00:00:00Z"
                },
                "3p-5h": {
                    "remaining_fraction": 0.99,
                    "reset_time": "2099-01-01T00:00:00Z"
                }
            },
            "agent_state": "idle",
            "terminal_width": 120
        })
        .to_string();

        let (ok_live, live_out) = run_bin(&[], &env_vars, Some(&live_payload));
        assert!(ok_live);
        assert!(live_out.contains("Quota(argon-weekly)"));
        assert!(live_out.contains("65%"));
    }
}

#[test]
#[cfg(unix)]
fn install_sh_version_logging_and_self_update_e2e() {
    use hud_cli::update::detect_release_platform;
    use std::os::unix::fs::PermissionsExt;

    let Ok((platform, archive_name, bin_name)) = detect_release_platform() else {
        return;
    };

    let tmp = TempDir::new("self-update-e2e");
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();

    // Build a genuine release bundle tarball for v9.9.0 with SHA256SUMS
    let stage_parent = tmp.path.join("dist");
    let stage = stage_parent.join("agy-hud");
    let bin_dir = stage.join("scripts/bin").join(platform);
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(stage.join("skills/hud/scripts")).unwrap();
    fs::create_dir_all(stage.join("skills/hud-update/scripts")).unwrap();

    fs::write(
        stage.join("plugin.json"),
        serde_json::to_string_pretty(&json!({
            "name": "agy-hud",
            "version": "9.9.0"
        }))
        .unwrap(),
    )
    .unwrap();
    fs::copy(workspace_root.join("install.sh"), stage.join("install.sh")).unwrap();
    fs::copy(
        workspace_root.join("uninstall.sh"),
        stage.join("uninstall.sh"),
    )
    .unwrap();
    fs::copy(
        workspace_root.join("scripts/agy-hud"),
        stage.join("scripts/agy-hud"),
    )
    .unwrap();
    fs::copy(
        workspace_root.join("skills/hud/SKILL.md"),
        stage.join("skills/hud/SKILL.md"),
    )
    .unwrap();
    fs::copy(
        workspace_root.join("skills/hud/scripts/hud-cli.sh"),
        stage.join("skills/hud/scripts/hud-cli.sh"),
    )
    .unwrap();
    fs::copy(
        workspace_root.join("skills/hud-update/SKILL.md"),
        stage.join("skills/hud-update/SKILL.md"),
    )
    .unwrap();
    fs::copy(
        workspace_root.join("skills/hud-update/scripts/hud-cli.sh"),
        stage.join("skills/hud-update/scripts/hud-cli.sh"),
    )
    .unwrap();
    fs::copy(env!("CARGO_BIN_EXE_agy-hud"), bin_dir.join(bin_name)).unwrap();
    fs::set_permissions(bin_dir.join(bin_name), fs::Permissions::from_mode(0o755)).unwrap();

    let archive_path = stage_parent.join(archive_name);
    let tar_ok = Command::new("tar")
        .args([
            "-C",
            &stage_parent.to_string_lossy(),
            "-czf",
            &archive_path.to_string_lossy(),
            "agy-hud",
        ])
        .status()
        .unwrap()
        .success();
    assert!(tar_ok);

    // Compute SHA256SUMS via shasum/sha256sum
    let sha_out = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "(sha256sum '{0}' 2>/dev/null || shasum -a 256 '{0}') | awk '{{print $1}}'",
            archive_path.display()
        ))
        .output()
        .unwrap();
    let hash = String::from_utf8_lossy(&sha_out.stdout).trim().to_string();
    assert_eq!(hash.len(), 64);
    fs::write(
        stage_parent.join("SHA256SUMS"),
        format!("{hash}  {archive_name}\n"),
    )
    .unwrap();

    // 1. Verify `install.sh --stack` prints clear version logs!
    let home_sh = tmp.path.join("home-sh");
    let cfg_sh = home_sh.join(".gemini/config");
    let cli_sh = tmp.path.join("cli-sh");
    let hud_sh = cli_sh.join("agy-hud");
    fs::create_dir_all(&home_sh).unwrap();
    let sh_out = Command::new("sh")
        .arg(workspace_root.join("install.sh"))
        .arg("--stack")
        .env("HOME", &home_sh)
        .env("USERPROFILE", &home_sh)
        .env("AGY_CONFIG_DIR", &cfg_sh)
        .env("AGY_CLI_DIR", &cli_sh)
        .env("AGY_HUD_HOME", &hud_sh)
        .env("AGY_HUD_ARCHIVE_DIR", &stage_parent)
        .env("AGY_SKIP_PLUGIN_INSTALL", "1")
        .output()
        .unwrap();
    let sh_stdout = String::from_utf8_lossy(&sh_out.stdout);
    assert!(sh_out.status.success(), "install.sh failed: {sh_stdout}");
    assert!(sh_stdout.contains("==> Installing agy-hud v9.9.0..."));
    assert!(sh_stdout.contains("==> Deploying native agy-hud v9.9.0 statusLine binary..."));
    assert!(sh_stdout.contains("==> Done! agy-hud v9.9.0 is active."));
    assert!(sh_stdout.contains("/hud") && sh_stdout.contains("/hud-update"));
    assert!(sh_stdout.contains("/agy-hud:hud") && sh_stdout.contains("/agy-hud:hud-update"));
    assert!(cfg_sh.join("skills/hud/.agy-hud-managed.json").is_file());
    assert!(
        cfg_sh
            .join("skills/hud-update/.agy-hud-managed.json")
            .is_file()
    );

    // 2. Test `agy-hud update --check`, live `/hud-update` badge, and `agy-hud update` self-upgrade!
    let home_upd = tmp.path.join("home-upd");
    let cfg_upd = home_upd.join(".gemini/config");
    let cli_upd = tmp.path.join("cli-upd");
    let hud_upd = cli_upd.join("agy-hud");
    fs::create_dir_all(&home_upd).unwrap();
    let upd_env: [(&str, &Path); 5] = [
        ("HOME", home_upd.as_path()),
        ("USERPROFILE", home_upd.as_path()),
        ("AGY_CONFIG_DIR", cfg_upd.as_path()),
        ("AGY_CLI_DIR", cli_upd.as_path()),
        ("AGY_HUD_HOME", hud_upd.as_path()),
    ];

    // Initial install with `--stack`
    let (ok_init, _) = run_bin(&["install", "--stack"], &upd_env, None);
    assert!(ok_init);

    let mock_release = json!({
        "tag_name": "v9.9.0",
        "draft": false,
        "prerelease": false,
        "html_url": "https://github.com/maxlee/agy-hud/releases/tag/v9.9.0"
    })
    .to_string();

    let check_out = Command::new(env!("CARGO_BIN_EXE_agy-hud"))
        .args(["update", "--check"])
        .env("HOME", &home_upd)
        .env("USERPROFILE", &home_upd)
        .env("AGY_CONFIG_DIR", &cfg_upd)
        .env("AGY_CLI_DIR", &cli_upd)
        .env("AGY_HUD_HOME", &hud_upd)
        .env("AGY_HUD_UPDATE_CHECK_JSON", &mock_release)
        .output()
        .unwrap();
    assert!(check_out.status.success());
    let check_json: Value = serde_json::from_slice(&check_out.stdout).unwrap();
    assert_eq!(check_json["updateAvailable"], true);
    assert_eq!(check_json["latestVersion"], "9.9.0");
    assert!(hud_upd.join("data/update-cache.json").exists());

    // Live render displays `↑ v9.9.0 · /hud-update` badge when unprefixed `/hud-update` shortcut is installed!
    let live_payload = json!({
        "cwd": "/tmp/demo",
        "model": { "id": "Gemini 4 Argon", "display_name": "Gemini 4 Argon" },
        "context_window": { "used_percentage": 15.0 },
        "agent_state": "idle",
        "terminal_width": 140
    })
    .to_string();
    let (ok_badge, badge_out) = run_bin(&[], &upd_env, Some(&live_payload));
    assert!(ok_badge);
    assert!(badge_out.contains("/hud-update"), "badge_out: {badge_out}");
    assert!(
        !badge_out.contains("/agy-hud:hud-update"),
        "expected unprefixed /hud-update when installed: {badge_out}"
    );
    assert!(badge_out.contains("v9.9.0"));

    // Run `agy-hud update` end-to-end (without needing to re-pass `--stack`)!
    let upgrade_out = Command::new(env!("CARGO_BIN_EXE_agy-hud"))
        .arg("update")
        .env("HOME", &home_upd)
        .env("USERPROFILE", &home_upd)
        .env("AGY_CONFIG_DIR", &cfg_upd)
        .env("AGY_CLI_DIR", &cli_upd)
        .env("AGY_HUD_HOME", &hud_upd)
        .env("AGY_HUD_UPDATE_CHECK_JSON", &mock_release)
        .env("AGY_HUD_ARCHIVE_DIR", &stage_parent)
        .env("AGY_SKIP_PLUGIN_INSTALL", "1")
        .output()
        .unwrap();
    let upgrade_stdout = String::from_utf8_lossy(&upgrade_out.stdout);
    assert!(
        upgrade_out.status.success(),
        "agy-hud update failed: {upgrade_stdout}"
    );
    let upgrade_json: Value = serde_json::from_str(&upgrade_stdout).unwrap();
    assert_eq!(upgrade_json["updated"], true);
    assert_eq!(upgrade_json["version"], "9.9.0");
    assert_eq!(upgrade_json["sha256"], hash);
    assert_eq!(upgrade_json["stackWithDefault"], true);

    // Verify `settings.json` preserved `stack_with_default: true`!
    let settings_after: Value =
        serde_json::from_str(&fs::read_to_string(cli_upd.join("settings.json")).unwrap()).unwrap();
    assert_eq!(settings_after["statusLine"]["stack_with_default"], true);
}

#[test]
fn resolve_transcript_path_recovers_cli_brain_when_stdin_points_to_ide_dir() {
    let tmp = TempDir::new("transcript-fallback");
    let cli_dir = tmp.path.join("antigravity-cli");
    let hud_dir = tmp.path.join("hud-home");
    let conv_id = "22f8ab3a-1a0d-46a3-b895-0b28433d01d0";

    let conv_root = cli_dir.join("brain").join(conv_id);
    let logs_dir = conv_root.join(".system_generated").join("logs");
    fs::create_dir_all(&logs_dir).unwrap();

    let steps = [
        json!({
            "step_index": 0,
            "source": "USER_EXPLICIT",
            "type": "USER_INPUT",
            "status": "DONE",
            "created_at": "2026-11-09T01:41:18Z",
            "content": "<USER_REQUEST>\n/plan Compare showcase vs live HUD\n</USER_REQUEST>"
        }),
        json!({
            "step_index": 1,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": "2026-11-09T01:42:00Z",
            "tool_calls": [
                { "name": "edit_file", "args": { "Path": "/Users/dev/agy-hud/render.rs" } },
                { "name": "edit_file", "args": { "Path": "/Users/dev/agy-hud/paths.rs" } },
                { "name": "invoke_subagent", "args": { "TypeName": "research", "Role": "Transcript Auditor" } }
            ]
        }),
        json!({ "step_index": 2, "source": "MODEL", "type": "GENERIC", "status": "DONE", "created_at": "2026-11-09T01:42:01Z", "content": "ok" }),
        json!({ "step_index": 3, "source": "MODEL", "type": "GENERIC", "status": "DONE", "created_at": "2026-11-09T01:42:02Z", "content": "ok" }),
        json!({ "step_index": 4, "source": "MODEL", "type": "GENERIC", "status": "DONE", "created_at": "2026-11-09T01:42:30Z", "content": "done" }),
    ];
    let transcript_body = steps
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    fs::write(logs_dir.join("transcript.jsonl"), transcript_body).unwrap();
    fs::write(
        conv_root.join("task.md"),
        "# Tasks\n- [x] Diagnose missing lines\n- [/] Recover real CLI transcript path\n",
    )
    .unwrap();

    // `agy` v1.5.3 bug: `transcript_path` points to nonexistent `.gemini/antigravity/brain/<id>/...`
    let bogus_ide_transcript = tmp
        .path
        .join("nonexistent-ide")
        .join(".gemini/antigravity/brain")
        .join(conv_id)
        .join(".system_generated/logs/transcript.jsonl");

    let payload = json!({
        "session_id": conv_id,
        "conversation_id": conv_id,
        "transcript_path": bogus_ide_transcript.to_string_lossy(),
        "cwd": tmp.path.to_string_lossy(),
        "model": { "id": "Gemini 4 Argon (Medium)", "display_name": "Gemini 4 Argon (Medium)", "effort": "medium" },
        "context_window": { "used_percentage": 22.1, "context_window_size": 1048576 },
        "agent_state": "idle",
        "plan_tier": "Google AI Ultra",
        "sandbox": { "enabled": false },
        "terminal_width": 130
    })
    .to_string();

    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let env_vars: [(&str, &Path); 5] = [
        ("HOME", fake_home.as_path()),
        ("USERPROFILE", fake_home.as_path()),
        ("AGY_CONFIG_DIR", gemini_cfg.as_path()),
        ("AGY_CLI_DIR", cli_dir.as_path()),
        ("AGY_HUD_HOME", hud_dir.as_path()),
    ];
    let (ok_live, live_stdout) = run_bin(&[], &env_vars, Some(&payload));
    assert!(ok_live, "live statusLine failed: {live_stdout}");
    let plain_live = hud_core::ansi::strip_ansi(&live_stdout);
    assert!(
        plain_live.contains("[PLAN]"),
        "missing [PLAN]: {plain_live}"
    );
    assert!(
        plain_live.contains("Ultra"),
        "missing Ultra tier: {plain_live}"
    );
    assert!(
        !plain_live.contains("Sandbox off"),
        "unexpected Sandbox off: {plain_live}"
    );
    assert!(
        plain_live.contains("edit_file ×2: paths.rs"),
        "missing folded tools: {plain_live}"
    );
    assert!(
        plain_live.contains("Skills: plan"),
        "missing skills: {plain_live}"
    );
    assert!(
        plain_live.contains("Transcript Auditor"),
        "missing subagent line: {plain_live}"
    );
    assert!(
        plain_live.contains("Recover real CLI transcript path"),
        "missing task.md todo line: {plain_live}"
    );

    // `preview --last` must also resolve the recovered transcript & task.md
    let (ok_prev, prev_stdout) = run_bin(&["preview", "--last"], &env_vars, None);
    assert!(ok_prev, "preview --last failed: {prev_stdout}");
    let plain_prev = hud_core::ansi::strip_ansi(&prev_stdout);
    assert!(plain_prev.contains("edit_file ×2: paths.rs"));
    assert!(plain_prev.contains("Recover real CLI transcript path"));
}

#[cfg(unix)]
#[test]
fn marketplace_plugin_router_runs_bundled_binary_and_sha256_bootstrap_fallback() {
    use std::os::unix::fs::PermissionsExt;

    let os_part = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        return;
    };
    let arch_part = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        return;
    };
    let platform = format!("{os_part}-{arch_part}");

    let tmp = TempDir::new("marketplace-router");
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let bin_exe = PathBuf::from(env!("CARGO_BIN_EXE_agy-hud"));

    // Simulate an unpacked read-only Marketplace plugin directory (no Cargo.toml, no target/)
    let mp_plugin = tmp.path.join("unpacked-plugin");
    fs::create_dir_all(mp_plugin.join("scripts/bin").join(&platform)).unwrap();
    fs::create_dir_all(mp_plugin.join("skills/hud/scripts")).unwrap();
    fs::copy(
        workspace_root.join("plugin.json"),
        mp_plugin.join("plugin.json"),
    )
    .unwrap();
    fs::copy(
        workspace_root.join("scripts/agy-hud"),
        mp_plugin.join("scripts/agy-hud"),
    )
    .unwrap();
    fs::copy(
        workspace_root.join("skills/hud/scripts/hud-cli.sh"),
        mp_plugin.join("skills/hud/scripts/hud-cli.sh"),
    )
    .unwrap();
    let bundled_bin = mp_plugin
        .join("scripts/bin")
        .join(&platform)
        .join("agy-hud");
    fs::copy(&bin_exe, &bundled_bin).unwrap();
    for exec_p in [
        mp_plugin.join("scripts/agy-hud"),
        mp_plugin.join("skills/hud/scripts/hud-cli.sh"),
        bundled_bin.clone(),
    ] {
        fs::set_permissions(&exec_p, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let fresh_home = tmp.path.join("fresh-home");
    let fresh_cfg = fresh_home.join(".gemini/config");
    let fresh_cli = tmp.path.join("fresh-cli");
    let fresh_hud = fresh_cli.join("agy-hud");

    // 1. Offline execution via bundled `scripts/bin/<platform>/agy-hud` (no $HUD_HOME/bin, no network)
    let offline_out = Command::new("sh")
        .arg("scripts/hud-cli.sh")
        .arg("doctor")
        .current_dir(mp_plugin.join("skills/hud"))
        .env("HOME", &fresh_home)
        .env("USERPROFILE", &fresh_home)
        .env("AGY_CONFIG_DIR", &fresh_cfg)
        .env("AGY_CLI_DIR", &fresh_cli)
        .env("AGY_HUD_HOME", &fresh_hud)
        .env("AGY_HUD_NO_BOOTSTRAP", "1")
        .env("AGY_SKIP_PLUGIN_INSTALL", "1")
        .output()
        .unwrap();
    assert!(
        offline_out.status.success(),
        "bundled binary router failed: {}",
        String::from_utf8_lossy(&offline_out.stderr)
    );
    let doc_json: Value = serde_json::from_slice(&offline_out.stdout).unwrap();
    assert_eq!(doc_json["runtime"]["engine"], "rust-native");

    // 2. Remove bundled binary and verify SHA-256-verified archive bootstrap fallback into $HUD_HOME/bin/agy-hud
    fs::remove_file(&bundled_bin).unwrap();
    let bundle_stage_root = tmp.path.join("archive-stage");
    let bundle_dir = bundle_stage_root.join("agy-hud");
    fs::create_dir_all(bundle_dir.join("scripts/bin").join(&platform)).unwrap();
    fs::copy(
        workspace_root.join("plugin.json"),
        bundle_dir.join("plugin.json"),
    )
    .unwrap();
    let archive_bin = bundle_dir
        .join("scripts/bin")
        .join(&platform)
        .join("agy-hud");
    fs::copy(&bin_exe, &archive_bin).unwrap();
    fs::set_permissions(&archive_bin, fs::Permissions::from_mode(0o755)).unwrap();

    let archive_name = format!("agy-hud-{platform}.tar.gz");
    let archive_path = bundle_stage_root.join(&archive_name);
    let tar_status = Command::new("tar")
        .args(["-C"])
        .arg(&bundle_stage_root)
        .args(["-czf"])
        .arg(&archive_path)
        .arg("agy-hud")
        .status()
        .unwrap();
    assert!(tar_status.success());

    let sha_out = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "(sha256sum '{0}' 2>/dev/null || shasum -a 256 '{0}') | awk '{{print $1}}'",
            archive_path.display()
        ))
        .output()
        .unwrap();
    let digest = String::from_utf8_lossy(&sha_out.stdout).trim().to_string();
    assert_eq!(digest.len(), 64);
    fs::write(
        bundle_stage_root.join("SHA256SUMS"),
        format!("{digest}  {archive_name}\n"),
    )
    .unwrap();

    let bootstrap_out = Command::new("sh")
        .arg("scripts/hud-cli.sh")
        .arg("--version")
        .current_dir(mp_plugin.join("skills/hud"))
        .env("HOME", &fresh_home)
        .env("USERPROFILE", &fresh_home)
        .env("AGY_CONFIG_DIR", &fresh_cfg)
        .env("AGY_CLI_DIR", &fresh_cli)
        .env("AGY_HUD_HOME", &fresh_hud)
        .env("AGY_HUD_ARCHIVE_DIR", &bundle_stage_root)
        .output()
        .unwrap();
    assert!(
        bootstrap_out.status.success(),
        "SHA-256 bootstrap fallback failed: {}",
        String::from_utf8_lossy(&bootstrap_out.stderr)
    );
    assert!(fresh_hud.join("bin/agy-hud").is_file());
    assert!(String::from_utf8_lossy(&bootstrap_out.stdout).contains("rust-native"));
}

#[test]
fn doctor_detects_disabled_plugin_in_gemini_config_json() {
    let tmp = TempDir::new("doctor-disabled-plugin");
    let fake_home = tmp.path.join("home");
    let gemini_cfg_dir = fake_home.join(".gemini/config");
    let plugin_dir = gemini_cfg_dir.join("plugins/agy-hud");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join("plugin.json"),
        serde_json::to_string_pretty(&json!({
            "name": "agy-hud",
            "version": env!("CARGO_PKG_VERSION")
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        gemini_cfg_dir.join("config.json"),
        serde_json::to_string_pretty(&json!({
            "plugins": {
                "agy-hud": {
                    "enabled": false
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let cli_dir = fake_home.join(".gemini/antigravity-cli");
    let hud_home = cli_dir.join("agy-hud");
    fs::create_dir_all(&cli_dir).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_agy-hud"))
        .arg("doctor")
        .env("HOME", &fake_home)
        .env("USERPROFILE", &fake_home)
        .env_remove("AGY_CONFIG_DIR")
        .env("AGY_CLI_DIR", &cli_dir)
        .env("AGY_HUD_HOME", &hud_home)
        .env("LC_ALL", "en_US.UTF-8")
        .env("LANG", "en_US.UTF-8")
        .env_remove("AGY_HUD_PLUGIN_LIST_JSON")
        .output()
        .unwrap();
    assert!(out.status.success());
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["installedPlugin"]["details"]["enabled"], false);
    let findings = doc["findings"]
        .as_array()
        .expect("doctor findings array")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        findings.contains("Installed agy-hud plugin is disabled"),
        "expected disabled plugin finding, got: {findings}"
    );
}

#[test]
fn windows_and_unix_executable_resolution_defeats_cwd_and_relative_path_hijack() {
    let tmp = TempDir::new("path-hijack-defense");
    let untrusted_cwd = tmp.path.join("untrusted-repo");
    let rel_bin = untrusted_cwd.join("rel-bin");
    let trusted_bin = tmp.path.join("trusted-system-bin");
    fs::create_dir_all(&rel_bin).unwrap();
    fs::create_dir_all(&trusted_bin).unwrap();

    // Plant decoy binaries in untrusted repo cwd + relative PATH + .bat/.cmd in trusted_bin
    for decoy in [
        untrusted_cwd.join("git.exe"),
        untrusted_cwd.join("git.bat"),
        untrusted_cwd.join("git"),
        rel_bin.join("git.exe"),
        rel_bin.join("git"),
        trusted_bin.join("git.bat"),
        trusted_bin.join("git.cmd"),
    ] {
        fs::write(&decoy, b"decoy").unwrap();
    }

    // Before real `git.exe` exists in `trusted_bin`, Windows resolution MUST return None (ignoring .bat/.cmd & relative dirs!)
    let path_entries =
        std::env::join_paths([Path::new("."), Path::new("rel-bin"), trusted_bin.as_path()])
            .unwrap();
    assert_eq!(
        resolve_executable_in_path("git", Some(&path_entries), true),
        None,
        "Windows lookup must reject relative PATH entries and .bat/.cmd shims"
    );

    // Plant genuine `git.exe` in absolute `trusted_bin` -> resolves strictly to `trusted_bin/git.exe`
    let real_win_exe = trusted_bin.join("git.exe");
    fs::write(&real_win_exe, b"MZ-real").unwrap();
    assert_eq!(
        resolve_executable_in_path("git", Some(&path_entries), true).as_deref(),
        Some(real_win_exe.as_path())
    );
    // Rejects relative command paths like `./git` or `sub\git`
    assert_eq!(
        resolve_executable_in_path("./git", Some(&path_entries), true),
        None
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let real_unix_bin = trusted_bin.join("git");
        fs::write(&real_unix_bin, b"#!/bin/sh\n").unwrap();
        // Non-executable file (`0o644`) must be skipped on Unix
        fs::set_permissions(&real_unix_bin, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            resolve_executable_in_path("git", Some(&path_entries), false),
            None
        );
        // Executable file (`0o755`) in absolute PATH dir resolves cleanly
        fs::set_permissions(&real_unix_bin, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            resolve_executable_in_path("git", Some(&path_entries), false).as_deref(),
            Some(real_unix_bin.as_path())
        );
    }
}

#[test]
fn scope_aware_git_include_filter_neutralization_and_unsafe_filter_head_fallback() {
    struct ScopedGitMock {
        scoped_output: Option<String>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl ProbeRunner for ScopedGitMock {
        fn run(&self, _cmd: &str, args: &[String], _cwd: Option<&Path>) -> Option<String> {
            self.calls.lock().unwrap().push(args.to_vec());
            if args.iter().any(|a| a == "--show-scope") {
                return self.scoped_output.clone();
            }
            if args.iter().any(|a| a == "status") {
                return Some("# branch.head feat/include-safe\n# branch.ab +2 -0\n".to_string());
            }
            None
        }
    }

    let tmp = TempDir::new("git-include-scope");
    let repo_a = tmp.path.join("repo-a");
    let git_a = repo_a.join(".git");
    fs::create_dir_all(&git_a).unwrap();
    fs::write(git_a.join("HEAD"), "ref: refs/heads/feat/include-safe\n").unwrap();
    fs::write(
        git_a.join("config"),
        "[core]\n\trepositoryformatversion = 0\n[include]\n\tpath = ../.gitconfig-extra\n[remote \"origin\"]\n\turl = git@github.com:Max/agy-hud.git\n",
    )
    .unwrap();

    // 1. `[include]` triggers `--show-scope -z`, preserves `global` `lfs`, neutralizes `local` `inc-evil`!
    let mock_a = ScopedGitMock {
        scoped_output: Some(
            "global\0filter.lfs.process\ngit-lfs filter-process\0local\0filter.inc-evil.clean\nsh -c pwn\0"
                .to_string(),
        ),
        calls: Mutex::new(Vec::new()),
    };
    let detected_a = detect_vcs_repo(&repo_a).expect("detect repo_a");
    let info_a = probe_vcs_uncached(&detected_a, &mock_a).expect("probe_vcs_uncached repo_a");
    assert_eq!(info_a.branch, "feat/include-safe");
    assert_eq!(info_a.ahead, 2);
    assert_eq!(
        info_a.repo_url.as_deref(),
        Some("https://github.com/Max/agy-hud")
    );

    let calls_a = mock_a.calls.lock().unwrap();
    assert_eq!(calls_a.len(), 2, "expected config --show-scope + status");
    let status_args = &calls_a[1];
    assert!(
        status_args.iter().any(|a| a == "filter.inc-evil.clean="),
        "included local filter must be neutralized: {status_args:?}"
    );
    assert!(
        !status_args.iter().any(|a| a.contains("filter.lfs.")),
        "global git-lfs filter must NOT be neutralized: {status_args:?}"
    );
    drop(calls_a);

    // 2. Unskippable filter name (`filter.bad=name.clean`) aborts BEFORE `git status` and falls back to `.git/HEAD`!
    let repo_b = tmp.path.join("repo-b");
    let git_b = repo_b.join(".git");
    fs::create_dir_all(&git_b).unwrap();
    fs::write(git_b.join("HEAD"), "ref: refs/heads/fallback-branch\n").unwrap();
    fs::write(
        git_b.join("config"),
        "[filter \"bad=name\"]\n\tclean = evil\n[remote \"origin\"]\n\turl = https://github.com/Max/agy-hud.git\n",
    )
    .unwrap();

    let mock_b = ScopedGitMock {
        scoped_output: None,
        calls: Mutex::new(Vec::new()),
    };
    let detected_b = detect_vcs_repo(&repo_b).expect("detect repo_b");
    let info_b = probe_vcs_uncached(&detected_b, &mock_b).expect("zero-subprocess HEAD fallback");
    assert_eq!(info_b.branch, "fallback-branch");
    assert_eq!(
        info_b.repo_url.as_deref(),
        Some("https://github.com/Max/agy-hud")
    );
    assert!(
        mock_b.calls.lock().unwrap().is_empty(),
        "Unsafe filter name in .git/config must abort before spawning ANY git subprocess!"
    );
}

#[test]
fn incremental_transcript_cursor_preserves_early_metadata_beyond_512kb_tail() {
    let tmp = TempDir::new("incremental-cursor");
    let tx_path = tmp.path.join("transcript.jsonl");
    let cursor_path = tmp.path.join("cursor.json");

    // Step 0 & Step 1 at byte 0: `/plan` slash mode, `hud` skill, and `context7` MCP call
    let mut f = fs::File::create(&tx_path).unwrap();
    writeln!(
        f,
        "{}",
        json!({
            "step_index": 0,
            "type": "USER_INPUT",
            "status": "DONE",
            "created_at": "2026-11-10T02:00:00Z",
            "content": "<USER_REQUEST>/plan Optimize transcript cursor</USER_REQUEST>"
        })
    )
    .unwrap();
    writeln!(
        f,
        "{}",
        json!({
            "step_index": 1,
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": "2026-11-10T02:00:04Z",
            "output_tokens": 180,
            "tool_calls": [
                { "name": "read_file", "status": "DONE", "args": { "Path": "/Users/max/.gemini/plugins/agy-hud/skills/hud/SKILL.md" } },
                { "name": "call_mcp_tool", "status": "DONE", "args": { "ServerName": "context7", "ToolName": "query-docs" } }
            ]
        })
    )
    .unwrap();
    writeln!(
        f,
        "{}",
        json!({
            "step_index": 2,
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": "2026-11-10T02:00:09Z",
            "output_tokens": 120,
            "content": "Initial setup complete."
        })
    )
    .unwrap();
    f.flush().unwrap();

    let stdin = AgyStdinData {
        agent_state: Some("idle".to_string()),
        ..AgyStdinData::default()
    };

    // Tick 1: builds initial `cursor.json` while file is small
    let s1 = read_and_parse_transcript_incremental(
        Some(&tx_path),
        Some(&cursor_path),
        None,
        Some(&stdin),
    );
    assert_eq!(s1.active_slash_mode.as_deref(), Some("plan"));
    assert_eq!(s1.skills, vec!["plan".to_string(), "hud".to_string()]);
    assert_eq!(s1.mcp_servers, vec!["context7".to_string()]);
    assert!(cursor_path.is_file(), "cursor.json must be persisted");

    // Append > 600 KB of filler `PLANNER_RESPONSE` steps (exceeding the 512 KB cold-start tail cap!)
    let filler_payload = "x".repeat(12_000);
    for idx in 3..58 {
        writeln!(
            f,
            "{}",
            json!({
                "step_index": idx,
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-10T02:05:00Z",
                "output_tokens": 90,
                "content": filler_payload,
                "tool_calls": [
                    { "name": "edit_file", "status": "DONE", "args": { "Path": "/Users/max/repo/src/main.rs" } }
                ]
            })
        )
        .unwrap();
    }
    f.flush().unwrap();
    assert!(
        fs::metadata(&tx_path).unwrap().len() > 600 * 1024,
        "transcript must exceed 512 KB tail cap"
    );

    // Tick 2: incremental cursor seeks from committed offset and preserves early skills, MCP, slash mode & tool counts!
    let s2 = read_and_parse_transcript_incremental(
        Some(&tx_path),
        Some(&cursor_path),
        None,
        Some(&stdin),
    );
    assert_eq!(s2.active_slash_mode.as_deref(), Some("plan"));
    assert_eq!(s2.skills, vec!["plan".to_string(), "hud".to_string()]);
    assert_eq!(s2.mcp_servers, vec!["context7".to_string()]);
    let edit_entry = s2
        .tools
        .iter()
        .find(|t| t.name == "edit_file")
        .expect("folded edit_file count");
    assert_eq!(edit_entry.count, 55);
}

#[test]
fn live_binary_cross_session_cost_ledger_and_invalid_config_warning_line() {
    let tmp = TempDir::new("live-ledger-and-warn");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");
    fs::create_dir_all(&hud_home).unwrap();

    // Write config enabling daily/weekly cost + an invalid key to trigger live `⚠ Config: ... · /agy-hud:hud`
    fs::write(
        hud_home.join("config.json"),
        r#"{"display":{"showDailyCost":true,"showWeeklyCost":true},"invalidMysteryFlag":123}"#,
    )
    .unwrap();

    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    fs::create_dir_all(&fake_home).unwrap();
    let env_vars: [(&str, &Path); 5] = [
        ("HOME", fake_home.as_path()),
        ("USERPROFILE", fake_home.as_path()),
        ("AGY_CONFIG_DIR", gemini_cfg.as_path()),
        ("AGY_CLI_DIR", cli_dir.as_path()),
        ("AGY_HUD_HOME", hud_home.as_path()),
    ];

    let payload_s1 = json!({
        "cwd": tmp.path,
        "conversation_id": "live-session-1",
        "agent_state": "idle",
        "terminal_width": 130,
        "model": { "id": "Gemini 3 Flash", "display_name": "Gemini 3 Flash" },
        "context_window": { "context_window_size": 1048576, "used_percentage": 20.0 },
        "cost": { "total_usd": 0.40, "estimated": false }
    })
    .to_string();

    let (ok1, out1) = run_bin(&[], &env_vars, Some(&payload_s1));
    assert!(ok1, "session 1 render failed: {out1}");
    let plain1 = strip_ansi(&out1);
    assert!(plain1.contains("Today $0.40"), "plain1: {plain1}");
    assert!(plain1.contains("Week $0.40"), "plain1: {plain1}");
    assert!(
        plain1.contains("invalidMysteryFlag") && plain1.contains("/agy-hud:hud"),
        "live statusLine must surface config warning with /agy-hud:hud remediation: {plain1}"
    );

    // Session 2 joins with `$0.85` -> `Today $1.25`, `Week $1.25`
    let payload_s2 = json!({
        "cwd": tmp.path,
        "conversation_id": "live-session-2",
        "agent_state": "idle",
        "terminal_width": 130,
        "model": { "id": "Gemini 3 Flash", "display_name": "Gemini 3 Flash" },
        "context_window": { "context_window_size": 1048576, "used_percentage": 25.0 },
        "cost": { "total_usd": 0.85, "estimated": false }
    })
    .to_string();

    let (ok2, out2) = run_bin(&[], &env_vars, Some(&payload_s2));
    assert!(ok2, "session 2 render failed: {out2}");
    let plain2 = strip_ansi(&out2);
    assert!(plain2.contains("Today $1.25"), "plain2: {plain2}");
    assert!(plain2.contains("Week $1.25"), "plain2: {plain2}");

    // Repeated 300ms tick with identical `$0.85` on Session 2 must NOT double count!
    let (ok3, out3) = run_bin(&[], &env_vars, Some(&payload_s2));
    assert!(ok3);
    let plain3 = strip_ansi(&out3);
    assert!(plain3.contains("Today $1.25"), "plain3: {plain3}");
    assert!(plain3.contains("Week $1.25"), "plain3: {plain3}");
    assert!(hud_home.join("data/cost-ledger.json").is_file());
}

#[test]
fn live_binary_learns_model_compaction_ceiling_across_sessions_and_hides_200k_badge() {
    let tmp = TempDir::new("live-compaction-learning");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");
    fs::create_dir_all(&hud_home).unwrap();

    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    fs::create_dir_all(&fake_home).unwrap();
    let env_vars: [(&str, &Path); 5] = [
        ("HOME", fake_home.as_path()),
        ("USERPROFILE", fake_home.as_path()),
        ("AGY_CONFIG_DIR", gemini_cfg.as_path()),
        ("AGY_CLI_DIR", cli_dir.as_path()),
        ("AGY_HUD_HOME", hud_home.as_path()),
    ];

    let write_transcript = |conv_id: &str, ckpt_iso: &str, ckpt_cache_read: u64| {
        let log_dir = cli_dir
            .join("brain")
            .join(conv_id)
            .join(".system_generated/logs");
        fs::create_dir_all(&log_dir).unwrap();
        let lines = [
            json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-11-10T00:00:00Z",
                "content": "Start session"
            }),
            json!({
                "step_index": 1,
                "source": "SYSTEM",
                "type": "CHECKPOINT",
                "status": "DONE",
                "created_at": ckpt_iso,
                "input_tokens": 5_000,
                "cache_read_tokens": ckpt_cache_read,
                "output_tokens": 2_000,
                "content": "# Resuming from a compaction"
            }),
            json!({
                "step_index": 2,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": ckpt_iso,
                "output_tokens": 120,
                "content": "Done"
            }),
        ];
        let body = lines
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
            + "\n";
        let path = log_dir.join("transcript.jsonl");
        fs::write(&path, body).unwrap();
        path
    };

    let tx1 = write_transcript("conv-512k-learn", "2026-11-10T01:00:00Z", 510_000);
    let payload_s1 = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-512k-learn",
        "transcript_path": tx1,
        "agent_state": "idle",
        "terminal_width": 140,
        "exceeds_200k_tokens": true,
        "model": { "id": "Gemini 5 Ultra (High)", "display_name": "Gemini 5 Ultra (High)", "effort": "high" },
        "context_window": {
            "context_window_size": 1_048_576,
            "current_usage": { "input_tokens": 256_000, "output_tokens": 0 }
        }
    })
    .to_string();
    let (ok1, out1) = run_bin(&[], &env_vars, Some(&payload_s1));
    assert!(ok1, "session 1 failed: {out1}");
    let plain1 = strip_ansi(&out1);
    assert!(
        plain1.contains("50.0% (256k/512k · ↻1)"),
        "plain1: {plain1}"
    );
    assert!(!plain1.contains("200k+"), "200k+ must not appear: {plain1}");

    let ceilings_dir = hud_home.join("data/compaction-ceilings");
    let shard_files: Vec<PathBuf> = fs::read_dir(&ceilings_dir)
        .expect("compaction-ceilings dir created")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    assert_eq!(
        shard_files.len(),
        1,
        "expected 1 shard file: {shard_files:?}"
    );
    let shard1: Value =
        serde_json::from_str(&fs::read_to_string(&shard_files[0]).unwrap()).unwrap();
    assert_eq!(shard1["kind"], "learned");
    assert_eq!(shard1["ceiling_tokens"], 512_000);
    assert_eq!(shard1["modelFamily"], "gemini 5 ultra");

    let snap_raw = fs::read_to_string(hud_home.join("data/last-snapshot.json")).unwrap();
    let snap_val: Value = serde_json::from_str(&snap_raw).unwrap();
    assert!(snap_val["lastCompactionAtMs"].as_i64().is_some());

    let (ok_prev, out_prev) = run_bin(&["preview", "--last"], &env_vars, None);
    assert!(ok_prev, "preview --last failed: {out_prev}");
    assert!(strip_ansi(&out_prev).contains("50.0% (256k/512k · ↻1)"));

    let payload_s2_low = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-512k-low-fresh",
        "agent_state": "idle",
        "terminal_width": 140,
        "exceeds_200k_tokens": true,
        "model": { "id": "Gemini 5 Ultra (Low)", "display_name": "Gemini 5 Ultra (Low)", "effort": "low" },
        "context_window": {
            "context_window_size": 1_048_576,
            "current_usage": { "input_tokens": 256_000, "output_tokens": 0 }
        }
    })
    .to_string();
    let (ok2, out2) = run_bin(&[], &env_vars, Some(&payload_s2_low));
    assert!(ok2, "session 2 failed: {out2}");
    let plain2 = strip_ansi(&out2);
    assert!(plain2.contains("50.0% (256k/512k)"), "plain2: {plain2}");
    assert!(!plain2.contains("200k+"), "plain2: {plain2}");

    let tx_stale = write_transcript("conv-older-256k", "2026-11-10T00:30:00Z", 251_500);
    let payload_s3_stale = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-older-256k",
        "transcript_path": tx_stale,
        "agent_state": "idle",
        "terminal_width": 140,
        "model": { "id": "Gemini 5 Ultra (Medium)", "display_name": "Gemini 5 Ultra (Medium)", "effort": "medium" },
        "context_window": {
            "context_window_size": 1_048_576,
            "current_usage": { "input_tokens": 128_000, "output_tokens": 0 }
        }
    })
    .to_string();
    let (ok3, out3) = run_bin(&[], &env_vars, Some(&payload_s3_stale));
    assert!(ok3, "session 3 failed: {out3}");
    assert!(strip_ansi(&out3).contains("50.0% (128k/256k · ↻1)"));
    let shard_after_stale: Value =
        serde_json::from_str(&fs::read_to_string(&shard_files[0]).unwrap()).unwrap();
    assert_eq!(shard_after_stale["kind"], "learned");
    assert_eq!(shard_after_stale["ceiling_tokens"], 512_000);

    let payload_524k = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-524k-win",
        "agent_state": "idle",
        "terminal_width": 140,
        "model": { "id": "Gemini 5 Ultra (Low)", "display_name": "Gemini 5 Ultra (Low)", "effort": "low" },
        "context_window": {
            "context_window_size": 524_288,
            "current_usage": { "input_tokens": 256_000, "output_tokens": 0 }
        }
    })
    .to_string();
    let (ok_524, out_524) = run_bin(&[], &env_vars, Some(&payload_524k));
    assert!(ok_524, "524k session failed: {out_524}");
    let plain_524 = strip_ansi(&out_524);
    assert!(
        plain_524.contains("256k/524k"),
        "expected isolated 524k window: {plain_524}"
    );

    let payload_bypass = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-bypass-650k",
        "agent_state": "idle",
        "terminal_width": 140,
        "model": { "id": "Gemini 5 Ultra (Low)", "display_name": "Gemini 5 Ultra (Low)", "effort": "low" },
        "context_window": {
            "context_window_size": 1_048_576,
            "current_usage": { "input_tokens": 650_000, "output_tokens": 0 }
        }
    })
    .to_string();
    let (ok_byp, out_byp) = run_bin(&[], &env_vars, Some(&payload_bypass));
    assert!(ok_byp, "bypass session failed: {out_byp}");
    assert!(strip_ansi(&out_byp).contains("650k/1.0M"));
    let shard_inv: Value =
        serde_json::from_str(&fs::read_to_string(&shard_files[0]).unwrap()).unwrap();
    assert_eq!(shard_inv["kind"], "invalidated");
    assert_eq!(shard_inv["bypassed_tokens"], 650_000);
    assert_eq!(shard_inv["previous_ceiling_tokens"], 512_000);

    let payload_after_inv = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-after-inv",
        "agent_state": "idle",
        "terminal_width": 140,
        "model": { "id": "Gemini 5 Ultra (High)", "display_name": "Gemini 5 Ultra (High)", "effort": "high" },
        "context_window": {
            "context_window_size": 1_048_576,
            "current_usage": { "input_tokens": 200_000, "output_tokens": 0 }
        }
    })
    .to_string();
    let (ok_post, out_post) = run_bin(&[], &env_vars, Some(&payload_after_inv));
    assert!(ok_post, "post-invalidation session failed: {out_post}");
    let plain_post = strip_ansi(&out_post);
    assert!(
        plain_post.contains("200k/1.0M"),
        "expected physical window after invalidation: {plain_post}"
    );
}

#[test]
fn japanese_localization_and_locale_autodetection_e2e() {
    let tmp = TempDir::new("ja-i18n");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");
    let env_vars: [(&str, &Path); 5] = [
        ("HOME", &fake_home),
        ("USERPROFILE", &fake_home),
        ("AGY_CONFIG_DIR", &gemini_cfg),
        ("AGY_CLI_DIR", &cli_dir),
        ("AGY_HUD_HOME", &hud_home),
    ];

    let payload = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-ja-test",
        "agent_state": "idle",
        "terminal_width": 108,
        "model": { "id": "Gemini 3 Flash", "display_name": "Gemini 3 Flash" },
        "context_window": {
            "context_window_size": 1_048_576,
            "current_usage": { "input_tokens": 50_000, "output_tokens": 0 }
        }
    })
    .to_string();

    // 1. Explicit set language=ja produces Japanese labels
    let (ok_set_ja, set_out) = run_bin(&["set", "language=ja"], &env_vars, None);
    assert!(ok_set_ja, "set language=ja failed: {set_out}");
    assert!(set_out.contains("language = \"ja\""));

    let (ok_run_ja, run_ja_out) = run_bin(&[], &env_vars, Some(&payload));
    assert!(ok_run_ja);
    let plain_ja = strip_ansi(&run_ja_out);
    assert!(
        plain_ja.contains("待機中"),
        "expected Japanese idle status '待機中', got: {plain_ja}"
    );
    assert!(
        plain_ja.contains("コンテキスト"),
        "expected Japanese context metric 'コンテキスト', got: {plain_ja}"
    );

    // 2. Preview with --set language=ja also renders Japanese
    let (ok_prev, prev_out) = run_bin(
        &["preview", "--state", "tool_use", "--set", "language=ja"],
        &env_vars,
        None,
    );
    assert!(ok_prev);
    let plain_prev = strip_ansi(&prev_out);
    assert!(
        plain_prev.contains("ツール実行"),
        "expected Japanese 'ツール実行', got: {plain_prev}"
    );

    // 3. Reset to language=auto and test auto-detection via LANG env var
    let (ok_set_auto, _) = run_bin(&["set", "language=auto"], &env_vars, None);
    assert!(ok_set_auto);

    // Subprocess with LANG=ja_JP.UTF-8 auto-detects Japanese
    let mut cmd_ja = Command::new(env!("CARGO_BIN_EXE_agy-hud"));
    cmd_ja
        .env("AGY_HUD_DISABLE_BG_UPDATE", "1")
        .env("HOME", &fake_home)
        .env("USERPROFILE", &fake_home)
        .env("AGY_CONFIG_DIR", &gemini_cfg)
        .env("AGY_CLI_DIR", &cli_dir)
        .env("AGY_HUD_HOME", &hud_home)
        .env("LANG", "ja_JP.UTF-8")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child_ja = cmd_ja.spawn().unwrap();
    child_ja
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out_ja = child_ja.wait_with_output().unwrap();
    assert!(out_ja.status.success());
    let ja_auto_str = strip_ansi(&String::from_utf8_lossy(&out_ja.stdout));
    assert!(
        ja_auto_str.contains("待機中"),
        "expected auto-detected Japanese '待機中', got: {ja_auto_str}"
    );

    // Subprocess with LANG=zh_CN.UTF-8 auto-detects Chinese
    let mut cmd_zh = Command::new(env!("CARGO_BIN_EXE_agy-hud"));
    cmd_zh
        .env("AGY_HUD_DISABLE_BG_UPDATE", "1")
        .env("HOME", &fake_home)
        .env("USERPROFILE", &fake_home)
        .env("AGY_CONFIG_DIR", &gemini_cfg)
        .env("AGY_CLI_DIR", &cli_dir)
        .env("AGY_HUD_HOME", &hud_home)
        .env("LANG", "zh_CN.UTF-8")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child_zh = cmd_zh.spawn().unwrap();
    child_zh
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out_zh = child_zh.wait_with_output().unwrap();
    assert!(out_zh.status.success());
    let zh_auto_str = strip_ansi(&String::from_utf8_lossy(&out_zh.stdout));
    assert!(
        zh_auto_str.contains("就绪"),
        "expected auto-detected Chinese '就绪', got: {zh_auto_str}"
    );

    // Explicit override language=en ignores LANG=ja_JP.UTF-8
    let _ = run_bin(&["set", "language=en"], &env_vars, None);
    let mut cmd_override = Command::new(env!("CARGO_BIN_EXE_agy-hud"));
    cmd_override
        .env("AGY_HUD_DISABLE_BG_UPDATE", "1")
        .env("HOME", &fake_home)
        .env("USERPROFILE", &fake_home)
        .env("AGY_CONFIG_DIR", &gemini_cfg)
        .env("AGY_CLI_DIR", &cli_dir)
        .env("AGY_HUD_HOME", &hud_home)
        .env("LANG", "ja_JP.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child_ovr = cmd_override.spawn().unwrap();
    child_ovr
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out_ovr = child_ovr.wait_with_output().unwrap();
    assert!(out_ovr.status.success());
    let en_override_str = strip_ansi(&String::from_utf8_lossy(&out_ovr.stdout));
    assert!(
        en_override_str.contains("READY"),
        "expected explicit English 'READY', got: {en_override_str}"
    );
}

#[test]
fn unprefixed_skills_install_standalone_exec_embedded_fallback_and_clean_uninstall() {
    let tmp = TempDir::new("unprefixed-install-e2e");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let skills_dir = gemini_cfg.join("skills");
    let sibling_skill_dir = skills_dir.join("user-custom");
    let cli_dir = fake_home.join(".gemini/antigravity-cli");
    let hud_home = cli_dir.join("agy-hud");

    fs::create_dir_all(&sibling_skill_dir).unwrap();
    fs::write(sibling_skill_dir.join("SKILL.md"), "# custom user skill\n").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gemini_cfg, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&skills_dir, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let env_vars: [(&str, &Path); 5] = [
        ("HOME", fake_home.as_path()),
        ("USERPROFILE", fake_home.as_path()),
        ("AGY_CONFIG_DIR", gemini_cfg.as_path()),
        ("AGY_CLI_DIR", cli_dir.as_path()),
        ("AGY_HUD_HOME", hud_home.as_path()),
    ];

    // 1. Install deploys managed unprefixed `/hud` and `/hud-update` skills without chmod 0700 on parent dirs
    let (ok_inst, inst_out) = run_bin(&["install", "--stack"], &env_vars, None);
    assert!(ok_inst, "install failed: {inst_out}");
    let inst_json: Value = serde_json::from_str(&inst_out).unwrap();
    assert_eq!(inst_json["unprefixedSkills"]["hud"], "installed");
    assert_eq!(inst_json["unprefixedSkills"]["hudUpdate"], "installed");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&gemini_cfg).unwrap().permissions().mode() & 0o777,
            0o755,
            "install must never chmod 0700 existing gemini_config_dir"
        );
        assert_eq!(
            fs::metadata(&skills_dir).unwrap().permissions().mode() & 0o777,
            0o755,
            "install must never chmod 0700 existing skills dir"
        );
        for name in ["hud", "hud-update"] {
            let d = skills_dir.join(name);
            assert_eq!(
                fs::metadata(d.join("SKILL.md"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o644
            );
            assert_eq!(
                fs::metadata(d.join(".agy-hud-managed.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o644
            );
            assert_eq!(
                fs::metadata(d.join("scripts/hud-cli.sh"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
    }

    for name in ["hud", "hud-update"] {
        let marker: Value = serde_json::from_str(
            &fs::read_to_string(skills_dir.join(name).join(".agy-hud-managed.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(marker["managedBy"], "agy-hud");
        assert_eq!(marker["skill"], name);
        assert_eq!(marker["version"], env!("CARGO_PKG_VERSION"));
    }

    // 2. `doctor` JSON and `--pretty` report both unprefixed skills installed & abbreviate `~`
    let (ok_doc, doc_out) = run_bin(&["doctor"], &env_vars, None);
    assert!(ok_doc);
    let doc_json: Value = serde_json::from_str(&doc_out).unwrap();
    assert_eq!(doc_json["unprefixedSkills"]["hud"], "installed");
    assert_eq!(doc_json["unprefixedSkills"]["hudUpdate"], "installed");

    let (ok_pretty, pretty_out) = run_bin(&["doctor", "--pretty"], &env_vars, None);
    assert!(ok_pretty);
    let pretty_plain = strip_ansi(&pretty_out);
    let pretty_norm = pretty_plain.replace('\\', "/");
    assert!(
        pretty_norm.contains(
            "Unprefixed Skills: /hud=installed · /hud-update=installed (~/.gemini/config/skills)"
        ),
        "unexpected pretty line: {pretty_plain}"
    );

    // 3. Execute standalone `skills/{hud,hud-update}/scripts/hud-cli.sh --version` outside any plugin root
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["hud", "hud-update"] {
            let sh_out = Command::new("sh")
                .args(["scripts/hud-cli.sh", "--version"])
                .current_dir(skills_dir.join(name))
                .env("HOME", &fake_home)
                .env("USERPROFILE", &fake_home)
                .env("AGY_CONFIG_DIR", &gemini_cfg)
                .env("AGY_CLI_DIR", &cli_dir)
                .env("AGY_HUD_HOME", &hud_home)
                .output()
                .unwrap();
            assert!(
                sh_out.status.success(),
                "standalone {name}/scripts/hud-cli.sh failed: {}",
                String::from_utf8_lossy(&sh_out.stderr)
            );
            let ver_str = String::from_utf8_lossy(&sh_out.stdout);
            assert!(ver_str.contains("rust-native"), "ver_str: {ver_str}");
        }

        // Verify `hud-cli.sh` fallback chain when `$HUD_HOME/bin/agy-hud` is absent
        let bin_path = hud_home.join("bin/agy-hud");
        let bin_backup = tmp.path.join("agy-hud.bak");
        fs::rename(&bin_path, &bin_backup).unwrap();

        let hud_plugin_router = hud_home.join("plugin/scripts/agy-hud");
        fs::create_dir_all(hud_plugin_router.parent().unwrap()).unwrap();
        fs::write(&hud_plugin_router, "#!/bin/sh\necho from-hud-home-plugin\n").unwrap();
        fs::set_permissions(&hud_plugin_router, fs::Permissions::from_mode(0o755)).unwrap();

        let cfg_plugin_router = gemini_cfg.join("plugins/agy-hud/scripts/agy-hud");
        fs::create_dir_all(cfg_plugin_router.parent().unwrap()).unwrap();
        fs::write(
            &cfg_plugin_router,
            "#!/bin/sh\necho from-gemini-config-plugin\n",
        )
        .unwrap();
        fs::set_permissions(&cfg_plugin_router, fs::Permissions::from_mode(0o755)).unwrap();

        let fb1 = Command::new("sh")
            .arg("scripts/hud-cli.sh")
            .current_dir(skills_dir.join("hud"))
            .env("HOME", &fake_home)
            .env("USERPROFILE", &fake_home)
            .env("AGY_CONFIG_DIR", &gemini_cfg)
            .env("AGY_CLI_DIR", &cli_dir)
            .env("AGY_HUD_HOME", &hud_home)
            .output()
            .unwrap();
        assert!(fb1.status.success());
        assert_eq!(
            String::from_utf8_lossy(&fb1.stdout).trim(),
            "from-hud-home-plugin"
        );

        fs::remove_file(&hud_plugin_router).unwrap();
        let fb2 = Command::new("sh")
            .arg("scripts/hud-cli.sh")
            .current_dir(skills_dir.join("hud-update"))
            .env("HOME", &fake_home)
            .env("USERPROFILE", &fake_home)
            .env("AGY_CONFIG_DIR", &gemini_cfg)
            .env("AGY_CLI_DIR", &cli_dir)
            .env("AGY_HUD_HOME", &hud_home)
            .output()
            .unwrap();
        assert!(fb2.status.success());
        assert_eq!(
            String::from_utf8_lossy(&fb2.stdout).trim(),
            "from-gemini-config-plugin"
        );

        fs::rename(&bin_backup, &bin_path).unwrap();
        fs::remove_dir_all(hud_home.join("plugin")).unwrap();
        fs::remove_dir_all(gemini_cfg.join("plugins")).unwrap();
    }

    // 4. Standalone binary (outside repo `target/`) tests `resolve_plugin_root` & compile-time `include_str!` fallback
    let standalone_dir = tmp.path.join("standalone-bin");
    fs::create_dir_all(&standalone_dir).unwrap();
    let standalone_bin_name = if cfg!(windows) {
        "agy-hud.exe"
    } else {
        "agy-hud"
    };
    let standalone_exe = standalone_dir.join(standalone_bin_name);
    fs::copy(env!("CARGO_BIN_EXE_agy-hud"), &standalone_exe).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&standalone_exe, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let iso_home = tmp.path.join("iso-home");
    let iso_cfg = iso_home.join(".gemini/config");
    let iso_cli = iso_home.join(".gemini/antigravity-cli");
    let iso_hud = iso_cli.join("agy-hud");
    fs::create_dir_all(&iso_home).unwrap();

    // 4a. `resolve_plugin_root` finds `gemini_config_dir/plugins/agy-hud`, then `hud_home/plugin`, then `cwd` ancestors
    let cfg_plugin = iso_cfg.join("plugins/agy-hud");
    fs::create_dir_all(&cfg_plugin).unwrap();
    fs::write(
        cfg_plugin.join("plugin.json"),
        r#"{"name":"agy-hud","version":"8.8.8"}"#,
    )
    .unwrap();
    let doc_cfg_root = Command::new(&standalone_exe)
        .arg("doctor")
        .current_dir(&standalone_dir)
        .env("HOME", &iso_home)
        .env("USERPROFILE", &iso_home)
        .env("AGY_CONFIG_DIR", &iso_cfg)
        .env("AGY_CLI_DIR", &iso_cli)
        .env("AGY_HUD_HOME", &iso_hud)
        .env("AGY_SKIP_PLUGIN_INSTALL", "1")
        .output()
        .unwrap();
    assert!(doc_cfg_root.status.success());
    let doc_cfg_json: Value = serde_json::from_slice(&doc_cfg_root.stdout).unwrap();
    assert_eq!(doc_cfg_json["plugin"]["version"], "8.8.8");

    fs::remove_dir_all(&cfg_plugin).unwrap();
    let staged_plugin = iso_hud.join("plugin");
    fs::create_dir_all(&staged_plugin).unwrap();
    fs::write(
        staged_plugin.join("plugin.json"),
        r#"{"name":"agy-hud","version":"7.7.7"}"#,
    )
    .unwrap();
    let doc_staged_root = Command::new(&standalone_exe)
        .arg("doctor")
        .current_dir(&standalone_dir)
        .env("HOME", &iso_home)
        .env("USERPROFILE", &iso_home)
        .env("AGY_CONFIG_DIR", &iso_cfg)
        .env("AGY_CLI_DIR", &iso_cli)
        .env("AGY_HUD_HOME", &iso_hud)
        .env("AGY_SKIP_PLUGIN_INSTALL", "1")
        .output()
        .unwrap();
    assert!(doc_staged_root.status.success());
    let doc_staged_json: Value = serde_json::from_slice(&doc_staged_root.stdout).unwrap();
    assert_eq!(doc_staged_json["plugin"]["version"], "7.7.7");

    fs::remove_dir_all(&staged_plugin).unwrap();
    let cwd_plugin = tmp.path.join("cwd-plugin");
    let cwd_skill = cwd_plugin.join("skills/hud");
    fs::create_dir_all(&cwd_skill).unwrap();
    fs::write(
        cwd_plugin.join("plugin.json"),
        r#"{"name":"agy-hud","version":"6.6.6"}"#,
    )
    .unwrap();
    let doc_cwd_root = Command::new(&standalone_exe)
        .arg("doctor")
        .current_dir(&cwd_skill)
        .env("HOME", &iso_home)
        .env("USERPROFILE", &iso_home)
        .env("AGY_CONFIG_DIR", &iso_cfg)
        .env("AGY_CLI_DIR", &iso_cli)
        .env("AGY_HUD_HOME", &iso_hud)
        .env("AGY_SKIP_PLUGIN_INSTALL", "1")
        .output()
        .unwrap();
    assert!(doc_cwd_root.status.success());
    let doc_cwd_json: Value = serde_json::from_slice(&doc_cwd_root.stdout).unwrap();
    assert_eq!(doc_cwd_json["plugin"]["version"], "6.6.6");

    // 4b. Compile-time embedded fallback installs skills when NO on-disk `skills/` exists anywhere
    let embed_inst = Command::new(&standalone_exe)
        .args(["install", "--stack"])
        .current_dir(&standalone_dir)
        .env("HOME", &iso_home)
        .env("USERPROFILE", &iso_home)
        .env("AGY_CONFIG_DIR", &iso_cfg)
        .env("AGY_CLI_DIR", &iso_cli)
        .env("AGY_HUD_HOME", &iso_hud)
        .output()
        .unwrap();
    assert!(
        embed_inst.status.success(),
        "embedded fallback install failed: {}",
        String::from_utf8_lossy(&embed_inst.stderr)
    );
    let embed_json: Value = serde_json::from_slice(&embed_inst.stdout).unwrap();
    assert_eq!(embed_json["unprefixedSkills"]["hud"], "installed");
    assert_eq!(embed_json["unprefixedSkills"]["hudUpdate"], "installed");
    let installed_hud_md = fs::read_to_string(iso_cfg.join("skills/hud/SKILL.md")).unwrap();
    assert!(installed_hud_md.contains("name: hud"));
    assert!(
        fs::read_to_string(iso_cfg.join("skills/hud-update/scripts/hud-cli.sh"))
            .unwrap()
            .contains("HUD_HOME")
    );

    // Clean uninstall on `iso_home` (no siblings) removes `skills/` completely
    let iso_uninst = Command::new(&standalone_exe)
        .arg("uninstall")
        .current_dir(&standalone_dir)
        .env("HOME", &iso_home)
        .env("USERPROFILE", &iso_home)
        .env("AGY_CONFIG_DIR", &iso_cfg)
        .env("AGY_CLI_DIR", &iso_cli)
        .env("AGY_HUD_HOME", &iso_hud)
        .output()
        .unwrap();
    assert!(iso_uninst.status.success());
    assert!(!iso_cfg.join("skills").exists());

    // 5. Uninstall on `fake_home` removes ONLY managed shortcuts while preserving `skills/user-custom`
    let (ok_uninst, uninst_out) = run_bin(&["uninstall"], &env_vars, None);
    assert!(ok_uninst);
    let uninst_json: Value = serde_json::from_str(&uninst_out).unwrap();
    assert_eq!(
        uninst_json["unprefixedSkillsRemoved"],
        json!(["hud", "hud-update"])
    );
    assert!(!skills_dir.join("hud").exists());
    assert!(!skills_dir.join("hud-update").exists());
    assert_eq!(
        fs::read_to_string(sibling_skill_dir.join("SKILL.md")).unwrap(),
        "# custom user skill\n"
    );
}

#[test]
fn unprefixed_skills_conflict_protection_dirs_symlinks_and_readonly_skills_json() {
    let tmp = TempDir::new("unprefixed-conflicts");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let skills_dir = gemini_cfg.join("skills");
    let cli_dir = fake_home.join(".gemini/antigravity-cli");
    let hud_home = cli_dir.join("agy-hud");

    // 1. Existing non-managed dir (`skills/hud`) and symlink/dangling symlink (`skills/hud-update`)
    fs::create_dir_all(skills_dir.join("hud")).unwrap();
    fs::write(skills_dir.join("hud/SKILL.md"), "# my custom hud\n").unwrap();

    #[cfg(unix)]
    std::os::unix::fs::symlink(
        tmp.path.join("nonexistent-skill-target"),
        skills_dir.join("hud-update"),
    )
    .unwrap();
    #[cfg(not(unix))]
    {
        fs::create_dir_all(skills_dir.join("hud-update")).unwrap();
        fs::write(
            skills_dir.join("hud-update/SKILL.md"),
            "# my custom hud-update\n",
        )
        .unwrap();
    }

    let env_vars: [(&str, &Path); 5] = [
        ("HOME", fake_home.as_path()),
        ("USERPROFILE", fake_home.as_path()),
        ("AGY_CONFIG_DIR", gemini_cfg.as_path()),
        ("AGY_CLI_DIR", cli_dir.as_path()),
        ("AGY_HUD_HOME", hud_home.as_path()),
    ];

    let (ok_inst, inst_out) = run_bin(&["install", "--stack"], &env_vars, None);
    assert!(ok_inst, "install failed: {inst_out}");
    let inst_json: Value = serde_json::from_str(&inst_out).unwrap();
    assert_eq!(inst_json["unprefixedSkills"]["hud"], "skipped-conflict");
    assert_eq!(
        inst_json["unprefixedSkills"]["hudUpdate"],
        "skipped-conflict"
    );
    assert_eq!(
        fs::read_to_string(skills_dir.join("hud/SKILL.md")).unwrap(),
        "# my custom hud\n"
    );
    assert!(!skills_dir.join("hud/.agy-hud-managed.json").exists());
    assert!(fs::symlink_metadata(skills_dir.join("hud-update")).is_ok());

    let (ok_doc, doc_out) = run_bin(&["doctor"], &env_vars, None);
    assert!(ok_doc);
    let doc_json: Value = serde_json::from_str(&doc_out).unwrap();
    assert_eq!(doc_json["unprefixedSkills"]["hud"], "conflict");
    assert_eq!(doc_json["unprefixedSkills"]["hudUpdate"], "conflict");
    let findings = doc_json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        findings.contains("preserved (not overwritten)"),
        "{findings}"
    );
    assert!(findings.contains("/agy-hud:hud"), "{findings}");
    assert!(findings.contains("/agy-hud:hud-update"), "{findings}");

    let (ok_uninst, uninst_out) = run_bin(&["uninstall"], &env_vars, None);
    assert!(ok_uninst);
    let uninst_json: Value = serde_json::from_str(&uninst_out).unwrap();
    assert_eq!(uninst_json["unprefixedSkillsRemoved"], json!([]));
    assert_eq!(
        fs::read_to_string(skills_dir.join("hud/SKILL.md")).unwrap(),
        "# my custom hud\n"
    );
    assert!(fs::symlink_metadata(skills_dir.join("hud-update")).is_ok());

    // 1b. Symlink pointing to a directory that DOES contain `.agy-hud-managed.json` must STILL be preserved
    #[cfg(unix)]
    {
        fs::remove_dir_all(skills_dir.join("hud")).unwrap();
        let target_dir = tmp.path.join("symlink-target-managed");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(
            target_dir.join(".agy-hud-managed.json"),
            r#"{"managedBy":"agy-hud","skill":"hud","version":"1.0.0"}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(&target_dir, skills_dir.join("hud")).unwrap();

        let (_, inst_sym) = run_bin(&["install", "--stack"], &env_vars, None);
        let inst_sym_json: Value = serde_json::from_str(&inst_sym).unwrap();
        assert_eq!(inst_sym_json["unprefixedSkills"]["hud"], "skipped-conflict");

        let (_, uninst_sym) = run_bin(&["uninstall"], &env_vars, None);
        let uninst_sym_json: Value = serde_json::from_str(&uninst_sym).unwrap();
        assert_eq!(uninst_sym_json["unprefixedSkillsRemoved"], json!([]));
        assert!(
            fs::symlink_metadata(skills_dir.join("hud"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(target_dir.join(".agy-hud-managed.json").is_file());
    }

    // 2. Strictly read-only `skills.json` conflict protection (JSONC + exclude + self-exclusion + inherits)
    let _ = fs::remove_dir_all(&skills_dir);

    let custom_root = fake_home.join("custom-skills");
    fs::create_dir_all(custom_root.join("hud")).unwrap();
    fs::create_dir_all(custom_root.join("hud-update")).unwrap();
    fs::write(custom_root.join("hud/SKILL.md"), "# foreign hud\n").unwrap();
    fs::write(
        custom_root.join("hud-update/SKILL.md"),
        "# excluded hud-update\n",
    )
    .unwrap();

    // Plant `agy-hud`'s own plugin skill dirs to verify self-exclusion
    let self_plugin_skills = gemini_cfg.join("plugins/agy-hud/skills");
    fs::create_dir_all(self_plugin_skills.join("hud-update")).unwrap();
    fs::write(
        self_plugin_skills.join("hud-update/SKILL.md"),
        "# self plugin skill\n",
    )
    .unwrap();
    let self_staged_skills = hud_home.join("plugin/skills");
    fs::create_dir_all(self_staged_skills.join("hud-update")).unwrap();
    fs::write(
        self_staged_skills.join("hud-update/SKILL.md"),
        "# self staged skill\n",
    )
    .unwrap();

    let skills_json_path = gemini_cfg.join("skills.json");
    let skills_json_raw = format!(
        r#"{{
  // User JSONC comment that must NEVER be stripped or rewritten
  "entries": [
    {{
      "path": "~/custom-skills",
      "exclude": ["hud-update"],
    }},
    {{
      "path": "~/.gemini/config/plugins/agy-hud/skills"
    }},
    {{
      "path": {:?}
    }},
  ]
}}
"#,
        self_staged_skills.to_string_lossy()
    );
    fs::write(&skills_json_path, &skills_json_raw).unwrap();

    let (ok_json_inst, json_inst_out) = run_bin(&["install", "--stack"], &env_vars, None);
    assert!(ok_json_inst, "install failed: {json_inst_out}");
    let json_inst: Value = serde_json::from_str(&json_inst_out).unwrap();
    assert_eq!(json_inst["unprefixedSkills"]["hud"], "skipped-conflict");
    assert_eq!(json_inst["unprefixedSkills"]["hudUpdate"], "installed");
    assert!(!skills_dir.join("hud").exists());
    assert!(
        skills_dir
            .join("hud-update/.agy-hud-managed.json")
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(&skills_json_path).unwrap(),
        skills_json_raw,
        "skills.json must remain strictly read-only"
    );

    let (_, json_doc_out) = run_bin(&["doctor"], &env_vars, None);
    let json_doc: Value = serde_json::from_str(&json_doc_out).unwrap();
    assert_eq!(json_doc["unprefixedSkills"]["hud"], "conflict");
    assert_eq!(json_doc["unprefixedSkills"]["hudUpdate"], "installed");

    let (_, json_uninst_out) = run_bin(&["uninstall"], &env_vars, None);
    let json_uninst: Value = serde_json::from_str(&json_uninst_out).unwrap();
    assert_eq!(
        json_uninst["unprefixedSkillsRemoved"],
        json!(["hud-update"])
    );
    assert_eq!(
        fs::read_to_string(&skills_json_path).unwrap(),
        skills_json_raw
    );

    // 3. Test `inherits` + `include_only` + direct `<name>` folder in `skills.json`
    let direct_hud_update = tmp.path.join("direct-dir/hud-update");
    fs::create_dir_all(&direct_hud_update).unwrap();
    fs::write(
        direct_hud_update.join("SKILL.md"),
        "# direct inherited hud-update\n",
    )
    .unwrap();
    let shared_json_path = gemini_cfg.join("shared-skills.json");
    fs::write(
        &shared_json_path,
        serde_json::to_string_pretty(&json!({
            "entries": [{ "path": direct_hud_update }]
        }))
        .unwrap(),
    )
    .unwrap();
    let inherited_raw = serde_json::to_string_pretty(&json!({
        "inherits": [{
            "path": "shared-skills.json",
            "include_only": ["hud-update"]
        }]
    }))
    .unwrap();
    fs::write(&skills_json_path, &inherited_raw).unwrap();

    let (_, inh_inst_out) = run_bin(&["install", "--stack"], &env_vars, None);
    let inh_inst: Value = serde_json::from_str(&inh_inst_out).unwrap();
    assert_eq!(inh_inst["unprefixedSkills"]["hud"], "installed");
    assert_eq!(
        inh_inst["unprefixedSkills"]["hudUpdate"],
        "skipped-conflict"
    );
    let (_, inh_doc_out) = run_bin(&["doctor"], &env_vars, None);
    let inh_doc: Value = serde_json::from_str(&inh_doc_out).unwrap();
    assert_eq!(inh_doc["unprefixedSkills"]["hud"], "installed");
    assert_eq!(inh_doc["unprefixedSkills"]["hudUpdate"], "conflict");
    assert_eq!(
        fs::read_to_string(&skills_json_path).unwrap(),
        inherited_raw
    );
}

#[test]
fn live_statusline_preview_and_uninstall_sh_fallback_with_short_and_prefixed_commands() {
    let tmp = TempDir::new("live-cmd-switch");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let skills_dir = gemini_cfg.join("skills");
    let cli_dir = fake_home.join(".gemini/antigravity-cli");
    let hud_home = cli_dir.join("agy-hud");

    fs::create_dir_all(hud_home.join("data")).unwrap();
    fs::write(
        hud_home.join("config.json"),
        r#"{"invalidKeyForTest":true}"#,
    )
    .unwrap();
    fs::write(
        hud_home.join("data/update-cache.json"),
        serde_json::to_string(&json!({
            "checkedAt": 1_700_000_000_000i64,
            "latestVersion": "9.9.0"
        }))
        .unwrap(),
    )
    .unwrap();

    let env_vars: [(&str, &Path); 5] = [
        ("HOME", fake_home.as_path()),
        ("USERPROFILE", fake_home.as_path()),
        ("AGY_CONFIG_DIR", gemini_cfg.as_path()),
        ("AGY_CLI_DIR", cli_dir.as_path()),
        ("AGY_HUD_HOME", hud_home.as_path()),
    ];
    let live_payload = json!({
        "cwd": tmp.path,
        "conversation_id": "cmd-switch-conv",
        "agent_state": "idle",
        "terminal_width": 140,
        "model": { "id": "Gemini 3 Flash", "display_name": "Gemini 3 Flash" },
        "context_window": { "context_window_size": 1_048_576, "used_percentage": 18.0 }
    })
    .to_string();

    // 1. Neither unprefixed shortcut installed -> live & preview use `/agy-hud:hud` and `/agy-hud:hud-update`
    let (ok_absent, out_absent) = run_bin(&[], &env_vars, Some(&live_payload));
    assert!(ok_absent);
    let plain_absent = strip_ansi(&out_absent);
    assert!(
        plain_absent.contains("· /agy-hud:hud"),
        "expected /agy-hud:hud config warning: {plain_absent}"
    );
    assert!(
        plain_absent.contains("↑ v9.9.0 · /agy-hud:hud-update"),
        "expected /agy-hud:hud-update badge: {plain_absent}"
    );

    let (ok_prev_absent, prev_absent) = run_bin(&["preview", "--state", "idle"], &env_vars, None);
    assert!(ok_prev_absent);
    assert!(strip_ansi(&prev_absent).contains("· /agy-hud:hud"));

    // 2. Mixed state: `/hud` conflicted while `/hud-update` installed -> `/agy-hud:hud` + `/hud-update`
    fs::create_dir_all(skills_dir.join("hud")).unwrap();
    fs::write(skills_dir.join("hud/SKILL.md"), "# custom hud\n").unwrap();
    let (ok_mixed_inst, _) = run_bin(&["install", "--stack"], &env_vars, None);
    assert!(ok_mixed_inst);

    let (ok_mixed, out_mixed) = run_bin(&[], &env_vars, Some(&live_payload));
    assert!(ok_mixed);
    let plain_mixed = strip_ansi(&out_mixed);
    assert!(
        plain_mixed.contains("· /agy-hud:hud"),
        "conflicted /hud must keep /agy-hud:hud: {plain_mixed}"
    );
    assert!(
        plain_mixed.contains("↑ v9.9.0 · /hud-update"),
        "installed /hud-update must use short command: {plain_mixed}"
    );
    assert!(!plain_mixed.contains("/agy-hud:hud-update"));

    // 3. Both installed cleanly -> live & preview use `/hud` and `/hud-update`
    fs::remove_dir_all(skills_dir.join("hud")).unwrap();
    let (ok_both_inst, _) = run_bin(&["install", "--stack"], &env_vars, None);
    assert!(ok_both_inst);

    let (ok_short, out_short) = run_bin(&[], &env_vars, Some(&live_payload));
    assert!(ok_short);
    let plain_short = strip_ansi(&out_short);
    assert!(plain_short.contains("· /hud"), "{plain_short}");
    assert!(
        plain_short.contains("↑ v9.9.0 · /hud-update"),
        "{plain_short}"
    );
    assert!(!plain_short.contains("/agy-hud:hud"), "{plain_short}");

    let (ok_prev_short, prev_short) = run_bin(&["preview", "--state", "idle"], &env_vars, None);
    assert!(ok_prev_short);
    let prev_short_plain = strip_ansi(&prev_short);
    assert!(prev_short_plain.contains("· /hud"), "{prev_short_plain}");
    assert!(
        !prev_short_plain.contains("/agy-hud:hud"),
        "{prev_short_plain}"
    );

    // 4. `uninstall.sh` binary-missing fallback cleans managed skill dirs while preserving foreign/symlinked ones
    #[cfg(unix)]
    {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        fs::remove_file(hud_home.join("bin/agy-hud")).unwrap();

        // Replace `skills/hud-update` with a foreign directory (no `.agy-hud-managed.json`), keep `skills/hud` managed
        fs::remove_dir_all(skills_dir.join("hud-update")).unwrap();
        fs::create_dir_all(skills_dir.join("hud-update")).unwrap();
        fs::write(
            skills_dir.join("hud-update/SKILL.md"),
            "# foreign hud-update\n",
        )
        .unwrap();

        let sh_uninst = Command::new("sh")
            .arg(workspace_root.join("uninstall.sh"))
            .env("HOME", &fake_home)
            .env("USERPROFILE", &fake_home)
            .env("AGY_CONFIG_DIR", &gemini_cfg)
            .env("AGY_CLI_DIR", &cli_dir)
            .env("AGY_HUD_HOME", &hud_home)
            .env("AGY_SKIP_PLUGIN_INSTALL", "1")
            .output()
            .unwrap();
        assert!(
            sh_uninst.status.success(),
            "uninstall.sh fallback failed: {}",
            String::from_utf8_lossy(&sh_uninst.stderr)
        );
        assert!(
            !skills_dir.join("hud").exists(),
            "managed skills/hud must be removed by uninstall.sh fallback"
        );
        assert_eq!(
            fs::read_to_string(skills_dir.join("hud-update/SKILL.md")).unwrap(),
            "# foreign hud-update\n",
            "foreign skills/hud-update must be preserved by uninstall.sh fallback"
        );
        assert!(!hud_home.exists());

        // Also test `uninstall.sh` fallback refuses to follow a symlinked `skills/hud` even if target has marker
        let sym_target = tmp.path.join("sym-managed-target");
        fs::create_dir_all(&sym_target).unwrap();
        fs::write(
            sym_target.join(".agy-hud-managed.json"),
            r#"{"managedBy":"agy-hud","skill":"hud","version":"1.0.0"}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(&sym_target, skills_dir.join("hud")).unwrap();
        let sh_uninst2 = Command::new("sh")
            .arg(workspace_root.join("uninstall.sh"))
            .env("HOME", &fake_home)
            .env("USERPROFILE", &fake_home)
            .env("AGY_CONFIG_DIR", &gemini_cfg)
            .env("AGY_CLI_DIR", &cli_dir)
            .env("AGY_HUD_HOME", &hud_home)
            .env("AGY_SKIP_PLUGIN_INSTALL", "1")
            .output()
            .unwrap();
        assert!(sh_uninst2.status.success());
        assert!(
            fs::symlink_metadata(skills_dir.join("hud"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(sym_target.join(".agy-hud-managed.json").is_file());
    }
}

fn push_pb_varint(out: &mut Vec<u8>, mut val: u64) {
    loop {
        let byte = (val & 0x7f) as u8;
        val >>= 7;
        if val == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

fn push_pb_bytes_field(out: &mut Vec<u8>, field_num: u64, data: &[u8]) {
    push_pb_varint(out, (field_num << 3) | 2);
    push_pb_varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

fn push_pb_varint_field(out: &mut Vec<u8>, field_num: u64, val: u64) {
    push_pb_varint(out, field_num << 3);
    push_pb_varint(out, val);
}

fn fixture_gen_blob(
    ceiling: Option<u64>,
    model_id: Option<&str>,
    display_name: Option<&str>,
    subagent_type: Option<&str>,
) -> Vec<u8> {
    let mut chat = Vec::new();
    if let Some(c) = ceiling {
        let mut f10 = Vec::new();
        push_pb_varint_field(&mut f10, 4, c);
        let mut f9 = Vec::new();
        push_pb_bytes_field(&mut f9, 10, &f10);
        push_pb_bytes_field(&mut chat, 9, &f9);
    }
    if let Some(mid) = model_id {
        push_pb_bytes_field(&mut chat, 19, mid.as_bytes());
    }
    if let Some(sub) = subagent_type {
        let mut entry = Vec::new();
        push_pb_bytes_field(&mut entry, 1, b"subagent_type");
        push_pb_bytes_field(&mut entry, 2, sub.as_bytes());
        push_pb_bytes_field(&mut chat, 20, &entry);
    }
    if let Some(disp) = display_name {
        push_pb_bytes_field(&mut chat, 21, disp.as_bytes());
    }
    let mut top = Vec::new();
    push_pb_bytes_field(&mut top, 1, &chat);
    top
}

fn fixture_exec_blob(max_token_limit: u64) -> Vec<u8> {
    let mut f2 = Vec::new();
    push_pb_varint_field(&mut f2, 5, max_token_limit);
    let mut f10 = Vec::new();
    push_pb_bytes_field(&mut f10, 2, &f2);
    let mut top = Vec::new();
    push_pb_bytes_field(&mut top, 10, &f10);
    top
}

fn push_sqlite_varint(out: &mut Vec<u8>, v: u64) {
    if v <= 0x7f {
        out.push(v as u8);
        return;
    }
    let mut buf = [0u8; 9];
    let mut len = 0usize;
    let mut x = v;
    while x > 0 {
        buf[8 - len] = (x & 0x7f) as u8;
        x >>= 7;
        len += 1;
    }
    for slot in &mut buf[(9 - len)..8] {
        *slot |= 0x80;
    }
    out.extend_from_slice(&buf[9 - len..9]);
}

enum FixtureCol<'a> {
    Null,
    Int(i64),
    Text(&'a str),
    Blob(&'a [u8]),
}

fn build_fixture_record(cols: &[FixtureCol<'_>]) -> Vec<u8> {
    let mut types_bytes = Vec::new();
    let mut body = Vec::new();
    for c in cols {
        match c {
            FixtureCol::Null => push_sqlite_varint(&mut types_bytes, 0),
            FixtureCol::Int(v) => {
                if *v == 0 {
                    push_sqlite_varint(&mut types_bytes, 8);
                } else if *v == 1 {
                    push_sqlite_varint(&mut types_bytes, 9);
                } else if let Ok(v8) = i8::try_from(*v) {
                    push_sqlite_varint(&mut types_bytes, 1);
                    body.push(v8 as u8);
                } else {
                    push_sqlite_varint(&mut types_bytes, 4);
                    body.extend_from_slice(&(*v as i32).to_be_bytes());
                }
            }
            FixtureCol::Text(s) => {
                push_sqlite_varint(&mut types_bytes, 13 + (s.len() as u64) * 2);
                body.extend_from_slice(s.as_bytes());
            }
            FixtureCol::Blob(b) => {
                push_sqlite_varint(&mut types_bytes, 12 + (b.len() as u64) * 2);
                body.extend_from_slice(b);
            }
        }
    }
    let mut hdr_len = 1 + types_bytes.len();
    if hdr_len >= 128 {
        hdr_len += 1;
    }
    let mut rec = Vec::with_capacity(hdr_len + body.len());
    push_sqlite_varint(&mut rec, hdr_len as u64);
    rec.extend_from_slice(&types_bytes);
    rec.extend_from_slice(&body);
    rec
}

fn build_leaf_page(pgno: u32, rows: &[(u64, Vec<u8>)]) -> Vec<u8> {
    const PAGE_SIZE: usize = 4096;
    let mut page = vec![0u8; PAGE_SIZE];
    let hdr_off = if pgno == 1 {
        page[0..16].copy_from_slice(b"SQLite format 3\0");
        page[16..18].copy_from_slice(&(PAGE_SIZE as u16).to_be_bytes());
        page[18] = 2;
        page[19] = 2;
        100
    } else {
        0
    };
    let mut content_end = PAGE_SIZE;
    let mut cell_ptrs = Vec::with_capacity(rows.len());
    for (rowid, payload) in rows {
        assert!(payload.len() <= PAGE_SIZE - 35);
        let mut cell = Vec::new();
        push_sqlite_varint(&mut cell, payload.len() as u64);
        push_sqlite_varint(&mut cell, *rowid);
        cell.extend_from_slice(payload);
        content_end -= cell.len();
        page[content_end..content_end + cell.len()].copy_from_slice(&cell);
        cell_ptrs.push(content_end as u16);
    }
    page[hdr_off] = 0x0d;
    page[hdr_off + 3..hdr_off + 5].copy_from_slice(&(rows.len() as u16).to_be_bytes());
    page[hdr_off + 5..hdr_off + 7].copy_from_slice(&(content_end as u16).to_be_bytes());
    for (i, ptr) in cell_ptrs.iter().enumerate() {
        let off = hdr_off + 8 + i * 2;
        page[off..off + 2].copy_from_slice(&ptr.to_be_bytes());
    }
    page
}

fn build_two_table_sqlite_db(gen_rows: &[(u64, Vec<u8>)], exec_rows: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let schema = build_leaf_page(
        1,
        &[
            (
                1,
                build_fixture_record(&[
                    FixtureCol::Text("table"),
                    FixtureCol::Text("gen_metadata"),
                    FixtureCol::Text("gen_metadata"),
                    FixtureCol::Int(2),
                    FixtureCol::Text("CREATE TABLE gen_metadata(idx INT, data BLOB)"),
                ]),
            ),
            (
                2,
                build_fixture_record(&[
                    FixtureCol::Text("table"),
                    FixtureCol::Text("executor_metadata"),
                    FixtureCol::Text("executor_metadata"),
                    FixtureCol::Int(3),
                    FixtureCol::Text("CREATE TABLE executor_metadata(idx INT, data BLOB)"),
                ]),
            ),
        ],
    );
    let gen_page = build_leaf_page(2, gen_rows);
    let exec_page = build_leaf_page(3, exec_rows);
    let mut out = Vec::with_capacity(4096 * 3);
    out.extend_from_slice(&schema);
    out.extend_from_slice(&gen_page);
    out.extend_from_slice(&exec_page);
    out
}

fn wal_checksum_le(data: &[u8], mut s0: u32, mut s1: u32) -> (u32, u32) {
    for chunk in data.as_chunks::<8>().0 {
        let x0 = u32::from_le_bytes(chunk[0..4].try_into().unwrap());
        let x1 = u32::from_le_bytes(chunk[4..8].try_into().unwrap());
        s0 = s0.wrapping_add(x0).wrapping_add(s1);
        s1 = s1.wrapping_add(x1).wrapping_add(s0);
    }
    (s0, s1)
}

fn build_fixture_wal(frames: &[(u32, u32, &[u8])]) -> Vec<u8> {
    const PAGE_SIZE: usize = 4096;
    let mut wh = [0u8; 32];
    wh[0..4].copy_from_slice(&0x377f_0682u32.to_be_bytes());
    wh[4..8].copy_from_slice(&3_007_000u32.to_be_bytes());
    wh[8..12].copy_from_slice(&(PAGE_SIZE as u32).to_be_bytes());
    let salt1 = 0x1122_3344u32;
    let salt2 = 0x5566_7788u32;
    wh[16..20].copy_from_slice(&salt1.to_be_bytes());
    wh[20..24].copy_from_slice(&salt2.to_be_bytes());
    let (mut s0, mut s1) = wal_checksum_le(&wh[0..24], 0, 0);
    wh[24..28].copy_from_slice(&s0.to_be_bytes());
    wh[28..32].copy_from_slice(&s1.to_be_bytes());

    let mut out = Vec::from(wh);
    for &(pgno, db_size, page_data) in frames {
        assert_eq!(page_data.len(), PAGE_SIZE);
        let mut fh = [0u8; 24];
        fh[0..4].copy_from_slice(&pgno.to_be_bytes());
        fh[4..8].copy_from_slice(&db_size.to_be_bytes());
        fh[8..12].copy_from_slice(&salt1.to_be_bytes());
        fh[12..16].copy_from_slice(&salt2.to_be_bytes());
        (s0, s1) = wal_checksum_le(&fh[0..8], s0, s1);
        (s0, s1) = wal_checksum_le(page_data, s0, s1);
        fh[16..20].copy_from_slice(&s0.to_be_bytes());
        fh[20..24].copy_from_slice(&s1.to_be_bytes());
        out.extend_from_slice(&fh);
        out.extend_from_slice(page_data);
    }
    out
}

#[test]
fn route_a_db_ceiling_wal_override_switch_guard_snapshot_preview_and_report_e2e() {
    let tmp = TempDir::new("route-a-e2e");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");
    let conv_dir = cli_dir.join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();

    let env_vars: [(&str, &Path); 5] = [
        ("HOME", &fake_home),
        ("USERPROFILE", &fake_home),
        ("AGY_CONFIG_DIR", &gemini_cfg),
        ("AGY_CLI_DIR", &cli_dir),
        ("AGY_HUD_HOME", &hud_home),
    ];

    // Pre-seed a STALE cross-session `compaction-ceilings/` cache (512k) for `gemini 4 hydrogen`.
    let ceilings_dir = hud_home.join("data/compaction-ceilings");
    fs::create_dir_all(&ceilings_dir).unwrap();
    let stale_scope_key = "gemini 4 hydrogen\n1048576";
    let stale_hash = hud_cli::paths::fnv1a_64_hex(stale_scope_key);
    let stale_shard = ceilings_dir.join(format!("{stale_hash}.json"));
    let stale_json = json!({
        "version": 1,
        "modelFamily": "gemini 4 hydrogen",
        "physWin": 1_048_576,
        "kind": "learned",
        "ceiling_tokens": 512_000,
        "observed_tokens": 510_000,
        "updatedAtMs": 1_700_000_000_000i64
    })
    .to_string();
    fs::write(&stale_shard, &stale_json).unwrap();

    // 1. Turn-1 SQLite .db with `gen_metadata = 256_000` + trailing shadow row (`1_048_576`)
    //    MUST override stale `512k` cache, skip shadow row, never create `-shm`, and never pollute `compaction-ceilings/`.
    let db_256 = conv_dir.join("conv-route-256.db");
    let row_256 = fixture_gen_blob(
        Some(256_000),
        Some("gemini-4-hydrogen"),
        Some("Gemini 4 Hydrogen (XHigh)"),
        None,
    );
    let row_shadow = fixture_gen_blob(
        Some(1_048_576),
        Some("gemini-2.5-flash-lite"),
        Some("Gemini 2.5 Flash Lite"),
        None,
    );
    let db_bytes_256 = build_two_table_sqlite_db(
        &[
            (
                1,
                build_fixture_record(&[FixtureCol::Null, FixtureCol::Blob(&row_256)]),
            ),
            (
                2,
                build_fixture_record(&[FixtureCol::Null, FixtureCol::Blob(&row_shadow)]),
            ),
        ],
        &[],
    );
    fs::write(&db_256, &db_bytes_256).unwrap();

    let payload_256 = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-route-256",
        "agent_state": "idle",
        "terminal_width": 130,
        "model": { "id": "Gemini 4 Hydrogen (XHigh)", "display_name": "Gemini 4 Hydrogen (XHigh)", "effort": "xhigh" },
        "context_window": {
            "context_window_size": 1_048_576,
            "current_usage": { "input_tokens": 128_000, "output_tokens": 0 }
        }
    })
    .to_string();

    let (ok1, out1) = run_bin(&[], &env_vars, Some(&payload_256));
    assert!(ok1, "Turn-1 256k render failed: {out1}");
    let plain1 = strip_ansi(&out1);
    assert!(
        plain1.contains("50.0% (128k/256k)"),
        "expected DB 256k overriding stale 512k cache: {plain1}"
    );
    assert!(
        !conv_dir.join("conv-route-256.db-shm").exists(),
        "read-only DB probe must never create -shm"
    );
    assert_eq!(
        fs::read_to_string(&stale_shard).unwrap(),
        stale_json,
        "Route A DB ceiling must never overwrite compaction-ceilings/"
    );
    assert!(
        fs::read_dir(hud_home.join("data/db-ceilings"))
            .unwrap()
            .flatten()
            .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json")),
        "expected db-ceilings/*.json cache file"
    );

    // 2. WAL commit frame updates `gen_metadata` to `512_000` (while trailing uncommitted frame says `1_048_576`)
    let row_512 = fixture_gen_blob(
        Some(512_000),
        Some("gemini-4-hydrogen"),
        Some("Gemini 4 Hydrogen (XHigh)"),
        None,
    );
    let row_uncommitted = fixture_gen_blob(
        Some(1_048_576),
        Some("gemini-4-hydrogen"),
        Some("Gemini 4 Hydrogen (XHigh)"),
        None,
    );
    let wal_page_512 = build_leaf_page(
        2,
        &[(
            1,
            build_fixture_record(&[FixtureCol::Null, FixtureCol::Blob(&row_512)]),
        )],
    );
    let wal_page_uncommitted = build_leaf_page(
        2,
        &[(
            1,
            build_fixture_record(&[FixtureCol::Null, FixtureCol::Blob(&row_uncommitted)]),
        )],
    );
    let wal_bytes = build_fixture_wal(&[(2, 3, &wal_page_512), (2, 0, &wal_page_uncommitted)]);
    fs::write(conv_dir.join("conv-route-256.db-wal"), &wal_bytes).unwrap();

    let (ok_wal, out_wal) = run_bin(&[], &env_vars, Some(&payload_256));
    assert!(ok_wal, "WAL 512k render failed: {out_wal}");
    let plain_wal = strip_ansi(&out_wal);
    assert!(
        plain_wal.contains("25.0% (128k/512k)"),
        "expected committed WAL 512k (ignoring uncommitted 1M): {plain_wal}"
    );
    // Repeat unchanged tick -> db-ceilings cache hit still returns 512k and never creates `-shm`
    let (ok_wal_cached, out_wal_cached) = run_bin(&[], &env_vars, Some(&payload_256));
    assert!(ok_wal_cached);
    assert!(strip_ansi(&out_wal_cached).contains("25.0% (128k/512k)"));
    assert!(!conv_dir.join("conv-route-256.db-shm").exists());

    // 3. `840k` DB ceiling (`claude-opus-5-5-medium`) + `last-snapshot.json` + `preview --last` + `report`
    let db_840 = conv_dir.join("conv-route-840.db");
    let row_840 = fixture_gen_blob(
        Some(840_000),
        Some("claude-opus-5-5-medium"),
        Some("Claude Opus 5.5 (Medium)"),
        None,
    );
    fs::write(
        &db_840,
        build_two_table_sqlite_db(
            &[(
                1,
                build_fixture_record(&[FixtureCol::Null, FixtureCol::Blob(&row_840)]),
            )],
            &[],
        ),
    )
    .unwrap();

    let payload_840 = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-route-840",
        "agent_state": "idle",
        "terminal_width": 130,
        "model": { "id": "Claude Opus 5.5 (Medium)", "display_name": "Claude Opus 5.5 (Medium)", "effort": "medium" },
        "context_window": {
            "context_window_size": 1_048_576,
            "current_usage": { "input_tokens": 420_000, "output_tokens": 0 }
        }
    })
    .to_string();

    let (ok_840, out_840) = run_bin(&[], &env_vars, Some(&payload_840));
    assert!(ok_840, "840k render failed: {out_840}");
    let plain_840 = strip_ansi(&out_840);
    assert!(
        plain_840.contains("50.0% (420k/840k)"),
        "expected DB 840k ceiling: {plain_840}"
    );
    assert_eq!(
        fs::read_dir(&ceilings_dir).unwrap().count(),
        1,
        "DB 840k must NOT create a compaction-ceilings shard"
    );

    let snap_raw = fs::read_to_string(hud_home.join("data/last-snapshot.json")).unwrap();
    let snap_json: Value = serde_json::from_str(&snap_raw).unwrap();
    assert!(
        snap_json["probes"]["db_ceiling"].as_str().is_some(),
        "last-snapshot.json must persist probes.db_ceiling: {snap_raw}"
    );

    let (ok_prev, out_prev) = run_bin(&["preview", "--last"], &env_vars, None);
    assert!(ok_prev, "preview --last failed: {out_prev}");
    assert!(
        strip_ansi(&out_prev).contains("50.0% (420k/840k)"),
        "preview --last must preserve db_ceiling: {}",
        strip_ansi(&out_prev)
    );

    let (ok_rep, out_rep) = run_bin(&["report", "--note", "route-a"], &env_vars, None);
    assert!(ok_rep, "report failed: {out_rep}");
    let rep_json: Value = serde_json::from_str(&out_rep).unwrap();
    let rep_db = rep_json["case"]["probes"]["db_ceiling"]
        .as_str()
        .expect("report preserves scrubbed db_ceiling");
    assert!(!rep_db.contains("conv-route-840.db"));
    assert!(
        rep_json["renderedPlainLines"]
            .to_string()
            .contains("50.0% (420k/840k)"),
        "report rendered lines must reflect 840k DB ceiling: {rep_json}"
    );

    // 4. Mid-session model switch guard: switching `stdin.model` to `Gemini 3 Flash` while DB still has `claude-opus-5-5-medium`
    //    must reject the stale DB ceiling and render honest `420k/?` with zero false warnings!
    let payload_switch = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-route-840",
        "agent_state": "idle",
        "terminal_width": 130,
        "model": { "id": "Gemini 3 Flash", "display_name": "Gemini 3 Flash" },
        "context_window": {
            "context_window_size": 1_048_576,
            "current_usage": { "input_tokens": 420_000, "output_tokens": 0 }
        }
    })
    .to_string();
    let (ok_sw, out_sw) = run_bin(&[], &env_vars, Some(&payload_switch));
    assert!(ok_sw, "model switch render failed: {out_sw}");
    let plain_sw = strip_ansi(&out_sw);
    assert!(
        plain_sw.contains("420k/?") && !plain_sw.contains("840k") && !plain_sw.contains("256k"),
        "mid-session model switch must reject stale DB ceiling: {plain_sw}"
    );
    assert!(!out_sw.contains("\x1b[31m") && !out_sw.contains("\x1b[33m"));

    // 5. `executor_metadata` fallback when `gen_metadata` has only shadow rows
    let db_exec = conv_dir.join("conv-exec-512.db");
    let exec_blob = fixture_exec_blob(512_000);
    fs::write(
        &db_exec,
        build_two_table_sqlite_db(
            &[(
                1,
                build_fixture_record(&[FixtureCol::Null, FixtureCol::Blob(&row_shadow)]),
            )],
            &[(
                1,
                build_fixture_record(&[FixtureCol::Null, FixtureCol::Blob(&exec_blob)]),
            )],
        ),
    )
    .unwrap();
    let payload_exec = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-exec-512",
        "agent_state": "idle",
        "terminal_width": 130,
        "model": { "id": "Gemini 3 Flash", "display_name": "Gemini 3 Flash" },
        "context_window": {
            "context_window_size": 1_048_576,
            "current_usage": { "input_tokens": 128_000, "output_tokens": 0 }
        }
    })
    .to_string();
    let (ok_exec, out_exec) = run_bin(&[], &env_vars, Some(&payload_exec));
    assert!(ok_exec, "executor_metadata fallback failed: {out_exec}");
    assert!(
        strip_ansi(&out_exec).contains("25.0% (128k/512k)"),
        "expected executor_metadata 512k fallback: {}",
        strip_ansi(&out_exec)
    );
}

#[test]
fn honest_cold_start_large_window_renders_unknown_without_false_warnings_e2e() {
    let tmp = TempDir::new("cold-start-unknown");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");
    fs::create_dir_all(&hud_home).unwrap();

    let env_vars: [(&str, &Path); 5] = [
        ("HOME", &fake_home),
        ("USERPROFILE", &fake_home),
        ("AGY_CONFIG_DIR", &gemini_cfg),
        ("AGY_CLI_DIR", &cli_dir),
        ("AGY_HUD_HOME", &hud_home),
    ];

    let payload = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-honest-cold-start",
        "agent_state": "idle",
        "terminal_width": 130,
        "model": { "id": "Gemini 4 Argon (High)", "display_name": "Gemini 4 Argon (High)", "effort": "high" },
        "context_window": {
            "context_window_size": 1_048_576,
            "used_percentage": 89.6,
            "current_usage": {
                "input_tokens": 840_000,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 100_000
            }
        }
    })
    .to_string();

    for mode in ["both", "tokens", "percent", "remaining"] {
        let (ok_set, set_out) = run_bin(
            &["set", &format!("display.contextValue={mode}")],
            &env_vars,
            None,
        );
        assert!(ok_set, "set {mode} failed: {set_out}");

        let (ok, out) = run_bin(&[], &env_vars, Some(&payload));
        assert!(ok, "cold start failed for {mode}: {out}");
        let plain = strip_ansi(&out);
        let ctx_line = plain.lines().find(|l| l.contains("Ctx")).unwrap_or(&plain);
        let before_cache = ctx_line.split('·').next().unwrap_or("");
        assert!(
            before_cache.contains("940k/?")
                && !before_cache.contains('%')
                && !ctx_line.contains("256k")
                && !ctx_line.contains('█')
                && ctx_line.contains("11% Cache"),
            "mode {mode} expected neutral `940k/?` without 256k or bar: {ctx_line}"
        );
        assert!(
            !out.contains("\x1b[31m") && !out.contains("\x1b[33m"),
            "cold-start on >=900k window must never emit yellow/red warning ANSI styles: {out:?}"
        );
    }
}

#[test]
fn quota_zero_percent_omitted_fraction_and_sentinel_resets_e2e() {
    let tmp = TempDir::new("quota-zero-sentinel");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");
    fs::create_dir_all(hud_home.join("data")).unwrap();

    // Stale quota cache says `gemini-5h` still has 0.35 remaining using genuine `CachedQuotaSnapshot` schema.
    fs::write(
        hud_home.join("data/quota-cache.json"),
        serde_json::to_string(&json!({
            "at": 1_700_000_000_000i64,
            "source": "/usage",
            "quota": {
                "gemini-5h": {
                    "remaining_fraction": 0.35,
                    "reset_time": "2099-01-01T00:00:00Z"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let env_vars: [(&str, &Path); 5] = [
        ("HOME", &fake_home),
        ("USERPROFILE", &fake_home),
        ("AGY_CONFIG_DIR", &gemini_cfg),
        ("AGY_CLI_DIR", &cli_dir),
        ("AGY_HUD_HOME", &hud_home),
    ];

    // Confirm `quota-cache.json` actually loads and reconciles higher official 0.80 down to cached 0.35 (65% used)
    let baseline_payload = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-quota-cache-check",
        "agent_state": "idle",
        "terminal_width": 140,
        "model": { "id": "Gemini 4 Argon", "display_name": "Gemini 4 Argon" },
        "context_window": { "used_percentage": 15.0 },
        "quota": {
            "gemini-5h": {
                "remaining_fraction": 0.80,
                "reset_time": "2099-01-01T00:00:00Z"
            }
        }
    })
    .to_string();
    let (ok_base, out_base) = run_bin(&[], &env_vars, Some(&baseline_payload));
    assert!(ok_base, "baseline cache reconciliation failed: {out_base}");
    let plain_base = strip_ansi(&out_base);
    assert!(
        plain_base.contains("Quota(5h)") && plain_base.contains("65%"),
        "expected cached 0.35 (65% used) to override live 0.80: {plain_base}"
    );

    // Live omitted `remaining_fraction` (`0%` remaining) must beat stale cached `0.35` (`65%` used)!
    let payload = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-quota-zero",
        "agent_state": "idle",
        "terminal_width": 140,
        "model": { "id": "Gemini 4 Argon", "display_name": "Gemini 4 Argon" },
        "context_window": { "used_percentage": 15.0 },
        "quota": {
            "gemini-5h": {
                "reset_time": "2099-01-01T00:00:00Z"
            },
            "gemini-weekly": {
                "remaining_fraction": 0.25,
                "reset_time": "1970-01-01T00:00:00Z"
            },
            "3p-weekly": {
                "remaining_fraction": 0.40,
                "reset_time": "0001-01-01T00:00:00Z"
            },
            "gemini-argon-wtu-weekly": {}
        }
    })
    .to_string();

    let (ok, out) = run_bin(&[], &env_vars, Some(&payload));
    assert!(ok, "quota 0% render failed: {out}");
    let plain = strip_ansi(&out);
    assert!(
        plain.contains("Quota(5h)") && plain.contains("100%") && plain.contains('d'),
        "expected depleted Quota(5h) 100% with future countdown: {plain}"
    );
    assert!(
        !plain.contains("65%")
            && !plain.contains("Quota(weekly)")
            && !plain.contains("75%")
            && !plain.contains("60%")
            && !plain.contains("argon-weekly"),
        "sentinel <= 2000-01-01, empty argon placeholder, and stale 65% must be dropped: {plain}"
    );
}

#[test]
fn implicit_compaction_dual_threshold_undo_immunity_and_cursor_shrink_e2e() {
    let tmp = TempDir::new("implicit-compaction-undo");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");
    let log_dir = cli_dir
        .join("brain")
        .join("conv-implicit-undo")
        .join(".system_generated/logs");
    fs::create_dir_all(&log_dir).unwrap();
    let tx_path = log_dir.join("transcript.jsonl");

    let env_vars: [(&str, &Path); 5] = [
        ("HOME", &fake_home),
        ("USERPROFILE", &fake_home),
        ("AGY_CONFIG_DIR", &gemini_cfg),
        ("AGY_CLI_DIR", &cli_dir),
        ("AGY_HUD_HOME", &hud_home),
    ];

    let step_lines = [
        json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "created_at": "2026-11-10T01:00:00Z", "content": "Init"}),
        json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-10T01:00:05Z", "model": "gemini-4-argon", "input_tokens": 20_000, "cache_read_tokens": 180_000}),
        // Shadow models must be skipped completely
        json!({"step_index": 2, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-10T01:00:06Z", "model": "gemini-2.5-flash-lite", "input_tokens": 3_000}),
        json!({"step_index": 3, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-10T01:00:07Z", "model": "MODEL_PLACEHOLDER_M50", "input_tokens": 2_000}),
        // 20% drop (200k -> 160k): >5k but <25%, must NOT trigger
        json!({"step_index": 4, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-10T01:00:10Z", "model": "gemini-4-argon", "input_tokens": 160_000}),
        // 4.5k drop (160k -> 155.5k): <=5k, must NOT trigger
        json!({"step_index": 5, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-10T01:00:12Z", "model": "gemini-4-argon", "input_tokens": 155_500}),
    ];
    let body1 = step_lines
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n")
        + "\n";
    fs::write(&tx_path, &body1).unwrap();

    let payload = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-implicit-undo",
        "transcript_path": tx_path,
        "agent_state": "idle",
        "terminal_width": 140,
        "model": { "id": "Gemini 4 Argon (High)", "display_name": "Gemini 4 Argon (High)", "effort": "high" },
        "context_window": {
            "context_window_size": 1_048_576,
            "current_usage": { "input_tokens": 128_000, "output_tokens": 0 }
        }
    })
    .to_string();

    // Tick 1: 0 compactions
    let (ok1, out1) = run_bin(&[], &env_vars, Some(&payload));
    assert!(ok1, "tick 1 failed: {out1}");
    let plain1 = strip_ansi(&out1);
    assert!(
        !plain1.contains('↻'),
        "no false compaction expected: {plain1}"
    );
    assert!(
        plain1.contains("128k/?"),
        "cold start before compaction: {plain1}"
    );

    // Tick 2: grow to 210_000 -> incremental cursor commits previous step state cleanly
    let mut f = fs::OpenOptions::new().append(true).open(&tx_path).unwrap();
    writeln!(
        f,
        "{}",
        json!({"step_index": 6, "type": "USER_INPUT", "status": "DONE", "created_at": "2026-11-10T01:01:00Z", "content": "Continue"})
    )
    .unwrap();
    writeln!(
        f,
        "{}",
        json!({"step_index": 7, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-10T01:01:05Z", "model": "gemini-4-argon", "input_tokens": 210_000})
    )
    .unwrap();
    f.flush().unwrap();

    let (ok2, out2) = run_bin(&[], &env_vars, Some(&payload));
    assert!(ok2);
    assert!(!strip_ansi(&out2).contains('↻'));

    // Tick 3: dual-threshold implicit compaction (210_000 -> 120_000: drop 90k > 5k && 42.9% >= 25%)
    writeln!(
        f,
        "{}",
        json!({"step_index": 8, "type": "USER_INPUT", "status": "DONE", "created_at": "2026-11-10T01:02:00Z", "content": "Next"})
    )
    .unwrap();
    writeln!(
        f,
        "{}",
        json!({"step_index": 9, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-10T01:02:05Z", "model": "gemini-4-argon", "input_tokens": 120_000})
    )
    .unwrap();
    f.flush().unwrap();

    let (ok3, out3) = run_bin(&[], &env_vars, Some(&payload));
    assert!(ok3, "tick 3 failed: {out3}");
    let plain3 = strip_ansi(&out3);
    assert!(
        plain3.contains("50.0% (128k/256k · ↻1)"),
        "expected implicit compaction ↻1 + snapped 256k: {plain3}"
    );
    let snap3: Value = serde_json::from_str(
        &fs::read_to_string(hud_home.join("data/last-snapshot.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(snap3["compactionCount"], 1);
    assert_eq!(snap3["lastCompactionTokens"], 210_000);

    // Tick 4: Undo/rewind appends smaller `step_index` (step 4 & 5) with large token drop (120k -> 40k) -> MUST NOT trigger!
    writeln!(
        f,
        "{}",
        json!({"step_index": 4, "type": "USER_INPUT", "status": "DONE", "created_at": "2026-11-10T01:03:00Z", "content": "Rewind"})
    )
    .unwrap();
    writeln!(
        f,
        "{}",
        json!({"step_index": 5, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-10T01:03:05Z", "model": "gemini-4-argon", "input_tokens": 40_000})
    )
    .unwrap();
    f.flush().unwrap();

    let (ok4, out4) = run_bin(&[], &env_vars, Some(&payload));
    assert!(ok4);
    assert!(
        strip_ansi(&out4).contains("↻1"),
        "Undo/rewind must keep compaction_count == 1: {}",
        strip_ansi(&out4)
    );
    let snap4: Value = serde_json::from_str(
        &fs::read_to_string(hud_home.join("data/last-snapshot.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(snap4["compactionCount"], 1);

    // Tick 5: File shrink (rewritten shorter than cursor offset) resets cursor cleanly & never resurrects old state!
    drop(f);
    let shrunk = format!(
        "{}\n{}\n",
        json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "created_at": "2026-11-10T02:00:00Z", "content": "Fresh start"}),
        json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-10T02:00:05Z", "model": "gemini-4-argon", "input_tokens": 15_000}),
    );
    fs::write(&tx_path, shrunk).unwrap();
    let (ok5, out5) = run_bin(&[], &env_vars, Some(&payload));
    assert!(ok5);
    assert!(!strip_ansi(&out5).contains('↻'));
    let snap5: Value = serde_json::from_str(
        &fs::read_to_string(hud_home.join("data/last-snapshot.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(snap5["compactionCount"], 0);
}

#[test]
fn collector_worker4_latency_cursor_tracking_and_file_shrink_reset() {
    let tmp = TempDir::new("collector-worker4");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");
    let conv_dir = cli_dir.join("conversations");
    let log_dir = cli_dir
        .join("brain")
        .join("conv-worker4")
        .join(".system_generated/logs");
    fs::create_dir_all(&conv_dir).unwrap();
    fs::create_dir_all(&log_dir).unwrap();

    let mut env_map = HashMap::new();
    env_map.insert("HOME".to_string(), fake_home.to_string_lossy().into_owned());
    env_map.insert(
        "USERPROFILE".to_string(),
        fake_home.to_string_lossy().into_owned(),
    );
    env_map.insert(
        "AGY_CONFIG_DIR".to_string(),
        gemini_cfg.to_string_lossy().into_owned(),
    );
    env_map.insert(
        "AGY_CLI_DIR".to_string(),
        cli_dir.to_string_lossy().into_owned(),
    );
    env_map.insert(
        "AGY_HUD_HOME".to_string(),
        hud_home.to_string_lossy().into_owned(),
    );
    let paths = AgyPaths::from_env_map(&env_map);
    let data_real = prepare_data_dir(&paths).expect("prepare data dir");

    let db_path = conv_dir.join("conv-worker4.db");
    let gen_row = fixture_gen_blob(
        Some(512_000),
        Some("gemini-4-hydrogen"),
        Some("Gemini 4 Hydrogen (XHigh)"),
        None,
    );
    fs::write(
        &db_path,
        build_two_table_sqlite_db(
            &[(
                1,
                build_fixture_record(&[FixtureCol::Null, FixtureCol::Blob(&gen_row)]),
            )],
            &[],
        ),
    )
    .unwrap();

    let tx_path = log_dir.join("transcript.jsonl");
    let tx_initial = [
        json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "created_at": "2026-11-10T01:00:00Z", "content": "One"}),
        json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-10T01:00:02Z", "model": "gemini-4-hydrogen", "input_tokens": 80_000}),
        json!({"step_index": 2, "type": "USER_INPUT", "status": "DONE", "created_at": "2026-11-10T01:00:10Z", "content": "Two"}),
        json!({"step_index": 3, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-10T01:00:12Z", "model": "gemini-4-hydrogen", "input_tokens": 110_000}),
    ]
    .iter()
    .map(serde_json::to_string)
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
    .join("\n")
        + "\n";
    fs::write(&tx_path, &tx_initial).unwrap();

    let stdin = AgyStdinData {
        cwd: Some(tmp.path.to_string_lossy().into_owned()),
        conversation_id: Some("conv-worker4".to_string()),
        transcript_path: Some(tx_path.to_string_lossy().into_owned()),
        agent_state: Some("idle".to_string()),
        model: Some(ModelStdin {
            id: Some("Gemini 4 Hydrogen (XHigh)".to_string()),
            display_name: Some("Gemini 4 Hydrogen (XHigh)".to_string()),
            effort: Some("xhigh".to_string()),
        }),
        context_window: Some(ContextWindowStdin {
            context_window_size: Some(1_048_576),
            current_usage: Some(CurrentUsageStdin {
                input_tokens: Some(110_000),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let logic_paths = build_logic_paths(&paths, Some(&stdin), Some(&data_real));
    let mut cfg = HudConfig::default();
    cfg.vcs.enabled = false;
    cfg.display.show_memory = false;

    // 1. Cold DB read latency + Worker 4 wiring
    let real_db = fs::canonicalize(&db_path).unwrap();
    let cache_path = db_ceiling_cache_path(&data_real, &real_db);
    let t_db = Instant::now();
    let obs = session_db::read_db_ceiling_from_path(
        &real_db,
        Some("Gemini 4 Hydrogen (XHigh)"),
        hud_core::is_shadow_or_utility_model,
    )
    .unwrap()
    .expect("db observation");
    let db_elapsed = t_db.elapsed();
    assert_eq!(obs.ceiling_tokens, 512_000);
    assert!(
        db_elapsed.as_millis() < 1_000,
        "DB read timed out: {db_elapsed:?}"
    );

    let collected1 = collect_render_inputs(
        &paths,
        &logic_paths,
        &cfg,
        &stdin,
        1_700_000_000_000,
        &SystemProbeRunner,
    );
    let parsed_db = parse_db_ceiling_info(&collected1.probes).expect("db_ceiling serialized");
    assert_eq!(parsed_db.ceiling_tokens, 512_000);
    assert_eq!(parsed_db.source, DbCeilingSource::GenMetadata);
    assert_eq!(parsed_db.model_family.as_deref(), Some("gemini 4 hydrogen"));
    assert!(cache_path.is_file(), "db-ceilings cache must be written");
    assert!(!conv_dir.join("conv-worker4.db-shm").exists());
    assert!(
        !paths.compaction_ceilings_dir.exists(),
        "compaction-ceilings/ must NOT be written for DB ceilings"
    );

    // 2. Deterministic cache-hit verification + CI-safe sanity bound
    let raw_cache_str = fs::read_to_string(&cache_path).unwrap();
    let mut cache_val: Value = serde_json::from_str(&raw_cache_str).unwrap();
    assert_eq!(cache_val["dbLen"], fs::metadata(&real_db).unwrap().len());
    assert_eq!(cache_val["hintKey"], "gemini 4 hydrogen");
    assert_eq!(cache_val["observation"]["ceilingTokens"], 512_000);

    // Mutate only the cached observation while preserving DB stat signature: proves Worker 4
    // hits `db-ceilings/*.json` without re-parsing the SQLite DB!
    cache_val["observation"]["ceilingTokens"] = json!(777_000);
    fs::write(&cache_path, serde_json::to_vec(&cache_val).unwrap()).unwrap();

    let t_cache = Instant::now();
    let cached_collected = collect_render_inputs(
        &paths,
        &logic_paths,
        &cfg,
        &stdin,
        1_700_000_000_000,
        &SystemProbeRunner,
    );
    let cache_elapsed = t_cache.elapsed();
    let cached_info = parse_db_ceiling_info(&cached_collected.probes).expect("cached db_ceiling");
    assert_eq!(cached_info.ceiling_tokens, 777_000);
    assert!(
        cache_elapsed.as_millis() < 1_000,
        "cache hit timed out: {cache_elapsed:?}"
    );
    // Restore original cache payload for subsequent steps
    fs::write(&cache_path, raw_cache_str).unwrap();

    // 3. TranscriptCursor preserves `prev_prompt_tokens` & `last_planner_step_next` and cleans up on shrink!
    let cursor_path = PathBuf::from(logic_paths.transcript_cursor.as_deref().unwrap());
    let cursor_v3: TranscriptCursor =
        serde_json::from_str(&fs::read_to_string(&cursor_path).unwrap()).unwrap();
    assert_eq!(cursor_v3.version, TRANSCRIPT_CURSOR_VERSION);
    assert_eq!(cursor_v3.prev_prompt_tokens, Some(80_000));
    assert_eq!(cursor_v3.last_planner_step_next, 2);

    // Shrink file to a 1-turn transcript (< cursor.offset): stale cursor must be deleted so regrowth does NOT resurrect it!
    let shrunk_1turn = format!(
        "{}\n",
        json!({"step_index": 0, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-10T02:00:00Z", "model": "gemini-4-hydrogen", "input_tokens": 20_000})
    );
    fs::write(&tx_path, shrunk_1turn).unwrap();
    let s_shrunk = read_and_parse_transcript_incremental(
        Some(&tx_path),
        Some(&cursor_path),
        None,
        Some(&stdin),
    );
    assert_eq!(s_shrunk.compaction_count, 0);
    assert!(
        !cursor_path.exists(),
        "stale cursor must be removed when shrunk file has commit_idx == 0"
    );

    // Grow file back past old offset with padding -> no stale compaction or resurrection
    let mut f = fs::OpenOptions::new().append(true).open(&tx_path).unwrap();
    writeln!(
        f,
        "{}",
        json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-10T02:01:00Z", "model": "gemini-4-hydrogen", "input_tokens": 30_000, "content": "y".repeat(2048)})
    )
    .unwrap();
    f.flush().unwrap();
    let s_regrown = read_and_parse_transcript_incremental(
        Some(&tx_path),
        Some(&cursor_path),
        None,
        Some(&stdin),
    );
    assert_eq!(s_regrown.compaction_count, 0);
}

#[test]
fn read_git_head_fallback_extracts_origin_from_common_dir_for_linked_worktree() {
    let tmp = TempDir::new("worktree-head-fallback");
    let common_dir = tmp.path.join("repo/.git");
    let git_dir = common_dir.join("worktrees/wt-1");
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(git_dir.join("HEAD"), "ref: refs/heads/feat/wt-fallback\n").unwrap();
    fs::write(
        common_dir.join("config"),
        "[remote \"origin\"]\n\turl = https://github.com/Max/agy-hud.git\n",
    )
    .unwrap();

    let info = read_git_head_fallback(&git_dir, &common_dir).expect("head fallback");
    assert_eq!(info.branch, "feat/wt-fallback");
    assert_eq!(
        info.repo_url.as_deref(),
        Some("https://github.com/Max/agy-hud")
    );
}

#[cfg(unix)]
#[test]
fn pr1_quota_refresh_auth_exit2_timeout_exit3_api_key_skip_and_uninstall_race() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new("quota-pr1");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");
    let bin_dir = tmp.path.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&hud_home).unwrap();

    // Bare `agy` executable placed on PATH to verify `resolve_executable_in_path` resolution + env forwarding
    let fake_agy = bin_dir.join("agy");
    let marker_file = tmp.path.join("agy-was-called");
    fs::write(&fake_agy, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&fake_agy, fs::Permissions::from_mode(0o755)).unwrap();

    let sys_path = std::env::var_os("PATH").unwrap_or_default();
    let mut joined_paths = vec![bin_dir];
    joined_paths.extend(std::env::split_paths(&sys_path));
    let combined_path = std::env::join_paths(joined_paths).unwrap();

    let run_with_extra = |args: &[&str], extra: &[(&str, &str)], stdin_data: Option<&str>| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_agy-hud"));
        cmd.args(args)
            .env_remove("GEMINI_API_KEY")
            .env_remove("AGY_ADC_AUTH")
            .env_remove("AGY_HUD_AGY_BIN")
            .env("HOME", &fake_home)
            .env("USERPROFILE", &fake_home)
            .env("AGY_CONFIG_DIR", &gemini_cfg)
            .env("AGY_CLI_DIR", &cli_dir)
            .env("AGY_HUD_HOME", &hud_home)
            .env("PATH", &combined_path)
            .env("AGY_HUD_DISABLE_BG_UPDATE", "1")
            .env("AGY_SKIP_PLUGIN_INSTALL", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in extra {
            cmd.env(k, v);
        }
        if stdin_data.is_some() {
            cmd.stdin(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }
        let mut child = cmd.spawn().unwrap();
        if let Some(input) = stdin_data
            && let Some(mut pipe) = child.stdin.take()
        {
            pipe.write_all(input.as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };

    // Wait: `run_agy_usage_isolated` uses `.env_clear()` and forwards specific keys; pass `MOCK_MODE` via a wrapper script per mode!
    let mk_mode_script = |name: &str, body: &str| {
        let p = tmp.path.join(name);
        fs::write(&p, body).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    };

    // 1. Exit code 2 -> UnsupportedAuthExit2: forwards GEMINI_API_KEY & AGY_ADC_AUTH, writes 24h backoff, clear error
    let agy_exit2 = mk_mode_script(
        "agy-exit2.sh",
        &format!(
            "#!/bin/sh\necho \"$GEMINI_API_KEY|$AGY_ADC_AUTH\" > \"{}\"\nexit 2\n",
            marker_file.display()
        ),
    );
    let (ok2, out2) = run_with_extra(
        &["quota", "refresh"],
        &[
            ("AGY_HUD_AGY_BIN", agy_exit2.to_str().unwrap()),
            ("GEMINI_API_KEY", "test-api-key"),
            ("AGY_ADC_AUTH", "1"),
        ],
        None,
    );
    assert!(!ok2);
    assert!(
        out2.contains("exit code 2") && out2.contains("GEMINI_API_KEY"),
        "expected clear exit code 2 / GEMINI_API_KEY message: {out2}"
    );
    assert_eq!(
        fs::read_to_string(&marker_file).unwrap().trim(),
        "test-api-key|1"
    );
    let backoff2: Value = serde_json::from_str(
        &fs::read_to_string(hud_home.join("data/quota-backoff.json")).unwrap(),
    )
    .unwrap();
    let retry_at = backoff2["nextRetryAt"].as_i64().unwrap();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert!(
        retry_at >= now_ms + 86_000_000,
        "expected ~24h cooldown retry_at={retry_at}, now={now_ms}"
    );

    // 2. Exit code 3 -> TimeoutExit3: normal backoff + clear timeout message
    let _ = fs::remove_file(hud_home.join("data/quota-backoff.json"));
    let agy_exit3 = mk_mode_script("agy-exit3.sh", "#!/bin/sh\nexit 3\n");
    let (ok3, out3) = run_with_extra(
        &["quota", "refresh"],
        &[("AGY_HUD_AGY_BIN", agy_exit3.to_str().unwrap())],
        None,
    );
    assert!(!ok3);
    assert!(
        out3.to_ascii_lowercase().contains("timed out") && out3.contains("exit code 3"),
        "expected clear timeout exit code 3 message: {out3}"
    );
    let backoff3: Value = serde_json::from_str(
        &fs::read_to_string(hud_home.join("data/quota-backoff.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(backoff3["failures"], 1);
    assert!(backoff3["nextRetryAt"].as_i64().unwrap() <= now_ms + 700_000);

    // 3. `maybe_spawn_background_quota_refresh` skips early when `GEMINI_API_KEY` is non-empty
    let _ = fs::remove_file(hud_home.join("data/quota-backoff.json"));
    let _ = fs::remove_file(&marker_file);
    fs::write(
        hud_home.join("config.json"),
        r#"{"quota":{"backgroundRefresh":true}}"#,
    )
    .unwrap();
    let agy_bare_marker = format!("#!/bin/sh\ntouch \"{}\"\nexit 0\n", marker_file.display());
    fs::write(&fake_agy, agy_bare_marker).unwrap();
    let render_payload = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-bg-skip",
        "agent_state": "working",
        "terminal_width": 120
    })
    .to_string();
    let (ok_bg, _) = run_with_extra(
        &[],
        &[("GEMINI_API_KEY", "non-empty-key")],
        Some(&render_payload),
    );
    assert!(ok_bg);
    std::thread::sleep(std::time::Duration::from_millis(80));
    assert!(
        !marker_file.exists(),
        "background quota refresh must NOT spawn when GEMINI_API_KEY is set"
    );
    assert!(!hud_home.join("data/quota.lock").exists());

    // 4. Bare `agy` resolution via PATH works in `run_agy_usage_isolated`
    let agy_bare_ok = r#"#!/bin/sh
cat <<'EOF'
{"command":{"data":{"groups":[{"buckets":[{"id":"gemini-5h","remaining_fraction":0.5,"reset_time":"2099-01-01T00:00:00Z"}]}]}}}
EOF
"#;
    fs::write(&fake_agy, agy_bare_ok).unwrap();
    let (ok_bare, out_bare) = run_with_extra(&["quota", "refresh"], &[], None);
    assert!(ok_bare, "bare `agy` PATH lookup failed: {out_bare}");
    assert!(hud_home.join("data/quota-cache.json").is_file());

    // 5. `hud_home` deletion race during `agy -p /usage` (both success and failure paths)
    let race_ok_script = format!(
        r#"#!/bin/sh
rm -rf "{}"
cat <<'EOF'
{{"command":{{"data":{{"groups":[{{"buckets":[{{"id":"gemini-5h","remaining_fraction":0.5,"reset_time":"2099-01-01T00:00:00Z"}}]}}]}}}}}}
EOF
"#,
        hud_home.display()
    );
    let agy_race_ok = mk_mode_script("agy-race-ok.sh", &race_ok_script);
    let (ok_race_bg, _) = run_with_extra(
        &["quota", "refresh", "--background"],
        &[("AGY_HUD_AGY_BIN", agy_race_ok.to_str().unwrap())],
        None,
    );
    assert!(ok_race_bg);
    assert!(
        !hud_home.exists(),
        "successful probe after hud_home deletion must NOT recreate hud_home/quota-cache.json"
    );

    fs::create_dir_all(&hud_home).unwrap();
    let race_err_script = format!("#!/bin/sh\nrm -rf \"{}\"\nexit 1\n", hud_home.display());
    let agy_race_err = mk_mode_script("agy-race-err.sh", &race_err_script);
    let (ok_race_err, _) = run_with_extra(
        &["quota", "refresh", "--background"],
        &[("AGY_HUD_AGY_BIN", agy_race_err.to_str().unwrap())],
        None,
    );
    assert!(ok_race_err);
    assert!(
        !hud_home.exists(),
        "failed probe after hud_home deletion must NOT recreate hud_home/quota-backoff.json"
    );

    // 6. Pre-start uninstall race: `--background --lock-token <tok>` must NOT recreate missing hud_home/data
    let (ok_missing_bg, _) = run_with_extra(
        &[
            "quota",
            "refresh",
            "--background",
            "--lock-token",
            "stale-tok",
        ],
        &[],
        None,
    );
    assert!(ok_missing_bg);
    assert!(
        !hud_home.exists(),
        "background refresh with --lock-token must NOT recreate hud_home after pre-start uninstall"
    );
}

#[test]
fn pr2_sharded_speed_cache_isolates_concurrent_sessions_and_post_render_maintenance_prunes() {
    let tmp = TempDir::new("pr2-speed-shards");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");
    fs::create_dir_all(&hud_home).unwrap();
    fs::write(
        hud_home.join("config.json"),
        r#"{"updates":{"enabled":false},"vcs":{"enabled":false}}"#,
    )
    .unwrap();

    let iso_secs_ago = |ago_secs: u64| -> String {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(ago_secs) as i64;
        let days = secs.div_euclid(86_400);
        let sod = secs.rem_euclid(86_400);
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
        format!(
            "{year:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            sod / 3600,
            (sod % 3600) / 60,
            sod % 60
        )
    };
    let t_user = iso_secs_ago(15);
    let t_planner = iso_secs_ago(5);

    let write_session_tx = |conv_id: &str, output_tokens: u64| {
        let log_dir = cli_dir
            .join("brain")
            .join(conv_id)
            .join(".system_generated/logs");
        fs::create_dir_all(&log_dir).unwrap();
        let tx = log_dir.join("transcript.jsonl");
        let body = format!(
            "{}\n{}\n",
            json!({
                "step_index": 0,
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": t_user,
                "content": "go"
            }),
            json!({
                "step_index": 1,
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": t_planner,
                "model": "gemini-4-argon",
                "output_tokens": output_tokens
            }),
        );
        fs::write(&tx, body).unwrap();
        tx
    };

    let tx_a = write_session_tx("conv-alpha", 1000);
    let tx_b = write_session_tx("conv-beta", 200);

    let env_vars: [(&str, &Path); 5] = [
        ("HOME", fake_home.as_path()),
        ("USERPROFILE", fake_home.as_path()),
        ("AGY_CONFIG_DIR", gemini_cfg.as_path()),
        ("AGY_CLI_DIR", cli_dir.as_path()),
        ("AGY_HUD_HOME", hud_home.as_path()),
    ];

    let payload_a = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-alpha",
        "transcript_path": tx_a,
        "agent_state": "idle",
        "terminal_width": 120,
        "model": { "id": "Gemini 4 Argon", "display_name": "Gemini 4 Argon" },
        "context_window": { "used_percentage": 15.0 }
    })
    .to_string();
    let payload_b = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-beta",
        "transcript_path": tx_b,
        "agent_state": "idle",
        "terminal_width": 120,
        "model": { "id": "Gemini 4 Argon", "display_name": "Gemini 4 Argon" },
        "context_window": { "used_percentage": 15.0 }
    })
    .to_string();

    // Run both sessions concurrently and sequentially — neither may overwrite the other!
    std::thread::scope(|s| {
        let h1 = s.spawn(|| run_bin(&[], &env_vars, Some(&payload_a)));
        let h2 = s.spawn(|| run_bin(&[], &env_vars, Some(&payload_b)));
        let (ok1, out1) = h1.join().unwrap();
        let (ok2, out2) = h2.join().unwrap();
        assert!(ok1, "alpha failed: {out1}");
        assert!(ok2, "beta failed: {out2}");
    });

    let data_dir = hud_home.join("data");
    assert!(
        !data_dir.join("speed-cache.json").exists(),
        "sharded session renders must NOT write legacy data/speed-cache.json"
    );
    let shard_a = data_dir.join("speed-cache").join(format!(
        "{}.json",
        hud_cli::paths::fnv1a_64_hex("conv-alpha")
    ));
    let shard_b = data_dir.join("speed-cache").join(format!(
        "{}.json",
        hud_cli::paths::fnv1a_64_hex("conv-beta")
    ));
    assert!(shard_a.is_file(), "missing alpha shard: {shard_a:?}");
    assert!(shard_b.is_file(), "missing beta shard: {shard_b:?}");

    let json_a: Value = serde_json::from_str(&fs::read_to_string(&shard_a).unwrap()).unwrap();
    let json_b: Value = serde_json::from_str(&fs::read_to_string(&shard_b).unwrap()).unwrap();
    assert_eq!(json_a["sessionKey"], "conv-alpha");
    assert_eq!(json_b["sessionKey"], "conv-beta");
    assert!(
        json_a["tpsX10"].as_u64().unwrap() > json_b["tpsX10"].as_u64().unwrap(),
        "alpha and beta speeds must remain isolated: alpha={json_a}, beta={json_b}"
    );

    // Seed 265 files into data/speed-cache/, clear .maintenance-at, and verify post-render pruning trims to 256!
    let sc_dir = data_dir.join("speed-cache");
    for i in 0..265 {
        fs::write(sc_dir.join(format!("extra-{i:03}.json")), b"{}").unwrap();
    }
    let _ = fs::remove_file(data_dir.join(".maintenance-at"));

    // Payload with neither ID falls back to legacy speed-cache.json cleanly
    let payload_no_id = json!({
        "cwd": tmp.path,
        "agent_state": "idle",
        "terminal_width": 120,
        "model": { "id": "Gemini 4 Argon", "display_name": "Gemini 4 Argon" },
        "context_window": { "used_percentage": 15.0 }
    })
    .to_string();
    let (ok_fallback, out_fallback) = run_bin(&[], &env_vars, Some(&payload_no_id));
    assert!(ok_fallback, "fallback failed: {out_fallback}");

    assert!(
        data_dir.join(".maintenance-at").is_file(),
        "post-render maintenance must write .maintenance-at"
    );
    let remaining_sc: usize = fs::read_dir(&sc_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|ft| ft.is_file()))
        .count();
    assert!(
        remaining_sc <= 256,
        "expected speed-cache dir trimmed to <= 256 files, got {remaining_sc}"
    );
}

#[test]
fn pr2_cost_ledger_skips_lock_when_unchanged_and_tracks_degraded_write_in_heartbeat() {
    let tmp = TempDir::new("pr2-ledger-and-heartbeat");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");
    fs::create_dir_all(&hud_home).unwrap();
    fs::write(
        hud_home.join("config.json"),
        r#"{"updates":{"enabled":false},"vcs":{"enabled":false},"display":{"showDailyCost":true,"showWeeklyCost":true}}"#,
    )
    .unwrap();

    let env_vars: [(&str, &Path); 5] = [
        ("HOME", fake_home.as_path()),
        ("USERPROFILE", fake_home.as_path()),
        ("AGY_CONFIG_DIR", gemini_cfg.as_path()),
        ("AGY_CLI_DIR", cli_dir.as_path()),
        ("AGY_HUD_HOME", hud_home.as_path()),
    ];

    let payload_40 = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-cost-skip",
        "agent_state": "idle",
        "terminal_width": 130,
        "model": { "id": "Gemini 3 Flash", "display_name": "Gemini 3 Flash" },
        "context_window": { "used_percentage": 10.0 },
        "cost": { "total_usd": 0.40, "estimated": false }
    })
    .to_string();

    // 1. Initial render writes cost-ledger.json and records heartbeat outcome = "ok"
    let (ok1, out1) = run_bin(&[], &env_vars, Some(&payload_40));
    assert!(ok1, "tick 1 failed: {out1}");
    assert!(strip_ansi(&out1).contains("Today $0.40"));
    let hb1: Value =
        serde_json::from_str(&fs::read_to_string(hud_home.join("heartbeat.json")).unwrap())
            .unwrap();
    assert_eq!(hb1["outcome"], "ok");

    // 2. Plant a fresh cost-ledger.lock sentinel: unchanged spend ($0.40) must NOT acquire lock!
    let lock_path = hud_home.join("data/cost-ledger.lock");
    fs::write(&lock_path, b"sentinel-lock").unwrap();
    let (ok2, out2) = run_bin(&[], &env_vars, Some(&payload_40));
    assert!(ok2, "tick 2 failed: {out2}");
    assert!(strip_ansi(&out2).contains("Today $0.40"));
    assert_eq!(
        fs::read_to_string(&lock_path).unwrap(),
        "sentinel-lock",
        "unchanged spend must skip with_exclusive_lock_file completely"
    );
    let hb2: Value =
        serde_json::from_str(&fs::read_to_string(hud_home.join("heartbeat.json")).unwrap())
            .unwrap();
    assert_eq!(hb2["outcome"], "ok");

    // 3. Increased spend ($0.90) while cost-ledger.lock is held -> attempts lock, fails non-fatally,
    //    preserves exit code 0 & output, and records HeartbeatInfo.outcome = "degraded:write"!
    let payload_90 = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-cost-skip",
        "agent_state": "idle",
        "terminal_width": 130,
        "model": { "id": "Gemini 3 Flash", "display_name": "Gemini 3 Flash" },
        "context_window": { "used_percentage": 10.0 },
        "cost": { "total_usd": 0.90, "estimated": false }
    })
    .to_string();
    let (ok3, out3) = run_bin(&[], &env_vars, Some(&payload_90));
    assert!(
        ok3,
        "degraded write must still exit 0 so agy never disables statusLine"
    );
    assert!(strip_ansi(&out3).contains("Today $0.90"));
    let hb3: Value =
        serde_json::from_str(&fs::read_to_string(hud_home.join("heartbeat.json")).unwrap())
            .unwrap();
    assert_eq!(hb3["outcome"], "degraded:write");

    // 4. Remove lock -> tick recovers cleanly to "ok", then plant directory at last-snapshot.json
    //    to verify snapshot write failure also reports "degraded:write" & surfaces in `doctor`!
    fs::remove_file(&lock_path).unwrap();
    let (ok4, _) = run_bin(&[], &env_vars, Some(&payload_90));
    assert!(ok4);
    let hb4: Value =
        serde_json::from_str(&fs::read_to_string(hud_home.join("heartbeat.json")).unwrap())
            .unwrap();
    assert_eq!(hb4["outcome"], "ok");

    let snap_path = hud_home.join("data/last-snapshot.json");
    fs::remove_file(&snap_path).unwrap();
    fs::create_dir_all(&snap_path).unwrap();
    let (ok5, out5) = run_bin(&[], &env_vars, Some(&payload_90));
    assert!(ok5, "last-snapshot write failure must still exit 0: {out5}");
    let hb5: Value =
        serde_json::from_str(&fs::read_to_string(hud_home.join("heartbeat.json")).unwrap())
            .unwrap();
    assert_eq!(hb5["outcome"], "degraded:write");

    let (ok_doc, doc_out) = run_bin(&["doctor"], &env_vars, None);
    assert!(ok_doc);
    assert!(
        doc_out.contains("degraded:write"),
        "doctor should report degraded:write outcome: {doc_out}"
    );
}

#[test]
fn pr2_lazy_unprefixed_skills_respects_updates_enabled_and_clean_config() {
    let tmp = TempDir::new("pr2-lazy-skills");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");

    fs::create_dir_all(hud_home.join("data")).unwrap();
    fs::write(
        hud_home.join("data/update-cache.json"),
        serde_json::to_string(&json!({
            "checkedAt": 1_700_000_000_000i64,
            "latestVersion": "9.9.0"
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        hud_home.join("config.json"),
        r#"{"updates":{"enabled":false},"vcs":{"enabled":false}}"#,
    )
    .unwrap();

    let env_vars: [(&str, &Path); 5] = [
        ("HOME", fake_home.as_path()),
        ("USERPROFILE", fake_home.as_path()),
        ("AGY_CONFIG_DIR", gemini_cfg.as_path()),
        ("AGY_CLI_DIR", cli_dir.as_path()),
        ("AGY_HUD_HOME", hud_home.as_path()),
    ];

    let (ok_inst, _) = run_bin(&["install", "--stack"], &env_vars, None);
    assert!(ok_inst);

    let payload = json!({
        "cwd": tmp.path,
        "conversation_id": "conv-lazy-skills",
        "agent_state": "idle",
        "terminal_width": 140,
        "model": { "id": "Gemini 3 Flash", "display_name": "Gemini 3 Flash" },
        "context_window": { "used_percentage": 18.0 }
    })
    .to_string();

    let (ok_no_upd, out_no_upd) = run_bin(&[], &env_vars, Some(&payload));
    assert!(ok_no_upd);
    let plain_no_upd = strip_ansi(&out_no_upd);
    assert!(!plain_no_upd.contains("v9.9.0"));
    assert!(!plain_no_upd.contains("Config:"));

    fs::write(
        hud_home.join("config.json"),
        r#"{"updates":{"enabled":true},"vcs":{"enabled":false}}"#,
    )
    .unwrap();
    let (ok_upd, out_upd) = run_bin(&[], &env_vars, Some(&payload));
    assert!(ok_upd);
    assert!(strip_ansi(&out_upd).contains("↑ v9.9.0 · /hud-update"));
}

#[test]
fn pr4_title_subcommand_install_flags_doctor_preview_and_uninstall_roundtrip() {
    // 1. OS command formatting for title
    let (posix_title_cmd, posix_warn) =
        format_title_command_for_platform("/Users/alice/it's/agy-hud", false);
    assert_eq!(posix_title_cmd, "'/Users/alice/it'\\''s/agy-hud' title");
    assert!(posix_warn.is_none());

    let (win_title_cmd, win_warn) = format_title_command_for_platform(
        r"C:\Users\alice\.gemini\antigravity-cli\agy-hud\bin\agy-hud.exe",
        true,
    );
    assert_eq!(
        win_title_cmd,
        r"C:\Users\alice\.gemini\antigravity-cli\agy-hud\bin\agy-hud.exe title"
    );
    assert!(!win_title_cmd.contains('"'));
    assert!(win_warn.is_none());

    // 2. Ultra-fast `agy-hud title`: piped JSON -> single plain line, zero ANSI, zero hud_home disk writes!
    let tmp = TempDir::new("pr4-title-e2e");
    let fake_home = tmp.path.join("home");
    let gemini_cfg = fake_home.join(".gemini/config");
    let cli_dir = tmp.path.join("cli");
    let hud_home = cli_dir.join("agy-hud");
    fs::create_dir_all(&cli_dir).unwrap();

    let env_vars: [(&str, &Path); 5] = [
        ("HOME", fake_home.as_path()),
        ("USERPROFILE", fake_home.as_path()),
        ("AGY_CONFIG_DIR", gemini_cfg.as_path()),
        ("AGY_CLI_DIR", cli_dir.as_path()),
        ("AGY_HUD_HOME", hud_home.as_path()),
    ];

    let title_payload = json!({
        "cwd": "/Users/dev/my-proj",
        "conversation_id": "conv-pr4-title",
        "agent_state": "idle",
        "vcs": { "type": "git", "branch": "feat/title", "dirty": true },
        "model": { "id": "Gemini 4 Neon (High)", "display_name": "Gemini 4 Neon (High)" },
        "context_window": { "used_percentage": 34.0, "context_window_size": 256_000 },
        "conversation_title": "\x1b[31mNative\ntitle mode\x1b[0m"
    })
    .to_string();

    let (ok_title, title_out) = run_bin(&["title"], &env_vars, Some(&title_payload));
    assert!(ok_title, "agy-hud title failed: {title_out}");
    assert_eq!(strip_ansi(&title_out), title_out);
    assert_eq!(
        title_out.trim_end(),
        "● my-proj (feat/title*) · Neon · 34% — Native title mode"
    );
    assert_eq!(title_out.lines().count(), 1);
    assert!(
        !hud_home.exists(),
        "agy-hud title must NEVER create hud_home/data/heartbeat/snapshot files"
    );

    // 2b. `AGY_HUD_NESTED=1` and `AGY_HUD_DISABLE=1` on `agy-hud title` emit zero output & zero files
    for (env_k, env_v) in [("AGY_HUD_NESTED", "1"), ("AGY_HUD_DISABLE", "1")] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_agy-hud"))
            .arg("title")
            .env("HOME", &fake_home)
            .env("USERPROFILE", &fake_home)
            .env("AGY_CONFIG_DIR", &gemini_cfg)
            .env("AGY_CLI_DIR", &cli_dir)
            .env("AGY_HUD_HOME", &hud_home)
            .env(env_k, env_v)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(title_payload.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
        assert!(!hud_home.exists());
    }

    // 2c. Open stdin pipe on `agy-hud title` exits immediately without waiting for EOF
    let started = Instant::now();
    let mut pipe_child = Command::new(env!("CARGO_BIN_EXE_agy-hud"))
        .arg("title")
        .env("HOME", &fake_home)
        .env("USERPROFILE", &fake_home)
        .env("AGY_CONFIG_DIR", &gemini_cfg)
        .env("AGY_CLI_DIR", &cli_dir)
        .env("AGY_HUD_HOME", &hud_home)
        .env("LC_ALL", "en_US.UTF-8")
        .env("LANG", "en_US.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut open_pipe = pipe_child.stdin.take().unwrap();
    open_pipe.write_all(title_payload.as_bytes()).unwrap();
    open_pipe.flush().unwrap();
    let status = loop {
        if let Some(st) = pipe_child.try_wait().unwrap() {
            break st;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_millis(3000),
            "agy-hud title blocked waiting for EOF on open pipe"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert!(status.success());
    drop(open_pipe);

    // 3. Seed foreign `title` in settings.json and verify `--with-title`, `--replace`, preserve, `--no-title`, `doctor`, `uninstall`
    let settings_path = cli_dir.join("settings.json");
    let foreign_title = json!({
        "type": "command",
        "command": "/usr/local/bin/custom-title.sh --token=keepsecret",
        "enabled": true
    });
    fs::write(
        &settings_path,
        serde_json::to_string_pretty(&json!({
            "colorScheme": "dark",
            "title": foreign_title
        }))
        .unwrap(),
    )
    .unwrap();

    // 3a. `install` without `--with-title` configures statusLine and leaves foreign title untouched (`kept-foreign` on uninstall)
    let (ok_plain_inst, plain_inst_out) = run_bin(&["install"], &env_vars, None);
    assert!(ok_plain_inst, "install failed: {plain_inst_out}");
    let plain_inst: Value = serde_json::from_str(&plain_inst_out).unwrap();
    assert_eq!(plain_inst["titleConfigured"], false);
    assert_eq!(plain_inst["titleCommand"], Value::Null);
    let settings_after_plain: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(settings_after_plain["title"], foreign_title);

    // 3b. `install --with-title` without `--replace` MUST error without clobbering foreign `title`
    let (ok_conflict, _) = run_bin(&["install", "--with-title"], &env_vars, None);
    assert!(!ok_conflict);
    let settings_after_conflict: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(settings_after_conflict["title"], foreign_title);

    // 3c. `install --with-title --replace` backs up foreign title to `previous-title.json` and writes `agy-hud title`
    let (ok_with_title, with_title_out) =
        run_bin(&["install", "--with-title", "--replace"], &env_vars, None);
    assert!(
        ok_with_title,
        "install --with-title --replace failed: {with_title_out}"
    );
    let with_title_json: Value = serde_json::from_str(&with_title_out).unwrap();
    assert_eq!(with_title_json["titleConfigured"], true);
    let expected_title_cmd = with_title_json["titleCommand"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(expected_title_cmd.ends_with(" title"));
    assert!(hud_home.join("previous-title.json").is_file());

    // `doctor` reports `title.kind == "agy-hud"` and `enabled == true`
    let (ok_doc, doc_out) = run_bin(&["doctor"], &env_vars, None);
    assert!(ok_doc);
    let doc_json: Value = serde_json::from_str(&doc_out).unwrap();
    assert_eq!(doc_json["title"]["kind"], "agy-hud");
    assert_eq!(doc_json["title"]["enabled"], true);

    // 3d. Re-running `install` without `--with-title` preserves user opt-in & unknown extra keys on `title`!
    let mut custom_key_settings: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    custom_key_settings["title"]["customHint"] = json!("preserve-me");
    fs::write(
        &settings_path,
        serde_json::to_string_pretty(&custom_key_settings).unwrap(),
    )
    .unwrap();

    let (ok_preserve, preserve_out) = run_bin(&["install"], &env_vars, None);
    assert!(ok_preserve);
    let preserve_json: Value = serde_json::from_str(&preserve_out).unwrap();
    assert_eq!(preserve_json["titleConfigured"], true);
    assert_eq!(preserve_json["titleCommand"], expected_title_cmd);
    let preserved_settings: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(preserved_settings["title"]["customHint"], "preserve-me");

    // 3e. `install --no-title` restores foreign `previous-title.json` and removes backup
    let (ok_no_title, no_title_out) = run_bin(&["install", "--no-title"], &env_vars, None);
    assert!(ok_no_title);
    let no_title_json: Value = serde_json::from_str(&no_title_out).unwrap();
    assert_eq!(no_title_json["titleConfigured"], false);
    assert_eq!(no_title_json["titleCommand"], Value::Null);
    assert!(!hud_home.join("previous-title.json").exists());
    let restored_by_flag: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(restored_by_flag["title"], foreign_title);

    // Uninstall while foreign title is active -> `titleAction == "kept-foreign"`
    let (ok_uninst_foreign, uninst_foreign_out) = run_bin(&["uninstall"], &env_vars, None);
    assert!(ok_uninst_foreign);
    let uninst_foreign: Value = serde_json::from_str(&uninst_foreign_out).unwrap();
    assert_eq!(uninst_foreign["titleAction"], "kept-foreign");
    let kept_settings: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(kept_settings["title"], foreign_title);

    // 3f. Re-enable with `--with-title --replace`, verify `uninstall` restores `previous-title.json` cleanly
    let (ok_re_replace, _) = run_bin(&["install", "--with-title", "--replace"], &env_vars, None);
    assert!(ok_re_replace);
    let (ok_uninst_restore, uninst_restore_out) = run_bin(&["uninstall"], &env_vars, None);
    assert!(ok_uninst_restore);
    let uninst_restore: Value = serde_json::from_str(&uninst_restore_out).unwrap();
    assert_eq!(uninst_restore["titleAction"], "restored-previous");
    assert!(!hud_home.exists());
    let restored_settings: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(restored_settings["title"], foreign_title);

    // 4. Clean settings (no prior `title`): `install --with-title` -> live render -> `preview --last --title` -> `uninstall` removes `"title"`
    fs::write(&settings_path, r#"{"colorScheme":"solarized"}"#).unwrap();
    let (ok_clean_inst, clean_inst_out) = run_bin(&["install", "--with-title"], &env_vars, None);
    assert!(ok_clean_inst);
    let clean_inst: Value = serde_json::from_str(&clean_inst_out).unwrap();
    assert_eq!(clean_inst["titleConfigured"], true);

    let (ok_live_snap, _) = run_bin(&[], &env_vars, Some(&title_payload));
    assert!(ok_live_snap);
    let (ok_prev_last, prev_last_out) = run_bin(&["preview", "--last", "--title"], &env_vars, None);
    assert!(ok_prev_last);
    assert_eq!(strip_ansi(&prev_last_out), prev_last_out);
    assert_eq!(
        prev_last_out.trim_end(),
        "● my-proj (feat/title*) · Neon · 34% — Native title mode"
    );

    let (ok_prev_showcase, prev_showcase_out) = run_bin(
        &["preview", "--state", "showcase", "--title"],
        &env_vars,
        None,
    );
    assert!(ok_prev_showcase);
    assert_eq!(strip_ansi(&prev_showcase_out), prev_showcase_out);

    let (ok_clean_uninst, clean_uninst_out) = run_bin(&["uninstall"], &env_vars, None);
    assert!(ok_clean_uninst);
    let clean_uninst: Value = serde_json::from_str(&clean_uninst_out).unwrap();
    assert_eq!(clean_uninst["titleAction"], "restored-official-default");
    assert!(!hud_home.exists());
    let final_settings: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(final_settings, json!({"colorScheme": "solarized"}));

    // 5. `uninstall --reset-default` removes `"title"` instead of restoring `previous-title.json`
    fs::write(
        &settings_path,
        serde_json::to_string_pretty(&json!({
            "colorScheme": "solarized",
            "title": foreign_title
        }))
        .unwrap(),
    )
    .unwrap();
    let (ok_rep2, _) = run_bin(&["install", "--with-title", "--replace"], &env_vars, None);
    assert!(ok_rep2);
    assert!(hud_home.join("previous-title.json").is_file());
    let (ok_reset_uninst, reset_uninst_out) =
        run_bin(&["uninstall", "--reset-default"], &env_vars, None);
    assert!(ok_reset_uninst);
    let reset_uninst: Value = serde_json::from_str(&reset_uninst_out).unwrap();
    assert_eq!(reset_uninst["titleAction"], "restored-official-default");
    assert!(!hud_home.exists());
    let reset_settings: Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(reset_settings, json!({"colorScheme": "solarized"}));
}
