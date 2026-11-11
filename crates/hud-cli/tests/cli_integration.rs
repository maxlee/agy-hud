use hud_cli::collector::{
    DetectedRepo, GIT_SAFE_PREFIX, HG_SAFE_PREFIX, JJ_PROBE_ARGS, ProbeRunner, SystemProbeRunner,
    compute_vcs_signature, detect_vcs_repo, extract_git_filter_names, probe_git, probe_hg,
    probe_jj, probe_vcs_cached, probe_vcs_uncached, read_and_parse_transcript_incremental,
    resolve_executable_in_path,
};
use hud_cli::install::format_statusline_command_for_platform;
use hud_cli::paths::{AgyPaths, build_logic_paths, derive_task_md_path};
use hud_core::ansi::strip_ansi;
use hud_core::types::{AgyStdinData, VcsStdin, VcsType, WorkspaceStdin};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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

    let mut env_map = HashMap::new();
    env_map.insert(
        "AGY_CLI_DIR".to_string(),
        tmp.path.join("cli").to_string_lossy().into_owned(),
    );
    env_map.insert(
        "AGY_HUD_HOME".to_string(),
        tmp.path.join("hud").to_string_lossy().into_owned(),
    );
    let paths = AgyPaths::from_env_map(&env_map);

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

    let env_vars: [(&str, &Path); 2] = [
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
    let clean_env: [(&str, &Path); 2] = [
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
    cmd.env("AGY_CLI_DIR", &cli_dir)
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
        .env("AGY_CLI_DIR", &cli_dir)
        .env("AGY_HUD_HOME", &hud_home)
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

    let mut env_map = HashMap::new();
    env_map.insert(
        "AGY_CLI_DIR".to_string(),
        tmp.path.join("cli").to_string_lossy().into_owned(),
    );
    env_map.insert(
        "AGY_HUD_HOME".to_string(),
        tmp.path.join("hud").to_string_lossy().into_owned(),
    );
    let paths = AgyPaths::from_env_map(&env_map);

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

        let env_vars: [(&str, &Path); 3] = [
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
    let cli_sh = tmp.path.join("cli-sh");
    let hud_sh = cli_sh.join("agy-hud");
    let sh_out = Command::new("sh")
        .arg(workspace_root.join("install.sh"))
        .arg("--stack")
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

    // 2. Test `agy-hud update --check`, live `/hud-update` badge, and `agy-hud update` self-upgrade!
    let cli_upd = tmp.path.join("cli-upd");
    let hud_upd = cli_upd.join("agy-hud");
    let upd_env: [(&str, &Path); 2] = [
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

    // Live render now displays `↑ v9.9.0 · /agy-hud:hud-update` badge!
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
    assert!(badge_out.contains("/agy-hud:hud-update"));
    assert!(badge_out.contains("v9.9.0"));

    // Run `agy-hud update` end-to-end (without needing to re-pass `--stack`)!
    let upgrade_out = Command::new(env!("CARGO_BIN_EXE_agy-hud"))
        .arg("update")
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

    let env_vars = [
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

    let fresh_cli = tmp.path.join("fresh-cli");
    let fresh_hud = fresh_cli.join("agy-hud");

    // 1. Offline execution via bundled `scripts/bin/<platform>/agy-hud` (no $HUD_HOME/bin, no network)
    let offline_out = Command::new("sh")
        .arg("scripts/hud-cli.sh")
        .arg("doctor")
        .current_dir(mp_plugin.join("skills/hud"))
        .env("AGY_CLI_DIR", &fresh_cli)
        .env("AGY_HUD_HOME", &fresh_hud)
        .env("AGY_HUD_NO_BOOTSTRAP", "1")
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
        .env("AGY_CLI_DIR", &cli_dir)
        .env("AGY_HUD_HOME", &hud_home)
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

    let env_vars: [(&str, &Path); 2] = [
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
