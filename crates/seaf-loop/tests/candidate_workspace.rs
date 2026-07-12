use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use seaf_core::{canonical_json_bytes, validate_loop_run, LoopInputDigests, LoopStatus};
use seaf_loop::{
    apply_candidate_development_evidence, cleanup_candidate_workspace, patch_digest,
    plan_candidate_workspace, provision_candidate_workspace, validate_candidate_workspace,
    DeveloperResponse, DeveloperStatus, DevelopmentEvidence, LoopWorkspace, PatchDecisionKind,
    PolicyDecision, Role,
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
fn candidate_plan_is_durable_authority_before_provisioning_creates_the_worktree() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let workspace = LoopWorkspace::create(&temp.path().join("runs"), "run-plan").unwrap();
    let digest = identity_digest(&source);

    let planned = plan_candidate_workspace(workspace.run_directory(), &source, &digest)
        .expect("plan candidate");
    assert_eq!(planned.schema_version, 2);
    assert_eq!(
        serde_json::to_value(&planned).unwrap()["run_directory_digest"],
        serde_json::json!(sha256_path(workspace.run_directory()))
    );
    assert_eq!(
        planned.lifecycle,
        seaf_core::CandidateWorkspaceLifecycle::Provisioning
    );
    assert!(!Path::new(&planned.path).exists());
    assert_eq!(planned.candidate_head, planned.starting_head);
    assert_eq!(planned.candidate_tree, planned.starting_tree);
    assert_eq!(planned.candidate_diff_digest, empty_sha256());
    assert!(planned.patch_transaction.is_none());
    assert!(planned.cleanup_started_at.is_none());
    assert!(planned.cleaned_at.is_none());

    let mut run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "run-plan".to_string(),
        ticket_id: "ticket-plan".to_string(),
        goal_id: "goal-plan".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: seaf_core::LoopInputDigests {
            ticket: "1".repeat(64),
            policy: "2".repeat(64),
            config: "3".repeat(64),
            repository: digest,
        },
    });
    run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
    run.candidate_workspace = Some(planned.clone());
    seaf_loop::state::save_run(&workspace, &run).expect("persist provisioning authority");

    let active = provision_candidate_workspace(&workspace).expect("provision exact plan");
    assert_eq!(
        active.lifecycle,
        seaf_core::CandidateWorkspaceLifecycle::Active
    );
    assert_eq!(active.starting_head, planned.starting_head);
    assert_eq!(active.starting_tree, planned.starting_tree);
    assert!(Path::new(&active.path).is_dir());
    let persisted = seaf_loop::state::load_run(&workspace).expect("load active run");
    assert_eq!(persisted.candidate_workspace.as_ref(), Some(&active));

    remove_worktree(&source, Path::new(&active.path));
}

#[test]
fn candidate_authority_versions_are_closed_and_match_the_public_schema() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let run_dir = temp.path().join("runs/run-version-contract");
    fs::create_dir_all(&run_dir).unwrap();
    let candidate = plan_candidate_workspace(&run_dir, &source, &identity_digest(&source)).unwrap();

    let mut run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "run-version-contract".to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "goal".to_string(),
        provider: "fake".to_string(),
        model: "model".to_string(),
        input_digests: LoopInputDigests {
            ticket: "1".repeat(64),
            policy: "2".repeat(64),
            config: "3".repeat(64),
            repository: identity_digest(&source),
        },
    });
    run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
    run.candidate_workspace = Some(candidate);

    let v2 = serde_json::to_value(&run).unwrap();
    assert!(validate_loop_run(&serde_json::from_value(v2.clone()).unwrap()).is_empty());

    let mut v2_missing_digest = v2.clone();
    v2_missing_digest["candidate_workspace"]
        .as_object_mut()
        .unwrap()
        .remove("run_directory_digest");
    let parsed: seaf_core::LoopRun = serde_json::from_value(v2_missing_digest).unwrap();
    assert!(validate_loop_run(&parsed)
        .iter()
        .any(|error| error.field == "candidate_workspace.run_directory_digest"));

    let mut v2_bad_digest = v2.clone();
    v2_bad_digest["candidate_workspace"]["run_directory_digest"] = serde_json::json!("BAD");
    let parsed: seaf_core::LoopRun = serde_json::from_value(v2_bad_digest).unwrap();
    assert!(validate_loop_run(&parsed)
        .iter()
        .any(|error| error.field == "candidate_workspace.run_directory_digest"));

    let mut v1 = v2.clone();
    v1["candidate_workspace"]["schema_version"] = serde_json::json!(1);
    v1["candidate_workspace"]
        .as_object_mut()
        .unwrap()
        .remove("run_directory_digest");
    assert!(validate_loop_run(&serde_json::from_value(v1.clone()).unwrap()).is_empty());

    let mut v1_with_digest = v1;
    v1_with_digest["candidate_workspace"]["run_directory_digest"] =
        serde_json::json!(sha256_path(&run_dir));
    let parsed: seaf_core::LoopRun = serde_json::from_value(v1_with_digest).unwrap();
    assert!(validate_loop_run(&parsed)
        .iter()
        .any(|error| error.field == "candidate_workspace.run_directory_digest"));

    let mut v1_null = serde_json::to_value(&run).unwrap();
    v1_null["candidate_workspace"]["schema_version"] = serde_json::json!(1);
    v1_null["candidate_workspace"]["run_directory_digest"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<seaf_core::LoopRun>(v1_null).is_err());

    let mut v2_null = serde_json::to_value(&run).unwrap();
    v2_null["candidate_workspace"]["run_directory_digest"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<seaf_core::LoopRun>(v2_null).is_err());

    let mut unknown = serde_json::to_value(&run).unwrap();
    unknown["candidate_workspace"]["schema_version"] = serde_json::json!(3);
    let parsed: seaf_core::LoopRun = serde_json::from_value(unknown).unwrap();
    assert!(validate_loop_run(&parsed)
        .iter()
        .any(|error| error.field == "candidate_workspace.schema_version"));

    let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../specs/loop-run.schema.json"
    )))
    .unwrap();
    let contract = &schema["properties"]["candidate_workspace"]["anyOf"][0];
    assert_eq!(
        contract["properties"]["schema_version"]["enum"],
        serde_json::json!([1, 2])
    );
    assert_eq!(
        contract["properties"]["run_directory_digest"]["pattern"],
        "^[a-f0-9]{64}$"
    );
    assert!(contract["allOf"].as_array().unwrap().iter().any(|branch| {
        branch["if"]["properties"]["schema_version"]["const"] == 1
            && branch["then"]["not"]["required"] == serde_json::json!(["run_directory_digest"])
    }));
    assert!(contract["allOf"].as_array().unwrap().iter().any(|branch| {
        branch["if"]["properties"]["schema_version"]["const"] == 2
            && branch["then"]["required"] == serde_json::json!(["run_directory_digest"])
    }));
}

#[test]
fn copied_or_moved_runs_cannot_operate_on_the_original_candidate() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let (workspace, candidate) =
        persisted_candidate_workspace(temp.path(), &source, "bound-run", LoopStatus::Running);
    let patch = "diff --git a/tracked.txt b/tracked.txt\nindex 1f7391f..39c5733 100644\n--- a/tracked.txt\n+++ b/tracked.txt\n@@ -1 +1 @@\n-source\n+candidate\n";
    persist_development_authority(
        &workspace,
        "bound-run",
        patch,
        vec!["tracked.txt".to_string()],
        false,
        PatchDecisionKind::Allowed,
        false,
    );
    let applied = apply_candidate_development_evidence(&workspace, &source).unwrap();
    let mut tampered_digest = applied.clone();
    tampered_digest.run_directory_digest = Some("f".repeat(64));
    assert!(
        validate_candidate_workspace(workspace.run_directory(), &source, &tampered_digest).is_err()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let linked_run = temp.path().join("linked-run");
        symlink(workspace.run_directory(), &linked_run).unwrap();
        assert!(validate_candidate_workspace(&linked_run, &source, &applied).is_err());
        fs::remove_file(linked_run).unwrap();
    }

    let copied_root = temp.path().join("copied-runs");
    fs::create_dir(&copied_root).unwrap();
    let copied_dir = copied_root.join("bound-run");
    copy_directory(workspace.run_directory(), &copied_dir);
    let copied_lock = copied_dir.join(".candidate-workspace.lock");
    if copied_lock.exists() {
        fs::remove_file(&copied_lock).unwrap();
    }
    let copied = LoopWorkspace::open(&copied_root, "bound-run").unwrap();
    let copied_before = fs::read(copied.run_file()).unwrap();
    let candidate_before = git(Path::new(&candidate.path), &["status", "--porcelain=v1"]);

    assert!(validate_candidate_workspace(&copied_dir, &source, &applied).is_err());
    assert!(apply_candidate_development_evidence(&copied, &source).is_err());
    assert!(seaf_loop::verify_candidate_patch_evidence(&copied, &source).is_err());
    let mut copied_run = seaf_loop::state::load_run(&copied).unwrap();
    copied_run.status = LoopStatus::Completed;
    seaf_loop::state::save_run(&copied, &copied_run).unwrap();
    let cleanup_before = fs::read(copied.run_file()).unwrap();
    assert!(cleanup_candidate_workspace(&copied, &source).is_err());
    assert_eq!(fs::read(copied.run_file()).unwrap(), cleanup_before);
    assert!(!copied_lock.exists());
    assert_eq!(
        git(Path::new(&candidate.path), &["status", "--porcelain=v1"]),
        candidate_before
    );
    assert!(Path::new(&candidate.path).is_dir());
    assert_ne!(
        copied_before, cleanup_before,
        "only the test's status transition may change the copy"
    );

    let moved_root = temp.path().join("moved-runs");
    fs::create_dir(&moved_root).unwrap();
    let moved_dir = moved_root.join("bound-run");
    fs::rename(workspace.run_directory(), &moved_dir).unwrap();
    assert!(validate_candidate_workspace(&moved_dir, &source, &applied).is_err());
    remove_worktree(&source, Path::new(&candidate.path));
}

#[test]
fn legacy_candidate_authority_is_read_only_and_rejected_before_lock_or_git_mutation() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let workspace = LoopWorkspace::create(&temp.path().join("runs"), "legacy-run").unwrap();
    let mut planned = plan_candidate_workspace(
        workspace.run_directory(),
        &source,
        &identity_digest(&source),
    )
    .unwrap();
    planned.schema_version = 1;
    let mut planned_json = serde_json::to_value(&planned).unwrap();
    planned_json
        .as_object_mut()
        .unwrap()
        .remove("run_directory_digest");
    let planned: seaf_core::CandidateWorkspaceState = serde_json::from_value(planned_json).unwrap();
    let mut run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "legacy-run".to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "goal".to_string(),
        provider: "fake".to_string(),
        model: "model".to_string(),
        input_digests: LoopInputDigests {
            ticket: "1".repeat(64),
            policy: "2".repeat(64),
            config: "3".repeat(64),
            repository: identity_digest(&source),
        },
    });
    run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
    run.candidate_workspace = Some(planned.clone());
    seaf_loop::state::save_run(&workspace, &run).unwrap();
    let before = fs::read(workspace.run_file()).unwrap();
    let lock = workspace.run_directory().join(".candidate-workspace.lock");

    let error = provision_candidate_workspace(&workspace).expect_err("v1 is forensic-only");
    assert!(error.to_string().contains("start a new run"), "{error}");
    assert_eq!(fs::read(workspace.run_file()).unwrap(), before);
    assert!(!lock.exists());
    assert!(!Path::new(&planned.path).exists());
}

#[test]
fn legacy_active_cleaning_and_cleaned_authority_rejects_every_candidate_operation_pre_lock() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let (workspace, candidate) = persisted_candidate_workspace(
        temp.path(),
        &source,
        "legacy-operations",
        LoopStatus::Running,
    );
    let lock = workspace.run_directory().join(".candidate-workspace.lock");
    if lock.exists() {
        fs::remove_file(&lock).unwrap();
    }
    let candidate_before = git(Path::new(&candidate.path), &["status", "--porcelain=v1"]);
    let mut run = seaf_loop::state::load_run(&workspace).unwrap();
    let authority = run.candidate_workspace.as_mut().unwrap();
    authority.schema_version = 1;
    authority.run_directory_digest = None;
    let legacy_active = authority.clone();
    seaf_loop::state::save_run(&workspace, &run).unwrap();
    let running_before = fs::read(workspace.run_file()).unwrap();

    assert!(
        validate_candidate_workspace(workspace.run_directory(), &source, &legacy_active).is_err()
    );
    assert!(apply_candidate_development_evidence(&workspace, &source).is_err());
    assert!(seaf_loop::verify_candidate_patch_evidence(&workspace, &source).is_err());
    assert_eq!(fs::read(workspace.run_file()).unwrap(), running_before);
    assert!(!lock.exists());

    for lifecycle in [
        seaf_core::CandidateWorkspaceLifecycle::Active,
        seaf_core::CandidateWorkspaceLifecycle::Cleaning,
        seaf_core::CandidateWorkspaceLifecycle::Cleaned,
    ] {
        let mut terminal = seaf_loop::state::load_run(&workspace).unwrap();
        terminal.status = LoopStatus::Completed;
        let authority = terminal.candidate_workspace.as_mut().unwrap();
        authority.lifecycle = lifecycle;
        authority.cleanup_started_at = matches!(
            lifecycle,
            seaf_core::CandidateWorkspaceLifecycle::Cleaning
                | seaf_core::CandidateWorkspaceLifecycle::Cleaned
        )
        .then(|| "started".to_string());
        authority.cleaned_at = (lifecycle == seaf_core::CandidateWorkspaceLifecycle::Cleaned)
            .then(|| "cleaned".to_string());
        seaf_loop::state::save_run(&workspace, &terminal).unwrap();
        let before = fs::read(workspace.run_file()).unwrap();
        assert!(cleanup_candidate_workspace(&workspace, &source).is_err());
        assert_eq!(fs::read(workspace.run_file()).unwrap(), before);
        assert!(!lock.exists());
        assert!(Path::new(&candidate.path).is_dir());
    }
    assert_eq!(
        git(Path::new(&candidate.path), &["status", "--porcelain=v1"]),
        candidate_before
    );
    remove_worktree(&source, Path::new(&candidate.path));
}

#[test]
fn copied_provisioning_authority_cannot_create_or_adopt_a_crash_remnant() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let runs = temp.path().join("runs");
    let workspace = LoopWorkspace::create(&runs, "provision-copy").unwrap();
    let digest = identity_digest(&source);
    let planned = plan_candidate_workspace(workspace.run_directory(), &source, &digest).unwrap();
    let mut run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "provision-copy".to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "goal".to_string(),
        provider: "fake".to_string(),
        model: "model".to_string(),
        input_digests: LoopInputDigests {
            ticket: "1".repeat(64),
            policy: "2".repeat(64),
            config: "3".repeat(64),
            repository: digest.clone(),
        },
    });
    run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
    run.candidate_workspace = Some(planned.clone());
    seaf_loop::state::save_run(&workspace, &run).unwrap();

    let copied_root = temp.path().join("copied-runs");
    fs::create_dir(&copied_root).unwrap();
    copy_directory(
        workspace.run_directory(),
        &copied_root.join("provision-copy"),
    );
    let copied_lock = copied_root
        .join("provision-copy")
        .join(".candidate-workspace.lock");
    if copied_lock.exists() {
        fs::remove_file(&copied_lock).unwrap();
    }
    let copied = LoopWorkspace::open(&copied_root, "provision-copy").unwrap();
    let copied_before = fs::read(copied.run_file()).unwrap();

    assert!(
        seaf_loop::create_candidate_workspace(copied.run_directory(), &source, &digest).is_err()
    );
    assert!(provision_candidate_workspace(&copied).is_err());
    assert_eq!(fs::read(copied.run_file()).unwrap(), copied_before);
    assert!(!copied_lock.exists());

    let remnant = create_candidate_workspace(workspace.run_directory(), &source, &digest).unwrap();
    let mut interrupted = seaf_loop::state::load_run(&workspace).unwrap();
    interrupted.candidate_workspace = Some(planned.clone());
    seaf_loop::state::save_run(&workspace, &interrupted).unwrap();
    assert!(provision_candidate_workspace(&copied).is_err());
    assert_eq!(fs::read(copied.run_file()).unwrap(), copied_before);
    assert!(!copied_lock.exists());
    assert!(Path::new(&remnant.path).is_dir());

    let recovered = provision_candidate_workspace(&workspace).expect("original adopts remnant");
    assert_eq!(
        recovered.lifecycle,
        seaf_core::CandidateWorkspaceLifecycle::Active
    );
    remove_worktree(&source, Path::new(&recovered.path));
}

#[test]
fn provisioning_uses_the_persisted_starting_head_instead_of_resnapshotting_source() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let workspace = LoopWorkspace::create(&temp.path().join("runs"), "run-stale-plan").unwrap();
    let digest = identity_digest(&source);
    let planned = plan_candidate_workspace(workspace.run_directory(), &source, &digest).unwrap();
    let mut run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "run-stale-plan".to_string(),
        ticket_id: "ticket-plan".to_string(),
        goal_id: "goal-plan".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: seaf_core::LoopInputDigests {
            ticket: "1".repeat(64),
            policy: "2".repeat(64),
            config: "3".repeat(64),
            repository: digest,
        },
    });
    run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
    run.candidate_workspace = Some(planned.clone());
    seaf_loop::state::save_run(&workspace, &run).unwrap();
    fs::write(source.join("tracked.txt"), "new source commit\n").unwrap();
    git_ok(&source, &["add", "tracked.txt"]);
    git_ok(&source, &["commit", "-qm", "advance source"]);

    let error = provision_candidate_workspace(&workspace)
        .expect_err("source drift must not silently replace planned authority");
    assert!(error.to_string().contains("starting HEAD"), "{error}");
    assert!(!Path::new(&planned.path).exists());
}

#[test]
fn provisioning_runtime_and_schema_contracts_are_closed() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let run_dir = temp.path().join("runs/run-provisioning-contract");
    fs::create_dir_all(&run_dir).unwrap();
    let digest = identity_digest(&source);
    let planned = plan_candidate_workspace(&run_dir, &source, &digest).unwrap();
    let mut run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "run-provisioning-contract".to_string(),
        ticket_id: "ticket-plan".to_string(),
        goal_id: "goal-plan".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: seaf_core::LoopInputDigests {
            ticket: "1".repeat(64),
            policy: "2".repeat(64),
            config: "3".repeat(64),
            repository: digest,
        },
    });
    run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
    run.candidate_workspace = Some(planned);
    assert!(validate_loop_run(&run).is_empty());

    let mut transaction_run = run.clone();
    transaction_run
        .candidate_workspace
        .as_mut()
        .unwrap()
        .patch_transaction = Some(seaf_core::CandidatePatchTransaction {
        schema_version: 1,
        phase: seaf_core::CandidatePatchPhase::Applying,
        intent: seaf_core::ArtifactReference {
            path: "artifacts/candidate-patch.intent.json".to_string(),
            digest: "a".repeat(64),
        },
        applied_evidence: None,
        started_at: "1".to_string(),
        applied_at: None,
    });
    assert!(validate_loop_run(&transaction_run).iter().any(|error| {
        error.field == "candidate_workspace.patch_transaction"
            && error.message.contains("absent while provisioning")
    }));

    let mut cleaning_run = run.clone();
    let candidate = cleaning_run.candidate_workspace.as_mut().unwrap();
    candidate.lifecycle = seaf_core::CandidateWorkspaceLifecycle::Cleaning;
    candidate.cleanup_started_at = Some("1".to_string());
    assert!(validate_loop_run(&cleaning_run).iter().any(|error| {
        error.field == "candidate_workspace.lifecycle" && error.message.contains("pending")
    }));

    let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../specs/loop-run.schema.json"
    )))
    .unwrap();
    let candidate = &schema["properties"]["candidate_workspace"]["anyOf"][0];
    assert_eq!(
        candidate["properties"]["lifecycle"]["enum"],
        serde_json::json!(["provisioning", "active", "cleaning", "cleaned"])
    );
    assert!(candidate["allOf"].as_array().unwrap().iter().any(|branch| {
        branch["if"]["properties"]["lifecycle"]["const"] == "provisioning"
            && branch["then"]["properties"]["patch_transaction"]["type"] == "null"
    }));
    let provisioning_run = schema["allOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|branch| {
            branch["if"]["properties"]["candidate_workspace"]["properties"]["lifecycle"]["const"]
                == "provisioning"
        })
        .expect("top-level provisioning run contract");
    let then = &provisioning_run["then"]["properties"];
    assert_eq!(then["status"]["const"], "pending");
    assert_eq!(then["current_step"]["const"], "research");
    assert_eq!(then["policy_decisions"]["maxItems"], 0);
    assert_eq!(then["provider_exchange_records"]["maxItems"], 0);
    assert_eq!(then["eval_report_path"]["type"], "null");
    assert_eq!(
        then["steps"]["items"]["properties"]["status"]["const"],
        "pending"
    );
    let status_lifecycle = |status: &str| {
        schema["allOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|branch| branch["if"]["properties"]["status"]["const"] == status)
            .expect("status lifecycle branch")
    };
    assert_eq!(
        status_lifecycle("pending")["then"]["properties"]["candidate_workspace"]["properties"]
            ["lifecycle"]["enum"],
        serde_json::json!(["provisioning", "active"])
    );
    assert_eq!(
        status_lifecycle("running")["then"]["properties"]["candidate_workspace"]["properties"]
            ["lifecycle"]["const"],
        "active"
    );
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
    assert!(error.to_string().contains("registered"), "{error}");
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
    prepare_candidate_workspace(&run_dir, &source, &digest);
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
    assert!(
        error
            .to_string()
            .contains("does not match its patch transaction phase"),
        "{error}"
    );
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
        error
            .to_string()
            .contains("does not match its patch transaction phase"),
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
    run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
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
    let active_lifecycle = contract["allOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|branch| branch["if"]["properties"]["lifecycle"]["const"] == "active")
        .expect("active lifecycle branch");
    assert_eq!(
        active_lifecycle["then"]["properties"]["cleaned_at"]["type"],
        "null"
    );
    assert_eq!(
        contract["properties"]["candidate_diff_digest"]["pattern"],
        "^[a-f0-9]{64}$"
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

#[test]
fn loop_execution_mode_defaults_legacy_and_accepts_only_isolated_candidate_opt_in() {
    let fixture = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/local-loop/runs/valid-loop-run.json"
    ));
    let legacy: seaf_core::LoopRun = serde_json::from_str(fixture).expect("legacy loop run");
    let legacy_json = serde_json::to_value(&legacy).expect("legacy run JSON");
    assert_eq!(
        legacy_json["execution_mode"],
        serde_json::json!("legacy_proposal_only"),
        "missing execution mode must deserialize to the explicit legacy proposal-only authority"
    );

    let mut isolated_json: serde_json::Value = serde_json::from_str(fixture).expect("fixture JSON");
    isolated_json["execution_mode"] = serde_json::json!("isolated_candidate");
    let isolated: seaf_core::LoopRun =
        serde_json::from_value(isolated_json).expect("isolated candidate mode");
    assert_eq!(
        serde_json::to_value(isolated).expect("isolated run JSON")["execution_mode"],
        serde_json::json!("isolated_candidate")
    );
    let mut explicit_null: serde_json::Value = serde_json::from_str(fixture).expect("fixture JSON");
    explicit_null["execution_mode"] = serde_json::Value::Null;
    assert!(
        serde_json::from_value::<seaf_core::LoopRun>(explicit_null).is_err(),
        "explicit null is not the same as a missing legacy execution mode"
    );

    let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../specs/loop-run.schema.json"
    )))
    .expect("loop schema");
    assert_eq!(
        schema["properties"]["execution_mode"]["enum"],
        serde_json::json!(["legacy_proposal_only", "isolated_candidate"])
    );
    assert_eq!(
        schema["properties"]["execution_mode"]["default"],
        "legacy_proposal_only"
    );
    let mode_branch = schema["allOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|branch| {
            branch["if"]["properties"]["execution_mode"]["const"] == "isolated_candidate"
        })
        .expect("execution mode candidate-presence branch");
    assert_eq!(
        mode_branch["then"]["properties"]["candidate_workspace"]["type"],
        "object"
    );
    let legacy_branch = schema["allOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|branch| {
            branch["if"]["properties"]["execution_mode"]["const"] == "legacy_proposal_only"
        })
        .expect("explicit legacy candidate-absence branch");
    assert_eq!(
        legacy_branch["then"]["properties"]["candidate_workspace"]["type"],
        "null"
    );
}

#[test]
fn pre_b1_candidate_run_without_execution_mode_migrates_and_remains_cleanup_safe() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let (workspace, _) = persisted_candidate_workspace(
        temp.path(),
        &source,
        "pre-b1-candidate",
        LoopStatus::Completed,
    );
    let mut legacy_json: serde_json::Value =
        serde_json::from_slice(&fs::read(workspace.run_file()).expect("current run bytes"))
            .expect("current run JSON");
    legacy_json
        .as_object_mut()
        .unwrap()
        .remove("execution_mode");
    fs::write(
        workspace.run_file(),
        serde_json::to_vec_pretty(&legacy_json).unwrap(),
    )
    .expect("pre-B1 run JSON");

    let migrated = seaf_loop::state::load_run(&workspace).expect("migrated candidate run");
    assert_eq!(
        migrated.execution_mode,
        seaf_core::LoopExecutionMode::IsolatedCandidate
    );
    assert!(validate_loop_run(&migrated).is_empty());
    assert_eq!(
        serde_json::to_value(&migrated).unwrap()["execution_mode"],
        "isolated_candidate"
    );
    assert_eq!(
        cleanup_candidate_workspace(&workspace, &source)
            .expect("migrated cleanup")
            .lifecycle,
        seaf_core::CandidateWorkspaceLifecycle::Cleaned
    );
}

#[test]
fn applying_candidate_patch_transaction_is_closed_and_keeps_parent_evidence_pristine() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    let digest = identity_digest(&source);
    let run_dir = temp.path().join("runs/run-applying-contract");
    fs::create_dir_all(&run_dir).expect("run dir");
    let candidate = create_candidate_workspace(&run_dir, &source, &digest).expect("candidate");
    let mut run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "run-applying-contract".to_string(),
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
    run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
    let mut json = serde_json::to_value(&run).expect("run JSON");
    json["execution_mode"] = serde_json::json!("isolated_candidate");
    json["candidate_workspace"]["patch_transaction"] = serde_json::json!({
        "schema_version": 1,
        "phase": "applying",
        "intent": {
            "path": "artifacts/candidate-patch-intent.json",
            "digest": "a".repeat(64)
        },
        "started_at": "1"
    });

    let applying: seaf_core::LoopRun =
        serde_json::from_value(json).expect("closed applying transaction contract");
    assert!(
        validate_loop_run(&applying).is_empty(),
        "Applying must retain the pristine parent candidate tree and diff evidence"
    );
    let mut legacy_with_candidate = applying.clone();
    legacy_with_candidate.execution_mode = seaf_core::LoopExecutionMode::LegacyProposalOnly;
    assert!(validate_loop_run(&legacy_with_candidate)
        .iter()
        .any(|error| error.field == "candidate_workspace"));

    let mut applied_without_material_effect = applying.clone();
    let transaction = applied_without_material_effect
        .candidate_workspace
        .as_mut()
        .unwrap()
        .patch_transaction
        .as_mut()
        .unwrap();
    transaction.phase = seaf_core::CandidatePatchPhase::Applied;
    transaction.applied_evidence = Some(seaf_core::ArtifactReference {
        path: "artifacts/candidate-patch-applied.json".to_string(),
        digest: "b".repeat(64),
    });
    transaction.applied_at = Some("2".to_string());
    let fields = validate_loop_run(&applied_without_material_effect)
        .into_iter()
        .map(|error| error.field)
        .collect::<Vec<_>>();
    assert!(
        fields.contains(&"candidate_workspace.candidate_tree".to_string()),
        "Applied cannot retain the starting tree"
    );
    assert!(
        fields.contains(&"candidate_workspace.candidate_diff_digest".to_string()),
        "Applied cannot retain an empty staged diff"
    );

    let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../specs/loop-run.schema.json"
    )))
    .expect("loop schema");
    let transaction = &schema["properties"]["candidate_workspace"]["anyOf"][0]["properties"]
        ["patch_transaction"]["anyOf"][0];
    assert_eq!(transaction["additionalProperties"], false);
    assert_eq!(
        transaction["properties"]["phase"]["enum"],
        serde_json::json!(["applying", "applied"])
    );
    let phase_branches = transaction["allOf"].as_array().expect("phase branches");
    let applying_branch = phase_branches
        .iter()
        .find(|branch| branch["if"]["properties"]["phase"]["const"] == "applying")
        .expect("Applying branch");
    assert_eq!(
        applying_branch["then"]["properties"]["applied_evidence"]["type"],
        "null"
    );
    let applied_branch = phase_branches
        .iter()
        .find(|branch| branch["if"]["properties"]["phase"]["const"] == "applied")
        .expect("Applied branch");
    assert_eq!(
        applied_branch["then"]["required"],
        serde_json::json!(["applied_evidence", "applied_at"])
    );
    remove_worktree(&source, Path::new(&candidate.path));
}

#[test]
fn candidate_patch_application_persists_intent_before_mutating_only_the_candidate() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    fs::write(source.join("unrelated.txt"), "stable\n").expect("unrelated fixture");
    git_ok(&source, &["add", "unrelated.txt"]);
    git_ok(&source, &["commit", "-qm", "add unrelated fixture"]);
    let source_head = git(&source, &["rev-parse", "HEAD"]);
    let source_status = git(
        &source,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let source_bytes = fs::read(source.join("tracked.txt")).expect("source bytes");
    let (workspace, candidate) =
        persisted_candidate_workspace(temp.path(), &source, "run-apply", LoopStatus::Running);
    let patch = "diff --git a/tracked.txt b/tracked.txt\nindex 1f7391f..39c5733 100644\n--- a/tracked.txt\n+++ b/tracked.txt\n@@ -1 +1 @@\n-source\n+candidate\n";
    let decision = PolicyDecision {
        patch_id: "run-apply".to_string(),
        patch_sha256: patch_digest(patch),
        changed_paths: vec!["tracked.txt".to_string()],
        decision: PatchDecisionKind::Allowed,
        reasons: Vec::new(),
        requires_human_review: false,
        apply_requested: false,
        applied: false,
    };
    let evidence = DevelopmentEvidence::new(
        "run-apply",
        DeveloperResponse {
            role: Role::Developer,
            status: DeveloperStatus::PatchProposed,
            summary: "change only the candidate".to_string(),
            changed_files: vec!["tracked.txt".to_string()],
            requires_human_review: false,
            patch: Some(patch.to_string()),
            context_request: None,
        },
        patch,
        decision,
    )
    .expect("Development evidence");
    let artifact_path = "artifacts/05-development.json";
    fs::create_dir_all(workspace.run_directory().join("artifacts")).expect("artifact dir");
    fs::write(
        workspace.run_directory().join(artifact_path),
        evidence.canonical_bytes().expect("canonical evidence"),
    )
    .expect("Development evidence artifact");
    let mut authoritative = seaf_loop::state::load_run(&workspace).expect("authoritative run");
    let development = authoritative
        .steps
        .iter_mut()
        .find(|step| step.name == seaf_core::LoopStepName::Development)
        .expect("Development step");
    development.artifact_path = Some(artifact_path.to_string());
    development.artifact_digest = Some(evidence.artifact_digest().expect("evidence digest"));
    development.status = seaf_core::LoopStepStatus::Completed;
    authoritative.current_step = seaf_core::LoopStepName::OutputReview;
    authoritative.policy_decisions.push(
        serde_json::from_value(serde_json::to_value(&evidence.policy_decision).unwrap()).unwrap(),
    );
    seaf_loop::state::save_run(&workspace, &authoritative).expect("Development authority");

    let applied =
        apply_candidate_development_evidence(&workspace, &source).expect("apply exact evidence");

    assert_eq!(
        applied.patch_transaction.as_ref().unwrap().phase,
        seaf_core::CandidatePatchPhase::Applied
    );
    assert_ne!(applied.candidate_tree, applied.starting_tree);
    assert_ne!(applied.candidate_diff_digest, empty_sha256());
    assert_eq!(
        fs::read_to_string(Path::new(&candidate.path).join("tracked.txt")).unwrap(),
        "candidate\n"
    );
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), source_head);
    assert_eq!(
        git(
            &source,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        source_status
    );
    assert_eq!(fs::read(source.join("tracked.txt")).unwrap(), source_bytes);
    let persisted = seaf_loop::state::load_run(&workspace).expect("persisted run");
    assert_eq!(persisted.candidate_workspace.as_ref(), Some(&applied));
    let resumed = seaf_loop::InitializedLoopRun::resume_isolated(
        &temp.path().join("runs"),
        persisted.clone(),
    )
    .expect("ordinary resume accepts exact Applied evidence");
    assert_eq!(resumed.run(), &persisted);
    let mut noop = NoopStepRunner;
    let runner = seaf_loop::LoopRunner::resume_verified(
        temp.path().join("runs"),
        persisted.clone(),
        &mut noop,
    )
    .expect("generic resume");
    let before_rerun = fs::read(workspace.run_file()).unwrap();
    let error = runner
        .rerun_from(seaf_core::LoopStepName::Development)
        .expect_err("Applied transaction permits only OutputReview rerun");
    assert!(error.to_string().contains("start a new run"), "{error}");
    assert_eq!(fs::read(workspace.run_file()).unwrap(), before_rerun);
    let verified = seaf_loop::verify_candidate_patch_evidence(&workspace, &source)
        .expect("exact Applied review projection");
    assert_eq!(verified.candidate_tree, applied.candidate_tree);
    assert_eq!(verified.applied_diff_digest, applied.candidate_diff_digest);
    let replayed = apply_candidate_development_evidence(&workspace, &source)
        .expect("exact Applied replay is idempotent");
    assert_eq!(replayed, applied);
    let transaction = applied.patch_transaction.as_ref().unwrap();
    let applied_evidence_path = workspace
        .run_directory()
        .join(&transaction.applied_evidence.as_ref().unwrap().path);
    let applied_evidence_bytes = fs::read(&applied_evidence_path).expect("applied evidence bytes");
    fs::write(
        &applied_evidence_path,
        [applied_evidence_bytes.as_slice(), b"\n"].concat(),
    )
    .expect("tamper applied evidence");
    assert!(apply_candidate_development_evidence(&workspace, &source).is_err());
    assert!(seaf_loop::verify_candidate_patch_evidence(&workspace, &source).is_err());
    fs::write(&applied_evidence_path, &applied_evidence_bytes).expect("restore applied evidence");

    let applied_evidence: serde_json::Value =
        serde_json::from_slice(&applied_evidence_bytes).expect("applied evidence JSON");
    let applied_diff_path = workspace.run_directory().join(
        applied_evidence["observed_candidate_diff"]["path"]
            .as_str()
            .expect("observed diff path"),
    );
    let applied_diff_bytes = fs::read(&applied_diff_path).expect("applied diff bytes");
    fs::write(
        &applied_diff_path,
        [applied_diff_bytes.as_slice(), b"tampered"].concat(),
    )
    .expect("tamper applied diff");
    assert!(apply_candidate_development_evidence(&workspace, &source).is_err());
    assert!(seaf_loop::verify_candidate_patch_evidence(&workspace, &source).is_err());
    fs::write(&applied_diff_path, applied_diff_bytes).expect("restore applied diff");
    assert_eq!(
        apply_candidate_development_evidence(&workspace, &source).expect("restored Applied replay"),
        applied
    );
    synthesize_interrupted_applying(&workspace, false);
    let recovered_staged = apply_candidate_development_evidence(&workspace, &source)
        .expect("recover exact staged state after index mutation");
    assert_same_applied_candidate(&recovered_staged, &applied);
    synthesize_interrupted_applying(&workspace, true);
    let recovered_pristine = apply_candidate_development_evidence(&workspace, &source)
        .expect("recover pristine state after Applying intent");
    assert_same_applied_candidate(&recovered_pristine, &applied);

    synthesize_interrupted_applying(&workspace, false);
    fs::write(Path::new(&candidate.path).join("unrelated.txt"), "drift\n")
        .expect("unrelated drift");
    assert!(
        apply_candidate_development_evidence(&workspace, &source).is_err(),
        "Applying recovery must reject unrelated worktree drift"
    );
    fs::write(Path::new(&candidate.path).join("unrelated.txt"), "stable\n")
        .expect("restore unrelated file");
    apply_candidate_development_evidence(&workspace, &source)
        .expect("recover after unrelated drift is removed");

    synthesize_interrupted_applying(&workspace, false);
    fs::write(Path::new(&candidate.path).join("rogue.txt"), "rogue\n").expect("rogue file");
    git_ok(Path::new(&candidate.path), &["add", "rogue.txt"]);
    assert!(
        apply_candidate_development_evidence(&workspace, &source).is_err(),
        "Applying recovery must reject a partial or extra index tree"
    );
    git_ok(
        Path::new(&candidate.path),
        &["reset", "--hard", "-q", "HEAD"],
    );
    apply_candidate_development_evidence(&workspace, &source)
        .expect("recover after partial index is removed");

    synthesize_interrupted_applying(&workspace, false);
    let candidate_path = Path::new(&candidate.path);
    fs::write(candidate_path.join("tracked.txt"), "coherent tamper\n")
        .expect("tampered changed path");
    git_ok(candidate_path, &["add", "tracked.txt"]);
    let tampered_tree = git(candidate_path, &["write-tree"]);
    let tampered_diff = git_bytes(
        candidate_path,
        &[
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
            "--",
        ],
    );
    let mut tampered_run = seaf_loop::state::load_run(&workspace).expect("Applying run");
    let transaction = tampered_run
        .candidate_workspace
        .as_mut()
        .unwrap()
        .patch_transaction
        .as_mut()
        .unwrap();
    let intent_path = workspace.run_directory().join(&transaction.intent.path);
    let mut intent: serde_json::Value =
        serde_json::from_slice(&fs::read(&intent_path).unwrap()).unwrap();
    let expected_diff_path = workspace
        .run_directory()
        .join(intent["expected_candidate_diff"]["path"].as_str().unwrap());
    fs::write(&expected_diff_path, &tampered_diff).expect("coherent expected diff tamper");
    intent["expected_candidate_tree"] = serde_json::json!(tampered_tree);
    intent["expected_candidate_diff"]["digest"] =
        serde_json::json!(hex::encode(Sha256::digest(&tampered_diff)));
    let intent_bytes = canonical_json_bytes(&intent).expect("canonical tampered intent");
    fs::write(&intent_path, &intent_bytes).expect("coherent intent tamper");
    transaction.intent.digest = hex::encode(Sha256::digest(&intent_bytes));
    seaf_loop::state::save_run(&workspace, &tampered_run).expect("coherent tampered authority");
    assert!(
        apply_candidate_development_evidence(&workspace, &source).is_err(),
        "coherently rewritten intent/diff/index must not replace authoritative Development"
    );
    remove_worktree(&source, Path::new(&candidate.path));
}

struct NoopStepRunner;

impl seaf_loop::StepRunner for NoopStepRunner {
    fn step_request(
        &mut self,
        _step: seaf_core::LoopStepName,
    ) -> Result<String, seaf_loop::RunnerError> {
        Ok(String::new())
    }

    fn run_step(
        &mut self,
        _step: seaf_core::LoopStepName,
        _request: &str,
    ) -> Result<seaf_loop::StepOutput, seaf_loop::RunnerError> {
        Ok(seaf_loop::StepOutput::completed("noop"))
    }
}

#[test]
fn candidate_patch_application_enforces_policy_decision_semantics_before_mutation() {
    for (suffix, decision, apply_requested, already_applied, should_materialize) in [
        (
            "allowed-no-intent",
            PatchDecisionKind::Allowed,
            false,
            false,
            true,
        ),
        (
            "allowed-intent",
            PatchDecisionKind::Allowed,
            true,
            false,
            true,
        ),
        (
            "review-no-intent",
            PatchDecisionKind::RequiresHumanReview,
            false,
            false,
            true,
        ),
        (
            "review-intent",
            PatchDecisionKind::RequiresHumanReview,
            true,
            false,
            true,
        ),
        ("rejected", PatchDecisionKind::Rejected, true, false, false),
        (
            "already-applied",
            PatchDecisionKind::Allowed,
            true,
            true,
            false,
        ),
    ] {
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("source");
        init_repo(&source);
        let run_id = format!("run-policy-{suffix}");
        let (workspace, candidate) =
            persisted_candidate_workspace(temp.path(), &source, &run_id, LoopStatus::Running);
        let patch = "diff --git a/tracked.txt b/tracked.txt\nindex 1f7391f..39c5733 100644\n--- a/tracked.txt\n+++ b/tracked.txt\n@@ -1 +1 @@\n-source\n+candidate\n";
        persist_development_authority(
            &workspace,
            &run_id,
            patch,
            vec!["tracked.txt".to_string()],
            apply_requested,
            decision,
            already_applied,
        );
        let result = apply_candidate_development_evidence(&workspace, &source);
        if should_materialize {
            let applied = result.expect("RequiresHumanReview may materialize candidate-only");
            assert_eq!(
                fs::read_to_string(Path::new(&candidate.path).join("tracked.txt")).unwrap(),
                "candidate\n"
            );
            assert_eq!(
                applied.patch_transaction.as_ref().unwrap().phase,
                seaf_core::CandidatePatchPhase::Applied
            );
        } else {
            assert!(result.is_err(), "unsafe policy state must fail closed");
            assert_eq!(
                fs::read_to_string(Path::new(&candidate.path).join("tracked.txt")).unwrap(),
                "source\n"
            );
            assert_eq!(
                git(Path::new(&candidate.path), &["write-tree"]),
                candidate.starting_tree
            );
            assert!(seaf_loop::state::load_run(&workspace)
                .unwrap()
                .candidate_workspace
                .unwrap()
                .patch_transaction
                .is_none());
        }
        remove_worktree(&source, Path::new(&candidate.path));
    }
}

#[test]
fn candidate_patch_application_requires_completed_development_on_a_running_run() {
    for (suffix, development_status, run_status) in [
        (
            "pending-development",
            seaf_core::LoopStepStatus::Pending,
            LoopStatus::Running,
        ),
        (
            "running-development",
            seaf_core::LoopStepStatus::Running,
            LoopStatus::Running,
        ),
        (
            "terminal-run",
            seaf_core::LoopStepStatus::Completed,
            LoopStatus::Completed,
        ),
    ] {
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("source");
        init_repo(&source);
        let run_id = format!("run-authority-{suffix}");
        let (workspace, candidate) =
            persisted_candidate_workspace(temp.path(), &source, &run_id, LoopStatus::Running);
        let patch = "diff --git a/tracked.txt b/tracked.txt\nindex 1f7391f..39c5733 100644\n--- a/tracked.txt\n+++ b/tracked.txt\n@@ -1 +1 @@\n-source\n+candidate\n";
        persist_development_authority(
            &workspace,
            &run_id,
            patch,
            vec!["tracked.txt".to_string()],
            false,
            PatchDecisionKind::Allowed,
            false,
        );
        let mut run = seaf_loop::state::load_run(&workspace).expect("run");
        run.status = run_status;
        run.steps
            .iter_mut()
            .find(|step| step.name == seaf_core::LoopStepName::Development)
            .unwrap()
            .status = development_status;
        seaf_loop::state::save_run(&workspace, &run).expect("invalid application authority");

        assert!(apply_candidate_development_evidence(&workspace, &source).is_err());
        assert_eq!(
            git(Path::new(&candidate.path), &["write-tree"]),
            candidate.starting_tree
        );
        assert_eq!(
            fs::read(Path::new(&candidate.path).join("tracked.txt")).unwrap(),
            b"source\n"
        );
        assert!(seaf_loop::state::load_run(&workspace)
            .unwrap()
            .candidate_workspace
            .unwrap()
            .patch_transaction
            .is_none());
        remove_worktree(&source, Path::new(&candidate.path));
    }
}

#[test]
fn candidate_patch_application_handles_exact_directory_file_transitions() {
    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    fs::create_dir(source.join("directory")).expect("directory fixture");
    fs::write(source.join("directory/child.txt"), "child\n").expect("directory child");
    fs::write(source.join("flat"), "flat\n").expect("flat fixture");
    git_ok(&source, &["add", "directory/child.txt", "flat"]);
    git_ok(&source, &["commit", "-qm", "add transition fixtures"]);
    let source_head = git(&source, &["rev-parse", "HEAD"]);

    fs::remove_file(source.join("directory/child.txt")).unwrap();
    fs::remove_dir(source.join("directory")).unwrap();
    fs::write(source.join("directory"), "directory became file\n").unwrap();
    fs::remove_file(source.join("flat")).unwrap();
    fs::create_dir(source.join("flat")).unwrap();
    fs::write(source.join("flat/child.txt"), "file became directory\n").unwrap();
    git_ok(&source, &["add", "-A"]);
    let patch = String::from_utf8(git_bytes(
        &source,
        &["diff", "--cached", "--binary", "--full-index", "HEAD", "--"],
    ))
    .unwrap();
    git_ok(&source, &["reset", "--hard", "-q", "HEAD"]);
    let (workspace, candidate) = persisted_candidate_workspace(
        temp.path(),
        &source,
        "run-path-transition",
        LoopStatus::Running,
    );
    let changed_paths = seaf_loop::parse_unified_diff(&patch)
        .expect("transition patch")
        .changed_paths;
    persist_development_authority(
        &workspace,
        "run-path-transition",
        &patch,
        changed_paths,
        false,
        PatchDecisionKind::Allowed,
        false,
    );

    apply_candidate_development_evidence(&workspace, &source)
        .expect("exact directory/file transitions");
    let candidate_path = Path::new(&candidate.path);
    assert_eq!(
        fs::read(candidate_path.join("directory")).unwrap(),
        b"directory became file\n"
    );
    assert_eq!(
        fs::read(candidate_path.join("flat/child.txt")).unwrap(),
        b"file became directory\n"
    );
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), source_head);
    assert!(source.join("directory/child.txt").is_file());
    assert!(source.join("flat").is_file());
    remove_worktree(&source, candidate_path);
}

#[cfg(unix)]
#[test]
fn candidate_patch_application_preserves_raw_add_delete_mode_symlink_and_filter_semantics() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let temp = tempfile::tempdir().expect("temp dir");
    let source = temp.path().join("source");
    init_repo(&source);
    fs::write(source.join("delete.txt"), "delete me\n").expect("delete fixture");
    fs::write(source.join("mode.txt"), "mode\n").expect("mode fixture");
    fs::write(source.join("ident.txt"), "$Id$\n").expect("ident fixture");
    fs::write(source.join("filtered.txt"), "raw filter\n").expect("filter fixture");
    fs::write(
        source.join(".gitattributes"),
        "ident.txt ident\nfiltered.txt filter=hostile\n",
    )
    .expect("attributes");
    git_ok(
        &source,
        &[
            "add",
            "delete.txt",
            "mode.txt",
            "ident.txt",
            "filtered.txt",
            ".gitattributes",
        ],
    );
    git_ok(&source, &["commit", "-qm", "add patch fixtures"]);

    fs::remove_file(source.join("delete.txt")).expect("delete fixture");
    fs::set_permissions(source.join("mode.txt"), fs::Permissions::from_mode(0o755))
        .expect("mode change");
    symlink("tracked.txt", source.join("added-link")).expect("symlink fixture");
    git_ok(&source, &["add", "-N", "added-link"]);
    fs::write(source.join("ident.txt"), "$Id$\nraw ident change\n").expect("ident change");
    fs::write(source.join("filtered.txt"), "raw filter changed\n").expect("filter change");
    let patch = String::from_utf8(git_bytes(
        &source,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
            "--",
        ],
    ))
    .expect("UTF-8 patch");
    git_ok(&source, &["reset", "--hard", "-q", "HEAD"]);
    if source.join("added-link").exists() {
        fs::remove_file(source.join("added-link")).expect("remove untracked link");
    }

    let source_head = git(&source, &["rev-parse", "HEAD"]);
    let source_status = git(
        &source,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let marker = temp.path().join("filter-ran");
    let filter = format!("touch {}; cat", marker.display());
    git_ok(&source, &["config", "filter.hostile.clean", &filter]);
    git_ok(&source, &["config", "filter.hostile.smudge", &filter]);
    git_ok(&source, &["config", "filter.hostile.required", "true"]);
    let (workspace, candidate) =
        persisted_candidate_workspace(temp.path(), &source, "run-raw-apply", LoopStatus::Running);
    let changed_paths = seaf_loop::parse_unified_diff(&patch)
        .expect("generated patch")
        .changed_paths;
    persist_development_authority(
        &workspace,
        "run-raw-apply",
        &patch,
        changed_paths,
        false,
        PatchDecisionKind::Allowed,
        false,
    );

    let applied = apply_candidate_development_evidence(&workspace, &source)
        .expect("raw candidate application");
    let candidate_path = Path::new(&candidate.path);
    assert!(!candidate_path.join("delete.txt").exists());
    assert_eq!(
        fs::metadata(candidate_path.join("mode.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0o111
    );
    assert_eq!(
        fs::read_link(candidate_path.join("added-link")).unwrap(),
        Path::new("tracked.txt")
    );
    assert_eq!(
        fs::read(candidate_path.join("ident.txt")).unwrap(),
        b"$Id$\nraw ident change\n"
    );
    assert_eq!(
        fs::read(candidate_path.join("filtered.txt")).unwrap(),
        b"raw filter changed\n"
    );
    assert!(
        !marker.exists(),
        "configured filter helper must never execute"
    );
    git_ok(&source, &["config", "--unset-all", "filter.hostile.clean"]);
    git_ok(&source, &["config", "--unset-all", "filter.hostile.smudge"]);
    git_ok(
        &source,
        &["config", "--unset-all", "filter.hostile.required"],
    );
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), source_head);
    assert_eq!(
        git(
            &source,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        source_status
    );
    assert_eq!(
        validate_candidate_workspace(workspace.run_directory(), &source, &applied)
            .expect("applied candidate validates"),
        applied
    );
    remove_worktree(&source, candidate_path);
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
    let planned = plan_candidate_workspace(workspace.run_directory(), source, &repository)
        .expect("candidate plan");
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
            repository: repository.clone(),
        },
    });
    run.candidate_workspace = Some(planned);
    run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
    seaf_loop::state::save_run(&workspace, &run).expect("persist candidate plan");
    let candidate =
        seaf_loop::create_candidate_workspace(workspace.run_directory(), source, &repository)
            .expect("candidate");
    let mut run = seaf_loop::state::load_run(&workspace).expect("active candidate run");
    run.status = status;
    seaf_loop::state::save_run(&workspace, &run).expect("persist candidate run status");
    (workspace, candidate)
}

fn sha256_path(path: &Path) -> String {
    let canonical = path.canonicalize().expect("canonical path");
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_os_str().as_encoded_bytes());
    hex::encode(hasher.finalize())
}

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir(destination).expect("copy destination");
    for entry in fs::read_dir(source).expect("copy source") {
        let entry = entry.expect("copy entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type().expect("entry type").is_dir() {
            copy_directory(&source_path, &destination_path);
        } else {
            fs::copy(&source_path, &destination_path).expect("copy file");
        }
    }
}

fn prepare_candidate_workspace(
    run_directory: &Path,
    source: &Path,
    repository: &str,
) -> LoopWorkspace {
    if !run_directory.join("run.json").exists() {
        if run_directory.exists() {
            assert!(
                fs::read_dir(run_directory).unwrap().next().is_none(),
                "candidate fixture run directory must be empty before scaffolding"
            );
            fs::remove_dir(run_directory).unwrap();
        }
        let runs_root = run_directory.parent().expect("runs root");
        let run_id = run_directory
            .file_name()
            .and_then(|value| value.to_str())
            .expect("UTF-8 run ID");
        let workspace = LoopWorkspace::create(runs_root, run_id).expect("fixture workspace");
        let planned = plan_candidate_workspace(workspace.run_directory(), source, repository)
            .expect("fixture plan");
        let mut run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
            run_id: run_id.to_string(),
            ticket_id: "T-fixture".to_string(),
            goal_id: "fixture".to_string(),
            provider: "fake".to_string(),
            model: "model".to_string(),
            input_digests: LoopInputDigests {
                ticket: "1".repeat(64),
                policy: "2".repeat(64),
                config: "3".repeat(64),
                repository: repository.to_string(),
            },
        });
        run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
        run.candidate_workspace = Some(planned);
        seaf_loop::state::save_run(&workspace, &run).expect("fixture authority");
        workspace
    } else {
        LoopWorkspace::open(
            run_directory.parent().expect("runs root"),
            run_directory
                .file_name()
                .and_then(|value| value.to_str())
                .expect("UTF-8 run ID"),
        )
        .expect("existing fixture workspace")
    }
}

fn create_candidate_workspace(
    run_directory: &Path,
    source: &Path,
    repository: &str,
) -> Result<seaf_core::CandidateWorkspaceState, seaf_loop::CandidateWorkspaceError> {
    let workspace = prepare_candidate_workspace(run_directory, source, repository);
    seaf_loop::create_candidate_workspace(workspace.run_directory(), source, repository)
}

fn persist_development_authority(
    workspace: &LoopWorkspace,
    run_id: &str,
    patch: &str,
    changed_paths: Vec<String>,
    apply_requested: bool,
    decision_kind: PatchDecisionKind,
    applied: bool,
) -> DevelopmentEvidence {
    let decision = PolicyDecision {
        patch_id: run_id.to_string(),
        patch_sha256: patch_digest(patch),
        changed_paths: changed_paths.clone(),
        decision: decision_kind,
        reasons: Vec::new(),
        requires_human_review: decision_kind == PatchDecisionKind::RequiresHumanReview,
        apply_requested,
        applied,
    };
    let evidence = DevelopmentEvidence::new(
        run_id,
        DeveloperResponse {
            role: Role::Developer,
            status: DeveloperStatus::PatchProposed,
            summary: "candidate-only patch".to_string(),
            changed_files: changed_paths,
            requires_human_review: decision_kind == PatchDecisionKind::RequiresHumanReview,
            patch: Some(patch.to_string()),
            context_request: None,
        },
        patch,
        decision,
    )
    .expect("Development evidence");
    let artifact_path = "artifacts/05-development.json";
    fs::create_dir_all(workspace.run_directory().join("artifacts")).expect("artifact dir");
    fs::write(
        workspace.run_directory().join(artifact_path),
        evidence.canonical_bytes().expect("canonical evidence"),
    )
    .expect("Development evidence artifact");
    let mut authoritative = seaf_loop::state::load_run(workspace).expect("authoritative run");
    let development = authoritative
        .steps
        .iter_mut()
        .find(|step| step.name == seaf_core::LoopStepName::Development)
        .expect("Development step");
    development.artifact_path = Some(artifact_path.to_string());
    development.artifact_digest = Some(evidence.artifact_digest().expect("evidence digest"));
    development.status = seaf_core::LoopStepStatus::Completed;
    authoritative.current_step = seaf_core::LoopStepName::OutputReview;
    authoritative.policy_decisions.push(
        serde_json::from_value(serde_json::to_value(&evidence.policy_decision).unwrap()).unwrap(),
    );
    seaf_loop::state::save_run(workspace, &authoritative).expect("Development authority");
    evidence
}

fn synthesize_interrupted_applying(workspace: &LoopWorkspace, reset_physical: bool) {
    let mut run = seaf_loop::state::load_run(workspace).expect("Applied run");
    let candidate = run.candidate_workspace.as_mut().expect("candidate");
    let candidate_path = PathBuf::from(&candidate.path);
    if reset_physical {
        git_ok(&candidate_path, &["reset", "--hard", "-q", "HEAD"]);
    }
    candidate.candidate_tree = candidate.starting_tree.clone();
    candidate.candidate_diff_digest = empty_sha256().to_string();
    let transaction = candidate
        .patch_transaction
        .as_mut()
        .expect("patch transaction");
    transaction.phase = seaf_core::CandidatePatchPhase::Applying;
    transaction.applied_evidence = None;
    transaction.applied_at = None;
    seaf_loop::state::save_run(workspace, &run).expect("synthesized Applying authority");
}

fn assert_same_applied_candidate(
    actual: &seaf_core::CandidateWorkspaceState,
    expected: &seaf_core::CandidateWorkspaceState,
) {
    assert_eq!(actual.candidate_tree, expected.candidate_tree);
    assert_eq!(actual.candidate_diff_digest, expected.candidate_diff_digest);
    assert_eq!(
        actual.patch_transaction.as_ref().unwrap().intent,
        expected.patch_transaction.as_ref().unwrap().intent
    );
    assert_eq!(
        actual.patch_transaction.as_ref().unwrap().applied_evidence,
        expected
            .patch_transaction
            .as_ref()
            .unwrap()
            .applied_evidence
    );
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
