use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, LoopInputDigests, LoopStatus, Policy,
    TicketAutonomy, TicketContext, TicketPriority, TicketStatus,
};
use seaf_models::{FakeProvider, ModelRequest, ModelResponse};

use super::*;
use crate::{
    policy_gate::{
        CommandOutput, GitCommandRunner, PatchCommand, PatchCommandRunner, PatchGateError,
    },
    recovery::{revise_provider_step, RecoveryAttemptV1, RecoverySourceRunV1},
    runner::{AuthoritativeRunInputSnapshots, InitializedLoopRun, LoopRunner, LoopRunnerConfig},
};

#[test]
fn spec_creation_recovery_uses_authenticated_reviewer_feedback() {
    let fixture = blocked_spec_review_fixture("spec-creation-revision-recovery");
    let blocked = crate::state::load_run(&fixture.workspace).expect("blocked run");
    assert_eq!(blocked.status, LoopStatus::Blocked);
    assert_eq!(blocked.current_step, LoopStepName::SpecReview);

    let initial_spec_request = initial_request_user_json(
        &fixture.workspace,
        &blocked,
        LoopStepName::SpecCreation,
        1,
    );
    assert!(
        initial_spec_request.get("revision_context").is_none(),
        "the first Spec Creation request must retain its original shape"
    );

    let prior_spec = load_role_artifact(
        &fixture.workspace,
        &blocked,
        LoopStepName::SpecCreation,
        Role::SpecWriter,
    );
    let reviewer_feedback = load_role_artifact(
        &fixture.workspace,
        &blocked,
        LoopStepName::SpecReview,
        Role::SpecReviewer,
    );
    assert!(matches!(
        reviewer_feedback.artifact.response,
        RoleResponse::Reviewer(ref response)
            if response.decision == ReviewDecision::RequestChanges
    ));
    let original_provider_ledger = blocked.provider_exchange_records.clone();
    let immutable_attempt_one = run_files(fixture.workspace.run_directory())
        .into_iter()
        .filter(|(path, _)| path != Path::new("run.json") && path != Path::new("log.md"))
        .collect::<BTreeMap<_, _>>();

    let revision = revise_provider_step(
        &fixture.workspace,
        LoopStepName::SpecCreation,
        "operator@example.invalid",
        "address authenticated Spec Review feedback",
    )
    .expect("create authenticated Spec Creation recovery");
    assert_eq!(revision.recovery.source_step_attempt, 1);
    assert_eq!(revision.recovery.next_step_attempt, 2);

    let initialized = InitializedLoopRun::resume_isolated_for_rerun(
        &fixture.runs_root,
        revision.run,
        LoopStepName::SpecCreation,
    )
    .expect("resume isolated Spec Creation rerun");
    let prepared = initialized
        .scaffold()
        .expect("resume scaffold")
        .publish_authoritative_inputs(fixture.snapshots.clone())
        .expect("resume authoritative inputs");
    let provider = FakeProvider::new(revised_provider_responses());
    let mut patch_runner = RecordingGitCommandRunner::default();
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(fixture.ticket.clone())
        .with_context_pack_request(context_request(&fixture.candidate, &fixture.ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(
                &fixture.candidate,
                &fixture.ticket,
                fixture.policy.clone(),
                true,
            ),
            &mut patch_runner,
        )
        .with_recovery_attempt(LoopStepName::SpecCreation, 2);
    let mut runner = LoopRunner::resume_initialized(prepared, &mut step_runner)
        .expect("resume authenticated rerun");
    runner
        .run_to_completion()
        .expect("finish revised provider steps");
    assert_eq!(runner.run().status, LoopStatus::AwaitingHumanReview);
    drop(runner);
    drop(step_runner);

    let requests = provider.requests().expect("recorded model requests");
    let revised_spec_request: serde_json::Value =
        serde_json::from_str(&requests[0].messages[0].content).expect("structured Spec Creation input");
    assert_eq!(
        revised_spec_request["revision_context"]["prior_spec"],
        serde_json::to_value(&prior_spec).expect("prior spec JSON")
    );
    assert_eq!(
        revised_spec_request["revision_context"]["reviewer_feedback"],
        serde_json::to_value(&reviewer_feedback).expect("reviewer feedback JSON")
    );
    assert!(
        !requests[0]
            .messages[0]
            .content
            .contains("address authenticated Spec Review feedback"),
        "operator audit reason must never enter the model request"
    );
    assert!(
        !requests[0]
            .messages[0]
            .content
            .contains("operator@example.invalid"),
        "operator actor must never enter the model request"
    );

    let completed = crate::state::load_run(&fixture.workspace).expect("completed revised run");
    assert!(
        completed
            .provider_exchange_records
            .starts_with(&original_provider_ledger),
        "the original provider ledger must remain an immutable prefix"
    );
    for attempt in [1, 2] {
        assert!(completed.provider_exchange_records.iter().any(|record| {
            record.step == LoopStepName::SpecCreation && record.step_attempt == attempt
        }));
    }
    let completed_files = run_files(fixture.workspace.run_directory());
    let revised_spec_path = completed
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::SpecCreation)
        .and_then(|record| record.artifact_path.as_deref())
        .expect("revised Spec Creation artifact path");
    assert_ne!(revised_spec_path, prior_spec.artifact_path);
    assert!(completed_files.contains_key(Path::new(&prior_spec.artifact_path)));
    assert!(completed_files.contains_key(Path::new(revised_spec_path)));
    for (path, bytes) in immutable_attempt_one {
        assert_eq!(
            completed_files.get(&path),
            Some(&bytes),
            "attempt-one immutable file changed: {}",
            path.display()
        );
    }

    git_ok(
        &fixture.source,
        &["worktree", "remove", "--force", fixture.candidate.to_str().unwrap()],
    );
}

#[test]
fn spec_creation_revision_recovery_rejects_substituted_source_artifact_digest() {
    let fixture = blocked_spec_review_fixture("spec-revision-substituted-digest");
    let reset = rewrite_recovery_source(&fixture, |source, _| {
        source
            .run
            .steps
            .iter_mut()
            .find(|record| record.name == LoopStepName::SpecCreation)
            .unwrap()
            .artifact_digest = Some("f".repeat(64));
    });

    assert_revision_context_rejected_before_provider(
        &fixture,
        reset,
        "failed to verify source SpecCreation artifact",
    );
}

#[test]
fn spec_creation_revision_recovery_rejects_source_status_mismatch() {
    let fixture = blocked_spec_review_fixture("spec-revision-source-status");
    let reset = rewrite_recovery_source(&fixture, |source, _| {
        source.run.status = LoopStatus::Failed;
    });

    assert_revision_context_rejected_before_provider(
        &fixture,
        reset,
        "source run must be Blocked at SpecReview",
    );
}

#[test]
fn spec_creation_revision_recovery_rejects_spec_review_status_mismatch() {
    let fixture = blocked_spec_review_fixture("spec-revision-review-status");
    let reset = rewrite_recovery_source(&fixture, |source, _| {
        source
            .run
            .steps
            .iter_mut()
            .find(|record| record.name == LoopStepName::SpecReview)
            .unwrap()
            .status = LoopStepStatus::Failed;
    });

    assert_revision_context_rejected_before_provider(
        &fixture,
        reset,
        "source SpecReview status must be Blocked",
    );
}

#[test]
fn spec_creation_revision_recovery_rejects_non_completed_prior_spec() {
    let fixture = blocked_spec_review_fixture("spec-revision-prior-status");
    let reset = rewrite_recovery_source(&fixture, |source, _| {
        source
            .run
            .steps
            .iter_mut()
            .find(|record| record.name == LoopStepName::SpecCreation)
            .unwrap()
            .status = LoopStepStatus::Blocked;
    });

    assert_revision_context_rejected_before_provider(
        &fixture,
        reset,
        "source SpecCreation status must be Completed",
    );
}

#[test]
fn spec_creation_revision_recovery_rejects_non_request_changes_decision() {
    let fixture = blocked_spec_review_fixture("spec-revision-review-decision");
    let reset = rewrite_recovery_source(&fixture, |source, workspace| {
        let response = parse_role_response(
            Role::SpecReviewer,
            &serde_json::json!({
                "role": "spec_reviewer",
                "decision": "approve_spec",
                "summary": "This substituted review approves the prior spec.",
                "blocking_issues": [],
                "non_blocking_issues": []
            })
            .to_string(),
        )
        .expect("valid substituted reviewer response");
        let artifact = ValidatedRoleArtifact::new(
            source.run.run_id.clone(),
            LoopStepName::SpecReview,
            Role::SpecReviewer,
            response,
        )
        .expect("substituted review artifact");
        let record = source
            .run
            .steps
            .iter_mut()
            .find(|record| record.name == LoopStepName::SpecReview)
            .unwrap();
        let path = record.artifact_path.as_deref().unwrap();
        let bytes = artifact.canonical_bytes().unwrap();
        crate::artifact_safety::write_private_fixture(
            workspace.run_directory().join(path),
            &bytes,
        )
        .unwrap();
        record.artifact_digest = Some(artifact.artifact_digest().unwrap());
    });

    assert_revision_context_rejected_before_provider(
        &fixture,
        reset,
        "source SpecReview decision must be RequestChanges",
    );
}

#[test]
fn spec_creation_revision_recovery_rejects_requested_attempt_mismatch_without_mutation() {
    let fixture = blocked_spec_review_fixture("spec-revision-attempt-mismatch");
    let revision = revise_provider_step(
        &fixture.workspace,
        LoopStepName::SpecCreation,
        "operator@example.invalid",
        "address authenticated Spec Review feedback",
    )
    .expect("create authenticated recovery");

    assert_revision_context_rejected_before_provider_at_attempt(
        &fixture,
        revision.run,
        3,
        "provider attempt does not match exact latest recovery authorization",
    );
}

#[test]
fn spec_creation_revision_recovery_rejects_wrong_recovery_step_without_mutation() {
    let fixture = blocked_spec_review_fixture("spec-revision-step-mismatch");
    let revision = revise_provider_step(
        &fixture.workspace,
        LoopStepName::Analysis,
        "operator@example.invalid",
        "revise analysis instead of the requested spec",
    )
    .expect("create authenticated Analysis recovery");

    assert_revision_context_rejected_before_provider_with_coordinates(
        &fixture,
        revision.run,
        LoopStepName::Analysis,
        LoopStepName::SpecCreation,
        2,
        "recovery attempt does not match the exact next runnable step",
    );
}

#[test]
fn upstream_recovery_replays_spec_creation_without_revision_context() {
    for (upstream, slug, spec_request_index) in [
        (LoopStepName::Research, "research-recovery-spec-replay", 2),
        (LoopStepName::Analysis, "analysis-recovery-spec-replay", 1),
    ] {
        let fixture = blocked_spec_review_fixture(slug);
        let revision = revise_provider_step(
            &fixture.workspace,
            upstream,
            "operator@example.invalid",
            "replay the complete provider chain",
        )
        .expect("create authenticated upstream recovery");
        let initialized = InitializedLoopRun::resume_isolated_for_rerun(
            &fixture.runs_root,
            revision.run,
            upstream,
        )
        .expect("resume isolated upstream rerun");
        let prepared = initialized
            .scaffold()
            .expect("resume scaffold")
            .publish_authoritative_inputs(fixture.snapshots.clone())
            .expect("resume authoritative inputs");
        let provider = FakeProvider::new(
            initial_provider_responses()
                .into_iter()
                .skip(2 - spec_request_index)
                .collect(),
        );
        let mut patch_runner = RecordingGitCommandRunner::default();
        let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
            .with_ticket(fixture.ticket.clone())
            .with_context_pack_request(context_request(&fixture.candidate, &fixture.ticket))
            .with_patch_gate(
                ProviderPatchGateConfig::for_ticket(
                    &fixture.candidate,
                    &fixture.ticket,
                    fixture.policy.clone(),
                    true,
                ),
                &mut patch_runner,
            )
            .with_recovery_attempt(upstream, 2);
        let mut runner = LoopRunner::resume_initialized(prepared, &mut step_runner)
            .expect("resume authenticated upstream recovery");
        runner
            .run_to_completion()
            .expect("upstream recovery must replay downstream Spec Creation normally");
        assert_eq!(runner.run().status, LoopStatus::Blocked);
        drop(runner);
        drop(step_runner);

        let requests = provider.requests().unwrap();
        assert_eq!(requests.len(), spec_request_index + 2);
        let spec_creation: serde_json::Value =
            serde_json::from_str(&requests[spec_request_index].messages[0].content).unwrap();
        assert!(
            spec_creation.get("revision_context").is_none(),
            "an upstream recovery must not relabel downstream Spec Creation as a direct revision"
        );
        let completed = crate::state::load_run(&fixture.workspace).unwrap();
        assert!(completed.provider_exchange_records.iter().any(|record| {
            record.step == LoopStepName::SpecCreation && record.step_attempt == 2
        }));
        assert!(patch_runner.calls.is_empty());

        git_ok(
            &fixture.source,
            &["worktree", "remove", "--force", fixture.candidate.to_str().unwrap()],
        );
    }
}

#[test]
fn ordinary_resume_reuses_interrupted_upstream_spec_creation_request_without_revision_context() {
    for (upstream, slug, calls_before_spec) in [
        (
            LoopStepName::Research,
            "interrupted-research-recovery-spec-replay",
            2,
        ),
        (
            LoopStepName::Analysis,
            "interrupted-analysis-recovery-spec-replay",
            1,
        ),
    ] {
        let fixture = blocked_spec_review_fixture(slug);
        let revision = revise_provider_step(
            &fixture.workspace,
            upstream,
            "operator@example.invalid",
            "replay the complete provider chain",
        )
        .expect("create authenticated upstream recovery");
        let initialized = InitializedLoopRun::resume_isolated_for_rerun(
            &fixture.runs_root,
            revision.run,
            upstream,
        )
        .expect("resume isolated upstream rerun");
        let prepared = initialized
            .scaffold()
            .expect("resume scaffold")
            .publish_authoritative_inputs(fixture.snapshots.clone())
            .expect("resume authoritative inputs");
        let first_provider = FakeProvider::new(
            initial_provider_responses()
                .into_iter()
                .skip(2 - calls_before_spec)
                .collect(),
        );
        let observer = |_workspace: &LoopWorkspace,
                        _run: &LoopRun,
                        coordinates: &ProviderExchangeCoordinates,
                        _request: &ArtifactReference| {
            if coordinates.step == LoopStepName::SpecCreation
                && coordinates.step_attempt == 2
            {
                panic!("interrupt after durable upstream Spec Creation request");
            }
        };
        let mut first_patch_runner = RecordingGitCommandRunner::default();
        let mut first_step_runner = ProviderStepRunner::new(&first_provider, "fake-model", 30_000)
            .with_ticket(fixture.ticket.clone())
            .with_context_pack_request(context_request(&fixture.candidate, &fixture.ticket))
            .with_patch_gate(
                ProviderPatchGateConfig::for_ticket(
                    &fixture.candidate,
                    &fixture.ticket,
                    fixture.policy.clone(),
                    true,
                ),
                &mut first_patch_runner,
            )
            .with_recovery_attempt(upstream, 2)
            .with_before_provider_reauthentication_observer(&observer);
        let mut first_runner = LoopRunner::resume_initialized(prepared, &mut first_step_runner)
            .expect("resume upstream recovery");
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = first_runner.run_to_completion();
        }));
        assert!(interrupted.is_err());
        drop(first_runner);
        drop(first_step_runner);
        assert_eq!(first_provider.requests().unwrap().len(), calls_before_spec);
        assert!(first_patch_runner.calls.is_empty());

        let interrupted = crate::state::load_run(&fixture.workspace).expect("interrupted run");
        let durable_request = initial_request_user_json(
            &fixture.workspace,
            &interrupted,
            LoopStepName::SpecCreation,
            2,
        );
        assert!(durable_request.get("revision_context").is_none());
        let initialized = InitializedLoopRun::resume_isolated(&fixture.runs_root, interrupted)
            .expect("ordinary isolated resume");
        let prepared = initialized
            .scaffold()
            .expect("ordinary resume scaffold")
            .publish_authoritative_inputs(fixture.snapshots.clone())
            .expect("ordinary resume authoritative inputs");
        let resumed_provider = FakeProvider::new(
            initial_provider_responses()
                .into_iter()
                .skip(2)
                .collect(),
        );
        let mut resumed_patch_runner = RecordingGitCommandRunner::default();
        let mut resumed_step_runner =
            ProviderStepRunner::new(&resumed_provider, "fake-model", 30_000)
                .with_ticket(fixture.ticket.clone())
                .with_context_pack_request(context_request(&fixture.candidate, &fixture.ticket))
                .with_patch_gate(
                    ProviderPatchGateConfig::for_ticket(
                        &fixture.candidate,
                        &fixture.ticket,
                        fixture.policy.clone(),
                        true,
                    ),
                    &mut resumed_patch_runner,
                );
        let mut resumed = LoopRunner::resume_initialized(prepared, &mut resumed_step_runner)
            .expect("ordinary resume retains the upstream prompt contract");
        resumed
            .run_to_completion()
            .expect("finish ordinary upstream recovery resume");
        assert_eq!(resumed.run().status, LoopStatus::Blocked);
        drop(resumed);
        drop(resumed_step_runner);

        let requests = resumed_provider.requests().unwrap();
        assert_eq!(requests.len(), 2);
        let replayed_request: serde_json::Value =
            serde_json::from_str(&requests[0].messages[0].content).unwrap();
        assert_eq!(replayed_request, durable_request);
        assert!(resumed_patch_runner.calls.is_empty());

        git_ok(
            &fixture.source,
            &[
                "worktree",
                "remove",
                "--force",
                fixture.candidate.to_str().unwrap(),
            ],
        );
    }
}

#[test]
fn ordinary_resume_reconstructs_direct_spec_creation_revision_context() {
    let fixture = blocked_spec_review_fixture("direct-spec-recovery-request-resume");
    let revision = revise_provider_step(
        &fixture.workspace,
        LoopStepName::SpecCreation,
        "operator@example.invalid",
        "address authenticated Spec Review feedback",
    )
    .expect("create authenticated Spec Creation recovery");
    let initialized = InitializedLoopRun::resume_isolated_for_rerun(
        &fixture.runs_root,
        revision.run,
        LoopStepName::SpecCreation,
    )
    .expect("resume isolated Spec Creation rerun");
    let prepared = initialized
        .scaffold()
        .expect("resume scaffold")
        .publish_authoritative_inputs(fixture.snapshots.clone())
        .expect("resume authoritative inputs");
    let first_provider = FakeProvider::new(revised_provider_responses());
    let observer = |_workspace: &LoopWorkspace,
                    _run: &LoopRun,
                    coordinates: &ProviderExchangeCoordinates,
                    _request: &ArtifactReference| {
        assert_eq!(coordinates.step, LoopStepName::SpecCreation);
        assert_eq!(coordinates.step_attempt, 2);
        panic!("interrupt after durable direct Spec Creation request");
    };
    let mut first_patch_runner = RecordingGitCommandRunner::default();
    let mut first_step_runner = ProviderStepRunner::new(&first_provider, "fake-model", 30_000)
        .with_ticket(fixture.ticket.clone())
        .with_context_pack_request(context_request(&fixture.candidate, &fixture.ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(
                &fixture.candidate,
                &fixture.ticket,
                fixture.policy.clone(),
                true,
            ),
            &mut first_patch_runner,
        )
        .with_recovery_attempt(LoopStepName::SpecCreation, 2)
        .with_before_provider_reauthentication_observer(&observer);
    let mut first_runner = LoopRunner::resume_initialized(prepared, &mut first_step_runner)
        .expect("resume direct recovery");
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = first_runner.run_next_step();
    }));
    assert!(interrupted.is_err());
    drop(first_runner);
    drop(first_step_runner);
    assert!(first_provider.requests().unwrap().is_empty());

    let interrupted = crate::state::load_run(&fixture.workspace).expect("interrupted run");
    let durable_request = initial_request_user_json(
        &fixture.workspace,
        &interrupted,
        LoopStepName::SpecCreation,
        2,
    );
    assert!(durable_request.get("revision_context").is_some());
    let initialized = InitializedLoopRun::resume_isolated(&fixture.runs_root, interrupted)
        .expect("ordinary isolated resume");
    let prepared = initialized
        .scaffold()
        .expect("ordinary resume scaffold")
        .publish_authoritative_inputs(fixture.snapshots.clone())
        .expect("ordinary resume authoritative inputs");
    let resumed_provider = FakeProvider::new(revised_provider_responses());
    let mut resumed_patch_runner = RecordingGitCommandRunner::default();
    let mut resumed_step_runner = ProviderStepRunner::new(&resumed_provider, "fake-model", 30_000)
        .with_ticket(fixture.ticket.clone())
        .with_context_pack_request(context_request(&fixture.candidate, &fixture.ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(
                &fixture.candidate,
                &fixture.ticket,
                fixture.policy.clone(),
                true,
            ),
            &mut resumed_patch_runner,
        );
    let mut resumed = LoopRunner::resume_initialized(prepared, &mut resumed_step_runner)
        .expect("ordinary resume reconstructs direct recovery context");
    resumed
        .run_to_completion()
        .expect("finish ordinary direct recovery resume");
    assert_eq!(resumed.run().status, LoopStatus::AwaitingHumanReview);
    drop(resumed);
    drop(resumed_step_runner);

    let requests = resumed_provider.requests().unwrap();
    let replayed_request: serde_json::Value =
        serde_json::from_str(&requests[0].messages[0].content).unwrap();
    assert_eq!(replayed_request, durable_request);

    git_ok(
        &fixture.source,
        &["worktree", "remove", "--force", fixture.candidate.to_str().unwrap()],
    );
}

#[test]
fn ordinary_run_clears_stale_spec_creation_revision_context() {
    let fixture = blocked_spec_review_fixture("spec-revision-stale-source");
    let blocked = crate::state::load_run(&fixture.workspace).unwrap();
    let prior_spec = load_role_artifact(
        &fixture.workspace,
        &blocked,
        LoopStepName::SpecCreation,
        Role::SpecWriter,
    );
    let reviewer_feedback = load_role_artifact(
        &fixture.workspace,
        &blocked,
        LoopStepName::SpecReview,
        Role::SpecReviewer,
    );
    let stale_context = SpecCreationRevisionContext {
        prior_spec: RevisionRoleArtifact {
            artifact_path: prior_spec.artifact_path,
            artifact_digest: prior_spec.artifact_digest,
            artifact: prior_spec.artifact,
        },
        reviewer_feedback: RevisionRoleArtifact {
            artifact_path: reviewer_feedback.artifact_path,
            artifact_digest: reviewer_feedback.artifact_digest,
            artifact: reviewer_feedback.artifact,
        },
    };
    let provider = FakeProvider::new(initial_provider_responses());
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
            .with_ticket(fixture.ticket.clone());
    step_runner.spec_creation_revision_context = Some(stale_context);
    let mut runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &fixture.runs_root,
            "ordinary-after-recovery",
            &fixture.ticket,
            "fake",
            "fake-model",
            LoopInputDigests {
                ticket: canonical_sha256_digest(&fixture.ticket).unwrap(),
                policy: "b".repeat(64),
                config: "c".repeat(64),
                repository: "d".repeat(64),
                eval_config: None,
            },
        ),
        &mut step_runner,
    )
    .expect("start ordinary run with reused provider runner");
    runner.run_to_completion().expect("finish ordinary run");
    drop(runner);
    drop(step_runner);

    let requests = provider.requests().unwrap();
    let spec_creation: serde_json::Value =
        serde_json::from_str(&requests[2].messages[0].content).unwrap();
    assert!(spec_creation.get("revision_context").is_none());

    git_ok(
        &fixture.source,
        &["worktree", "remove", "--force", fixture.candidate.to_str().unwrap()],
    );
}

struct BlockedSpecReviewFixture {
    _temp: tempfile::TempDir,
    runs_root: PathBuf,
    source: PathBuf,
    candidate: PathBuf,
    ticket: TicketSpec,
    policy: Policy,
    snapshots: AuthoritativeRunInputSnapshots,
    workspace: LoopWorkspace,
}

fn blocked_spec_review_fixture(run_id: &str) -> BlockedSpecReviewFixture {
    let temp = tempfile::tempdir().expect("temp directory");
    let runs_root = temp.path().join("runs");
    let source = temp.path().join("source");
    fs::create_dir(&source).expect("source directory");
    git_ok(&source, &["init", "-q"]);
    git_ok(&source, &["config", "user.email", "test@example.com"]);
    git_ok(&source, &["config", "user.name", "SEAF Test"]);
    fs::create_dir(source.join("src")).expect("source tree");
    fs::write(source.join("src/lib.rs"), "pub fn existing() {}\n").expect("source file");
    git_ok(&source, &["add", "."]);
    git_ok(&source, &["commit", "-qm", "initial"]);

    let ticket = ticket();
    let policy = policy();
    let config = serde_json::json!({"policy_path":"seaf.policy.json"});
    let repository = serde_json::json!({"source":source.canonicalize().unwrap()});
    let eval_config = seaf_core::parse_eval_config(
        "evals:\n  allow_commands: []\n  required:\n    - name: tests\n      command: cargo test\n",
    )
    .expect("eval config");
    let snapshots = AuthoritativeRunInputSnapshots {
        ticket: canonical_json_bytes(&ticket).unwrap(),
        provider_ticket: canonical_json_bytes(&ticket).unwrap(),
        policy: canonical_json_bytes(&policy).unwrap(),
        config: canonical_json_bytes(&config).unwrap(),
        repository: canonical_json_bytes(&repository).unwrap(),
        eval_config: canonical_json_bytes(&eval_config).unwrap(),
    };
    let initialized = InitializedLoopRun::create_isolated(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            run_id,
            &ticket,
            "fake",
            "fake-model",
            LoopInputDigests {
                ticket: canonical_sha256_digest(&ticket).unwrap(),
                policy: canonical_sha256_digest(&policy).unwrap(),
                config: canonical_sha256_digest(&config).unwrap(),
                repository: canonical_sha256_digest(&repository).unwrap(),
                eval_config: Some(canonical_sha256_digest(&eval_config).unwrap()),
            },
        ),
        &source,
        &snapshots,
    )
    .expect("initialize isolated run");
    let candidate = PathBuf::from(
        &initialized
            .run()
            .candidate_workspace
            .as_ref()
            .expect("candidate authority")
            .path,
    );
    let prepared = initialized
        .scaffold()
        .expect("scaffold")
        .publish_authoritative_inputs(snapshots.clone())
        .expect("authoritative inputs");
    let provider = FakeProvider::new(initial_provider_responses());
    let mut patch_runner = RecordingGitCommandRunner::default();
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&candidate, &ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(&candidate, &ticket, policy.clone(), true),
            &mut patch_runner,
        );
    let mut runner = LoopRunner::start_initialized(prepared, &mut step_runner).expect("start loop");
    runner
        .run_to_completion()
        .expect("stop cleanly at blocked Spec Review");
    assert_eq!(runner.run().status, LoopStatus::Blocked);
    drop(runner);
    drop(step_runner);
    assert_eq!(provider.requests().unwrap().len(), 4);
    assert!(patch_runner.calls.is_empty());

    let workspace = LoopWorkspace::open(&runs_root, run_id).expect("open workspace");
    BlockedSpecReviewFixture {
        _temp: temp,
        runs_root,
        source,
        candidate,
        ticket,
        policy,
        snapshots,
        workspace,
    }
}

fn rewrite_recovery_source(
    fixture: &BlockedSpecReviewFixture,
    mutate: impl FnOnce(&mut RecoverySourceRunV1, &LoopWorkspace),
) -> LoopRun {
    let revision = revise_provider_step(
        &fixture.workspace,
        LoopStepName::SpecCreation,
        "operator@example.invalid",
        "address authenticated Spec Review feedback",
    )
    .expect("create authenticated recovery");
    let mut reset = revision.run;
    let reference = reset.latest_recovery.clone().expect("recovery reference");
    let recovery_path = fixture
        .workspace
        .run_directory()
        .join(&reference.artifact.path);
    let mut recovery: RecoveryAttemptV1 =
        serde_json::from_slice(&fs::read(&recovery_path).unwrap()).unwrap();
    let source_path = fixture
        .workspace
        .run_directory()
        .join(&recovery.source_run.path);
    let mut source: RecoverySourceRunV1 =
        serde_json::from_slice(&fs::read(&source_path).unwrap()).unwrap();
    mutate(&mut source, &fixture.workspace);

    let source_bytes = canonical_json_bytes(&source).unwrap();
    recovery.source_run.digest = sha256(&source_bytes);
    recovery.source_run_digest = canonical_sha256_digest(&source.run).unwrap();
    let recovery_bytes = canonical_json_bytes(&recovery).unwrap();
    reset
        .latest_recovery
        .as_mut()
        .unwrap()
        .artifact
        .digest = sha256(&recovery_bytes);
    crate::artifact_safety::write_private_fixture(&source_path, &source_bytes).unwrap();
    crate::artifact_safety::write_private_fixture(&recovery_path, &recovery_bytes).unwrap();
    crate::state::write_raw_canonical_run_fixture(&fixture.workspace.run_file(), &reset).unwrap();
    reset
}

fn assert_revision_context_rejected_before_provider(
    fixture: &BlockedSpecReviewFixture,
    reset: LoopRun,
    expected: &str,
) {
    assert_revision_context_rejected_before_provider_at_attempt(fixture, reset, 2, expected);
}

fn assert_revision_context_rejected_before_provider_at_attempt(
    fixture: &BlockedSpecReviewFixture,
    reset: LoopRun,
    attempt: u32,
    expected: &str,
) {
    assert_revision_context_rejected_before_provider_with_coordinates(
        fixture,
        reset,
        LoopStepName::SpecCreation,
        LoopStepName::SpecCreation,
        attempt,
        expected,
    );
}

fn assert_revision_context_rejected_before_provider_with_coordinates(
    fixture: &BlockedSpecReviewFixture,
    reset: LoopRun,
    rerun_step: LoopStepName,
    requested_step: LoopStepName,
    attempt: u32,
    expected: &str,
) {
    let initialized = InitializedLoopRun::resume_isolated_for_rerun(
        &fixture.runs_root,
        reset,
        rerun_step,
    )
    .expect("resume isolated rerun authority");
    let prepared = initialized
        .scaffold()
        .expect("resume scaffold")
        .publish_authoritative_inputs(fixture.snapshots.clone())
        .expect("resume authoritative inputs");
    let provider = FakeProvider::new(revised_provider_responses());
    let mut patch_runner = RecordingGitCommandRunner::default();
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(fixture.ticket.clone())
        .with_context_pack_request(context_request(&fixture.candidate, &fixture.ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(
                &fixture.candidate,
                &fixture.ticket,
                fixture.policy.clone(),
                true,
            ),
            &mut patch_runner,
        )
        .with_recovery_attempt(requested_step, attempt);
    let run_directory_before = run_files(fixture.workspace.run_directory());
    let source_before = repository_authority(&fixture.source);
    let candidate_before = repository_authority(&fixture.candidate);
    let error = match LoopRunner::resume_initialized(prepared, &mut step_runner) {
        Ok(mut runner) => {
            let error = runner
                .run_next_step()
                .expect_err("invalid revision context must fail closed");
            drop(runner);
            error
        }
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("SpecCreation revision context"),
        "{error}"
    );
    assert!(error.to_string().contains(expected), "{error}");
    drop(step_runner);

    assert!(provider.requests().unwrap().is_empty());
    assert!(patch_runner.calls.is_empty());
    assert_eq!(
        run_files(fixture.workspace.run_directory()),
        run_directory_before,
        "invalid revision authority must not mutate any run-directory file"
    );
    assert_eq!(repository_authority(&fixture.source), source_before);
    assert_eq!(repository_authority(&fixture.candidate), candidate_before);

    git_ok(
        &fixture.source,
        &["worktree", "remove", "--force", fixture.candidate.to_str().unwrap()],
    );
}

#[derive(Debug, serde::Serialize)]
struct ExpectedRevisionRoleArtifact {
    artifact_path: String,
    artifact_digest: String,
    artifact: ValidatedRoleArtifact,
}

fn load_role_artifact(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    step: LoopStepName,
    role: Role,
) -> ExpectedRevisionRoleArtifact {
    let record = run.steps.iter().find(|record| record.name == step).unwrap();
    let path = record.artifact_path.as_deref().expect("artifact path");
    let digest = record.artifact_digest.as_deref().expect("artifact digest");
    ExpectedRevisionRoleArtifact {
        artifact_path: path.to_string(),
        artifact_digest: digest.to_string(),
        artifact: ValidatedRoleArtifact::load(workspace, path, digest, &run.run_id, step, role)
            .expect("validated role artifact"),
    }
}

fn initial_request_user_json(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    step: LoopStepName,
    attempt: u32,
) -> serde_json::Value {
    let reference = run
        .provider_exchange_records
        .iter()
        .find(|record| {
            record.step == step
                && record.step_attempt == attempt
                && record.kind == ProviderExchangeKind::Initial
                && record.phase == ProviderExchangePhase::Request
        })
        .expect("initial provider request");
    let record = load_provider_exchange_record(workspace.run_directory(), reference)
        .expect("provider request record");
    let bytes = load_provider_exchange_request(workspace.run_directory(), &record.request)
        .expect("provider request audit");
    let request: ModelRequest = serde_json::from_slice(&bytes).expect("typed model request");
    serde_json::from_str(&request.messages[0].content).expect("structured user JSON")
}

#[derive(Default)]
struct RecordingGitCommandRunner {
    calls: Vec<PatchCommand>,
}

impl PatchCommandRunner for RecordingGitCommandRunner {
    fn run(
        &mut self,
        repo_root: &Path,
        command: PatchCommand,
        patch: &str,
    ) -> Result<CommandOutput, PatchGateError> {
        self.calls.push(command);
        GitCommandRunner.run(repo_root, command, patch)
    }
}

fn ticket() -> TicketSpec {
    TicketSpec {
        ticket_id: "T-M2-07A".to_string(),
        goal_id: "production-use".to_string(),
        title: "Recover Spec Creation with review feedback".to_string(),
        status: TicketStatus::Ready,
        priority: TicketPriority::P1,
        problem: "A revised spec must address authenticated reviewer feedback.".to_string(),
        research_questions: Vec::new(),
        context: TicketContext {
            relevant_files: vec!["src/lib.rs".to_string()],
            forbidden_files: Vec::new(),
        },
        autonomy: TicketAutonomy {
            level: 1,
            apply_patch: true,
            allow_shell_commands: Vec::new(),
        },
        acceptance_criteria: vec!["Use the blocked review as revision input.".to_string()],
        eval: None,
    }
}

fn policy() -> Policy {
    Policy {
        policy_id: "test-policy".to_string(),
        default_autonomy_level: 1,
        forbidden_paths: vec!["secrets/**".to_string()],
        requires_human_review: vec!["dependency_changes".to_string()],
        allowed_without_review: vec!["source_changes".to_string()],
    }
}

fn context_request(repository_root: &Path, ticket: &TicketSpec) -> ContextPackRequest {
    ContextPackRequest::for_ticket(
        repository_root,
        Path::new("unused"),
        ticket,
        &[],
        ContextLimits {
            max_bytes_per_file: 4096,
            max_total_bytes: 8192,
        },
    )
}

fn initial_provider_responses() -> Vec<Result<ModelResponse, seaf_models::ModelError>> {
    responses([
        include_str!("../../../../fixtures/model-responses/research.valid.json").to_string(),
        include_str!("../../../../fixtures/model-responses/analyzer.valid.json").to_string(),
        serde_json::json!({
            "role": "spec_writer",
            "status": "passed",
            "summary": "Add the requested function without an error contract.",
            "findings": [],
            "risks": [],
            "next_step_recommendation": "Review the narrow implementation."
        })
        .to_string(),
        serde_json::json!({
            "role": "spec_reviewer",
            "decision": "request_changes",
            "summary": "The spec omits the required error behavior.",
            "blocking_issues": [{
                "summary": "Specify the error contract.",
                "evidence": "The acceptance criteria do not state how invalid input fails."
            }],
            "non_blocking_issues": []
        })
        .to_string(),
    ])
}

fn revised_provider_responses() -> Vec<Result<ModelResponse, seaf_models::ModelError>> {
    responses([
        serde_json::json!({
            "role": "spec_writer",
            "status": "passed",
            "summary": "Add the requested function and return an error for invalid input.",
            "findings": [],
            "risks": [],
            "next_step_recommendation": "Verify the explicit error contract."
        })
        .to_string(),
        serde_json::json!({
            "role": "spec_reviewer",
            "decision": "approve_spec",
            "summary": "The revised spec includes the required error behavior.",
            "blocking_issues": [],
            "non_blocking_issues": []
        })
        .to_string(),
        serde_json::json!({
            "role": "developer",
            "status": "patch_proposed",
            "summary": "Add the narrow implementation.",
            "changed_files": ["src/new.rs"],
            "requires_human_review": false,
            "patch": "diff --git a/src/new.rs b/src/new.rs\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/src/new.rs\n@@ -0,0 +1 @@\n+pub fn added() {}\n"
        })
        .to_string(),
        serde_json::json!({
            "role": "output_reviewer",
            "decision": "approve_for_tests",
            "summary": "The candidate matches the revised approved spec.",
            "blocking_issues": [],
            "non_blocking_issues": []
        })
        .to_string(),
    ])
}

fn responses<const N: usize>(contents: [String; N]) -> Vec<Result<ModelResponse, seaf_models::ModelError>> {
    contents
        .into_iter()
        .map(|content| {
            Ok(ModelResponse {
                content,
                latency_ms: 1,
                raw_provider_metadata: serde_json::Value::Null,
            })
        })
        .collect()
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn repository_authority(root: &Path) -> (Vec<u8>, Vec<u8>) {
    (
        git_output(root, &["rev-parse", "HEAD"]),
        git_output(root, &["status", "--porcelain=v1", "-z"]),
    )
}

fn git_output(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git command");
    assert!(output.status.success(), "git {:?}", args);
    output.stdout
}

fn run_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let mut children = fs::read_dir(directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let path = child.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                visit(root, &path, files);
            } else if metadata.is_file() {
                files.insert(path.strip_prefix(root).unwrap().to_path_buf(), fs::read(path).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn git_ok(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}
