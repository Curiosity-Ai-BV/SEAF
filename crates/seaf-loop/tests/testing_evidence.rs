use seaf_core::{ArtifactReference, CheckStatus, EvalCheck};
use seaf_loop::{LoopWorkspace, TestingEvidence};

fn evidence() -> TestingEvidence {
    TestingEvidence {
        schema_version: 1,
        evaluation_attempt: None,
        recovery: None,
        execution_intent: None,
        run_id: "run-1".to_string(),
        ticket_id: "ticket-1".to_string(),
        goal_id: "goal-1".to_string(),
        approved_run_digest: "1".repeat(64),
        ticket_digest: "2".repeat(64),
        eval_config: ArtifactReference {
            path: "inputs/eval-config.json".to_string(),
            digest: "3".repeat(64),
        },
        candidate_diff: ArtifactReference {
            path: "artifacts/candidate-patch.applied.diff".to_string(),
            digest: "4".repeat(64),
        },
        starting_head: "5".repeat(40),
        human_approval_digest: "6".repeat(64),
        policy_decision_digest: "7".repeat(64),
        started_at: "100".to_string(),
        completed_at: "101".to_string(),
        checks: vec![EvalCheck {
            name: "unit".to_string(),
            status: CheckStatus::Passed,
            duration_ms: Some(1),
            stdout_path: Some("artifacts/eval/unit.stdout.log".to_string()),
            stdout_digest: Some("8".repeat(64)),
            stderr_path: Some("artifacts/eval/unit.stderr.log".to_string()),
            stderr_digest: Some("9".repeat(64)),
            summary: Some("passed".to_string()),
        }],
        passed: true,
    }
}

#[test]
fn testing_evidence_accepts_exact_ordered_check_and_log_bindings() {
    evidence().validate().expect("exact evidence");
}

#[test]
fn testing_evidence_rejects_aggregate_and_log_pair_mismatches() {
    let mut invalid = evidence();
    invalid.passed = false;
    invalid.checks[0].stdout_digest = None;

    let error = invalid.validate().expect_err("mismatched evidence");

    assert!(error.to_string().contains("aggregate"));
    assert!(error.to_string().contains("stdout"));
}

#[test]
fn testing_evidence_rejects_completion_before_start() {
    let mut invalid = evidence();
    invalid.completed_at = "99".to_string();

    let error = invalid.validate().expect_err("time cannot move backwards");

    assert!(error.to_string().contains("completed_at"));
}

#[test]
fn testing_evidence_requires_canonical_bounded_unix_seconds() {
    for timestamp in ["", "01", "-1", "1.0", "18446744073709551616"] {
        let mut invalid = evidence();
        invalid.started_at = timestamp.to_string();

        let error = invalid.validate().expect_err("noncanonical timestamp");

        assert!(
            error.to_string().contains("started_at"),
            "{timestamp}: {error}"
        );
    }
}

#[test]
fn testing_evidence_rejects_nonportable_raw_artifact_paths() {
    for path in [
        r"artifacts\eval\unit.stdout.log",
        "C:/artifacts/eval/unit.stdout.log",
        "artifacts//eval/unit.stdout.log",
    ] {
        let mut invalid = evidence();
        invalid.checks[0].stdout_path = Some(path.to_string());

        let error = invalid.validate().expect_err("nonportable path");

        assert!(error.to_string().contains("stdout_path"), "{error}");
    }
}

#[test]
fn testing_evidence_rejects_reused_log_paths() {
    let mut invalid = evidence();
    invalid.checks[0].stderr_path = invalid.checks[0].stdout_path.clone();

    let error = invalid.validate().expect_err("reused log path");

    assert!(error.to_string().contains("duplicated log path"));
}

#[test]
fn testing_evidence_denies_unknown_fields() {
    let mut value = serde_json::to_value(evidence()).unwrap();
    value["untrusted"] = serde_json::json!(true);

    let error = serde_json::from_value::<TestingEvidence>(value).unwrap_err();

    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn testing_evidence_loads_only_canonical_digest_bound_run_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = LoopWorkspace::create(&temp.path().join("runs"), "run-1").unwrap();
    let evidence = evidence();
    let bytes = evidence.canonical_bytes().unwrap();
    let reference = ArtifactReference {
        path: "artifacts/07-testing.json".to_string(),
        digest: evidence.artifact_digest().unwrap(),
    };
    seaf_loop::workspace::write_artifact(workspace.run_directory(), &reference.path, &bytes)
        .unwrap();

    let loaded = TestingEvidence::load(&workspace, &reference, "run-1").unwrap();
    assert_eq!(loaded, evidence);

    let mut substituted = reference;
    substituted.digest = "0".repeat(64);
    let error =
        TestingEvidence::load(&workspace, &substituted, "run-1").expect_err("substituted digest");
    assert!(error.to_string().contains("digest mismatch"));
}
