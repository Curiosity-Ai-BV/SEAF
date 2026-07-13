use std::{fs, path::Path, process::Command};

#[cfg(unix)]
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

use seaf_core::{
    LoopInputDigests, LoopRun, LoopStatus, LoopStepName, LoopStepStatus, ProviderExchangeKind,
    ProviderExchangePhase, ProviderExchangeRecord, ProviderRole, TicketContext, TicketSpec,
    TicketStatus,
};
use seaf_loop::{
    persist_provider_exchange_record_reference, stage_provider_exchange_record,
    state::{create_run, finish_step, NewLoopRun},
    write_provider_exchange_request, ArtifactContent, AuthoritativeRunInputSnapshots,
    ContextManifest, InitializedLoopRun, LoopRunner, LoopRunnerConfig, LoopWorkspace,
    ProviderExchangeCoordinates, StepOutput, StepRunner, UNTRUSTED_CONTEXT_MARKER,
};

#[cfg(unix)]
#[test]
fn run_tree_is_private_even_when_process_umask_is_zero() {
    const CHILD: &str = "SEAF_PRIVATE_RUN_TREE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("run_tree_is_private_even_when_process_umask_is_zero")
            .arg("--nocapture")
            .env(CHILD, "1")
            .status()
            .expect("spawn isolated umask child");
        assert!(status.success(), "private run-tree child failed");
        return;
    }

    // SAFETY: this branch runs in a dedicated child process because umask is process-global.
    unsafe { libc::umask(0) };
    let temp = tempfile::tempdir().unwrap();
    let runs_root = temp.path().join("runs");
    let workspace = LoopWorkspace::create(&runs_root, "private-run").unwrap();
    let run = create_run(NewLoopRun {
        run_id: "private-run".to_string(),
        ticket_id: "ticket-private".to_string(),
        goal_id: "goal-private".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: test_input_digests(),
    });
    seaf_loop::state::save_run(&workspace, &run).unwrap();
    workspace.append_log("private line").unwrap();
    seaf_loop::workspace::write_artifact(
        workspace.run_directory(),
        "prompts/request.md",
        b"private prompt",
    )
    .unwrap();

    for directory in ["", "prompts", "responses", "artifacts"] {
        let mode = fs::symlink_metadata(workspace.run_directory().join(directory))
            .unwrap()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "directory {directory:?} mode");
    }
    for file in [
        "run.json",
        "provider-exchange.lock",
        "context-manifest.json",
        "log.md",
        "prompts/request.md",
    ] {
        let mode = fs::symlink_metadata(workspace.run_directory().join(file))
            .unwrap()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "file {file} mode");
    }

    let (source, initialized, snapshots) = isolated_fixture(temp.path(), "private-isolated");
    let candidate = initialized
        .run()
        .candidate_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    let private_run = initialized
        .scaffold()
        .unwrap()
        .publish_authoritative_inputs(snapshots)
        .unwrap();
    for directory in ["inputs", "prompts", "responses", "artifacts"] {
        assert_eq!(
            fs::symlink_metadata(private_run.workspace().run_directory().join(directory))
                .unwrap()
                .mode()
                & 0o777,
            0o700,
            "isolated directory {directory} mode"
        );
    }
    for file in [
        ".candidate-workspace.lock",
        "inputs/ticket.json",
        "inputs/policy.json",
        "inputs/config.json",
        "inputs/repository.json",
        "inputs/eval-config.json",
        "ticket.snapshot.json",
    ] {
        assert_eq!(
            fs::symlink_metadata(private_run.workspace().run_directory().join(file))
                .unwrap()
                .mode()
                & 0o777,
            0o600,
            "isolated file {file} mode"
        );
    }
    git_ok(&source, &["worktree", "remove", "--force", &candidate]);
}

#[cfg(unix)]
#[test]
fn broad_existing_run_permissions_fail_closed_without_mutating_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let runs_root = temp.path().join("runs");
    let workspace = LoopWorkspace::create(&runs_root, "broad-run").unwrap();
    let run = create_run(NewLoopRun {
        run_id: "broad-run".to_string(),
        ticket_id: "ticket-broad".to_string(),
        goal_id: "goal-broad".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: test_input_digests(),
    });
    seaf_loop::state::save_run(&workspace, &run).unwrap();
    let run_path = workspace.run_file();
    let before = fs::read(&run_path).unwrap();
    fs::set_permissions(&run_path, fs::Permissions::from_mode(0o644)).unwrap();

    let error = seaf_loop::state::load_run(&workspace).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("chmod 600"), "{message}");
    assert_eq!(fs::read(&run_path).unwrap(), before);
    assert_eq!(
        fs::symlink_metadata(&run_path).unwrap().mode() & 0o777,
        0o644
    );
}

#[cfg(unix)]
#[test]
fn broad_existing_run_and_scaffold_directory_modes_fail_with_chmod_guidance() {
    for relative in ["", "prompts"] {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        let workspace = LoopWorkspace::create(&runs_root, "broad-directory").unwrap();
        let run = create_run(NewLoopRun {
            run_id: "broad-directory".to_string(),
            ticket_id: "ticket-broad-directory".to_string(),
            goal_id: "goal-broad-directory".to_string(),
            provider: "fake".to_string(),
            model: "fake".to_string(),
            input_digests: test_input_digests(),
        });
        seaf_loop::state::save_run(&workspace, &run).unwrap();
        let before = read_tree_bytes(workspace.run_directory());
        let path = workspace.run_directory().join(relative);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let error = if relative.is_empty() {
            LoopWorkspace::open_minimal(&runs_root, "broad-directory").unwrap_err()
        } else {
            LoopWorkspace::open(&runs_root, "broad-directory").unwrap_err()
        };
        assert!(error.to_string().contains("chmod 700"), "{error}");
        assert_eq!(read_tree_bytes(workspace.run_directory()), before);
        assert_eq!(fs::symlink_metadata(path).unwrap().mode() & 0o777, 0o755);
    }
}

#[test]
fn isolated_initialization_persists_and_provisions_before_runtime_scaffold_and_prepare() {
    let temp = tempfile::tempdir().expect("temp");
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    git_ok(&source, &["init", "-q"]);
    git_ok(&source, &["config", "user.email", "test@example.com"]);
    git_ok(&source, &["config", "user.name", "SEAF Test"]);
    fs::write(source.join("tracked.txt"), "source\n").unwrap();
    git_ok(&source, &["add", "tracked.txt"]);
    git_ok(&source, &["commit", "-qm", "initial"]);
    let runs_root = temp.path().join("runs");
    let ticket_bytes = seaf_core::canonical_json_bytes(&ticket()).unwrap();
    let policy_bytes =
        seaf_core::canonical_json_bytes(&serde_json::json!({"policy": "test"})).unwrap();
    let config_bytes =
        seaf_core::canonical_json_bytes(&serde_json::json!({"config": "test"})).unwrap();
    let repository_bytes = seaf_core::canonical_json_bytes(&serde_json::json!({
        "repository": source.canonicalize().unwrap()
    }))
    .unwrap();
    let eval_config = seaf_core::parse_eval_config(
        "evals:\n  allow_commands: []\n  required:\n    - name: tests\n      command: cargo test\n",
    )
    .unwrap();
    let eval_config_bytes = seaf_core::canonical_json_bytes(&eval_config).unwrap();
    let digest = |bytes: &[u8]| {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    };
    let config = LoopRunnerConfig::for_ticket(
        &runs_root,
        "isolated-init",
        &ticket(),
        "fake-provider",
        "fake-model",
        LoopInputDigests {
            ticket: digest(&ticket_bytes),
            policy: digest(&policy_bytes),
            config: digest(&config_bytes),
            repository: digest(&repository_bytes),
            eval_config: Some(digest(&eval_config_bytes)),
        },
    );
    let snapshots = AuthoritativeRunInputSnapshots {
        ticket: ticket_bytes.clone(),
        policy: policy_bytes,
        config: config_bytes,
        repository: repository_bytes,
        eval_config: eval_config_bytes,
        provider_ticket: ticket_bytes,
    };

    let initialized =
        InitializedLoopRun::create_isolated(config, &source, &snapshots).expect("initialize");
    assert_eq!(
        initialized
            .run()
            .candidate_workspace
            .as_ref()
            .unwrap()
            .lifecycle,
        seaf_core::CandidateWorkspaceLifecycle::Active
    );
    let run_dir = runs_root.join("isolated-init");
    assert!(run_dir.join("run.json").is_file());
    assert!(
        run_dir.join("provider-exchange.lock").is_file(),
        "isolated initialization must use the shared durable run-state publisher"
    );
    for absent in [
        "artifacts",
        "prompts",
        "responses",
        "context-manifest.json",
        "log.md",
    ] {
        assert!(
            !run_dir.join(absent).exists(),
            "{absent} must wait for Active"
        );
    }

    let scaffolded = initialized.scaffold().expect("scaffold active run");
    for present in [
        "artifacts",
        "prompts",
        "responses",
        "context-manifest.json",
        "log.md",
    ] {
        assert!(
            run_dir.join(present).exists(),
            "{present} must be scaffolded"
        );
    }
    let prepared = scaffolded
        .publish_authoritative_inputs(snapshots)
        .expect("publish exact input set");
    let mut step_runner = RecordingStepRunner::new();
    let runner =
        LoopRunner::start_initialized(prepared, &mut step_runner).expect("prepare provider");
    assert_eq!(
        runner.run().execution_mode,
        seaf_core::LoopExecutionMode::IsolatedCandidate
    );
    assert!(fs::read_to_string(run_dir.join("log.md"))
        .unwrap()
        .contains("started isolated provider run"));
    let candidate = runner
        .run()
        .candidate_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    drop(runner);
    git_ok(&source, &["worktree", "remove", "--force", &candidate]);
}

#[test]
fn isolated_initialization_requires_eval_authority_before_run_directory_creation() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    let error = InitializedLoopRun::create_isolated(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "missing-eval-authority",
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        temp.path(),
        &AuthoritativeRunInputSnapshots {
            ticket: Vec::new(),
            policy: Vec::new(),
            config: Vec::new(),
            repository: Vec::new(),
            eval_config: Vec::new(),
            provider_ticket: Vec::new(),
        },
    )
    .expect_err("isolated provider authority must include eval config");

    assert!(error.to_string().contains("eval config"), "{error}");
    assert!(!runs_root.join("missing-eval-authority").exists());
}

#[test]
fn isolated_initialization_screens_exact_run_and_scaffold_payloads_before_creating_its_leaf() {
    for (label, secret) in [
        ("status", "pending"),
        ("active", "active"),
        ("key", "run_id"),
        ("boundary", "\"status\": \"pending\""),
        ("scaffold-log", "# Loop run log\n"),
        ("scaffold-key", "max_bytes_per_file"),
        (
            "scaffold-boundary",
            "\"max_bytes_per_file\": 0,\n  \"max_total_bytes\"",
        ),
        ("scaffold-marker", UNTRUSTED_CONTEXT_MARKER),
    ] {
        let temp = tempfile::tempdir().expect("temp");
        let run_id = format!("prospective-run-{label}");
        let runs_root = temp.path().join("runs");
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        git_ok(&source, &["init", "-q"]);
        git_ok(&source, &["config", "user.email", "test@example.com"]);
        git_ok(&source, &["config", "user.name", "SEAF Test"]);
        fs::write(source.join("tracked.txt"), "source\n").unwrap();
        git_ok(&source, &["add", "tracked.txt"]);
        git_ok(&source, &["commit", "-qm", "initial"]);
        let eval_config: seaf_core::EvalConfig = serde_json::from_value(serde_json::json!({
            "evals": {
                "allow_commands": ["true"],
                "required": [{
                    "name": "tests",
                    "command": "true",
                    "env": {"API_TOKEN": secret}
                }]
            }
        }))
        .unwrap();
        let eval_config = seaf_core::canonical_json_bytes(&eval_config).unwrap();
        let ticket_bytes = seaf_core::canonical_json_bytes(&ticket()).unwrap();
        let policy =
            seaf_core::canonical_json_bytes(&serde_json::json!({"policy": label})).unwrap();
        let project_config =
            seaf_core::canonical_json_bytes(&serde_json::json!({"config": label})).unwrap();
        let repository = seaf_core::canonical_json_bytes(&serde_json::json!({
            "repository": source.canonicalize().unwrap(),
            "case": label
        }))
        .unwrap();
        let digest = |bytes: &[u8]| {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(bytes))
        };
        let snapshots = AuthoritativeRunInputSnapshots {
            ticket: ticket_bytes.clone(),
            provider_ticket: ticket_bytes.clone(),
            policy: policy.clone(),
            config: project_config.clone(),
            repository: repository.clone(),
            eval_config: eval_config.clone(),
        };
        let config = LoopRunnerConfig::for_ticket(
            &runs_root,
            &run_id,
            &ticket(),
            "fake-provider",
            "fake-model",
            LoopInputDigests {
                ticket: digest(&ticket_bytes),
                policy: digest(&policy),
                config: digest(&project_config),
                repository: digest(&repository),
                eval_config: Some(digest(&eval_config)),
            },
        );

        let result = InitializedLoopRun::create_isolated(config, &source, &snapshots);
        let error = match result {
            Ok(initialized) => {
                let candidate = initialized
                    .run()
                    .candidate_workspace
                    .as_ref()
                    .unwrap()
                    .path
                    .clone();
                git_ok(&source, &["worktree", "remove", "--force", &candidate]);
                drop(initialized);
                fs::remove_dir_all(runs_root.join(&run_id)).unwrap();
                panic!("{label} collision unexpectedly created an isolated run")
            }
            Err(error) => error,
        };

        assert!(error.to_string().contains("credential material"), "{error}");
        assert!(!error.to_string().contains(secret));
        assert!(!runs_root.join(&run_id).exists(), "{label}");
    }
}

#[test]
fn isolated_scaffold_rechecks_exact_existing_payloads_under_the_run_guard() {
    let temp = tempfile::tempdir().expect("temp");
    let secret = "historical-scaffold-secret-value";
    let (source, initialized, snapshots) =
        isolated_fixture_with_secret(temp.path(), "guarded-scaffold-recheck", secret);
    let candidate = initialized
        .run()
        .candidate_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    let run = initialized.run().clone();
    let run_directory = initialized.workspace().run_directory().to_path_buf();
    write_private_fixture_file(
        run_directory.join("log.md"),
        format!("# Loop run log\n{secret}").as_bytes(),
    );
    let before = read_tree_bytes(&run_directory);

    let error = initialized
        .scaffold()
        .expect_err("exact existing scaffold bytes must be screened under the guard");

    assert!(error.to_string().contains("credential material"), "{error}");
    assert!(!error.to_string().contains(secret));
    assert_eq!(read_tree_bytes(&run_directory), before);
    for relative in ["prompts", "responses", "artifacts", "context-manifest.json"] {
        assert!(!run_directory.join(relative).exists(), "{relative}");
    }

    fs::write(run_directory.join("log.md"), b"# Loop run log\n").unwrap();
    InitializedLoopRun::resume_isolated_with_inputs(&temp.path().join("runs"), run, &snapshots)
        .expect("clean authenticated retry")
        .scaffold()
        .expect("clean retry scaffold");
    git_ok(&source, &["worktree", "remove", "--force", &candidate]);
}

#[test]
fn active_resume_rederives_redactor_and_rejects_unsafe_historical_scaffold_without_mutation() {
    let temp = tempfile::tempdir().expect("temp");
    let secret = "persisted-scaffold-secret-value";
    let (source, initialized, snapshots) =
        isolated_fixture_with_secret(temp.path(), "historical-scaffold-resume", secret);
    let candidate = initialized
        .run()
        .candidate_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    let prepared = initialized
        .scaffold()
        .unwrap()
        .publish_authoritative_inputs(snapshots)
        .unwrap();
    let run = prepared.run().clone();
    let run_directory = prepared.workspace().run_directory().to_path_buf();
    fs::write(
        run_directory.join("log.md"),
        format!("# Loop run log\n{secret}"),
    )
    .unwrap();
    let before = read_tree_bytes(&run_directory);

    let error = InitializedLoopRun::resume_isolated(&temp.path().join("runs"), run.clone())
        .expect_err("historical scaffold material must block resume");

    assert!(error.to_string().contains("credential material"), "{error}");
    assert!(!error.to_string().contains(secret));
    assert_eq!(read_tree_bytes(&run_directory), before);

    fs::write(run_directory.join("log.md"), b"# Loop run log\n").unwrap();
    InitializedLoopRun::resume_isolated(&temp.path().join("runs"), run)
        .expect("clean historical retry must resume");
    git_ok(&source, &["worktree", "remove", "--force", &candidate]);
}

#[test]
fn isolated_provisioning_resume_screens_the_exact_active_run_before_candidate_creation() {
    let temp = tempfile::tempdir().expect("temp");
    let run_id = "prospective-active-resume";
    let runs_root = temp.path().join("runs");
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    git_ok(&source, &["init", "-q"]);
    git_ok(&source, &["config", "user.email", "test@example.com"]);
    git_ok(&source, &["config", "user.name", "SEAF Test"]);
    fs::write(source.join("tracked.txt"), "source\n").unwrap();
    git_ok(&source, &["add", "tracked.txt"]);
    git_ok(&source, &["commit", "-qm", "initial"]);
    let eval_config = seaf_core::parse_eval_config(
        "evals:\n  allow_commands: [true]\n  required:\n    - name: tests\n      command: true\n      env:\n        API_TOKEN: active\n",
    )
    .unwrap();
    let eval_config = seaf_core::canonical_json_bytes(&eval_config).unwrap();
    let ticket_bytes = seaf_core::canonical_json_bytes(&ticket()).unwrap();
    let policy = seaf_core::canonical_json_bytes(&serde_json::json!({"policy": run_id})).unwrap();
    let project_config =
        seaf_core::canonical_json_bytes(&serde_json::json!({"config": run_id})).unwrap();
    let repository = seaf_core::canonical_json_bytes(&serde_json::json!({
        "repository": source.canonicalize().unwrap(),
        "run": run_id
    }))
    .unwrap();
    let digest = |bytes: &[u8]| {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    };
    let snapshots = AuthoritativeRunInputSnapshots {
        ticket: ticket_bytes.clone(),
        provider_ticket: ticket_bytes.clone(),
        policy: policy.clone(),
        config: project_config.clone(),
        repository: repository.clone(),
        eval_config: eval_config.clone(),
    };
    let workspace = LoopWorkspace::create(&runs_root, run_id).unwrap();
    let mut provisioning = create_run(NewLoopRun {
        run_id: run_id.to_string(),
        ticket_id: ticket().ticket_id,
        goal_id: ticket().goal_id,
        provider: "fake-provider".to_string(),
        model: "fake-model".to_string(),
        input_digests: LoopInputDigests {
            ticket: digest(&ticket_bytes),
            policy: digest(&policy),
            config: digest(&project_config),
            repository: digest(&repository),
            eval_config: Some(digest(&eval_config)),
        },
    });
    provisioning.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
    provisioning.candidate_workspace = Some(
        seaf_loop::plan_candidate_workspace(
            workspace.run_directory(),
            &source,
            &provisioning.input_digests.repository,
        )
        .unwrap(),
    );
    let candidate_path = provisioning
        .candidate_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    let mut run_bytes = serde_json::to_vec_pretty(&provisioning).unwrap();
    run_bytes.push(b'\n');
    write_private_fixture_file(workspace.run_file(), &run_bytes);
    let run_directory = workspace.run_directory().to_path_buf();
    let before = read_tree_bytes(&run_directory);
    drop(workspace);

    let result =
        InitializedLoopRun::resume_isolated_with_inputs(&runs_root, provisioning, &snapshots);
    let error = match result {
        Ok(resumed) => {
            let created_candidate = resumed
                .run()
                .candidate_workspace
                .as_ref()
                .unwrap()
                .path
                .clone();
            git_ok(
                &source,
                &["worktree", "remove", "--force", &created_candidate],
            );
            drop(resumed);
            panic!("active collision unexpectedly resumed candidate provisioning")
        }
        Err(error) => error,
    };

    assert!(error.to_string().contains("credential material"), "{error}");
    assert!(!error.to_string().contains("API_TOKEN"));
    assert_eq!(read_tree_bytes(&run_directory), before);
    assert!(!Path::new(&candidate_path).exists());
}

#[test]
fn minimal_provisioning_resume_preflights_before_creating_candidate_locks() {
    for case in ["missing-snapshots", "colliding-snapshots", "clean"] {
        let temp = tempfile::tempdir().expect("temp");
        let runs_root = temp.path().join("runs");
        let run_id = format!("minimal-provisioning-{case}");
        let run_directory = runs_root.join(&run_id);
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        git_ok(&source, &["init", "-q"]);
        git_ok(&source, &["config", "user.email", "test@example.com"]);
        git_ok(&source, &["config", "user.name", "SEAF Test"]);
        fs::write(source.join("tracked.txt"), "source\n").unwrap();
        git_ok(&source, &["add", "tracked.txt"]);
        git_ok(&source, &["commit", "-qm", "initial"]);
        create_private_fixture_directory(&runs_root);
        create_private_fixture_directory(&run_directory);

        let secret = if case == "colliding-snapshots" {
            "\"lifecycle\": \"active\""
        } else {
            "never-present-in-run-authority"
        };
        let eval_config: seaf_core::EvalConfig = serde_json::from_value(serde_json::json!({
            "evals": {
                "allow_commands": ["true"],
                "required": [{
                    "name": "tests",
                    "command": "true",
                    "env": {"API_TOKEN": secret}
                }]
            }
        }))
        .unwrap();
        let ticket_bytes = seaf_core::canonical_json_bytes(&ticket()).unwrap();
        let policy =
            seaf_core::canonical_json_bytes(&serde_json::json!({"policy": run_id})).unwrap();
        let project_config =
            seaf_core::canonical_json_bytes(&serde_json::json!({"config": run_id})).unwrap();
        let repository = seaf_core::canonical_json_bytes(&serde_json::json!({
            "repository": source.canonicalize().unwrap(),
            "run": run_id
        }))
        .unwrap();
        let eval_config = seaf_core::canonical_json_bytes(&eval_config).unwrap();
        let digest = |bytes: &[u8]| {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(bytes))
        };
        let snapshots = AuthoritativeRunInputSnapshots {
            ticket: ticket_bytes.clone(),
            provider_ticket: ticket_bytes.clone(),
            policy: policy.clone(),
            config: project_config.clone(),
            repository: repository.clone(),
            eval_config: eval_config.clone(),
        };
        let mut provisioning = create_run(NewLoopRun {
            run_id: run_id.clone(),
            ticket_id: ticket().ticket_id,
            goal_id: ticket().goal_id,
            provider: "fake-provider".to_string(),
            model: "fake-model".to_string(),
            input_digests: LoopInputDigests {
                ticket: digest(&ticket_bytes),
                policy: digest(&policy),
                config: digest(&project_config),
                repository: digest(&repository),
                eval_config: Some(digest(&eval_config)),
            },
        });
        provisioning.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
        provisioning.candidate_workspace = Some(
            seaf_loop::plan_candidate_workspace(
                &run_directory,
                &source,
                &provisioning.input_digests.repository,
            )
            .unwrap(),
        );
        assert!(seaf_core::validate_loop_run(&provisioning).is_empty());
        let candidate_path = provisioning
            .candidate_workspace
            .as_ref()
            .unwrap()
            .path
            .clone();
        let mut run_bytes = serde_json::to_vec_pretty(&provisioning).unwrap();
        run_bytes.push(b'\n');
        write_private_fixture_file(run_directory.join("run.json"), &run_bytes);
        let before = read_tree_bytes(&run_directory);

        if case == "clean" {
            let resumed = InitializedLoopRun::resume_isolated_with_inputs(
                &runs_root,
                provisioning,
                &snapshots,
            )
            .expect("clean authenticated Provisioning resume");
            assert_eq!(
                resumed
                    .run()
                    .candidate_workspace
                    .as_ref()
                    .unwrap()
                    .lifecycle,
                seaf_core::CandidateWorkspaceLifecycle::Active
            );
            assert!(run_directory.join(".candidate-workspace.lock").is_file());
            git_ok(&source, &["worktree", "remove", "--force", &candidate_path]);
        } else {
            let error = if case == "missing-snapshots" {
                InitializedLoopRun::resume_isolated(&runs_root, provisioning).unwrap_err()
            } else {
                InitializedLoopRun::resume_isolated_with_inputs(
                    &runs_root,
                    provisioning,
                    &snapshots,
                )
                .unwrap_err()
            };
            assert!(
                error.to_string().contains(if case == "missing-snapshots" {
                    "requires authoritative input snapshots"
                } else {
                    "credential material"
                }),
                "{case}: {error}"
            );
            assert!(!error.to_string().contains(secret), "{case}: {error}");
            assert_eq!(read_tree_bytes(&run_directory), before, "{case}");
            assert!(!run_directory.join(".candidate-workspace.lock").exists());
            assert!(!Path::new(&candidate_path).exists());
        }
    }
}

#[test]
fn public_state_writer_cannot_bypass_isolated_provisioning_preflight() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    git_ok(&source, &["init", "-q"]);
    git_ok(&source, &["config", "user.email", "test@example.com"]);
    git_ok(&source, &["config", "user.name", "SEAF Test"]);
    fs::write(source.join("tracked.txt"), "source\n").unwrap();
    git_ok(&source, &["add", "tracked.txt"]);
    git_ok(&source, &["commit", "-qm", "initial"]);
    let workspace = LoopWorkspace::create(&runs_root, "isolated-writer-bypass").unwrap();
    let mut run = create_run(NewLoopRun {
        run_id: "isolated-writer-bypass".to_string(),
        ticket_id: "P2-005".to_string(),
        goal_id: "phase-2".to_string(),
        provider: "fake-provider".to_string(),
        model: "fake-model".to_string(),
        input_digests: LoopInputDigests {
            ticket: "a".repeat(64),
            policy: "b".repeat(64),
            config: "c".repeat(64),
            repository: "d".repeat(64),
            eval_config: Some("e".repeat(64)),
        },
    });
    run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
    run.candidate_workspace = Some(
        seaf_loop::plan_candidate_workspace(
            workspace.run_directory(),
            &source,
            &run.input_digests.repository,
        )
        .unwrap(),
    );
    let before = read_tree_bytes(workspace.run_directory());

    let error = seaf_loop::state::save_run(&workspace, &run)
        .expect_err("the public writer must not mint isolated provisioning authority");

    assert!(error.to_string().contains("public state writer"), "{error}");
    assert_eq!(read_tree_bytes(workspace.run_directory()), before);
    assert!(!workspace.run_file().exists());
    assert!(!Path::new(&run.candidate_workspace.unwrap().path).exists());
}

#[test]
fn stale_initialized_token_rejects_before_any_input_snapshot_publication() {
    let temp = tempfile::tempdir().expect("temp");
    let (source, initialized, snapshots) = isolated_fixture(temp.path(), "stale-input-token");
    let candidate = initialized
        .run()
        .candidate_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    let scaffolded = initialized.scaffold().unwrap();
    let run_path = scaffolded.workspace().run_file();
    let mut run: serde_json::Value = serde_json::from_slice(&fs::read(&run_path).unwrap()).unwrap();
    run["updated_at"] = serde_json::json!("tampered-after-scaffold");
    fs::write(&run_path, serde_json::to_vec_pretty(&run).unwrap()).unwrap();

    let error = scaffolded
        .publish_authoritative_inputs(snapshots)
        .expect_err("stale token must fail before snapshot publication");
    assert!(error.to_string().contains("changed"), "{error}");
    let run_dir = run_path.parent().unwrap();
    assert!(!run_dir.join("inputs").exists());
    assert!(!run_dir.join("ticket.snapshot.json").exists());
    git_ok(&source, &["worktree", "remove", "--force", &candidate]);
}

#[test]
fn scaffold_recovers_an_exact_prefix_but_rejects_a_collision_before_new_entries() {
    let temp = tempfile::tempdir().expect("temp");
    let (source, initialized, _snapshots) = isolated_fixture(temp.path(), "scaffold-prefix");
    let candidate = initialized
        .run()
        .candidate_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    let run_dir = initialized.workspace().run_directory().to_path_buf();
    create_private_fixture_directory(&run_dir.join("prompts"));
    write_private_fixture_file(run_dir.join("log.md"), b"# Loop run log\n");
    initialized.scaffold().expect("exact prefix is retryable");
    assert!(run_dir.join("responses").is_dir());
    assert!(run_dir.join("artifacts").is_dir());
    assert!(run_dir.join("context-manifest.json").is_file());
    git_ok(&source, &["worktree", "remove", "--force", &candidate]);

    let (source, initialized, _snapshots) = isolated_fixture(temp.path(), "scaffold-collision");
    let candidate = initialized
        .run()
        .candidate_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    let run_dir = initialized.workspace().run_directory().to_path_buf();
    write_private_fixture_file(run_dir.join("log.md"), b"partial");
    let error = initialized
        .scaffold()
        .expect_err("partial final must collide");
    assert!(error.to_string().contains("canonical header"), "{error}");
    assert!(!run_dir.join("prompts").exists());
    assert!(!run_dir.join("responses").exists());
    assert!(!run_dir.join("artifacts").exists());
    assert!(!run_dir.join("context-manifest.json").exists());
    git_ok(&source, &["worktree", "remove", "--force", &candidate]);
}

#[test]
fn input_snapshots_recover_exact_prefix_and_preflight_collision_before_new_publication() {
    let temp = tempfile::tempdir().expect("temp");
    let (source, initialized, snapshots) = isolated_fixture(temp.path(), "snapshot-prefix");
    let candidate = initialized
        .run()
        .candidate_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    let scaffolded = initialized.scaffold().unwrap();
    let run_dir = scaffolded.workspace().run_directory().to_path_buf();
    create_private_fixture_directory(&run_dir.join("inputs"));
    write_private_fixture_file(run_dir.join("inputs/ticket.json"), &snapshots.ticket);
    scaffolded
        .publish_authoritative_inputs(snapshots)
        .expect("exact snapshot prefix is retryable");
    for relative in [
        "inputs/policy.json",
        "inputs/config.json",
        "inputs/repository.json",
        "inputs/eval-config.json",
        "ticket.snapshot.json",
    ] {
        assert!(run_dir.join(relative).is_file(), "missing {relative}");
    }
    git_ok(&source, &["worktree", "remove", "--force", &candidate]);

    let (source, initialized, snapshots) = isolated_fixture(temp.path(), "snapshot-collision");
    let candidate = initialized
        .run()
        .candidate_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    let scaffolded = initialized.scaffold().unwrap();
    let run_dir = scaffolded.workspace().run_directory().to_path_buf();
    fs::create_dir(run_dir.join("inputs")).unwrap();
    fs::write(run_dir.join("inputs/eval-config.json"), b"partial").unwrap();
    let error = scaffolded
        .publish_authoritative_inputs(snapshots)
        .expect_err("collision must fail before publishing missing snapshots");
    assert!(error.to_string().contains("collision"), "{error}");
    for relative in [
        "inputs/ticket.json",
        "inputs/policy.json",
        "inputs/config.json",
        "inputs/repository.json",
        "ticket.snapshot.json",
    ] {
        assert!(!run_dir.join(relative).exists(), "unexpected {relative}");
    }
    git_ok(&source, &["worktree", "remove", "--force", &candidate]);

    let (source, initialized, snapshots) = isolated_fixture(temp.path(), "snapshot-hole");
    let candidate = initialized
        .run()
        .candidate_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    let scaffolded = initialized.scaffold().unwrap();
    let run_dir = scaffolded.workspace().run_directory().to_path_buf();
    fs::create_dir(run_dir.join("inputs")).unwrap();
    fs::write(run_dir.join("inputs/config.json"), &snapshots.config).unwrap();
    let before = read_tree_bytes(&run_dir);
    let error = scaffolded
        .publish_authoritative_inputs(snapshots)
        .expect_err("a matching snapshot after a missing entry is not an exact prefix");
    assert!(error.to_string().contains("exact prefix"), "{error}");
    assert_eq!(read_tree_bytes(&run_dir), before);
    git_ok(&source, &["worktree", "remove", "--force", &candidate]);
}

#[test]
fn authoritative_eval_snapshot_requires_the_shared_typed_contract() {
    let temp = tempfile::tempdir().expect("temp");
    for (run_id, value) in [
        ("typed-eval-unknown", serde_json::json!({"forged": true})),
        (
            "typed-eval-empty-required",
            serde_json::json!({"evals": {"allow_commands": [], "required": []}}),
        ),
    ] {
        let forged = seaf_core::canonical_json_bytes(&value).unwrap();
        let error = isolated_fixture_with_eval_bytes(temp.path(), run_id, forged)
            .expect_err("generic canonical JSON cannot forge typed eval authority");

        assert!(error.to_string().contains("eval config"), "{error}");
        assert!(!temp.path().join("runs").join(run_id).exists());
    }
}

#[test]
fn state_save_with_stale_empty_exchange_vector_cannot_erase_first_concurrent_request() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    let ticket = ticket();
    let mut step_runner = FirstConcurrentExchangeRunner { workspace: None };
    let mut runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "first-exchange-race",
            &ticket,
            "fake",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start");

    let error = runner
        .run_next_step()
        .expect_err("stale empty state must not erase the first exchange");

    assert!(error.to_string().contains("changed before ordinary"));
    let workspace = LoopWorkspace::open(&runs_root, "first-exchange-race").expect("workspace");
    let persisted = seaf_loop::state::load_run(&workspace).expect("persisted run");
    assert_eq!(persisted.provider_exchange_records.len(), 1);
    assert_eq!(
        persisted.provider_exchange_records[0].phase,
        ProviderExchangePhase::Request
    );
    assert_eq!(persisted.status, LoopStatus::Running);
}

struct FirstConcurrentExchangeRunner {
    workspace: Option<LoopWorkspace>,
}

impl StepRunner for FirstConcurrentExchangeRunner {
    fn prepare_run(
        &mut self,
        workspace: &LoopWorkspace,
        _run: &LoopRun,
    ) -> Result<(), seaf_loop::RunnerError> {
        self.workspace = Some(workspace.clone());
        Ok(())
    }

    fn step_request(&mut self, _step: LoopStepName) -> Result<String, seaf_loop::RunnerError> {
        Ok("ordinary step request".to_string())
    }

    fn run_step(
        &mut self,
        step: LoopStepName,
        _request: &str,
    ) -> Result<StepOutput, seaf_loop::RunnerError> {
        let workspace = self.workspace.as_ref().expect("prepared workspace");
        let coordinates = ProviderExchangeCoordinates {
            run_id: "first-exchange-race".to_string(),
            step,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
        };
        let request = write_provider_exchange_request(
            workspace.run_directory(),
            &coordinates,
            b"concurrent request",
        )
        .expect("concurrent request");
        let record = ProviderExchangeRecord {
            schema_version: 1,
            run_id: coordinates.run_id,
            step: coordinates.step,
            role: coordinates.role,
            step_attempt: coordinates.step_attempt,
            exchange_index: coordinates.exchange_index,
            kind: coordinates.kind,
            context_round: coordinates.context_round,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: None,
            request,
            response: None,
            expansion: None,
            outcome: None,
        };
        let reference = stage_provider_exchange_record(workspace.run_directory(), &record)
            .expect("stage concurrent request");
        persist_provider_exchange_record_reference(workspace, reference)
            .expect("append concurrent request");
        Ok(StepOutput::completed("ordinary step response"))
    }
}

#[test]
fn state_finish_step_rejects_unpaired_or_malformed_artifact_integrity() {
    let base = NewLoopRun {
        run_id: "artifact-integrity".to_string(),
        ticket_id: "P2-005".to_string(),
        goal_id: "phase-2".to_string(),
        provider: "fake-provider".to_string(),
        model: "fake-model".to_string(),
        input_digests: test_input_digests(),
    };
    let mut unpaired = create_run(base.clone());
    let error = finish_step(
        &mut unpaired,
        LoopStepName::Research,
        LoopStepStatus::Completed,
        Some("artifacts/01-research.json".to_string()),
        None,
    )
    .expect_err("artifact path without digest must fail");
    assert!(error.to_string().contains("artifact path and digest"));

    let mut malformed = create_run(base);
    let error = finish_step(
        &mut malformed,
        LoopStepName::Research,
        LoopStepStatus::Completed,
        Some("artifacts/01-research.json".to_string()),
        Some("not-a-digest".to_string()),
    )
    .expect_err("malformed artifact digest must fail");
    assert!(error.to_string().contains("artifact digest"));
}

#[test]
fn state_finish_output_review_updates_barrier_fields_without_touching_downstream_steps() {
    let mut run = create_run(NewLoopRun {
        run_id: "atomic-human-review".to_string(),
        ticket_id: "T-LOCAL-001".to_string(),
        goal_id: "local_agent_loop_mvp".to_string(),
        provider: "fake".to_string(),
        model: "fake-model".to_string(),
        input_digests: test_input_digests(),
    });
    run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
    for record in &mut run.steps {
        if matches!(
            record.name,
            LoopStepName::Research
                | LoopStepName::Analysis
                | LoopStepName::SpecCreation
                | LoopStepName::SpecReview
                | LoopStepName::Development
        ) {
            record.status = LoopStepStatus::Completed;
        }
    }
    run.steps
        .iter_mut()
        .find(|record| record.name == LoopStepName::OutputReview)
        .unwrap()
        .status = LoopStepStatus::Running;
    run.current_step = LoopStepName::OutputReview;
    run.status = LoopStatus::Running;

    finish_step(
        &mut run,
        LoopStepName::OutputReview,
        LoopStepStatus::Passed,
        Some("artifacts/06-output-review.md".to_string()),
        Some("6".repeat(64)),
    )
    .expect("finish OutputReview");

    assert_eq!(run.status, LoopStatus::AwaitingHumanReview);
    assert_eq!(run.current_step, LoopStepName::Testing);
    let testing = run
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::Testing)
        .unwrap();
    let eval = run
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::EvalReport)
        .unwrap();
    assert_eq!(testing.status, LoopStepStatus::Pending);
    assert_eq!(eval.status, LoopStepStatus::Pending);
    assert!(testing.artifact_path.is_none() && testing.artifact_digest.is_none());
    assert!(eval.artifact_path.is_none() && eval.artifact_digest.is_none());
}

#[test]
fn state_creation_preserves_exact_effective_input_digests() {
    let input_digests = LoopInputDigests {
        ticket: "a".repeat(64),
        policy: "b".repeat(64),
        config: "c".repeat(64),
        repository: "d".repeat(64),
        eval_config: None,
    };

    let run = create_run(NewLoopRun {
        run_id: "run-with-input-digests".to_string(),
        ticket_id: "T-LOCAL-001".to_string(),
        goal_id: "local_agent_loop_mvp".to_string(),
        provider: "fake-provider".to_string(),
        model: "fake-model".to_string(),
        input_digests: input_digests.clone(),
    });

    assert_eq!(run.input_digests, input_digests);
}

#[test]
fn state_resume_skips_completed_steps_after_interruption() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut first_runner = RecordingStepRunner::with_prefix("initial");
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-resume",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut first_runner,
    )
    .expect("start run");

    run.run_next_step().expect("run first step");
    drop(run);

    assert_eq!(first_runner.calls, vec![LoopStepName::Research]);

    let mut resumed_runner = RecordingStepRunner::with_prefix("resumed");
    let mut resumed =
        LoopRunner::resume(&runs_root, "run-resume", &mut resumed_runner).expect("resume run");

    resumed.run_to_completion().expect("complete resumed run");
    let final_status = resumed.run().status;
    drop(resumed);

    assert!(
        !resumed_runner.calls.contains(&LoopStepName::Research),
        "completed steps should not be rerun on resume"
    );
    assert_eq!(final_status, seaf_core::LoopStatus::Completed);
}

#[test]
fn state_start_rejects_duplicate_run_id_without_touching_audit_files() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut first_runner = RecordingStepRunner::with_prefix("first");
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-duplicate",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut first_runner,
    )
    .expect("start run");
    run.run_next_step().expect("run first step");
    drop(run);

    let run_dir = runs_root.join("run-duplicate");
    let original_run_json = std::fs::read_to_string(run_dir.join("run.json")).expect("run json");
    let original_prompt =
        std::fs::read_to_string(run_dir.join("prompts/01-research.prompt.md")).expect("prompt");

    let mut duplicate_runner = RecordingStepRunner::with_prefix("duplicate");
    let error = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-duplicate",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut duplicate_runner,
    )
    .expect_err("duplicate run id should fail");

    assert!(error.to_string().contains("already exists"));
    assert_eq!(
        std::fs::read_to_string(run_dir.join("run.json")).expect("run json"),
        original_run_json
    );
    assert_eq!(
        std::fs::read_to_string(run_dir.join("prompts/01-research.prompt.md")).expect("prompt"),
        original_prompt
    );
    assert!(
        !run_dir
            .join("prompts/01-research.attempt-002.prompt.md")
            .exists(),
        "duplicate start must not create a second prompt attempt"
    );
    assert!(duplicate_runner.calls.is_empty());
}

#[test]
fn legacy_start_cleans_a_fresh_workspace_when_initial_state_publication_fails() {
    let temp = tempfile::tempdir().unwrap();
    let runs_root = temp.path().join("runs");
    let run_id = "initial-publication-failure";
    let mut step_runner = InitialStateCollisionRunner;

    let error = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            run_id,
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect_err("invalid fresh run.json target must fail initial state publication");

    assert!(
        error.to_string().contains("run-state publication")
            && error.to_string().contains("run.json"),
        "{error}"
    );
    assert!(
        !runs_root.join(run_id).exists(),
        "a failed fresh start must not strand its scaffolded workspace"
    );
}

#[test]
fn terminal_resume_reauthenticates_the_stable_run_lock_before_returning() {
    let temp = tempfile::tempdir().unwrap();
    let runs_root = temp.path().join("runs");
    let run_id = "terminal-resume-lock";
    let mut initial = RecordingStepRunner::new();
    let runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            run_id,
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut initial,
    )
    .unwrap();
    drop(runner);
    let run_dir = runs_root.join(run_id);
    let mut terminal = read_run(&run_dir);
    terminal.status = LoopStatus::Blocked;
    terminal.current_step = LoopStepName::Research;
    terminal.steps[0].status = LoopStepStatus::Blocked;
    write_run(&run_dir, &terminal);
    let lock = run_dir.join("provider-exchange.lock");
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    let before = read_tree_bytes(&run_dir);
    let mut resumed_runner = RecordingStepRunner::new();

    let error = LoopRunner::resume(&runs_root, run_id, &mut resumed_runner)
        .expect_err("terminal resume must resync through the stable lock");

    assert!(error.to_string().contains("regular file"), "{error}");
    assert_eq!(read_tree_bytes(&run_dir), before);
    assert!(resumed_runner.calls.is_empty());
}

#[test]
fn state_passed_run_status_is_terminal_even_with_runnable_steps() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut setup_runner = RecordingStepRunner::new();
    let run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-passed-terminal",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut setup_runner,
    )
    .expect("start run");
    drop(run);

    let run_dir = runs_root.join("run-passed-terminal");
    let mut persisted = read_run(&run_dir);
    persisted.status = LoopStatus::Passed;
    persisted.current_step = LoopStepName::Research;
    persisted.steps[0].status = LoopStepStatus::Running;
    persisted.steps[1].status = LoopStepStatus::Pending;
    write_run(&run_dir, &persisted);

    let mut step_runner = RecordingStepRunner::with_prefix("resume");
    let mut resumed =
        LoopRunner::resume(&runs_root, "run-passed-terminal", &mut step_runner).expect("resume");

    let did_run = resumed.run_next_step().expect("run next step");
    drop(resumed);

    assert!(!did_run);
    assert!(step_runner.calls.is_empty());
    assert!(
        !run_dir.join("prompts/01-research.prompt.md").exists(),
        "terminal passed runs should not execute runnable steps"
    );
}

#[test]
fn state_resume_reruns_persisted_running_step() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut setup_runner = RecordingStepRunner::new();
    let run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-running-resume",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut setup_runner,
    )
    .expect("start run");
    drop(run);

    let run_dir = runs_root.join("run-running-resume");
    let mut persisted = read_run(&run_dir);
    persisted.status = LoopStatus::Running;
    persisted.current_step = LoopStepName::Research;
    persisted.steps[0].status = LoopStepStatus::Running;
    write_run(&run_dir, &persisted);

    let mut step_runner = RecordingStepRunner::with_prefix("resumed-running");
    let mut resumed =
        LoopRunner::resume(&runs_root, "run-running-resume", &mut step_runner).expect("resume");

    resumed.run_next_step().expect("run running step");
    drop(resumed);

    assert_eq!(step_runner.calls, vec![LoopStepName::Research]);
    assert_file_contains(
        &run_dir.join("prompts/01-research.prompt.md"),
        "resumed-running request for research",
    );
}

#[test]
fn state_blocked_step_updates_run_json_and_stops_execution() {
    assert_terminal_step_output_updates_run_and_stops(
        "run-blocked-terminal",
        LoopStepStatus::Blocked,
        LoopStatus::Blocked,
    );
}

#[test]
fn state_failed_step_updates_run_json_and_stops_execution() {
    assert_terminal_step_output_updates_run_and_stops(
        "run-failed-terminal",
        LoopStepStatus::Failed,
        LoopStatus::Failed,
    );
}

#[test]
fn state_writes_run_workspace_prompt_response_artifact_and_log() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut step_runner = RecordingStepRunner::with_prefix("first");
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-artifacts",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start run");

    run.run_next_step().expect("run first step");

    let run_dir = runs_root.join("run-artifacts");
    assert!(run_dir.join("run.json").is_file());
    let manifest_json =
        std::fs::read_to_string(run_dir.join("context-manifest.json")).expect("manifest");
    let manifest: ContextManifest =
        serde_json::from_str(&manifest_json).expect("empty context manifest");
    assert_eq!(manifest.untrusted_context_marker, UNTRUSTED_CONTEXT_MARKER);
    assert_eq!(manifest.total_context_bytes, 0);
    assert!(manifest.files.is_empty());
    assert!(run_dir.join("prompts/01-research.prompt.md").is_file());
    assert!(run_dir.join("responses/01-research.raw.txt").is_file());
    assert!(run_dir.join("artifacts/01-research.md").is_file());
    assert!(run_dir.join("log.md").is_file());
    assert_file_contains(
        &run_dir.join("prompts/01-research.prompt.md"),
        "first request for research",
    );
    assert_file_contains(
        &run_dir.join("responses/01-research.raw.txt"),
        "first response for research",
    );

    let run_json = std::fs::read_to_string(run_dir.join("run.json")).expect("run json");
    let persisted: seaf_core::LoopRun = serde_json::from_str(&run_json).expect("run json");
    assert_eq!(persisted.status, seaf_core::LoopStatus::Running);
    assert_eq!(persisted.current_step, LoopStepName::Analysis);
    let research = persisted
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::Research)
        .expect("research step");
    assert_eq!(research.status, LoopStepStatus::Completed);
    assert_eq!(
        research.artifact_path.as_deref(),
        Some("artifacts/01-research.md")
    );
}

#[test]
fn state_persists_request_before_failed_step_execution() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut step_runner =
        RecordingStepRunner::with_prefix("failing").failing_on(LoopStepName::Research);
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-failed-step",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start run");

    let error = run.run_next_step().expect_err("step should fail");
    assert!(error.to_string().contains("forced failure"));

    let run_dir = runs_root.join("run-failed-step");
    assert_file_contains(
        &run_dir.join("prompts/01-research.prompt.md"),
        "failing request for research",
    );
    assert!(
        !run_dir.join("responses/01-research.raw.txt").exists(),
        "response should not be written when execution fails before producing one"
    );
}

#[test]
fn state_persists_response_before_rejecting_invalid_step_status() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut step_runner =
        RecordingStepRunner::with_prefix("invalid").non_terminal_on(LoopStepName::Research);
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-invalid-step",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start run");

    let error = run
        .run_next_step()
        .expect_err("non-terminal status should fail");
    assert!(error.to_string().contains("terminal"));

    let run_dir = runs_root.join("run-invalid-step");
    assert_file_contains(
        &run_dir.join("prompts/01-research.prompt.md"),
        "invalid request for research",
    );
    assert_file_contains(
        &run_dir.join("responses/01-research.raw.txt"),
        "invalid response for research",
    );
}

#[test]
fn state_artifact_extensions_fall_back_to_bin_when_invalid() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut step_runner = RecordingStepRunner::with_prefix("unsafe-extension")
        .with_artifact(ArtifactContent::new("../bad path", b"unsafe extension"));
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-artifact-extension",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start run");

    run.run_next_step().expect("run first step");

    let run_dir = runs_root.join("run-artifact-extension");
    assert!(run_dir.join("artifacts/01-research.bin").is_file());
    let persisted = read_run(&run_dir);
    let research = persisted
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::Research)
        .expect("research step");
    assert_eq!(
        research.artifact_path.as_deref(),
        Some("artifacts/01-research.bin")
    );
}

#[test]
fn state_second_attempt_preserves_first_artifact_and_selects_new_exact_extension() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "run-attempt-safe-artifacts";
    let mut first_runner = RecordingStepRunner::with_prefix("first")
        .with_artifact(ArtifactContent::new("json", b"first artifact"));
    let mut runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            run_id,
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut first_runner,
    )
    .expect("start run");
    runner.run_next_step().expect("finish first attempt");
    drop(runner);

    let run_dir = runs_root.join(run_id);
    let first_path = run_dir.join("artifacts/01-research.json");
    let first_bytes = std::fs::read(&first_path).expect("first artifact");
    let mut persisted = read_run(&run_dir);
    seaf_loop::state::reset_from_step(&mut persisted, LoopStepName::Research)
        .expect("reset test fixture");
    write_run(&run_dir, &persisted);

    let mut second_runner = RecordingStepRunner::with_prefix("second")
        .with_artifact(ArtifactContent::new("yaml", b"second artifact"));
    let mut resumed = LoopRunner::resume(&runs_root, run_id, &mut second_runner).expect("resume");
    resumed.run_next_step().expect("finish second attempt");
    drop(resumed);

    assert_eq!(
        std::fs::read(first_path).expect("preserved first"),
        first_bytes
    );
    assert_eq!(
        std::fs::read(run_dir.join("artifacts/01-research.attempt-002.yaml"))
            .expect("second artifact"),
        b"second artifact"
    );
    let current = read_run(&run_dir);
    let research = current
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::Research)
        .expect("research step");
    assert_eq!(
        research.artifact_path.as_deref(),
        Some("artifacts/01-research.attempt-002.yaml")
    );
    let second_digest = ArtifactContent::new("yaml", b"second artifact").digest();
    assert_eq!(
        research.artifact_digest.as_deref(),
        Some(second_digest.as_str())
    );
}

#[test]
fn public_legacy_rerun_api_is_retired_without_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "pre-reset-fixed-ambiguity";
    let mut first_runner = RecordingStepRunner::with_prefix("first");
    let mut runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            run_id,
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut first_runner,
    )
    .unwrap();
    runner.run_next_step().unwrap();
    drop(runner);
    let run_dir = runs_root.join(run_id);
    write_private_fixture_file(
        run_dir.join("prompts/01-research.attempt-002.prompt.md"),
        b"historical second attempt",
    );
    let mut historical = read_run(&run_dir);
    historical.status = LoopStatus::Blocked;
    historical.current_step = LoopStepName::Research;
    historical.steps[0].status = LoopStepStatus::Blocked;
    write_run(&run_dir, &historical);
    let mut resumed_runner = RecordingStepRunner::with_prefix("must-not-run");
    let resumed = LoopRunner::resume(&runs_root, run_id, &mut resumed_runner).unwrap();
    let before = read_tree_bytes(&run_dir);

    let error = resumed
        .rerun_from(LoopStepName::Research)
        .expect_err("public legacy rerun must return migration guidance");

    assert!(
        error.to_string().contains("legacy rerun is retired"),
        "{error}"
    );
    assert_eq!(read_tree_bytes(&run_dir), before);
    assert!(resumed_runner.calls.is_empty());
}

#[test]
fn state_resume_missing_run_directory_reports_clear_error() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let mut step_runner = RecordingStepRunner::new();

    let error =
        LoopRunner::resume(&runs_root, "missing-run", &mut step_runner).expect_err("missing run");

    assert!(
        error.to_string().contains("run directory does not exist"),
        "error should identify the missing run directory, got: {error}"
    );
}

#[test]
fn state_resume_verified_rejects_when_disk_run_changes_before_resync() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "resume-verified-state";
    let mut initial_runner = RecordingStepRunner::with_prefix("initial");
    let mut runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            run_id,
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut initial_runner,
    )
    .expect("start run");
    runner.run_next_step().expect("finish research");
    drop(runner);

    let run_dir = runs_root.join(run_id);
    let verified = read_run(&run_dir);
    let mut replacement = verified.clone();
    replacement.ticket_id = "replacement-ticket".to_string();
    replacement.steps[0].status = LoopStepStatus::Pending;
    write_run(&run_dir, &replacement);
    let before = read_tree_bytes(&run_dir);

    let mut resumed_runner = RecordingStepRunner::with_prefix("resumed");
    let error = LoopRunner::resume_verified(&runs_root, verified, &mut resumed_runner)
        .expect_err("verified token must not bypass changed durable run authority");

    assert!(error.to_string().contains("exact run resync"), "{error}");
    assert_eq!(read_tree_bytes(&run_dir), before);
    assert!(resumed_runner.calls.is_empty());
}

#[cfg(unix)]
#[test]
fn state_resume_rejects_symlinked_run_directory_without_touching_external_target() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let external_runs = temp_dir.path().join("external-runs");
    let run_id = "symlinked-run-directory";
    create_test_run(&external_runs, run_id);
    fs::create_dir(&runs_root).expect("runs root");
    symlink(external_runs.join(run_id), runs_root.join(run_id)).expect("run directory symlink");
    let external_run = external_runs.join(run_id);
    let before = read_tree_bytes(&external_run);
    let mut step_runner = RecordingStepRunner::new();

    let result = LoopRunner::resume(&runs_root, run_id, &mut step_runner);

    let error = result.expect_err("symlinked run directory must fail closed");
    assert!(error.to_string().contains("symlink"), "{error}");
    assert_eq!(read_tree_bytes(&external_run), before);
    assert!(step_runner.calls.is_empty());
}

#[cfg(unix)]
#[test]
fn state_resume_rejects_symlinked_run_file_without_touching_external_target() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let runs_root = temp.path().join("runs");
    let run_id = "symlinked-run-file";
    let mut initial = RecordingStepRunner::new();
    let runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            run_id,
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut initial,
    )
    .unwrap();
    drop(runner);
    let run_path = runs_root.join(run_id).join("run.json");
    let outside = temp.path().join("outside-run.json");
    let outside_bytes = fs::read(&run_path).unwrap();
    fs::write(&outside, &outside_bytes).unwrap();
    fs::remove_file(&run_path).unwrap();
    symlink(&outside, &run_path).unwrap();
    let mut resumed = RecordingStepRunner::new();

    let error = LoopRunner::resume(&runs_root, run_id, &mut resumed)
        .expect_err("run.json symlink must fail closed");

    assert!(error.to_string().contains("regular file"), "{error}");
    assert_eq!(fs::read(outside).unwrap(), outside_bytes);
    assert!(resumed.calls.is_empty());
}

#[cfg(unix)]
#[test]
fn state_resume_rejects_symlinked_workspace_files_without_touching_external_targets() {
    for relative_path in ["run.json", "log.md", "context-manifest.json"] {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let runs_root = temp_dir.path().join("runs");
        let run_id = "symlinked-workspace-file";
        create_test_run(&runs_root, run_id);
        let run_dir = runs_root.join(run_id);
        let local_path = run_dir.join(relative_path);
        let external_path = temp_dir.path().join(format!("external-{}", relative_path));
        let original = fs::read(&local_path).expect("workspace file");
        fs::write(&external_path, &original).expect("external file");
        fs::remove_file(&local_path).expect("remove workspace file");
        symlink(&external_path, &local_path).expect("workspace file symlink");
        let mut step_runner = RecordingStepRunner::new();

        let result = LoopRunner::resume(&runs_root, run_id, &mut step_runner);

        let error = result.expect_err("symlinked workspace file must fail closed");
        assert!(error.to_string().contains("symlink"), "{error}");
        assert_eq!(fs::read(&external_path).expect("external bytes"), original);
        assert!(step_runner.calls.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn state_resume_rejects_symlinked_layout_directories_without_touching_external_targets() {
    for directory in ["prompts", "responses", "artifacts"] {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let runs_root = temp_dir.path().join("runs");
        let run_id = "symlinked-layout-directory";
        create_test_run(&runs_root, run_id);
        let run_dir = runs_root.join(run_id);
        let external_dir = temp_dir.path().join(format!("external-{directory}"));
        fs::create_dir(&external_dir).expect("external layout dir");
        fs::write(external_dir.join("sentinel"), b"unchanged").expect("external sentinel");
        fs::remove_dir(run_dir.join(directory)).expect("remove empty layout dir");
        symlink(&external_dir, run_dir.join(directory)).expect("layout directory symlink");
        let mut step_runner = RecordingStepRunner::new();

        let result = LoopRunner::resume(&runs_root, run_id, &mut step_runner);

        let error = result.expect_err("symlinked layout directory must fail closed");
        assert!(error.to_string().contains("symlink"), "{error}");
        assert_eq!(
            fs::read(external_dir.join("sentinel")).expect("external sentinel"),
            b"unchanged"
        );
        assert!(step_runner.calls.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn state_resume_verified_rejects_symlinked_prompt_child_before_prepare() {
    assert_resume_verified_child_symlink_is_rejected("prompts/01-research.prompt.md");
}

#[cfg(unix)]
#[test]
fn state_resume_verified_rejects_symlinked_response_child_before_prepare() {
    assert_resume_verified_child_symlink_is_rejected("responses/01-research.raw.txt");
}

#[cfg(unix)]
#[test]
fn state_resume_verified_rejects_symlinked_artifact_child_before_prepare() {
    assert_resume_verified_child_symlink_is_rejected("artifacts/01-research.md");
}

#[test]
fn state_step_rejects_exhausted_prompt_attempts_before_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "exhausted-prompt-attempts";
    let mut step_runner = RecordingStepRunner::with_prefix("must-not-run");
    let mut runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            run_id,
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start run");
    let run_dir = runs_root.join(run_id);
    write_private_fixture_file(
        run_dir.join("prompts/01-research.attempt-4294967295.prompt.md"),
        b"highest possible prompt attempt",
    );
    let before = read_tree_bytes(&run_dir);

    let error = runner
        .run_next_step()
        .expect_err("exhausted prompt attempts must fail closed");

    assert!(
        error.to_string().contains("prompt attempt") && error.to_string().contains("exhausted"),
        "overflow error must be actionable, got {error}"
    );
    assert_eq!(read_tree_bytes(&run_dir), before);
    drop(runner);
    assert!(step_runner.request_calls.is_empty());
    assert!(step_runner.calls.is_empty());
}

#[test]
fn state_resume_verified_rejects_exhausted_next_attempt_before_prepare_or_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "resume-exhausted-prompt-attempts";
    create_test_run(&runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    write_private_fixture_file(
        run_dir.join("prompts/01-research.attempt-4294967295.prompt.md"),
        b"highest possible prompt attempt",
    );
    let verified = read_run(&run_dir);
    let before = read_tree_bytes(&run_dir);
    let mut step_runner = RecordingStepRunner::with_prefix("must-not-prepare");

    let error = LoopRunner::resume_verified(&runs_root, verified, &mut step_runner)
        .expect_err("resume must preflight exhausted attempts");

    assert!(
        error.to_string().contains("prompt attempt") && error.to_string().contains("exhausted"),
        "overflow error must be actionable, got {error}"
    );
    assert_eq!(read_tree_bytes(&run_dir), before);
    assert_eq!(step_runner.prepare_calls, 0);
    assert!(step_runner.request_calls.is_empty());
    assert!(step_runner.calls.is_empty());
}

struct InitialStateCollisionRunner;

impl StepRunner for InitialStateCollisionRunner {
    fn prepare_fresh_run(
        &mut self,
        workspace: &LoopWorkspace,
        _run: &LoopRun,
    ) -> Result<(), seaf_loop::RunnerError> {
        let run_file = workspace.run_file();
        fs::create_dir(&run_file)
            .map_err(|error| seaf_loop::RunnerError::Step(error.to_string()))?;
        #[cfg(unix)]
        fs::set_permissions(&run_file, fs::Permissions::from_mode(0o700))
            .map_err(|error| seaf_loop::RunnerError::Step(error.to_string()))?;
        Ok(())
    }

    fn step_request(&mut self, _step: LoopStepName) -> Result<String, seaf_loop::RunnerError> {
        unreachable!("initial publication fails before any step")
    }

    fn run_step(
        &mut self,
        _step: LoopStepName,
        _request: &str,
    ) -> Result<StepOutput, seaf_loop::RunnerError> {
        unreachable!("initial publication fails before any step")
    }
}

struct RecordingStepRunner {
    prepare_calls: usize,
    calls: Vec<LoopStepName>,
    request_calls: Vec<LoopStepName>,
    prefix: &'static str,
    fail_on: Option<LoopStepName>,
    non_terminal_on: Option<LoopStepName>,
    terminal_status: Option<LoopStepStatus>,
    artifact: Option<ArtifactContent>,
}

impl RecordingStepRunner {
    fn new() -> Self {
        Self::with_prefix("recorded")
    }

    fn with_prefix(prefix: &'static str) -> Self {
        Self {
            prepare_calls: 0,
            calls: Vec::new(),
            request_calls: Vec::new(),
            prefix,
            fail_on: None,
            non_terminal_on: None,
            terminal_status: None,
            artifact: None,
        }
    }

    fn failing_on(mut self, step: LoopStepName) -> Self {
        self.fail_on = Some(step);
        self
    }

    fn non_terminal_on(mut self, step: LoopStepName) -> Self {
        self.non_terminal_on = Some(step);
        self
    }

    fn returning_status(mut self, status: LoopStepStatus) -> Self {
        self.terminal_status = Some(status);
        self
    }

    fn with_artifact(mut self, artifact: ArtifactContent) -> Self {
        self.artifact = Some(artifact);
        self
    }
}

impl StepRunner for RecordingStepRunner {
    fn prepare_run(
        &mut self,
        _workspace: &LoopWorkspace,
        _run: &LoopRun,
    ) -> Result<(), seaf_loop::RunnerError> {
        self.prepare_calls += 1;
        Ok(())
    }

    fn step_request(&mut self, step: LoopStepName) -> Result<String, seaf_loop::RunnerError> {
        self.request_calls.push(step);
        Ok(format!("{} request for {}", self.prefix, step_label(step)))
    }

    fn run_step(
        &mut self,
        step: LoopStepName,
        request: &str,
    ) -> Result<StepOutput, seaf_loop::RunnerError> {
        self.calls.push(step);
        if self.fail_on == Some(step) {
            return Err(seaf_loop::RunnerError::Step(format!(
                "forced failure after {request}"
            )));
        }

        let artifact = self.artifact.clone().unwrap_or_else(|| {
            ArtifactContent::markdown(format!("{} artifact for {}", self.prefix, step_label(step)))
        });
        let mut output =
            StepOutput::completed(format!("{} response for {}", self.prefix, step_label(step)))
                .with_artifact(artifact);
        if self.non_terminal_on == Some(step) {
            output.status = LoopStepStatus::Running;
        } else if let Some(status) = self.terminal_status {
            output.status = status;
        }
        Ok(output)
    }
}

fn assert_terminal_step_output_updates_run_and_stops(
    run_id: &str,
    step_status: LoopStepStatus,
    run_status: LoopStatus,
) {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut step_runner =
        RecordingStepRunner::with_prefix("terminal").returning_status(step_status);
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            run_id,
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start run");

    assert!(run.run_next_step().expect("run terminal step"));
    assert!(!run.run_next_step().expect("terminal run should stop"));
    drop(run);

    assert_eq!(step_runner.calls, vec![LoopStepName::Research]);

    let run_dir = runs_root.join(run_id);
    let persisted = read_run(&run_dir);
    assert_eq!(persisted.status, run_status);
    let research = persisted
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::Research)
        .expect("research step");
    assert_eq!(research.status, step_status);
    assert_file_contains(
        &run_dir.join("responses/01-research.raw.txt"),
        "terminal response for research",
    );
}

fn ticket() -> TicketSpec {
    TicketSpec {
        ticket_id: "P2-005".to_string(),
        goal_id: "phase-2".to_string(),
        title: "Add loop workspace and state machine".to_string(),
        status: TicketStatus::Ready,
        priority: seaf_core::TicketPriority::P2,
        problem: "Loop runs must be restartable and auditable.".to_string(),
        research_questions: Vec::new(),
        context: TicketContext {
            relevant_files: Vec::new(),
            forbidden_files: Vec::new(),
        },
        autonomy: seaf_core::TicketAutonomy {
            level: 1,
            apply_patch: false,
            allow_shell_commands: Vec::new(),
        },
        acceptance_criteria: vec![
            "A run can be resumed after interruption.".to_string(),
            "Every model request and response is stored.".to_string(),
        ],
        eval: None,
    }
}

fn test_input_digests() -> LoopInputDigests {
    LoopInputDigests {
        ticket: "a".repeat(64),
        policy: "b".repeat(64),
        config: "c".repeat(64),
        repository: "d".repeat(64),
        eval_config: None,
    }
}

fn git_ok(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn isolated_fixture(
    root: &Path,
    run_id: &str,
) -> (
    std::path::PathBuf,
    InitializedLoopRun,
    AuthoritativeRunInputSnapshots,
) {
    let eval_config = seaf_core::parse_eval_config(
        "evals:\n  allow_commands: []\n  required:\n    - name: tests\n      command: cargo test\n",
    )
    .unwrap();
    let eval_config = seaf_core::canonical_json_bytes(&eval_config).unwrap();
    isolated_fixture_with_eval_bytes(root, run_id, eval_config).unwrap()
}

fn isolated_fixture_with_secret(
    root: &Path,
    run_id: &str,
    secret: &str,
) -> (
    std::path::PathBuf,
    InitializedLoopRun,
    AuthoritativeRunInputSnapshots,
) {
    let eval_config: seaf_core::EvalConfig = serde_json::from_value(serde_json::json!({
        "evals": {
            "allow_commands": ["true"],
            "required": [{
                "name": "tests",
                "command": "true",
                "env": {"API_TOKEN": secret}
            }]
        }
    }))
    .unwrap();
    isolated_fixture_with_eval_bytes(
        root,
        run_id,
        seaf_core::canonical_json_bytes(&eval_config).unwrap(),
    )
    .unwrap()
}

fn isolated_fixture_with_eval_bytes(
    root: &Path,
    run_id: &str,
    eval_config: Vec<u8>,
) -> Result<
    (
        std::path::PathBuf,
        InitializedLoopRun,
        AuthoritativeRunInputSnapshots,
    ),
    seaf_loop::RunnerError,
> {
    let source = root.join(format!("{run_id}-source"));
    fs::create_dir(&source).unwrap();
    git_ok(&source, &["init", "-q"]);
    git_ok(&source, &["config", "user.email", "test@example.com"]);
    git_ok(&source, &["config", "user.name", "SEAF Test"]);
    fs::write(source.join("tracked.txt"), "source\n").unwrap();
    git_ok(&source, &["add", "tracked.txt"]);
    git_ok(&source, &["commit", "-qm", "initial"]);
    let ticket_bytes = seaf_core::canonical_json_bytes(&ticket()).unwrap();
    let policy = seaf_core::canonical_json_bytes(&serde_json::json!({"policy": run_id})).unwrap();
    let config = seaf_core::canonical_json_bytes(&serde_json::json!({"config": run_id})).unwrap();
    let repository = seaf_core::canonical_json_bytes(&serde_json::json!({
        "repository": source.canonicalize().unwrap(),
        "run": run_id
    }))
    .unwrap();
    let digest = |bytes: &[u8]| {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    };
    let initialized = InitializedLoopRun::create_isolated(
        LoopRunnerConfig::for_ticket(
            root.join("runs"),
            run_id,
            &ticket(),
            "fake-provider",
            "fake-model",
            LoopInputDigests {
                ticket: digest(&ticket_bytes),
                policy: digest(&policy),
                config: digest(&config),
                repository: digest(&repository),
                eval_config: Some(digest(&eval_config)),
            },
        ),
        &source,
        &AuthoritativeRunInputSnapshots {
            provider_ticket: ticket_bytes.clone(),
            ticket: ticket_bytes.clone(),
            policy: policy.clone(),
            config: config.clone(),
            repository: repository.clone(),
            eval_config: eval_config.clone(),
        },
    )?;
    Ok((
        source,
        initialized,
        AuthoritativeRunInputSnapshots {
            provider_ticket: ticket_bytes.clone(),
            ticket: ticket_bytes,
            policy,
            config,
            repository,
            eval_config,
        },
    ))
}

fn create_test_run(runs_root: &Path, run_id: &str) {
    let mut step_runner = RecordingStepRunner::new();
    let runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            runs_root,
            run_id,
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("create test run");
    drop(runner);
}

fn read_tree_bytes(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
        let mut entries = fs::read_dir(current)
            .expect("read tree")
            .collect::<Result<Vec<_>, _>>()
            .expect("tree entries");
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root)
                        .expect("relative path")
                        .to_path_buf(),
                    fs::read(path).expect("tree file"),
                ));
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, &mut files);
    files
}

#[cfg(unix)]
fn assert_resume_verified_child_symlink_is_rejected(relative_path: &str) {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "symlinked-step-child";
    create_test_run(&runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    let external_path = temp_dir.path().join("external-target");
    let original = b"external target must stay unchanged";
    fs::write(&external_path, original).expect("external target");
    symlink(&external_path, run_dir.join(relative_path)).expect("step child symlink");
    let verified = read_run(&run_dir);
    let before = read_tree_bytes(&run_dir);
    let mut step_runner = RecordingStepRunner::with_prefix("symlink-guard");

    let error = LoopRunner::resume_verified(&runs_root, verified, &mut step_runner)
        .expect_err("resume must reject symlinked child before prepare");

    assert!(error.to_string().contains("symlink"), "{error}");
    assert_eq!(read_tree_bytes(&run_dir), before);
    assert_eq!(
        fs::read(&external_path).expect("external target bytes"),
        original,
        "{relative_path} must not be followed"
    );
    assert_eq!(step_runner.prepare_calls, 0);
    assert!(step_runner.request_calls.is_empty());
    assert!(step_runner.calls.is_empty());
}

fn assert_file_contains(path: &Path, expected: &str) {
    let content = std::fs::read_to_string(path).expect("read file");
    assert!(
        content.contains(expected),
        "{path:?} should contain {expected:?}; got {content:?}"
    );
}

fn read_run(run_dir: &Path) -> LoopRun {
    let run_json = std::fs::read_to_string(run_dir.join("run.json")).expect("run json");
    serde_json::from_str(&run_json).expect("run json")
}

fn write_run(run_dir: &Path, run: &LoopRun) {
    let mut json = serde_json::to_vec_pretty(run).expect("run json");
    json.push(b'\n');
    std::fs::write(run_dir.join("run.json"), json).expect("write run json");
}

#[cfg(unix)]
fn create_private_fixture_directory(path: &Path) {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path).unwrap();
}

#[cfg(not(unix))]
fn create_private_fixture_directory(_path: &Path) {
    panic!("private loop workspace tests require Unix")
}

#[cfg(unix)]
fn write_private_fixture_file(path: impl AsRef<Path>, bytes: &[u8]) {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(path).unwrap();
    std::io::Write::write_all(&mut file, bytes).unwrap();
}

#[cfg(not(unix))]
fn write_private_fixture_file(_path: impl AsRef<Path>, _bytes: &[u8]) {
    panic!("private loop workspace tests require Unix")
}

fn step_label(step: LoopStepName) -> &'static str {
    match step {
        LoopStepName::Research => "research",
        LoopStepName::Analysis => "analysis",
        LoopStepName::SpecCreation => "spec creation",
        LoopStepName::SpecReview => "spec review",
        LoopStepName::Development => "development",
        LoopStepName::OutputReview => "output review",
        LoopStepName::Testing => "testing",
        LoopStepName::EvalReport => "eval report",
    }
}
