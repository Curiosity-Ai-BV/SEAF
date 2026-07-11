use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use seaf_core::{validate_loop_run, LoopInputDigests, LoopStatus};
use seaf_loop::{
    cleanup_candidate_workspace, create_candidate_workspace, validate_candidate_workspace,
    LoopWorkspace,
};
use sha2::{Digest, Sha256};

#[test]
fn candidate_apply_changes_only_the_bound_worktree_and_resume_reuses_it() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let source_before = git(&source, &["status", "--porcelain=v1"]);
    let source_head = git(&source, &["rev-parse", "HEAD"]);
    let run_dir = temp.path().join("runs/run-1");
    fs::create_dir_all(&run_dir).expect("run dir");

    let digest = identity_digest(&source);
    let candidate =
        create_candidate_workspace(&run_dir, &source, &digest).expect("create candidate");
    assert_eq!(
        fs::read_to_string(source.join("tracked.txt")).unwrap(),
        "source\n"
    );
    assert_eq!(git(&source, &["status", "--porcelain=v1"]), source_before);
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), source_head);
    assert_eq!(candidate.starting_head, source_head);
    assert!(!Path::new(&candidate.path).starts_with(&source));
    assert_eq!(candidate.candidate_diff_digest, empty_sha256());
    assert_eq!(
        validate_candidate_workspace(&run_dir, &source, &candidate)
            .expect("resume validates exact candidate"),
        candidate
    );
    remove_worktree(&source, Path::new(&candidate.path));
}

#[test]
fn candidate_creation_is_idempotent_and_disables_repository_checkout_hooks() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let hook_marker = source.join("hook-ran");
    let hook = source.join(".git/hooks/post-checkout");
    fs::write(
        &hook,
        format!("#!/bin/sh\nprintf hook > '{}'\n", hook_marker.display()),
    )
    .expect("write hook");
    make_executable(&hook);
    let run_dir = temp.path().join("runs/run-adopt");
    fs::create_dir_all(&run_dir).expect("run dir");
    let digest = identity_digest(&source);

    let first = create_candidate_workspace(&run_dir, &source, &digest).expect("first create");
    assert!(
        !hook_marker.exists(),
        "candidate creation must not run repository hooks"
    );
    let adopted = create_candidate_workspace(&run_dir, &source, &digest).expect("exact adoption");
    assert_eq!(adopted, first);

    remove_worktree(&source, Path::new(&first.path));
    let cleaned = first;
    fs::create_dir(&cleaned.path).expect("substitute ordinary directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cleaned.path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let error = create_candidate_workspace(&run_dir, &source, &digest)
        .expect_err("wrong existing path must never be adopted");
    assert!(error.to_string().contains("registered worktree"), "{error}");
    fs::remove_dir(&cleaned.path).expect("remove substituted directory");
}

#[test]
fn candidate_creation_and_inspection_never_execute_repository_helpers_or_filters() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let marker = temp.path().join("helper-ran");
    let helper = temp.path().join("malicious-helper");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nprintf invoked >> '{}'\nexit 1\n",
            marker.display()
        ),
    )
    .expect("helper");
    make_executable(&helper);
    fs::write(
        source.join(".gitattributes"),
        "tracked.txt filter=evil diff=evil\n",
    )
    .expect("attributes");
    git_ok(&source, &["add", ".gitattributes"]);
    git_ok(&source, &["commit", "-qm", "helper attributes"]);
    let helper_text = helper.to_str().unwrap();
    for (key, value) in [
        ("filter.evil.smudge", helper_text),
        ("filter.evil.process", helper_text),
        ("filter.evil.required", "true"),
        ("diff.evil.textconv", helper_text),
        ("diff.external", helper_text),
        ("core.fsmonitor", helper_text),
    ] {
        git_ok(&source, &["config", key, value]);
    }
    let run_dir = temp.path().join("runs/run-helpers");
    fs::create_dir_all(&run_dir).expect("run dir");
    let digest = identity_digest(&source);

    let candidate = create_candidate_workspace(&run_dir, &source, &digest)
        .expect("sanitized candidate creation");
    assert!(!marker.exists(), "checkout helpers or filters executed");
    let committed = git_bytes(&source, &["cat-file", "blob", "HEAD:tracked.txt"]);
    assert_eq!(
        fs::read(Path::new(&candidate.path).join("tracked.txt")).unwrap(),
        committed
    );

    git_ok(&source, &["config", "--unset", "filter.evil.process"]);
    git_ok(&source, &["config", "--unset", "filter.evil.smudge"]);
    git_ok(&source, &["config", "--unset", "filter.evil.required"]);
    fs::write(Path::new(&candidate.path).join("tracked.txt"), "changed\n").unwrap();
    git_ok(
        Path::new(&candidate.path),
        &["-c", "core.fsmonitor=false", "add", "tracked.txt"],
    );
    if marker.exists() {
        fs::remove_file(&marker).expect("clear fixture-side helper marker");
    }
    assert!(validate_candidate_workspace(&run_dir, &source, &candidate).is_err());
    assert!(
        !marker.exists(),
        "inspection executed fsmonitor/diff/textconv helpers"
    );
    remove_worktree(&source, Path::new(&candidate.path));
}

#[test]
fn candidate_materialization_preserves_ident_markers_as_exact_committed_bytes() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    fs::write(source.join(".gitattributes"), "tracked.txt ident\n").unwrap();
    fs::write(source.join("tracked.txt"), "$Id$\n").unwrap();
    git_ok(&source, &["add", ".gitattributes", "tracked.txt"]);
    git_ok(&source, &["commit", "-qm", "ident fixture"]);
    let committed = git_bytes(&source, &["cat-file", "blob", "HEAD:tracked.txt"]);
    assert_eq!(committed, b"$Id$\n");
    let run_dir = temp.path().join("runs/run-ident");
    fs::create_dir_all(&run_dir).unwrap();

    let candidate = create_candidate_workspace(&run_dir, &source, &identity_digest(&source))
        .expect("raw candidate");
    assert_eq!(
        fs::read(Path::new(&candidate.path).join("tracked.txt")).unwrap(),
        committed,
        "candidate bytes must bypass Git ident expansion"
    );
    remove_worktree(&source, Path::new(&candidate.path));
}

#[cfg(unix)]
#[test]
fn candidate_materialization_preserves_executable_and_raw_symlink_index_modes() {
    use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};

    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let executable = source.join("run.sh");
    fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();
    let raw_target = std::ffi::OsString::from_vec(vec![b't', b'a', b'r', b'g', b'e', b't', 0xff]);
    std::os::unix::fs::symlink(&raw_target, source.join("raw-link")).unwrap();
    git_ok(&source, &["add", "run.sh", "raw-link"]);
    git_ok(&source, &["commit", "-qm", "mode fixtures"]);
    let run_dir = temp.path().join("runs/run-modes");
    fs::create_dir_all(&run_dir).unwrap();

    let candidate = create_candidate_workspace(&run_dir, &source, &identity_digest(&source))
        .expect("raw mode candidate");
    let candidate_root = Path::new(&candidate.path);
    assert_ne!(
        fs::metadata(candidate_root.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert_eq!(
        fs::read_link(candidate_root.join("raw-link")).unwrap(),
        PathBuf::from(raw_target)
    );
    validate_candidate_workspace(&run_dir, &source, &candidate).expect("raw modes validate");
    remove_worktree(&source, candidate_root);
}

#[test]
fn candidate_creation_preserves_dirty_source_and_rejects_dirty_crash_adoption() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    fs::write(source.join("tracked.txt"), "user dirty source\n").unwrap();
    let source_status = git(&source, &["status", "--porcelain=v1"]);
    let run_dir = temp.path().join("runs/run-dirty-adoption");
    fs::create_dir_all(&run_dir).unwrap();
    let digest = identity_digest(&source);

    let candidate = create_candidate_workspace(&run_dir, &source, &digest)
        .expect("dirty source does not affect isolated HEAD candidate");
    assert_eq!(
        fs::read_to_string(source.join("tracked.txt")).unwrap(),
        "user dirty source\n"
    );
    assert_eq!(git(&source, &["status", "--porcelain=v1"]), source_status);
    fs::write(
        Path::new(&candidate.path).join("tracked.txt"),
        "dirty remnant\n",
    )
    .unwrap();
    let error = create_candidate_workspace(&run_dir, &source, &digest)
        .expect_err("dirty crash remnant must not be adopted");
    assert!(
        error.to_string().contains("differs from its index"),
        "{error}"
    );
    remove_worktree(&source, Path::new(&candidate.path));
}

#[test]
fn candidate_git_subprocesses_ignore_repository_redirection_and_config_injection_env() {
    const CHILD: &str = "SEAF_CANDIDATE_ENV_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let source = PathBuf::from(std::env::var_os("SEAF_TEST_SOURCE").unwrap());
        let run_dir = PathBuf::from(std::env::var_os("SEAF_TEST_RUN_DIR").unwrap());
        let output = PathBuf::from(std::env::var_os("SEAF_TEST_OUTPUT").unwrap());
        let digest = std::env::var("SEAF_TEST_DIGEST").unwrap();
        let candidate =
            create_candidate_workspace(&run_dir, &source, &digest).expect("sanitized create");
        validate_candidate_workspace(&run_dir, &source, &candidate).expect("sanitized validate");
        fs::write(output, serde_json::to_vec(&candidate).unwrap()).unwrap();
        return;
    }

    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    let redirected = temp.path().join("redirected");
    init_repo(&source);
    init_repo(&redirected);
    let run_dir = temp.path().join("runs/run-env");
    fs::create_dir_all(&run_dir).expect("run dir");
    let output_path = temp.path().join("candidate.json");
    let marker = temp.path().join("env-helper-ran");
    let helper = temp.path().join("env-helper");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nprintf invoked >> '{}'\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    make_executable(&helper);
    let digest = identity_digest(&source);
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "candidate_git_subprocesses_ignore_repository_redirection_and_config_injection_env",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("SEAF_TEST_SOURCE", &source)
        .env("SEAF_TEST_RUN_DIR", &run_dir)
        .env("SEAF_TEST_OUTPUT", &output_path)
        .env("SEAF_TEST_DIGEST", &digest)
        .env("GIT_DIR", redirected.join(".git"))
        .env("GIT_WORK_TREE", &redirected)
        .env("GIT_INDEX_FILE", redirected.join("injected-index"))
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.fsmonitor")
        .env("GIT_CONFIG_VALUE_0", &helper)
        .output()
        .expect("child test");
    assert!(
        child.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(!marker.exists(), "injected config helper executed");
    let candidate: seaf_core::CandidateWorkspaceState =
        serde_json::from_slice(&fs::read(output_path).unwrap()).unwrap();
    assert_eq!(
        candidate.source_worktree_root,
        source.canonicalize().unwrap().display().to_string()
    );
    remove_worktree(&source, Path::new(&candidate.path));
}

#[test]
fn candidate_identity_and_bytes_ignore_blob_tree_and_commit_replace_refs() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let original_head = git(&source, &["rev-parse", "HEAD"]);
    let original_tree = git(&source, &["rev-parse", "HEAD^{tree}"]);
    let original_blob = git(&source, &["rev-parse", "HEAD:tracked.txt"]);
    let replacement_file = temp.path().join("replacement.txt");
    fs::write(&replacement_file, "replacement bytes\n").unwrap();
    let replacement_blob = git(
        &source,
        &["hash-object", "-w", replacement_file.to_str().unwrap()],
    );
    let tree_input = format!("100644 blob {replacement_blob}\ttracked.txt\n");
    let replacement_tree = git_with_stdin(&source, &["mktree"], tree_input.as_bytes());
    let replacement_commit = git(
        &source,
        &["commit-tree", &replacement_tree, "-m", "replacement"],
    );
    for (original, replacement) in [
        (&original_blob, &replacement_blob),
        (&original_tree, &replacement_tree),
        (&original_head, &replacement_commit),
    ] {
        git_ok(&source, &["replace", original, replacement]);
    }
    let run_dir = temp.path().join("runs/run-replace-refs");
    fs::create_dir_all(&run_dir).unwrap();

    let candidate = create_candidate_workspace(&run_dir, &source, &identity_digest(&source))
        .expect("replace refs are disabled");
    assert_eq!(candidate.starting_head, original_head);
    assert_eq!(candidate.starting_tree, original_tree);
    assert_eq!(
        fs::read_to_string(Path::new(&candidate.path).join("tracked.txt")).unwrap(),
        "source\n"
    );
    remove_worktree(&source, Path::new(&candidate.path));
}

#[test]
fn concurrent_creators_serialize_and_adopt_without_deleting_the_winner() {
    use std::sync::{Arc, Barrier};

    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let run_dir = temp.path().join("runs/run-concurrent");
    fs::create_dir_all(&run_dir).unwrap();
    let digest = identity_digest(&source);
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let source = source.clone();
        let run_dir = run_dir.clone();
        let digest = digest.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            create_candidate_workspace(&run_dir, &source, &digest)
        }));
    }
    barrier.wait();
    let first = handles.remove(0).join().unwrap().expect("creator");
    let second = handles.remove(0).join().unwrap().expect("adopter");
    assert_eq!(first, second);
    assert!(Path::new(&first.path).is_dir());
    validate_candidate_workspace(&run_dir, &source, &first).expect("winner remains valid");
    remove_worktree(&source, Path::new(&first.path));
}

#[cfg(unix)]
#[test]
fn candidate_authority_directories_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let run_dir = temp.path().join("runs/run-private");
    fs::create_dir_all(&run_dir).unwrap();
    let candidate = create_candidate_workspace(&run_dir, &source, &identity_digest(&source))
        .expect("private candidate");
    let candidate_path = PathBuf::from(&candidate.path);
    let repository_root = candidate_path.parent().unwrap();
    let shared_root = repository_root.parent().unwrap();
    for path in [shared_root, repository_root, candidate_path.as_path()] {
        assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
    }
    remove_worktree(&source, &candidate_path);
}

#[test]
fn preapply_candidate_rejects_staged_new_files_until_m1_05b_binds_them() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let run_dir = temp.path().join("runs/run-new-file");
    fs::create_dir_all(&run_dir).expect("run dir");
    let digest = identity_digest(&source);
    let candidate =
        create_candidate_workspace(&run_dir, &source, &digest).expect("create candidate");
    fs::write(
        Path::new(&candidate.path).join("new.txt"),
        "new candidate file\n",
    )
    .expect("new candidate file");
    git_ok(Path::new(&candidate.path), &["add", "new.txt"]);
    let error = validate_candidate_workspace(&run_dir, &source, &candidate)
        .expect_err("M1-05a must reject staged patch bytes");
    assert!(error.to_string().contains("starting tree"), "{error}");
    assert!(!source.join("new.txt").exists());
    remove_worktree(&source, Path::new(&candidate.path));
}

#[test]
fn resume_rejects_substituted_candidate_before_mutating_either_checkout() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    let other = temp.path().join("other");
    init_repo(&source);
    init_repo(&other);
    let run_dir = temp.path().join("runs/run-2");
    fs::create_dir_all(&run_dir).expect("run dir");
    let digest = identity_digest(&source);
    let original =
        create_candidate_workspace(&run_dir, &source, &digest).expect("create candidate");
    let mut candidate = original.clone();
    candidate.git_common_dir = git(&other, &["rev-parse", "--git-common-dir"]);
    let source_before = git(&source, &["status", "--porcelain=v1"]);

    let error = validate_candidate_workspace(&run_dir, &source, &candidate)
        .expect_err("wrong repository identity must fail closed");
    assert!(
        error.to_string().contains("Git common directory"),
        "{error}"
    );
    assert_eq!(git(&source, &["status", "--porcelain=v1"]), source_before);
    remove_worktree(&source, Path::new(&original.path));
}

#[test]
fn cleanup_refuses_active_runs_and_removes_only_the_verified_bound_worktree() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let (workspace, candidate) =
        persisted_candidate_workspace(temp.path(), &source, "run-3", LoopStatus::Pending);
    let candidate_path = candidate.path.clone();
    let source_head = git(&source, &["rev-parse", "HEAD"]);
    let source_status = git(&source, &["status", "--porcelain=v1"]);

    for status in [LoopStatus::Pending, LoopStatus::Running] {
        let mut run = seaf_loop::state::load_run(&workspace).expect("run");
        run.status = status;
        seaf_loop::state::save_run(&workspace, &run).expect("persist active status");
        let error = cleanup_candidate_workspace(&workspace, &source)
            .expect_err("active runs retain their candidate");
        assert!(error.to_string().contains("active"), "{error}");
        assert!(Path::new(&candidate_path).is_dir());
    }

    let mut run = seaf_loop::state::load_run(&workspace).expect("run");
    run.status = LoopStatus::Completed;
    seaf_loop::state::save_run(&workspace, &run).expect("persist terminal status");
    let cleaned =
        cleanup_candidate_workspace(&workspace, &source).expect("explicit terminal cleanup");
    assert!(!Path::new(&candidate_path).exists());
    assert!(cleaned.cleaned_at.is_some());
    assert!(source.join("tracked.txt").is_file());
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), source_head);
    assert_eq!(git(&source, &["status", "--porcelain=v1"]), source_status);
    assert_eq!(
        fs::read_to_string(source.join("tracked.txt")).unwrap(),
        "source\n"
    );
    assert_eq!(
        cleanup_candidate_workspace(&workspace, &source)
            .expect("cleanup evidence makes retry idempotent"),
        cleaned
    );
    let authoritative = seaf_loop::state::load_run(&workspace).expect("cleaned run");
    assert_eq!(authoritative.candidate_workspace.as_ref(), Some(&cleaned));
    let mut tampered_cleaned = cleaned.clone();
    tampered_cleaned.path = temp.path().join("not-the-candidate").display().to_string();
    let mut tampered_run = authoritative.clone();
    tampered_run.candidate_workspace = Some(tampered_cleaned);
    seaf_loop::state::save_run(&workspace, &tampered_run).expect("persist tampered authority");
    assert!(cleanup_candidate_workspace(&workspace, &source).is_err());
    seaf_loop::state::save_run(&workspace, &authoritative).expect("restore authority");
    fs::create_dir(&cleaned.path).expect("reappearing cleaned candidate path");
    assert!(cleanup_candidate_workspace(&workspace, &source).is_err());
    fs::remove_dir(&cleaned.path).expect("remove reappearing path");
    git_ok(
        &source,
        &[
            "worktree",
            "add",
            "--detach",
            &cleaned.path,
            &cleaned.starting_head,
        ],
    );
    assert!(cleanup_candidate_workspace(&workspace, &source).is_err());
    remove_worktree(&source, Path::new(&cleaned.path));
}

#[test]
fn candidate_validation_rejects_source_head_movement_without_mutating_candidate() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let digest = identity_digest(&source);
    let run_dir = temp.path().join("runs/run-source-head");
    fs::create_dir_all(&run_dir).expect("run dir");
    let candidate = create_candidate_workspace(&run_dir, &source, &digest).expect("candidate");
    let candidate_status = git(Path::new(&candidate.path), &["status", "--porcelain=v1"]);
    fs::write(source.join("source-moved.txt"), "new source commit\n").unwrap();
    git_ok(&source, &["add", "source-moved.txt"]);
    git_ok(&source, &["commit", "-qm", "move source head"]);

    let error = validate_candidate_workspace(&run_dir, &source, &candidate)
        .expect_err("source HEAD movement must fail closed");
    assert!(error.to_string().contains("source HEAD"), "{error}");
    assert_eq!(
        git(Path::new(&candidate.path), &["status", "--porcelain=v1"]),
        candidate_status
    );
    remove_worktree(&source, Path::new(&candidate.path));
}

#[test]
fn cleanup_reconciles_durable_intent_after_the_bound_worktree_was_removed() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let (workspace, candidate) = persisted_candidate_workspace(
        temp.path(),
        &source,
        "run-cleanup-crash",
        LoopStatus::Completed,
    );
    let mut interrupted = seaf_loop::state::load_run(&workspace).expect("run");
    let authority = interrupted.candidate_workspace.as_mut().unwrap();
    authority.lifecycle = seaf_core::CandidateWorkspaceLifecycle::Cleaning;
    authority.cleanup_started_at = Some("cleanup-intent".to_string());
    seaf_loop::state::save_run(&workspace, &interrupted).expect("durable cleanup intent");
    remove_worktree(&source, Path::new(&candidate.path));
    fs::write(source.join("after-intent.txt"), "source advances\n").unwrap();
    git_ok(&source, &["add", "after-intent.txt"]);
    git_ok(&source, &["commit", "-qm", "advance after cleanup intent"]);

    let cleaned = cleanup_candidate_workspace(&workspace, &source)
        .expect("retry finalizes a durable post-remove cleanup");
    assert_eq!(
        cleaned.lifecycle,
        seaf_core::CandidateWorkspaceLifecycle::Cleaned
    );
    assert!(cleaned.cleanup_started_at.is_some());
    assert!(cleaned.cleaned_at.is_some());
    assert_eq!(
        seaf_loop::state::load_run(&workspace)
            .unwrap()
            .candidate_workspace,
        Some(cleaned)
    );
}

#[test]
fn terminal_cleanup_releases_the_exact_candidate_after_source_head_advances() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let (workspace, candidate) = persisted_candidate_workspace(
        temp.path(),
        &source,
        "run-cleanup-source-moved",
        LoopStatus::Completed,
    );
    fs::write(source.join("later.txt"), "later source commit\n").unwrap();
    git_ok(&source, &["add", "later.txt"]);
    git_ok(&source, &["commit", "-qm", "later source commit"]);
    let source_head = git(&source, &["rev-parse", "HEAD"]);

    cleanup_candidate_workspace(&workspace, &source)
        .expect("terminal cleanup does not require stale source HEAD");
    assert!(!Path::new(&candidate.path).exists());
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), source_head);
    assert_eq!(
        fs::read_to_string(source.join("later.txt")).unwrap(),
        "later source commit\n"
    );
}

#[test]
fn cleanup_rejects_tampered_authoritative_candidate_before_removal() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let (workspace, candidate) = persisted_candidate_workspace(
        temp.path(),
        &source,
        "run-cleanup-tamper",
        LoopStatus::Completed,
    );
    let mut run = seaf_loop::state::load_run(&workspace).expect("run");
    run.candidate_workspace.as_mut().unwrap().path =
        temp.path().join("substituted").display().to_string();
    seaf_loop::state::save_run(&workspace, &run).expect("tampered run fixture");
    assert!(cleanup_candidate_workspace(&workspace, &source).is_err());
    assert!(Path::new(&candidate.path).is_dir());
    remove_worktree(&source, Path::new(&candidate.path));
}

#[cfg(unix)]
#[test]
fn cleanup_rejects_a_symlinked_candidate_lock_before_state_or_worktree_mutation() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let (workspace, candidate) = persisted_candidate_workspace(
        temp.path(),
        &source,
        "run-cleanup-lock",
        LoopStatus::Completed,
    );
    let target = temp.path().join("lock-target");
    fs::write(&target, "target").unwrap();
    fs::remove_file(workspace.run_directory().join(".candidate-workspace.lock")).unwrap();
    std::os::unix::fs::symlink(
        &target,
        workspace.run_directory().join(".candidate-workspace.lock"),
    )
    .unwrap();
    let before = fs::read(workspace.run_file()).unwrap();

    assert!(cleanup_candidate_workspace(&workspace, &source).is_err());
    assert_eq!(fs::read(workspace.run_file()).unwrap(), before);
    assert!(Path::new(&candidate.path).is_dir());
    remove_worktree(&source, Path::new(&candidate.path));
}

#[test]
fn active_candidate_must_remain_registered_and_detached() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let digest = identity_digest(&source);

    let attached_run = temp.path().join("runs/run-attached");
    fs::create_dir_all(&attached_run).expect("run dir");
    let attached = create_candidate_workspace(&attached_run, &source, &digest).expect("candidate");
    git_ok(
        Path::new(&attached.path),
        &["switch", "-qc", "candidate-branch"],
    );
    let adoption = create_candidate_workspace(&attached_run, &source, &digest)
        .expect_err("attached candidate must not be adopted after a crash cut");
    assert!(adoption.to_string().contains("detached"), "{adoption}");
    let error = validate_candidate_workspace(&attached_run, &source, &attached)
        .expect_err("attached branch at the same HEAD/tree must fail closed");
    assert!(error.to_string().contains("detached"), "{error}");
    remove_worktree(&source, Path::new(&attached.path));

    let unregistered_run = temp.path().join("runs/run-unregistered");
    fs::create_dir_all(&unregistered_run).expect("run dir");
    let unregistered =
        create_candidate_workspace(&unregistered_run, &source, &digest).expect("candidate");
    let original = PathBuf::from(&unregistered.path);
    let moved = original.with_extension("saved");
    fs::rename(&original, &moved).expect("temporarily move worktree");
    git_ok(&source, &["worktree", "prune"]);
    fs::rename(&moved, &original).expect("restore physical worktree");
    let error = validate_candidate_workspace(&unregistered_run, &source, &unregistered)
        .expect_err("physical candidate without registration must fail closed");
    assert!(error.to_string().contains("registered"), "{error}");
    fs::remove_dir_all(&original).expect("remove unregistered test worktree");
}

#[test]
fn candidate_validation_rejects_unstaged_ordinary_and_ignored_untracked_bytes() {
    for case in ["unstaged", "ordinary-untracked", "ignored-untracked"] {
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("source");
        init_repo(&source);
        if case == "ignored-untracked" {
            fs::write(source.join(".gitignore"), "*.ignored\n").unwrap();
            git_ok(&source, &["add", ".gitignore"]);
            git_ok(&source, &["commit", "-qm", "ignore fixture"]);
        }
        let digest = identity_digest(&source);
        let run_dir = temp.path().join(format!("runs/run-{case}"));
        fs::create_dir_all(&run_dir).expect("run dir");
        let candidate = create_candidate_workspace(&run_dir, &source, &digest).expect("candidate");
        match case {
            "unstaged" => {
                fs::write(Path::new(&candidate.path).join("tracked.txt"), "unstaged\n").unwrap();
            }
            "ordinary-untracked" => {
                fs::write(
                    Path::new(&candidate.path).join("ordinary.txt"),
                    "ordinary\n",
                )
                .unwrap();
            }
            "ignored-untracked" => {
                fs::write(
                    Path::new(&candidate.path).join("secret.ignored"),
                    "ignored\n",
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let error = validate_candidate_workspace(&run_dir, &source, &candidate)
            .expect_err("extra candidate bytes must fail closed");
        assert!(
            error.to_string().contains("worktree differs")
                || error.to_string().contains("untracked"),
            "{case}: {error}"
        );
        remove_worktree(&source, Path::new(&candidate.path));
    }
}

#[cfg(unix)]
#[test]
fn candidate_validation_rejects_executable_mode_drift() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let digest = identity_digest(&source);
    let run_dir = temp.path().join("runs/run-mode-drift");
    fs::create_dir_all(&run_dir).expect("run dir");
    let candidate = create_candidate_workspace(&run_dir, &source, &digest).expect("candidate");
    let tracked = Path::new(&candidate.path).join("tracked.txt");
    let mut permissions = fs::metadata(&tracked).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&tracked, permissions).unwrap();

    let error = validate_candidate_workspace(&run_dir, &source, &candidate)
        .expect_err("executable bit drift must fail closed");
    assert!(error.to_string().contains("executable mode"), "{error}");
    remove_worktree(&source, Path::new(&candidate.path));
}

#[test]
fn candidate_validation_rejects_missing_symlinked_wrong_head_and_tampered_diff_state() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let digest = identity_digest(&source);

    let run_tamper = temp.path().join("runs/run-tamper");
    fs::create_dir_all(&run_tamper).expect("run dir");
    let candidate = create_candidate_workspace(&run_tamper, &source, &digest).expect("candidate");
    let mut tampered = candidate.clone();
    tampered.candidate_diff_digest = identity_digest(Path::new("tampered"));
    let error = validate_candidate_workspace(&run_tamper, &source, &tampered)
        .expect_err("tampered diff evidence rejected");
    assert!(
        error.to_string().contains("starting tree and empty diff"),
        "{error}"
    );
    remove_worktree(&source, Path::new(&candidate.path));

    let run_head = temp.path().join("runs/run-head");
    fs::create_dir_all(&run_head).expect("run dir");
    let candidate = create_candidate_workspace(&run_head, &source, &digest).expect("candidate");
    fs::write(Path::new(&candidate.path).join("commit.txt"), "commit\n").unwrap();
    git_ok(Path::new(&candidate.path), &["add", "commit.txt"]);
    git_ok(
        Path::new(&candidate.path),
        &["commit", "-qm", "unauthorized"],
    );
    let error = validate_candidate_workspace(&run_head, &source, &candidate)
        .expect_err("wrong candidate HEAD rejected");
    assert!(error.to_string().contains("HEAD"), "{error}");
    remove_worktree(&source, Path::new(&candidate.path));

    let run_missing = temp.path().join("runs/run-missing");
    fs::create_dir_all(&run_missing).expect("run dir");
    let candidate = create_candidate_workspace(&run_missing, &source, &digest).expect("candidate");
    remove_worktree(&source, Path::new(&candidate.path));
    assert!(validate_candidate_workspace(&run_missing, &source, &candidate).is_err());

    #[cfg(unix)]
    {
        let run_link = temp.path().join("runs/run-link");
        fs::create_dir_all(&run_link).expect("run dir");
        let candidate = create_candidate_workspace(&run_link, &source, &digest).expect("candidate");
        let original = PathBuf::from(&candidate.path);
        let moved = original.with_extension("moved");
        fs::rename(&original, &moved).expect("move candidate");
        std::os::unix::fs::symlink(&moved, &original).expect("candidate symlink");
        assert!(validate_candidate_workspace(&run_link, &source, &candidate).is_err());
        fs::remove_file(&original).expect("remove symlink");
        fs::rename(&moved, &original).expect("restore candidate");
        remove_worktree(&source, &original);
    }
}

#[test]
fn candidate_contract_is_closed_and_rust_schema_invariants_stay_aligned() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let digest = identity_digest(&source);
    let run_dir = temp.path().join("runs/run-contract");
    fs::create_dir_all(&run_dir).expect("run dir");
    let candidate = create_candidate_workspace(&run_dir, &source, &digest).expect("candidate");
    let mut run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "run-contract".to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "goal".to_string(),
        provider: "fake".to_string(),
        model: "model".to_string(),
        input_digests: LoopInputDigests {
            ticket: identity_digest(Path::new("ticket")),
            policy: identity_digest(Path::new("policy")),
            config: identity_digest(Path::new("config")),
            repository: digest,
        },
    });
    run.candidate_workspace = Some(candidate.clone());
    assert!(validate_loop_run(&run).is_empty());

    let mut invalid = run.clone();
    invalid.candidate_workspace.as_mut().unwrap().candidate_head = "f".repeat(40);
    assert!(validate_loop_run(&invalid)
        .iter()
        .any(|error| error.field == "candidate_workspace.candidate_head"));
    let mut wrong_repository = run.clone();
    wrong_repository.input_digests.repository = identity_digest(Path::new("wrong-repository"));
    assert!(validate_loop_run(&wrong_repository)
        .iter()
        .any(|error| { error.field == "candidate_workspace.repository_identity_digest" }));
    let mut active_cleanup = run.clone();
    {
        let candidate = active_cleanup.candidate_workspace.as_mut().unwrap();
        candidate.lifecycle = seaf_core::CandidateWorkspaceLifecycle::Cleaning;
        candidate.cleanup_started_at = Some("cleanup".to_string());
    }
    assert!(validate_loop_run(&active_cleanup)
        .iter()
        .any(|error| error.field == "candidate_workspace.lifecycle"));
    let mut json = serde_json::to_value(&run).expect("run JSON");
    json["candidate_workspace"]["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<seaf_core::LoopRun>(json).is_err());
    let mut explicit_nulls = serde_json::to_value(&run).expect("run JSON");
    explicit_nulls["candidate_workspace"]["cleanup_started_at"] = serde_json::Value::Null;
    explicit_nulls["candidate_workspace"]["cleaned_at"] = serde_json::Value::Null;
    let explicit_nulls: seaf_core::LoopRun =
        serde_json::from_value(explicit_nulls).expect("explicit null options remain compatible");
    assert!(validate_loop_run(&explicit_nulls).is_empty());

    let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../specs/loop-run.schema.json"
    )))
    .expect("loop schema");
    let contract = &schema["properties"]["candidate_workspace"]["anyOf"][0];
    assert_eq!(contract["additionalProperties"], false);
    assert_eq!(
        contract["allOf"][0]["then"]["properties"]["cleaned_at"]["type"],
        "null"
    );
    assert_eq!(
        contract["properties"]["candidate_diff_digest"]["const"],
        empty_sha256()
    );
    for field in [
        "path",
        "source_worktree_root",
        "git_common_dir",
        "starting_head",
        "starting_tree",
        "candidate_head",
        "candidate_tree",
        "candidate_diff_digest",
        "lifecycle",
    ] {
        assert!(contract["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == field));
    }
    remove_worktree(&source, Path::new(&candidate.path));
}

fn init_repo(path: &Path) {
    fs::create_dir_all(path).expect("repo dir");
    git_ok(path, &["init", "-q"]);
    git_ok(path, &["config", "user.email", "seaf@example.invalid"]);
    git_ok(path, &["config", "user.name", "SEAF Test"]);
    fs::write(path.join("tracked.txt"), "source\n").expect("tracked file");
    git_ok(path, &["add", "tracked.txt"]);
    git_ok(path, &["commit", "-qm", "initial"]);
}

fn persisted_candidate_workspace(
    root: &Path,
    source: &Path,
    run_id: &str,
    status: LoopStatus,
) -> (LoopWorkspace, seaf_core::CandidateWorkspaceState) {
    let runs_root = root.join("runs");
    let workspace = LoopWorkspace::create(&runs_root, run_id).expect("loop workspace");
    let repository = identity_digest(source);
    let candidate = create_candidate_workspace(workspace.run_directory(), source, &repository)
        .expect("candidate");
    let mut run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: run_id.to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "goal".to_string(),
        provider: "fake".to_string(),
        model: "model".to_string(),
        input_digests: LoopInputDigests {
            ticket: identity_digest(Path::new("ticket")),
            policy: identity_digest(Path::new("policy")),
            config: identity_digest(Path::new("config")),
            repository,
        },
    });
    run.status = status;
    run.candidate_workspace = Some(candidate.clone());
    seaf_loop::state::save_run(&workspace, &run).expect("persist candidate run");
    (workspace, candidate)
}

fn git_ok(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn git_bytes(path: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git_with_stdin(path: &Path, args: &[&str], stdin: &[u8]) -> String {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new("git")
        .args(args)
        .current_dir(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn empty_sha256() -> &'static str {
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
}

fn identity_digest(path: &Path) -> String {
    hex::encode(Sha256::digest(path.as_os_str().as_encoded_bytes()))
}

fn remove_worktree(source: &Path, candidate: &Path) {
    git_ok(
        source,
        &["worktree", "remove", "--force", candidate.to_str().unwrap()],
    );
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
