use std::{fs, path::Path};

use seaf_core::canonical_json_bytes;
use seaf_loop::{migrate_loop_run, MigrationStatus};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn canonical(value: &Value) -> Vec<u8> {
    canonical_json_bytes(value).expect("canonical JSON")
}

fn write_private(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory");
        #[cfg(unix)]
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .expect("private parent directory");
    }
    fs::write(path, bytes).expect("fixture file");
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("private fixture file");
}

fn read_tree_bytes(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<(String, Vec<u8>)>) {
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
                        .to_string_lossy()
                        .into_owned(),
                    fs::read(path).expect("tree file"),
                ));
            }
        }
    }
    let mut files = Vec::new();
    visit(root, root, &mut files);
    files
}

fn legacy_run_fixture(runs_root: &Path, run_id: &str) -> Vec<(String, Vec<u8>)> {
    let run_dir = runs_root.join(run_id);
    fs::create_dir_all(run_dir.join("inputs")).expect("inputs directory");
    fs::create_dir_all(run_dir.join("artifacts")).expect("artifacts directory");

    let ticket = json!({
        "ticket_id": "T-MIGRATE-001",
        "goal_id": "migration-goal",
        "title": "Migrate one authenticated run",
        "status": "ready",
        "priority": "p1",
        "problem": "Legacy durable files need an explicit version",
        "research_questions": [],
        "context": { "relevant_files": ["src/lib.rs"], "forbidden_files": [] },
        "autonomy": { "level": 1, "apply_patch": false, "allow_shell_commands": [] },
        "acceptance_criteria": ["The run remains authenticated"]
    });
    let policy = json!({
        "policy_id": "migration-policy",
        "default_autonomy_level": 1,
        "forbidden_paths": [".git/**"],
        "requires_human_review": [
            "dependency_changes", "database_migrations", "auth_code", "payment_code",
            "privacy_sensitive_code", "network_permission_changes", "ci_changes",
            "eval_changes", "policy_changes", "updater_changes", "signing_changes"
        ],
        "allowed_without_review": ["tests", "documentation", "non_sensitive_copy_changes"]
    });
    let decision = json!({
        "patch_id": run_id,
        "patch_sha256": format!("sha256:{}", "a".repeat(64)),
        "changed_paths": ["src/lib.rs"],
        "decision": "allowed",
        "reasons": [],
        "requires_human_review": false,
        "apply_requested": false,
        "applied": false
    });
    let ticket_bytes = canonical(&ticket);
    let policy_bytes = canonical(&policy);
    let decision_bytes = canonical(&decision);
    let old_decision_digest = digest(&decision_bytes);
    let testing = canonical(&json!({
        "forensic": "authenticated testing sentinel",
        "payload": old_decision_digest,
        "payload_digest": old_decision_digest,
        "ticket_digest": old_decision_digest,
        "policy_decision_digest": old_decision_digest,
        "policy_decision": {"payload": old_decision_digest}
    }));
    let testing_digest = digest(&testing);
    let candidate = b"candidate diff bytes".to_vec();
    let config = canonical(&json!({"config": "unchanged"}));
    let repository = canonical(&json!({"repository": "unchanged"}));
    let eval_config = canonical(&json!({"eval": "unchanged"}));
    let eval_report = json!({
        "eval_report_id": "eval_legacy-run",
        "patch_id": run_id,
        "goal_id": "migration-goal",
        "passed": true,
        "summary": "legacy integrated report",
        "checks": [{
            "name": "focused",
            "status": "passed",
            "duration_ms": 1,
            "stdout_path": "artifacts/07-testing.check-001.stdout.log",
            "stdout_digest": digest(b""),
            "stderr_path": "artifacts/07-testing.check-001.stderr.log",
            "stderr_digest": digest(b""),
            "summary": null
        }],
        "risk_level": "low",
        "decision": "approve_for_human_review",
        "loop_evidence": {
            "schema_version": 1,
            "run_id": run_id,
            "ticket_id": "T-MIGRATE-001",
            "ticket_digest": digest(&ticket_bytes),
            "eval_config": {"path": "inputs/eval-config.json", "digest": digest(&eval_config)},
            "candidate_diff": {"path": "artifacts/candidate.diff", "digest": digest(&candidate)},
            "starting_head": "d".repeat(40),
            "human_approval_digest": "b".repeat(64),
            "policy_decision_digest": digest(&decision_bytes),
            "testing_evidence": {
                "path": "artifacts/07-testing.json",
                "digest": testing_digest
            }
        }
    });
    let eval_bytes = canonical(&eval_report);

    let mut steps = [
        "research",
        "analysis",
        "spec_creation",
        "spec_review",
        "development",
        "output_review",
        "testing",
        "eval_report",
    ]
    .into_iter()
    .map(|name| json!({"name": name, "status": "pending"}))
    .collect::<Vec<_>>();
    steps[6] = json!({
        "name": "testing",
        "status": "pending",
        "artifact_path": "artifacts/07-testing.json",
        "artifact_digest": testing_digest
    });
    steps[7] = json!({
        "name": "eval_report",
        "status": "pending",
        "artifact_path": "artifacts/08-eval-report.json",
        "artifact_digest": digest(&eval_bytes)
    });
    let run = json!({
        "run_id": run_id,
        "ticket_id": "T-MIGRATE-001",
        "goal_id": "migration-goal",
        "provider": "fake",
        "model": "fake-model",
        "input_digests": {
            "ticket": digest(&ticket_bytes),
            "policy": digest(&policy_bytes),
            "config": digest(&config),
            "repository": digest(&repository),
            "eval_config": digest(&eval_config)
        },
        "execution_mode": "legacy_proposal_only",
        "status": "pending",
        "current_step": "research",
        "started_at": "1",
        "updated_at": "1",
        "steps": steps,
        "policy_decisions": [decision],
        "provider_exchange_records": [],
        "eval_report_path": "artifacts/08-eval-report.json"
    });

    let files = vec![
        ("inputs/ticket.json".to_string(), ticket_bytes),
        ("ticket.snapshot.json".to_string(), canonical(&ticket)),
        ("inputs/policy.json".to_string(), policy_bytes),
        ("inputs/config.json".to_string(), config),
        ("inputs/repository.json".to_string(), repository),
        ("inputs/eval-config.json".to_string(), eval_config),
        (
            format!("artifacts/{run_id}.policy-decision.json"),
            decision_bytes,
        ),
        ("artifacts/07-testing.json".to_string(), testing),
        (
            "artifacts/07-testing.check-001.stdout.log".to_string(),
            Vec::new(),
        ),
        (
            "artifacts/07-testing.check-001.stderr.log".to_string(),
            Vec::new(),
        ),
        ("artifacts/08-eval-report.json".to_string(), eval_bytes),
        ("artifacts/candidate.diff".to_string(), candidate),
        (
            "forensic.json".to_string(),
            b"{\"schema_version\":0}\n".to_vec(),
        ),
        ("provider-exchange.lock".to_string(), Vec::new()),
        ("run.json".to_string(), canonical(&run)),
    ];
    for (relative, bytes) in &files {
        write_private(&run_dir.join(relative), bytes);
    }
    files
}

fn replace_authenticated_testing_payload(run_dir: &Path, value: &Value) -> Vec<u8> {
    let testing = canonical(value);
    fs::write(run_dir.join("artifacts/07-testing.json"), &testing).unwrap();
    let mut report: Value =
        serde_json::from_slice(&fs::read(run_dir.join("artifacts/08-eval-report.json")).unwrap())
            .unwrap();
    report["loop_evidence"]["testing_evidence"]["digest"] = json!(digest(&testing));
    let report = canonical(&report);
    fs::write(run_dir.join("artifacts/08-eval-report.json"), &report).unwrap();
    let mut run: Value =
        serde_json::from_slice(&fs::read(run_dir.join("run.json")).unwrap()).unwrap();
    run["steps"][6]["artifact_digest"] = json!(digest(&testing));
    run["steps"][7]["artifact_digest"] = json!(digest(&report));
    fs::write(run_dir.join("run.json"), canonical(&run)).unwrap();
    testing
}

#[test]
fn whole_run_migration_preserves_authenticated_authority_and_byte_exact_backup() {
    let temp = tempfile::tempdir().expect("temp directory");
    let runs_root = temp.path().join("runs");
    fs::create_dir(&runs_root).expect("runs root");
    let run_id = "legacy-run";
    let original = legacy_run_fixture(&runs_root, run_id);

    let outcome = migrate_loop_run(&runs_root, run_id).expect("legacy migration");

    assert_eq!(outcome.status, MigrationStatus::Migrated);
    let run_dir = runs_root.join(run_id);
    let backup = runs_root.join(format!(".{run_id}.migration-v0-v1.backup"));
    for (relative, bytes) in &original {
        assert_eq!(
            fs::read(backup.join(relative)).unwrap(),
            *bytes,
            "{relative}"
        );
    }
    assert_eq!(
        fs::read(run_dir.join("forensic.json")).unwrap(),
        b"{\"schema_version\":0}\n"
    );
    for relative in [
        "inputs/ticket.json",
        "ticket.snapshot.json",
        "inputs/policy.json",
        "run.json",
        "artifacts/legacy-run.policy-decision.json",
        "artifacts/08-eval-report.json",
    ] {
        let value: Value =
            serde_json::from_slice(&fs::read(run_dir.join(relative)).unwrap()).unwrap();
        assert_eq!(value["schema_version"], 1, "{relative}");
    }

    let run: Value = serde_json::from_slice(&fs::read(run_dir.join("run.json")).unwrap()).unwrap();
    let ticket = fs::read(run_dir.join("inputs/ticket.json")).unwrap();
    let policy = fs::read(run_dir.join("inputs/policy.json")).unwrap();
    let report = fs::read(run_dir.join("artifacts/08-eval-report.json")).unwrap();
    let decision = fs::read(run_dir.join("artifacts/legacy-run.policy-decision.json")).unwrap();
    assert_eq!(run["input_digests"]["ticket"], digest(&ticket));
    assert_eq!(run["input_digests"]["policy"], digest(&policy));
    assert_eq!(run["steps"][7]["artifact_digest"], digest(&report));
    let report: Value = serde_json::from_slice(&report).unwrap();
    assert_eq!(report["loop_evidence"]["ticket_digest"], digest(&ticket));
    assert_eq!(
        report["loop_evidence"]["policy_decision_digest"],
        digest(&decision)
    );
    assert!(run_dir.join("migration-v0-v1.result.json").is_file());

    let retry = migrate_loop_run(&runs_root, run_id).expect("idempotent retry");
    assert_eq!(retry.status, MigrationStatus::AlreadyCurrent);
}

#[test]
fn unsupported_or_malformed_versions_fail_before_any_run_publication() {
    for (case, version, extra_field) in [
        ("explicit-v0", json!(0), false),
        ("future-v2", json!(2), false),
        ("malformed-version", json!("1"), false),
        ("malformed-current", json!(1), true),
    ] {
        let temp = tempfile::tempdir().expect("temp directory");
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).expect("runs root");
        let run_id = format!("reject-{case}");
        legacy_run_fixture(&runs_root, &run_id);
        let ticket_path = runs_root.join(&run_id).join("inputs/ticket.json");
        let mut ticket: Value = serde_json::from_slice(&fs::read(&ticket_path).unwrap()).unwrap();
        ticket["schema_version"] = version;
        if extra_field {
            ticket["unexpected"] = json!(true);
        }
        fs::write(&ticket_path, canonical(&ticket)).unwrap();
        let before = read_tree_bytes(&runs_root.join(&run_id));

        let error = migrate_loop_run(&runs_root, &run_id)
            .expect_err("unsupported or malformed contract must fail closed");

        assert!(
            error.to_string().contains("schema_version")
                || error.to_string().contains("unknown field"),
            "{case}: {error}"
        );
        assert_eq!(read_tree_bytes(&runs_root.join(&run_id)), before, "{case}");
        for suffix in ["intent.json", "staged", "backup"] {
            assert!(
                !runs_root
                    .join(format!(".{run_id}.migration-v0-v1.{suffix}"))
                    .exists(),
                "{case}: no transaction sibling may be published"
            );
        }
    }
}

#[test]
fn failed_authentication_leaves_the_selected_legacy_run_byte_identical() {
    let temp = tempfile::tempdir().expect("temp directory");
    let runs_root = temp.path().join("runs");
    fs::create_dir(&runs_root).expect("runs root");
    let run_id = "tampered-legacy-run";
    legacy_run_fixture(&runs_root, run_id);
    fs::write(
        runs_root.join(run_id).join("artifacts/candidate.diff"),
        b"substituted candidate bytes",
    )
    .unwrap();
    let before = read_tree_bytes(&runs_root.join(run_id));

    let error = migrate_loop_run(&runs_root, run_id)
        .expect_err("tampered authenticated graph must not migrate");

    assert!(error.to_string().contains("digest mismatch"), "{error}");
    assert_eq!(read_tree_bytes(&runs_root.join(run_id)), before);
    assert!(!runs_root
        .join(format!(".{run_id}.migration-v0-v1.intent.json"))
        .exists());
}

#[cfg(unix)]
#[test]
fn migration_rejects_a_symlinked_authenticated_ancestor_without_touching_external_bytes() {
    let temp = tempfile::tempdir().expect("temp directory");
    let runs_root = temp.path().join("runs");
    fs::create_dir(&runs_root).expect("runs root");
    let run_id = "symlinked-inputs";
    legacy_run_fixture(&runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    let external_inputs = temp.path().join("external-inputs");
    fs::rename(run_dir.join("inputs"), &external_inputs).expect("move authenticated inputs");
    symlink(&external_inputs, run_dir.join("inputs")).expect("symlink authenticated ancestor");
    let external_before = read_tree_bytes(&external_inputs);
    let run_before = fs::read(run_dir.join("run.json")).expect("source run");

    let error = migrate_loop_run(&runs_root, run_id)
        .expect_err("an authenticated path must never traverse a symlink ancestor");

    assert!(
        error.to_string().contains("symlink")
            || error.to_string().contains("real 0700 directory")
            || error.to_string().contains("Not a directory"),
        "{error}"
    );
    assert_eq!(read_tree_bytes(&external_inputs), external_before);
    assert_eq!(fs::read(run_dir.join("run.json")).unwrap(), run_before);
    assert!(fs::symlink_metadata(run_dir.join("inputs"))
        .unwrap()
        .file_type()
        .is_symlink());
    for suffix in ["intent.json", "staged", "backup"] {
        assert!(!runs_root
            .join(format!(".{run_id}.migration-v0-v1.{suffix}"))
            .exists());
    }
}

#[test]
fn migration_rewrites_only_digest_fields_and_preserves_matching_payload_strings() {
    let temp = tempfile::tempdir().expect("temp directory");
    let runs_root = temp.path().join("runs");
    fs::create_dir(&runs_root).expect("runs root");
    let run_id = "payload-digest";
    let original = legacy_run_fixture(&runs_root, run_id);
    let original_testing = original
        .iter()
        .find(|(path, _)| path == "artifacts/07-testing.json")
        .map(|(_, bytes)| bytes.clone())
        .expect("testing fixture");
    let old_decision_digest = digest(
        &original
            .iter()
            .find(|(path, _)| path.ends_with(".policy-decision.json"))
            .expect("decision fixture")
            .1,
    );

    migrate_loop_run(&runs_root, run_id).expect("legacy migration");

    let run_dir = runs_root.join(run_id);
    assert_eq!(
        fs::read(run_dir.join("artifacts/07-testing.json")).unwrap(),
        original_testing,
        "arbitrary payload strings and containers are not migration authority"
    );
    let report: Value =
        serde_json::from_slice(&fs::read(run_dir.join("artifacts/08-eval-report.json")).unwrap())
            .unwrap();
    assert_ne!(
        report["loop_evidence"]["policy_decision_digest"],
        old_decision_digest
    );
}

#[test]
fn generic_reachable_payload_does_not_discover_an_incidental_path_digest_pair() {
    let temp = tempfile::tempdir().expect("temp directory");
    let runs_root = temp.path().join("runs");
    fs::create_dir(&runs_root).expect("runs root");
    let run_id = "incidental-reference";
    legacy_run_fixture(&runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    let forensic = fs::read(run_dir.join("forensic.json")).unwrap();
    let payload = json!({
        "content": "this is not an ArtifactReference",
        "path": "forensic.json",
        "digest": digest(&forensic)
    });
    let testing_before = replace_authenticated_testing_payload(&run_dir, &payload);

    migrate_loop_run(&runs_root, run_id)
        .expect("generic payload path/digest fields must not create graph authority");

    assert_eq!(
        fs::read(run_dir.join("artifacts/07-testing.json")).unwrap(),
        testing_before
    );
    assert_eq!(fs::read(run_dir.join("forensic.json")).unwrap(), forensic);
}

#[test]
fn malformed_managed_development_evidence_fails_before_intent_creation() {
    let temp = tempfile::tempdir().expect("temp directory");
    let runs_root = temp.path().join("runs");
    fs::create_dir(&runs_root).expect("runs root");
    let run_id = "malformed-development";
    legacy_run_fixture(&runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    let evidence = canonical(&json!({
        "run_id": run_id,
        "step": "development",
        "role": "developer",
        "policy_decision": {"patch_id": run_id}
    }));
    write_private(&run_dir.join("artifacts/05-development.json"), &evidence);
    let run_path = run_dir.join("run.json");
    let mut run: Value = serde_json::from_slice(&fs::read(&run_path).unwrap()).unwrap();
    run["steps"][4]["artifact_path"] = json!("artifacts/05-development.json");
    run["steps"][4]["artifact_digest"] = json!(digest(&evidence));
    fs::write(&run_path, canonical(&run)).unwrap();
    let before = read_tree_bytes(&run_dir);

    let error = migrate_loop_run(&runs_root, run_id)
        .expect_err("malformed typed Development evidence must fail closed");

    assert!(
        error.to_string().contains("Development evidence"),
        "{error}"
    );
    assert_eq!(read_tree_bytes(&run_dir), before);
    assert!(!runs_root
        .join(format!(".{run_id}.migration-v0-v1.intent.json"))
        .exists());
}
