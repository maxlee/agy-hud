use hud_cli::collector::{
    DetectedRepo, GIT_SAFE_PREFIX, HG_SAFE_PREFIX, JJ_PROBE_ARGS, ProbeRunner, SystemProbeRunner,
    compute_vcs_signature, detect_vcs_repo, extract_git_filter_names, probe_git, probe_hg,
    probe_jj, probe_vcs_cached,
};
use hud_cli::paths::{AgyPaths, derive_task_md_path};
use hud_core::types::{VcsStdin, VcsType};
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
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
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
    assert_eq!(inst_json["engine"], "rust-native");
    assert_eq!(inst_json["replaced"], "other");
    assert!(Path::new(inst_json["backupPath"].as_str().unwrap()).exists());
    let expected_bin = if cfg!(windows) {
        "agy-hud.exe"
    } else {
        "agy-hud"
    };
    assert!(hud_home.join("bin").join(expected_bin).exists());

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
    assert_eq!(uninst_json["removedBackups"], 1);
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
