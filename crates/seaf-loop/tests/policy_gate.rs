use std::{fs, path::Path};

use seaf_core::Policy;
use seaf_loop::{
    gate_patch, CommandOutput, PatchCommand, PatchCommandRunner, PatchDecisionKind, PatchGateError,
    PatchGateRequest,
};

fn fixture(name: &str) -> &'static str {
    match name {
        "allowed-doc.diff" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/patches/allowed-doc.diff"
        )),
        "forbidden-secret.diff" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/patches/forbidden-secret.diff"
        )),
        "path-traversal.diff" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/patches/path-traversal.diff"
        )),
        "binary.diff" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/patches/binary.diff"
        )),
        _ => panic!("unknown fixture: {name}"),
    }
}

#[test]
fn policy_gate_writes_artifacts_without_mutating_when_apply_is_not_requested() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let mut runner = RecordingRunner::default();

    let decision = gate_patch(
        PatchGateRequest {
            repo_root: temp_dir.path(),
            artifact_dir: &temp_dir.path().join("artifacts"),
            patch_id: "patch-001",
            patch: fixture("allowed-doc.diff"),
            policy: &policy(),
            apply_patch: false,
        },
        &mut runner,
    )
    .expect("dry-run gate");

    assert_eq!(decision.decision, PatchDecisionKind::Allowed);
    assert_eq!(decision.changed_paths, vec!["docs/example.md"]);
    assert!(!decision.requires_human_review);
    assert!(!decision.applied);
    assert!(
        runner.commands.is_empty(),
        "dry-run patch artifact creation must not invoke git apply"
    );
    assert!(temp_dir.path().join("artifacts/patch-001.diff").is_file());
    assert!(temp_dir
        .path()
        .join("artifacts/patch-001.policy-decision.json")
        .is_file());
}

#[test]
fn policy_gate_rejects_forbidden_paths_and_never_invokes_apply() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let mut runner = RecordingRunner::default();

    let decision = gate_patch(
        PatchGateRequest {
            repo_root: temp_dir.path(),
            artifact_dir: &temp_dir.path().join("artifacts"),
            patch_id: "patch-forbidden",
            patch: fixture("forbidden-secret.diff"),
            policy: &policy(),
            apply_patch: true,
        },
        &mut runner,
    )
    .expect("forbidden gate");

    assert_eq!(decision.decision, PatchDecisionKind::Rejected);
    assert!(decision
        .reasons
        .iter()
        .any(|reason| reason.code == "forbidden_path"));
    assert!(!decision.applied);
    assert!(
        runner.commands.is_empty(),
        "forbidden paths must be blocked before git apply --check or apply"
    );
}

#[test]
fn policy_gate_rejects_path_traversal_without_mutating_the_working_tree() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo_file = temp_dir.path().join("docs/example.md");
    fs::create_dir_all(repo_file.parent().expect("parent")).expect("create docs");
    fs::write(&repo_file, "old line\n").expect("seed file");
    let mut runner = RecordingRunner::default();

    let decision = gate_patch(
        PatchGateRequest {
            repo_root: temp_dir.path(),
            artifact_dir: &temp_dir.path().join("artifacts"),
            patch_id: "patch-traversal",
            patch: fixture("path-traversal.diff"),
            policy: &policy(),
            apply_patch: true,
        },
        &mut runner,
    )
    .expect("path traversal gate");

    assert_eq!(decision.decision, PatchDecisionKind::Rejected);
    assert!(decision
        .reasons
        .iter()
        .any(|reason| reason.code == "invalid_patch_path"));
    assert_eq!(
        fs::read_to_string(&repo_file).expect("read repo file"),
        "old line\n",
        "path traversal rejection must leave the working tree unchanged"
    );
    assert!(runner.commands.is_empty());
}

#[test]
fn policy_gate_rejects_binary_patches_because_text_review_is_impossible() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let mut runner = RecordingRunner::default();

    let decision = gate_patch(
        PatchGateRequest {
            repo_root: temp_dir.path(),
            artifact_dir: &temp_dir.path().join("artifacts"),
            patch_id: "patch-binary",
            patch: fixture("binary.diff"),
            policy: &policy(),
            apply_patch: true,
        },
        &mut runner,
    )
    .expect("binary gate");

    assert_eq!(decision.decision, PatchDecisionKind::Rejected);
    assert!(decision
        .reasons
        .iter()
        .any(|reason| reason.code == "binary_patch"));
    assert!(runner.commands.is_empty());
}

#[test]
fn policy_gate_requires_human_review_for_ci_eval_dependency_and_sensitive_paths() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let mut runner = RecordingRunner::default();
    let patch = r#"diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml
index 1111111..2222222 100644
--- a/.github/workflows/ci.yml
+++ b/.github/workflows/ci.yml
@@ -1 +1 @@
-old
+new
diff --git a/Cargo.lock b/Cargo.lock
index 1111111..2222222 100644
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1 +1 @@
-old
+new
"#;

    let decision = gate_patch(
        PatchGateRequest {
            repo_root: temp_dir.path(),
            artifact_dir: &temp_dir.path().join("artifacts"),
            patch_id: "patch-review",
            patch,
            policy: &policy(),
            apply_patch: true,
        },
        &mut runner,
    )
    .expect("review gate");

    assert_eq!(decision.decision, PatchDecisionKind::RequiresHumanReview);
    assert!(decision.requires_human_review);
    assert!(decision
        .reasons
        .iter()
        .any(|reason| reason.code == "review_required_ci"));
    assert!(decision
        .reasons
        .iter()
        .any(|reason| reason.code == "review_required_dependency"));
    assert!(
        runner.commands.is_empty(),
        "review-required patches must wait for human approval instead of applying"
    );
}

#[test]
fn policy_gate_rejects_malformed_git_header_in_dry_run() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let mut runner = RecordingRunner::default();
    let patch = r#"diff --git a/docs/a.md b/docs/a.md extra
index 1111111..2222222 100644
--- a/docs/a.md
+++ b/docs/a.md
@@ -1 +1 @@
-old
+new
"#;

    let decision = gate_patch(
        PatchGateRequest {
            repo_root: temp_dir.path(),
            artifact_dir: &temp_dir.path().join("artifacts"),
            patch_id: "patch-malformed-header",
            patch,
            policy: &policy(),
            apply_patch: false,
        },
        &mut runner,
    )
    .expect("malformed header gate");

    assert_eq!(decision.decision, PatchDecisionKind::Rejected);
    assert!(decision
        .reasons
        .iter()
        .any(|reason| reason.code == "invalid_patch"));
    assert!(runner.commands.is_empty());
}

#[test]
fn policy_gate_requires_human_review_for_all_review_required_categories() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let mut runner = RecordingRunner::default();
    let patch = r#"diff --git a/evals/smoke.yml b/evals/smoke.yml
index 1111111..2222222 100644
--- a/evals/smoke.yml
+++ b/evals/smoke.yml
@@ -1 +1 @@
-old
+new
diff --git a/docs/security/forbidden-shortcuts.md b/docs/security/forbidden-shortcuts.md
index 1111111..2222222 100644
--- a/docs/security/forbidden-shortcuts.md
+++ b/docs/security/forbidden-shortcuts.md
@@ -1 +1 @@
-old
+new
diff --git a/src/updater/metadata.rs b/src/updater/metadata.rs
index 1111111..2222222 100644
--- a/src/updater/metadata.rs
+++ b/src/updater/metadata.rs
@@ -1 +1 @@
-old
+new
diff --git a/docs/signing/process.md b/docs/signing/process.md
index 1111111..2222222 100644
--- a/docs/signing/process.md
+++ b/docs/signing/process.md
@@ -1 +1 @@
-old
+new
diff --git a/src/auth/session.rs b/src/auth/session.rs
index 1111111..2222222 100644
--- a/src/auth/session.rs
+++ b/src/auth/session.rs
@@ -1 +1 @@
-old
+new
diff --git a/src/billing/stripe.rs b/src/billing/stripe.rs
index 1111111..2222222 100644
--- a/src/billing/stripe.rs
+++ b/src/billing/stripe.rs
@@ -1 +1 @@
-old
+new
"#;

    let decision = gate_patch(
        PatchGateRequest {
            repo_root: temp_dir.path(),
            artifact_dir: &temp_dir.path().join("artifacts"),
            patch_id: "patch-review-categories",
            patch,
            policy: &policy(),
            apply_patch: true,
        },
        &mut runner,
    )
    .expect("review category gate");

    assert_eq!(decision.decision, PatchDecisionKind::RequiresHumanReview);
    for code in [
        "review_required_eval",
        "review_required_policy",
        "review_required_updater",
        "review_required_signing",
        "review_required_auth",
        "review_required_payment",
    ] {
        assert!(
            decision.reasons.iter().any(|reason| reason.code == code),
            "{code} should require human review"
        );
    }
    assert!(runner.commands.is_empty());
}

#[test]
fn policy_gate_uses_default_policy_category_keys_for_review_required_paths() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let mut runner = RecordingRunner::default();
    let patch = r#"diff --git a/Cargo.lock b/Cargo.lock
index 1111111..2222222 100644
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1 +1 @@
-old
+new
diff --git a/db/migrations/001_create_users.sql b/db/migrations/001_create_users.sql
index 1111111..2222222 100644
--- a/db/migrations/001_create_users.sql
+++ b/db/migrations/001_create_users.sql
@@ -1 +1 @@
-old
+new
diff --git a/src/auth/session.rs b/src/auth/session.rs
index 1111111..2222222 100644
--- a/src/auth/session.rs
+++ b/src/auth/session.rs
@@ -1 +1 @@
-old
+new
diff --git a/src/billing/stripe.rs b/src/billing/stripe.rs
index 1111111..2222222 100644
--- a/src/billing/stripe.rs
+++ b/src/billing/stripe.rs
@@ -1 +1 @@
-old
+new
diff --git a/src/privacy/pii.rs b/src/privacy/pii.rs
index 1111111..2222222 100644
--- a/src/privacy/pii.rs
+++ b/src/privacy/pii.rs
@@ -1 +1 @@
-old
+new
diff --git a/src/network/permissions.rs b/src/network/permissions.rs
index 1111111..2222222 100644
--- a/src/network/permissions.rs
+++ b/src/network/permissions.rs
@@ -1 +1 @@
-old
+new
"#;

    let decision = gate_patch(
        PatchGateRequest {
            repo_root: temp_dir.path(),
            artifact_dir: &temp_dir.path().join("artifacts"),
            patch_id: "patch-default-policy-categories",
            patch,
            policy: &default_policy(),
            apply_patch: true,
        },
        &mut runner,
    )
    .expect("default policy category gate");

    assert_eq!(decision.decision, PatchDecisionKind::RequiresHumanReview);
    for (code, pattern) in [
        ("review_required_dependency", "dependency_changes"),
        ("review_required_database_migration", "database_migrations"),
        ("review_required_auth", "auth_code"),
        ("review_required_payment", "payment_code"),
        ("review_required_privacy", "privacy_sensitive_code"),
        (
            "review_required_network_permission",
            "network_permission_changes",
        ),
    ] {
        assert!(
            decision
                .reasons
                .iter()
                .any(|reason| reason.code == code && reason.pattern.as_deref() == Some(pattern)),
            "{code} should be gated by canonical policy key {pattern}"
        );
    }
    assert!(runner.commands.is_empty());
}

#[test]
fn policy_gate_does_not_escalate_category_absent_from_policy() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let mut runner = RecordingRunner::default();
    let patch = r#"diff --git a/Cargo.lock b/Cargo.lock
index 1111111..2222222 100644
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1 +1 @@
-old
+new
"#;

    let decision = gate_patch(
        PatchGateRequest {
            repo_root: temp_dir.path(),
            artifact_dir: &temp_dir.path().join("artifacts"),
            patch_id: "patch-no-category",
            patch,
            policy: &policy_with_review_entries(["auth_code"]),
            apply_patch: false,
        },
        &mut runner,
    )
    .expect("absent category gate");

    assert_eq!(decision.decision, PatchDecisionKind::Allowed);
    assert!(
        decision.reasons.is_empty(),
        "dependency paths should not require review when dependency_changes is absent"
    );
}

#[test]
fn policy_gate_still_respects_path_like_review_patterns() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let mut runner = RecordingRunner::default();
    let patch = r#"diff --git a/src/risky/change.rs b/src/risky/change.rs
index 1111111..2222222 100644
--- a/src/risky/change.rs
+++ b/src/risky/change.rs
@@ -1 +1 @@
-old
+new
"#;

    let decision = gate_patch(
        PatchGateRequest {
            repo_root: temp_dir.path(),
            artifact_dir: &temp_dir.path().join("artifacts"),
            patch_id: "patch-path-pattern",
            patch,
            policy: &policy_with_review_entries(["src/risky/**"]),
            apply_patch: false,
        },
        &mut runner,
    )
    .expect("path-like review pattern gate");

    assert_eq!(decision.decision, PatchDecisionKind::RequiresHumanReview);
    assert!(decision
        .reasons
        .iter()
        .any(|reason| reason.code == "policy_requires_human_review"
            && reason.pattern.as_deref() == Some("src/risky/**")));
}

#[test]
fn policy_gate_runs_git_apply_check_before_explicit_apply() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let mut runner = RecordingRunner::default();

    let decision = gate_patch(
        PatchGateRequest {
            repo_root: temp_dir.path(),
            artifact_dir: &temp_dir.path().join("artifacts"),
            patch_id: "patch-apply",
            patch: fixture("allowed-doc.diff"),
            policy: &policy(),
            apply_patch: true,
        },
        &mut runner,
    )
    .expect("apply gate");

    assert_eq!(decision.decision, PatchDecisionKind::Allowed);
    assert!(decision.applied);
    assert_eq!(
        runner.commands,
        vec![PatchCommand::GitApplyCheck, PatchCommand::GitApply],
        "git apply --check must be the positive control before mutation"
    );
}

#[test]
fn policy_gate_bad_patch_stops_after_apply_check_and_preserves_files() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo_file = temp_dir.path().join("docs/example.md");
    fs::create_dir_all(repo_file.parent().expect("parent")).expect("create docs");
    fs::write(&repo_file, "actual content\n").expect("seed file");
    let mut runner = RecordingRunner {
        fail_check: true,
        ..RecordingRunner::default()
    };

    let decision = gate_patch(
        PatchGateRequest {
            repo_root: temp_dir.path(),
            artifact_dir: &temp_dir.path().join("artifacts"),
            patch_id: "patch-bad",
            patch: fixture("allowed-doc.diff"),
            policy: &policy(),
            apply_patch: true,
        },
        &mut runner,
    )
    .expect("bad patch gate");

    assert_eq!(decision.decision, PatchDecisionKind::Rejected);
    assert!(decision
        .reasons
        .iter()
        .any(|reason| reason.code == "git_apply_check_failed"));
    let check_failure = decision
        .reasons
        .iter()
        .find(|reason| reason.code == "git_apply_check_failed")
        .expect("check failure reason");
    assert_eq!(check_failure.pattern, None);
    assert_eq!(
        check_failure.details.as_deref(),
        Some("patch does not apply")
    );
    assert_eq!(runner.commands, vec![PatchCommand::GitApplyCheck]);
    assert_eq!(
        fs::read_to_string(repo_file).expect("read repo file"),
        "actual content\n",
        "bad patches must leave the working tree unchanged"
    );
}

#[test]
fn policy_gate_decision_artifact_has_structured_shape() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let mut runner = RecordingRunner::default();

    let decision = gate_patch(
        PatchGateRequest {
            repo_root: temp_dir.path(),
            artifact_dir: &temp_dir.path().join("artifacts"),
            patch_id: "patch-shape",
            patch: fixture("allowed-doc.diff"),
            policy: &policy(),
            apply_patch: false,
        },
        &mut runner,
    )
    .expect("shape gate");

    let artifact = fs::read_to_string(
        temp_dir
            .path()
            .join("artifacts/patch-shape.policy-decision.json"),
    )
    .expect("decision artifact");
    let value: serde_json::Value = serde_json::from_str(&artifact).expect("decision json");

    assert_eq!(value["patch_id"], "patch-shape");
    assert_eq!(value["decision"], "allowed");
    assert_eq!(
        value["changed_paths"],
        serde_json::json!(["docs/example.md"])
    );
    assert_eq!(value["requires_human_review"], false);
    assert_eq!(value["applied"], false);
    assert_eq!(
        serde_json::from_value::<seaf_loop::PolicyDecision>(value).expect("typed decision"),
        decision
    );
}

#[derive(Default)]
struct RecordingRunner {
    commands: Vec<PatchCommand>,
    fail_check: bool,
}

impl PatchCommandRunner for RecordingRunner {
    fn run(
        &mut self,
        _repo_root: &Path,
        command: PatchCommand,
        _patch: &str,
    ) -> Result<CommandOutput, PatchGateError> {
        self.commands.push(command);
        if self.fail_check && command == PatchCommand::GitApplyCheck {
            return Ok(CommandOutput::failure("patch does not apply"));
        }
        Ok(CommandOutput::success())
    }
}

fn policy() -> Policy {
    Policy {
        policy_id: "test-policy".to_string(),
        default_autonomy_level: 1,
        forbidden_paths: vec!["secrets/**".to_string(), "infra/signing/**".to_string()],
        requires_human_review: vec![
            "dependency_changes".to_string(),
            "eval_changes".to_string(),
            "ci_changes".to_string(),
            "policy_changes".to_string(),
            "updater_changes".to_string(),
            "signing_changes".to_string(),
            "auth_code".to_string(),
            "payment_code".to_string(),
        ],
        allowed_without_review: Vec::new(),
    }
}

fn default_policy() -> Policy {
    policy_with_review_entries([
        "dependency_changes",
        "database_migrations",
        "auth_code",
        "payment_code",
        "privacy_sensitive_code",
        "network_permission_changes",
    ])
}

fn policy_with_review_entries<const N: usize>(entries: [&str; N]) -> Policy {
    Policy {
        policy_id: "test-policy".to_string(),
        default_autonomy_level: 1,
        forbidden_paths: vec!["secrets/**".to_string(), "infra/signing/**".to_string()],
        requires_human_review: entries.into_iter().map(str::to_string).collect(),
        allowed_without_review: Vec::new(),
    }
}
