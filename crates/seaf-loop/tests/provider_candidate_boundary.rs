use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, LoopInputDigests, LoopStepName, Policy,
    ProviderExchangeKind, ProviderExchangePhase, ProviderExchangeRecord, ProviderRole,
    TicketAutonomy, TicketContext, TicketPriority, TicketSpec, TicketStatus,
};
use seaf_loop::{
    artifacts::write_step_request, stage_provider_exchange_record, verify_candidate_patch_evidence,
    write_provider_exchange_request, AuthoritativeRunInputSnapshots, CommandOutput, ContextLimits,
    ContextPackRequest, InitializedLoopRun, LoopRunner, LoopRunnerConfig, LoopWorkspace,
    PatchCommand, PatchCommandRunner, PatchGateError, PreparedLoopRun, ProviderExchangeCoordinates,
    ProviderPatchGateConfig, ProviderStepRunner, StepRunner, PROVIDER_EXCHANGE_SCHEMA_VERSION,
};
use seaf_models::{FakeProvider, ModelResponse};

#[test]
fn provider_rejects_context_and_patch_roots_that_are_not_the_candidate() {
    for wrong_context in [true, false] {
        let fixture = fixture(if wrong_context {
            "wrong-context"
        } else {
            "wrong-patch"
        });
        let candidate = fixture.candidate.clone();
        let context_root = if wrong_context {
            &fixture.source
        } else {
            &candidate
        };
        let patch_root = if wrong_context {
            &candidate
        } else {
            &fixture.source
        };
        let provider = FakeProvider::new(Vec::new());
        let mut patch_runner = RecordingPatchRunner::default();
        let mut runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
            .with_ticket(fixture.ticket.clone())
            .with_context_pack_request(context_request(context_root, &fixture.ticket))
            .with_patch_gate(
                ProviderPatchGateConfig::for_ticket(
                    patch_root,
                    &fixture.ticket,
                    fixture.policy.clone(),
                    true,
                ),
                &mut patch_runner,
            );
        let error = LoopRunner::start_initialized(fixture.prepared, &mut runner)
            .expect_err("each configured root must independently equal candidate");
        assert!(error.to_string().contains("must both equal"), "{error}");
        assert!(provider.requests().unwrap().is_empty());
        remove_candidate(&fixture.source, &candidate);
    }
}

#[test]
fn completed_development_is_applied_only_to_the_candidate_before_output_review() {
    let fixture = fixture("candidate-patch-check");
    let source_before = source_evidence(&fixture.source);
    let candidate_before = source_evidence(&fixture.candidate);
    let responses = candidate_responses(false);
    let provider = FakeProvider::new(responses);
    let mut patch_runner = RecordingPatchRunner::default();
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
        );
    let mut loop_runner =
        LoopRunner::start_initialized(fixture.prepared, &mut step_runner).expect("start");
    for _ in 0..5 {
        assert!(loop_runner.run_next_step().expect("provider step"));
    }
    let completed = loop_runner.run().clone();
    drop(loop_runner);
    drop(step_runner);
    assert_eq!(patch_runner.calls.len(), 1);
    assert_eq!(
        patch_runner.calls[0].0,
        fixture.candidate.canonicalize().unwrap()
    );
    assert_eq!(patch_runner.calls[0].1, PatchCommand::GitApplyCheck);
    let decision = &completed.policy_decisions[0];
    assert_eq!(decision.get("apply_requested").unwrap(), true);
    assert_eq!(decision.get("applied").unwrap(), false);
    assert_eq!(source_evidence(&fixture.source), source_before);
    let applied = completed
        .candidate_workspace
        .as_ref()
        .and_then(|candidate| candidate.patch_transaction.as_ref())
        .expect("completed Development must publish a candidate patch transaction");
    assert_eq!(
        applied.phase,
        seaf_core::CandidatePatchPhase::Applied,
        "Development cannot finish successfully until the candidate is exactly Applied"
    );
    let candidate_after = source_evidence(&fixture.candidate);
    assert_eq!(
        candidate_after.0, candidate_before.0,
        "candidate HEAD stays detached at the starting commit"
    );
    assert_ne!(
        candidate_after.1, candidate_before.1,
        "candidate index must contain the applied patch"
    );
    assert_eq!(
        candidate_after.2, candidate_before.2,
        "unrelated candidate bytes stay unchanged"
    );
    assert_eq!(
        fs::read_to_string(fixture.candidate.join("src/new.rs")).unwrap(),
        "pub fn added() {}\n"
    );
    let workspace = LoopWorkspace::open(&fixture.runs_root, "candidate-patch-check").unwrap();
    let verified = verify_candidate_patch_evidence(&workspace, &fixture.source)
        .expect("Applied candidate must project closed read-only review evidence");
    let development = completed
        .steps
        .iter()
        .find(|record| record.name == seaf_core::LoopStepName::Development)
        .unwrap();
    assert_eq!(
        verified.development_evidence.path,
        development.artifact_path.clone().unwrap()
    );
    assert_eq!(
        verified.development_evidence.digest,
        development.artifact_digest.clone().unwrap()
    );
    assert!(!verified.policy_decision.applied);
    assert_eq!(
        verified.policy_decision_digest,
        canonical_sha256_digest(&verified.policy_decision).unwrap()
    );
    assert_eq!(
        verified.candidate_tree,
        completed
            .candidate_workspace
            .as_ref()
            .unwrap()
            .candidate_tree
    );
    assert_eq!(
        verified.applied_diff.digest,
        completed
            .candidate_workspace
            .as_ref()
            .unwrap()
            .candidate_diff_digest
    );
    assert_eq!(verified.applied_diff_digest, verified.applied_diff.digest);
    assert!(verified
        .applied_diff_content
        .contains("diff --git a/src/new.rs b/src/new.rs"));
    assert!(verified.applied_diff_content.contains("+pub fn added() {}"));
    remove_candidate(&fixture.source, &fixture.candidate);
}

#[test]
fn output_review_receives_only_the_exact_verified_applied_subject() {
    let fixture = fixture("candidate-output-review");
    let provider = FakeProvider::new(candidate_responses(true));
    let mut patch_runner = RecordingPatchRunner::default();
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
        );
    let mut loop_runner =
        LoopRunner::start_initialized(fixture.prepared, &mut step_runner).expect("start");
    for _ in 0..5 {
        assert!(loop_runner.run_next_step().expect("pre-review step"));
    }
    let workspace = LoopWorkspace::open(&fixture.runs_root, "candidate-output-review").unwrap();
    let expected = verify_candidate_patch_evidence(&workspace, &fixture.source).unwrap();
    assert!(loop_runner.run_next_step().expect("OutputReview"));
    drop(loop_runner);
    drop(step_runner);

    let requests = provider.requests().unwrap();
    let role_input: serde_json::Value =
        serde_json::from_str(&requests[5].messages[0].content).unwrap();
    let keys = role_input
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from([
            "approved_spec_identity",
            "input_digests",
            "instructions",
            "run_id",
            "verified_candidate_patch",
        ])
    );
    let actual: seaf_loop::VerifiedCandidatePatchEvidence =
        serde_json::from_value(role_input["verified_candidate_patch"].clone()).unwrap();
    assert_eq!(actual, expected);
    let rendered = requests[5].messages[0].content.as_str();
    assert!(!rendered.contains("patch_proposed"));
    assert!(!rendered.contains("repository_context"));
    assert!(!rendered.contains("T-CANDIDATE"));
    assert_eq!(source_evidence(&fixture.source).1, "");
    remove_candidate(&fixture.source, &fixture.candidate);
}

#[test]
fn recovery_rejects_a_staged_output_review_subject_substitution_before_cas() {
    let fixture = fixture("candidate-output-review-substitution");
    let source_before = source_evidence(&fixture.source);
    let provider = FakeProvider::new(candidate_responses(false));
    let mut patch_runner = RecordingPatchRunner::default();
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
        );
    let mut loop_runner =
        LoopRunner::start_initialized(fixture.prepared, &mut step_runner).expect("start");
    for _ in 0..5 {
        assert!(loop_runner.run_next_step().expect("pre-review step"));
    }
    let run = loop_runner.run().clone();
    drop(loop_runner);
    drop(step_runner);
    let candidate_before = source_evidence(&fixture.candidate);
    let workspace =
        LoopWorkspace::open(&fixture.runs_root, "candidate-output-review-substitution").unwrap();

    let generator_provider = FakeProvider::new(Vec::new());
    let mut generator_patch_runner = RecordingPatchRunner::default();
    let mut generator = ProviderStepRunner::new(&generator_provider, "fake-model", 30_000)
        .with_ticket(fixture.ticket.clone())
        .with_context_pack_request(context_request(&fixture.candidate, &fixture.ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(
                &fixture.candidate,
                &fixture.ticket,
                fixture.policy.clone(),
                true,
            ),
            &mut generator_patch_runner,
        );
    generator.prepare_run(&workspace, &run).unwrap();
    generator
        .prepare_step(&workspace, &run, LoopStepName::OutputReview)
        .unwrap();
    let request_text = generator.step_request(LoopStepName::OutputReview).unwrap();
    drop(generator);
    let mut request: seaf_models::ModelRequest = serde_json::from_str(&request_text).unwrap();
    let mut subject: serde_json::Value =
        serde_json::from_str(&request.messages[0].content).unwrap();
    subject["verified_candidate_patch"]["candidate_tree"] =
        serde_json::Value::String("0".repeat(40));
    request.messages[0].content = serde_json::to_string(&subject).unwrap();
    let request_bytes = serde_json::to_vec_pretty(&request).unwrap();
    write_step_request(
        &workspace,
        LoopStepName::OutputReview,
        1,
        std::str::from_utf8(&request_bytes).unwrap(),
    )
    .unwrap();
    let coordinates = ProviderExchangeCoordinates {
        run_id: run.run_id.clone(),
        step: LoopStepName::OutputReview,
        role: ProviderRole::OutputReviewer,
        step_attempt: 1,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
    };
    let request_reference =
        write_provider_exchange_request(workspace.run_directory(), &coordinates, &request_bytes)
            .unwrap();
    stage_provider_exchange_record(
        workspace.run_directory(),
        &ProviderExchangeRecord {
            schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: run.run_id.clone(),
            step: LoopStepName::OutputReview,
            role: ProviderRole::OutputReviewer,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: run
                .provider_exchange_records
                .last()
                .map(|record| record.digest.clone()),
            request: request_reference.clone(),
            response: None,
            expansion: None,
            outcome: None,
        },
    )
    .unwrap();
    let run_tree_before = snapshot_files(workspace.run_directory());
    let provider_calls_before = generator_provider.requests().unwrap();

    let mut recovery_patch_runner = RecordingPatchRunner::default();
    let mut recovery = ProviderStepRunner::new(&generator_provider, "fake-model", 30_000)
        .with_ticket(fixture.ticket.clone())
        .with_context_pack_request(context_request(&fixture.candidate, &fixture.ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(
                &fixture.candidate,
                &fixture.ticket,
                fixture.policy.clone(),
                true,
            ),
            &mut recovery_patch_runner,
        );
    let error = recovery
        .prepare_run(&workspace, &run)
        .expect_err("substituted staged OutputReview subject must fail before CAS");
    assert!(
        error.to_string().contains("exact verified candidate patch"),
        "{error}"
    );
    drop(recovery);
    assert_eq!(snapshot_files(workspace.run_directory()), run_tree_before);
    assert_eq!(
        generator_provider.requests().unwrap(),
        provider_calls_before
    );
    assert!(recovery_patch_runner.calls.is_empty());
    assert_eq!(source_evidence(&fixture.source), source_before);
    assert_eq!(source_evidence(&fixture.candidate), candidate_before);
    fs::remove_file(
        workspace.run_directory().join(
            "artifacts/06-output-review.attempt-001.exchange-001.initial.request.record.json",
        ),
    )
    .unwrap();
    fs::remove_file(workspace.run_directory().join(&request_reference.path)).unwrap();
    fs::remove_file(
        workspace
            .run_directory()
            .join("prompts/06-output-review.prompt.md"),
    )
    .unwrap();
    write_step_request(&workspace, LoopStepName::OutputReview, 1, &request_text).unwrap();
    let valid_request = write_provider_exchange_request(
        workspace.run_directory(),
        &coordinates,
        request_text.as_bytes(),
    )
    .unwrap();
    stage_provider_exchange_record(
        workspace.run_directory(),
        &ProviderExchangeRecord {
            schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: run.run_id.clone(),
            step: LoopStepName::OutputReview,
            role: ProviderRole::OutputReviewer,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: run
                .provider_exchange_records
                .last()
                .map(|record| record.digest.clone()),
            request: valid_request,
            response: None,
            expansion: None,
            outcome: None,
        },
    )
    .unwrap();
    let mut valid_patch_runner = RecordingPatchRunner::default();
    let mut valid_recovery = ProviderStepRunner::new(&generator_provider, "fake-model", 30_000)
        .with_ticket(fixture.ticket.clone())
        .with_context_pack_request(context_request(&fixture.candidate, &fixture.ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(
                &fixture.candidate,
                &fixture.ticket,
                fixture.policy.clone(),
                true,
            ),
            &mut valid_patch_runner,
        );
    valid_recovery
        .prepare_run(&workspace, &run)
        .expect("exact staged OutputReview subject adopts without provider replay");
    drop(valid_recovery);
    let adopted = seaf_loop::state::load_run(&workspace).unwrap();
    assert_eq!(
        adopted
            .provider_exchange_records
            .iter()
            .filter(|record| record.step == LoopStepName::OutputReview)
            .count(),
        1
    );
    assert!(valid_patch_runner.calls.is_empty());
    assert_eq!(
        generator_provider.requests().unwrap(),
        provider_calls_before
    );
    assert_eq!(source_evidence(&fixture.source), source_before);
    assert_eq!(source_evidence(&fixture.candidate), candidate_before);
    remove_candidate(&fixture.source, &fixture.candidate);
}

#[test]
fn applied_candidate_allows_only_output_review_rerun_without_resetting_development() {
    let fixture = fixture("candidate-output-review-rerun");
    let source_before = source_evidence(&fixture.source);
    let mut responses = candidate_responses(true);
    responses.push(response(
        r#"{"role":"output_reviewer","decision":"approve_for_tests","summary":"The same applied candidate still matches.","blocking_issues":[],"non_blocking_issues":[]}"#,
    ));
    let provider = FakeProvider::new(responses);
    let mut patch_runner = RecordingPatchRunner::default();
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
        );
    let mut loop_runner =
        LoopRunner::start_initialized(fixture.prepared, &mut step_runner).expect("start");
    for _ in 0..6 {
        assert!(loop_runner.run_next_step().expect("through OutputReview"));
    }
    let before = loop_runner.run().clone();
    loop_runner = loop_runner
        .rerun_from(LoopStepName::OutputReview)
        .expect("Applied candidate permits OutputReview-only rerun");
    assert!(loop_runner
        .run_next_step()
        .expect("OutputReview attempt two"));
    let after = loop_runner.run().clone();
    assert_eq!(after.candidate_workspace, before.candidate_workspace);
    assert_eq!(after.policy_decisions, before.policy_decisions);
    assert_eq!(
        after
            .steps
            .iter()
            .find(|step| step.name == LoopStepName::Development),
        before
            .steps
            .iter()
            .find(|step| step.name == LoopStepName::Development)
    );
    assert_eq!(source_evidence(&fixture.source), source_before);
    let output_attempts = after
        .provider_exchange_records
        .iter()
        .filter(|record| {
            record.step == LoopStepName::OutputReview
                && record.phase == ProviderExchangePhase::Request
                && record.kind == ProviderExchangeKind::Initial
        })
        .map(|record| record.step_attempt)
        .collect::<Vec<_>>();
    assert_eq!(output_attempts, vec![1, 2]);
    drop(loop_runner);
    drop(step_runner);
    assert_eq!(
        patch_runner.calls.len(),
        1,
        "rerun never repeats patch gating"
    );
    remove_candidate(&fixture.source, &fixture.candidate);
}

#[test]
fn pre_b3_completed_development_with_output_review_history_is_rejected_before_scaffold() {
    let fixture = fixture("candidate-pre-b3-output-history");
    let mut responses = candidate_responses(false);
    responses.push(Err(seaf_models::ModelError::provider(
        "injected OutputReview provider failure",
        false,
        serde_json::Value::Null,
    )));
    let provider = FakeProvider::new(responses);
    let mut patch_runner = RecordingPatchRunner::default();
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
        );
    let mut loop_runner =
        LoopRunner::start_initialized(fixture.prepared, &mut step_runner).expect("start");
    for _ in 0..6 {
        assert!(loop_runner
            .run_next_step()
            .expect("through failed OutputReview"));
    }
    drop(loop_runner);
    drop(step_runner);

    git_ok(&fixture.candidate, &["reset", "--hard", "HEAD"]);
    let workspace =
        LoopWorkspace::open(&fixture.runs_root, "candidate-pre-b3-output-history").unwrap();
    for relative in [
        "artifacts/candidate-patch.intent.json",
        "artifacts/candidate-patch.expected.diff",
        "artifacts/candidate-patch.applied.diff",
        "artifacts/candidate-patch.applied.json",
    ] {
        fs::remove_file(workspace.run_directory().join(relative)).unwrap();
    }
    let mut legacy = seaf_loop::state::load_run(&workspace).unwrap();
    legacy.status = seaf_core::LoopStatus::Running;
    legacy.current_step = LoopStepName::OutputReview;
    let candidate = legacy.candidate_workspace.as_mut().unwrap();
    candidate.candidate_tree = candidate.starting_tree.clone();
    candidate.candidate_diff_digest =
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string();
    candidate.patch_transaction = None;
    let output = legacy
        .steps
        .iter_mut()
        .find(|step| step.name == LoopStepName::OutputReview)
        .unwrap();
    output.status = seaf_core::LoopStepStatus::Pending;
    output.artifact_path = None;
    output.artifact_digest = None;
    seaf_loop::state::save_run(&workspace, &legacy).unwrap();
    let before = snapshot_files(workspace.run_directory());

    let error = InitializedLoopRun::resume_isolated(&fixture.runs_root, legacy)
        .expect_err("proposal-only OutputReview history cannot migrate into Applied authority");
    assert!(error.to_string().contains("no provider history"), "{error}");
    assert_eq!(snapshot_files(workspace.run_directory()), before);
    assert_eq!(source_evidence(&fixture.source).1, "");
    assert_eq!(source_evidence(&fixture.candidate).1, "");
    remove_candidate(&fixture.source, &fixture.candidate);
}

#[test]
fn applied_candidate_rejects_development_rerun_without_mutation() {
    let fixture = fixture("candidate-development-rerun-rejected");
    let source_before = source_evidence(&fixture.source);
    let provider = FakeProvider::new(candidate_responses(false));
    let mut patch_runner = RecordingPatchRunner::default();
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
        );
    let mut loop_runner =
        LoopRunner::start_initialized(fixture.prepared, &mut step_runner).expect("start");
    for _ in 0..5 {
        assert!(loop_runner.run_next_step().expect("through Development"));
    }
    let candidate_before = source_evidence(&fixture.candidate);
    let workspace =
        LoopWorkspace::open(&fixture.runs_root, "candidate-development-rerun-rejected").unwrap();
    let run_tree_before = snapshot_files(workspace.run_directory());
    let provider_calls_before = provider.requests().unwrap();
    let error = loop_runner
        .rerun_from(LoopStepName::Development)
        .expect_err("Development rerun requires a new candidate run");
    assert!(error.to_string().contains("start a new run"), "{error}");
    drop(step_runner);
    assert_eq!(snapshot_files(workspace.run_directory()), run_tree_before);
    assert_eq!(provider.requests().unwrap(), provider_calls_before);
    assert_eq!(patch_runner.calls.len(), 1);
    assert_eq!(source_evidence(&fixture.source), source_before);
    assert_eq!(source_evidence(&fixture.candidate), candidate_before);
    remove_candidate(&fixture.source, &fixture.candidate);
}

#[test]
fn non_completed_development_never_creates_a_transaction_or_calls_output_review() {
    enum Case {
        Rejected,
        Blocked,
        ProviderFailure,
    }
    for (name, case) in [
        ("rejected", Case::Rejected),
        ("blocked", Case::Blocked),
        ("provider-failure", Case::ProviderFailure),
    ] {
        let fixture = fixture(&format!("candidate-development-{name}"));
        let source_before = source_evidence(&fixture.source);
        let candidate_before = source_evidence(&fixture.candidate);
        let mut responses = candidate_responses(false);
        responses.pop();
        responses.push(match case {
            Case::Rejected => response(
                r#"{"role":"developer","status":"patch_proposed","summary":"Forbidden","changed_files":["secrets/key.txt"],"requires_human_review":false,"patch":"diff --git a/secrets/key.txt b/secrets/key.txt\nnew file mode 100644\n--- /dev/null\n+++ b/secrets/key.txt\n@@ -0,0 +1 @@\n+secret\n"}"#,
            ),
            Case::Blocked => response(
                r#"{"role":"developer","status":"blocked","summary":"Cannot implement safely.","changed_files":[],"requires_human_review":true}"#,
            ),
            Case::ProviderFailure => Err(seaf_models::ModelError::provider(
                "injected Development provider failure",
                false,
                serde_json::Value::Null,
            )),
        });
        let provider = FakeProvider::new(responses);
        let mut patch_runner = RecordingPatchRunner::default();
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
            );
        let mut loop_runner =
            LoopRunner::start_initialized(fixture.prepared, &mut step_runner).expect("start");
        for _ in 0..5 {
            assert!(loop_runner.run_next_step().expect("through Development"));
        }
        let run = loop_runner.run().clone();
        drop(loop_runner);
        drop(step_runner);
        assert!(run
            .candidate_workspace
            .as_ref()
            .unwrap()
            .patch_transaction
            .is_none());
        assert!(run
            .provider_exchange_records
            .iter()
            .all(|record| record.step != LoopStepName::OutputReview));
        assert_eq!(provider.requests().unwrap().len(), 5);
        assert_eq!(source_evidence(&fixture.source), source_before);
        assert_eq!(source_evidence(&fixture.candidate), candidate_before);
        remove_candidate(&fixture.source, &fixture.candidate);
    }
}

#[test]
fn development_apply_failure_keeps_durable_evidence_but_withholds_finish_and_output_review() {
    let fixture = fixture("candidate-development-apply-fault");
    let source_before = source_evidence(&fixture.source);
    let provider = FakeProvider::new(candidate_responses(false));
    let mut patch_runner = RecordingPatchRunner::default();
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
        );
    let mut loop_runner =
        LoopRunner::start_initialized(fixture.prepared, &mut step_runner).expect("start");
    for _ in 0..4 {
        assert!(loop_runner.run_next_step().expect("pre-Development"));
    }
    fs::write(fixture.candidate.join("unrelated.txt"), "fault injection\n").unwrap();
    let error = loop_runner
        .run_next_step()
        .expect_err("candidate drift must fail after Development evidence publication");
    assert!(error.to_string().contains("untracked"), "{error}");
    drop(loop_runner);
    drop(step_runner);

    let workspace =
        LoopWorkspace::open(&fixture.runs_root, "candidate-development-apply-fault").unwrap();
    let persisted = seaf_loop::state::load_run(&workspace).unwrap();
    let development = persisted
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::Development)
        .unwrap();
    assert_eq!(development.status, seaf_core::LoopStepStatus::Completed);
    assert!(development.artifact_path.is_some());
    assert!(development.artifact_digest.is_some());
    assert_eq!(persisted.policy_decisions.len(), 1);
    assert!(persisted
        .candidate_workspace
        .as_ref()
        .unwrap()
        .patch_transaction
        .is_none());
    assert!(persisted
        .provider_exchange_records
        .iter()
        .all(|record| record.step != LoopStepName::OutputReview));
    let log = fs::read_to_string(workspace.run_directory().join("log.md")).unwrap();
    assert!(log.contains("started step Development"));
    assert!(!log.contains("finished step Development"));
    assert_eq!(provider.requests().unwrap().len(), 5);
    assert_eq!(source_evidence(&fixture.source), source_before);
    remove_candidate(&fixture.source, &fixture.candidate);
}

#[test]
fn blocked_output_review_can_resume_and_rerun_only_the_same_applied_subject() {
    let fixture = fixture("candidate-output-review-blocked-resume");
    let source_before = source_evidence(&fixture.source);
    let mut responses = candidate_responses(false);
    responses.push(response(
        r#"{"role":"output_reviewer","decision":"request_changes","summary":"Review again.","blocking_issues":[{"summary":"Recheck","evidence":"candidate diff"}],"non_blocking_issues":[]}"#,
    ));
    responses.push(response(
        r#"{"role":"output_reviewer","decision":"approve_for_tests","summary":"The same applied subject now passes review.","blocking_issues":[],"non_blocking_issues":[]}"#,
    ));
    let provider = FakeProvider::new(responses);
    let mut first_patch_runner = RecordingPatchRunner::default();
    let mut first_step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
        );
    let mut first =
        LoopRunner::start_initialized(fixture.prepared, &mut first_step_runner).expect("start");
    for _ in 0..6 {
        assert!(first.run_next_step().expect("through blocked OutputReview"));
    }
    assert_eq!(first.run().status, seaf_core::LoopStatus::Blocked);
    let blocked = first.run().clone();
    let applied_before = blocked.candidate_workspace.clone();
    drop(first);
    drop(first_step_runner);
    assert_eq!(first_patch_runner.calls.len(), 1);

    let prepared = InitializedLoopRun::resume_isolated(&fixture.runs_root, blocked)
        .expect("Applied blocked run resumes read-only")
        .scaffold()
        .unwrap()
        .publish_authoritative_inputs(authoritative_snapshots(
            &fixture.source,
            &fixture.ticket,
            &fixture.policy,
        ))
        .unwrap();
    let mut resumed_patch_runner = RecordingPatchRunner::default();
    let mut resumed_step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
    let resumed = LoopRunner::resume_initialized(prepared, &mut resumed_step_runner)
        .expect("resume exact blocked history");
    let mut rerun = resumed
        .rerun_from(LoopStepName::OutputReview)
        .expect("rerun OutputReview after terminal resume");
    assert!(rerun.run_next_step().expect("OutputReview attempt two"));
    assert_eq!(rerun.run().candidate_workspace, applied_before);
    assert_eq!(source_evidence(&fixture.source), source_before);
    drop(rerun);
    drop(resumed_step_runner);
    assert!(resumed_patch_runner.calls.is_empty());
    remove_candidate(&fixture.source, &fixture.candidate);
}

#[test]
fn resumed_output_review_rejects_configured_model_mismatch_before_any_mutation() {
    let fixture = fixture("candidate-output-review-model-mismatch");
    let source_before = source_evidence(&fixture.source);
    let provider = FakeProvider::new(candidate_responses(false));
    let mut patch_runner = RecordingPatchRunner::default();
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
        );
    let mut loop_runner =
        LoopRunner::start_initialized(fixture.prepared, &mut step_runner).expect("start");
    for _ in 0..5 {
        assert!(loop_runner.run_next_step().expect("through Development"));
    }
    let run = loop_runner.run().clone();
    drop(loop_runner);
    drop(step_runner);
    let candidate_before = source_evidence(&fixture.candidate);
    let workspace =
        LoopWorkspace::open(&fixture.runs_root, "candidate-output-review-model-mismatch").unwrap();
    let run_tree_before = snapshot_files(workspace.run_directory());
    let mismatch_provider = FakeProvider::new(Vec::new());
    let mut mismatch_patch_runner = RecordingPatchRunner::default();
    let mut mismatch = ProviderStepRunner::new(&mismatch_provider, "wrong-model", 30_000)
        .with_ticket(fixture.ticket.clone())
        .with_context_pack_request(context_request(&fixture.candidate, &fixture.ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(
                &fixture.candidate,
                &fixture.ticket,
                fixture.policy.clone(),
                true,
            ),
            &mut mismatch_patch_runner,
        );
    let error = mismatch
        .prepare_run(&workspace, &run)
        .expect_err("configured model must equal authoritative run model");
    assert!(error.to_string().contains("model"), "{error}");
    drop(mismatch);
    assert_eq!(snapshot_files(workspace.run_directory()), run_tree_before);
    assert!(mismatch_provider.requests().unwrap().is_empty());
    assert!(mismatch_patch_runner.calls.is_empty());
    assert_eq!(source_evidence(&fixture.source), source_before);
    assert_eq!(source_evidence(&fixture.candidate), candidate_before);
    remove_candidate(&fixture.source, &fixture.candidate);
}

fn candidate_responses(
    include_output_review: bool,
) -> Vec<Result<ModelResponse, seaf_models::ModelError>> {
    let mut responses = vec![
        response(include_str!(
            "../../../fixtures/model-responses/research.valid.json"
        )),
        response(include_str!(
            "../../../fixtures/model-responses/analyzer.valid.json"
        )),
        response(include_str!(
            "../../../fixtures/model-responses/spec_writer.valid.json"
        )),
        response(include_str!(
            "../../../fixtures/model-responses/spec_reviewer.valid.json"
        )),
        response(
            r#"{"role":"developer","status":"patch_proposed","summary":"Add file","changed_files":["src/new.rs"],"requires_human_review":false,"patch":"diff --git a/src/new.rs b/src/new.rs\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/src/new.rs\n@@ -0,0 +1 @@\n+pub fn added() {}\n"}"#,
        ),
    ];
    if include_output_review {
        responses.push(response(
            r#"{"role":"output_reviewer","decision":"approve_for_tests","summary":"The applied candidate matches the approved spec.","blocking_issues":[],"non_blocking_issues":[]}"#,
        ));
    }
    responses
}

fn snapshot_files(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut std::collections::BTreeMap<String, Vec<u8>>,
    ) {
        let mut entries = fs::read_dir(directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                visit(root, &path, files);
            } else if metadata.is_file() {
                files.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    visit(root, root, &mut files);
    files
}

struct Fixture {
    _temp: tempfile::TempDir,
    runs_root: PathBuf,
    source: PathBuf,
    candidate: PathBuf,
    ticket: TicketSpec,
    policy: Policy,
    prepared: PreparedLoopRun,
}

fn fixture(run_id: &str) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    git_ok(&source, &["init", "-q"]);
    git_ok(&source, &["config", "user.email", "test@example.com"]);
    git_ok(&source, &["config", "user.name", "SEAF Test"]);
    fs::create_dir(source.join("src")).unwrap();
    fs::write(source.join("src/lib.rs"), "pub fn existing() {}\n").unwrap();
    git_ok(&source, &["add", "."]);
    git_ok(&source, &["commit", "-qm", "initial"]);
    let ticket = ticket();
    let policy = policy();
    let config = serde_json::json!({"policy_path":"seaf.policy.json"});
    let repository = serde_json::json!({"source":source.canonicalize().unwrap()});
    let ticket_bytes = canonical_json_bytes(&ticket).unwrap();
    let policy_bytes = canonical_json_bytes(&policy).unwrap();
    let config_bytes = canonical_json_bytes(&config).unwrap();
    let repository_bytes = canonical_json_bytes(&repository).unwrap();
    let runs_root = temp.path().join("runs");
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
            },
        ),
        &source,
    )
    .unwrap();
    let candidate = PathBuf::from(&initialized.run().candidate_workspace.as_ref().unwrap().path);
    let prepared = initialized
        .scaffold()
        .unwrap()
        .publish_authoritative_inputs(AuthoritativeRunInputSnapshots {
            ticket: ticket_bytes.clone(),
            provider_ticket: ticket_bytes,
            policy: policy_bytes,
            config: config_bytes,
            repository: repository_bytes,
        })
        .unwrap();
    Fixture {
        _temp: temp,
        runs_root,
        source,
        candidate,
        ticket,
        policy,
        prepared,
    }
}

fn authoritative_snapshots(
    source: &Path,
    ticket: &TicketSpec,
    policy: &Policy,
) -> AuthoritativeRunInputSnapshots {
    let ticket_bytes = canonical_json_bytes(ticket).unwrap();
    AuthoritativeRunInputSnapshots {
        ticket: ticket_bytes.clone(),
        provider_ticket: ticket_bytes,
        policy: canonical_json_bytes(policy).unwrap(),
        config: canonical_json_bytes(&serde_json::json!({"policy_path":"seaf.policy.json"}))
            .unwrap(),
        repository: canonical_json_bytes(
            &serde_json::json!({"source":source.canonicalize().unwrap()}),
        )
        .unwrap(),
    }
}

fn ticket() -> TicketSpec {
    TicketSpec {
        ticket_id: "T-CANDIDATE".into(),
        goal_id: "production-use".into(),
        title: "Check candidate patch".into(),
        status: TicketStatus::Ready,
        priority: TicketPriority::P1,
        problem: "Patch must stay isolated.".into(),
        research_questions: vec![],
        context: TicketContext {
            relevant_files: vec!["src/lib.rs".into()],
            forbidden_files: vec![],
        },
        autonomy: TicketAutonomy {
            level: 1,
            apply_patch: true,
            allow_shell_commands: vec![],
        },
        acceptance_criteria: vec!["Source remains unchanged.".into()],
        eval: None,
    }
}

fn policy() -> Policy {
    Policy {
        policy_id: "test".into(),
        default_autonomy_level: 1,
        forbidden_paths: vec!["secrets/**".into()],
        requires_human_review: vec!["dependency_changes".into()],
        allowed_without_review: vec!["source_changes".into()],
    }
}

fn context_request(root: &Path, ticket: &TicketSpec) -> ContextPackRequest {
    ContextPackRequest::for_ticket(
        root,
        Path::new("unused"),
        ticket,
        &[],
        ContextLimits {
            max_bytes_per_file: 4096,
            max_total_bytes: 8192,
        },
    )
}

#[derive(Default)]
struct RecordingPatchRunner {
    calls: Vec<(PathBuf, PatchCommand)>,
}
impl PatchCommandRunner for RecordingPatchRunner {
    fn run(
        &mut self,
        root: &Path,
        command: PatchCommand,
        _patch: &str,
    ) -> Result<CommandOutput, PatchGateError> {
        self.calls.push((root.canonicalize().unwrap(), command));
        Ok(CommandOutput::success())
    }
}

fn response(content: &str) -> Result<ModelResponse, seaf_models::ModelError> {
    Ok(ModelResponse {
        content: content.to_string(),
        latency_ms: 1,
        raw_provider_metadata: serde_json::Value::Null,
    })
}
fn source_evidence(root: &Path) -> (String, String, Vec<u8>) {
    (
        git(root, &["rev-parse", "HEAD"]),
        git(root, &["status", "--porcelain=v1"]),
        fs::read(root.join("src/lib.rs")).unwrap(),
    )
}
fn remove_candidate(source: &Path, candidate: &Path) {
    git_ok(
        source,
        &["worktree", "remove", "--force", candidate.to_str().unwrap()],
    );
}
fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}
fn git_ok(root: &Path, args: &[&str]) {
    let _ = git(root, args);
}
