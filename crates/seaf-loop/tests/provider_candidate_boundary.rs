use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, ArtifactReference, CheckStatus, EvalCheck,
    EvalDecision, EvalLoopEvidence, EvalReport, LoopInputDigests, LoopStepName, Policy,
    ProviderExchangeKind, ProviderExchangePhase, ProviderExchangeRecord, ProviderRole,
    RecoveryReference, RiskLevel, TicketAutonomy, TicketContext, TicketPriority, TicketSpec,
    TicketStatus,
};
use seaf_loop::recovery::{
    EvaluationInvalidationAttemptV3, EvaluationInvalidationSourceRunV3,
    EvaluationPrefixAuthorityV1, EvaluationPrefixSpellingV1, EvaluationRecoveryAction,
    EvaluationRecoveryAttemptV2, EvaluationRecoveryReportDisposition,
    EvaluationRecoverySourceRunV2, EVALUATION_RECOVERY_SCHEMA_VERSION,
};
use seaf_loop::{
    adopt_approved_evaluation, approve_candidate_for_testing, artifacts::write_step_request,
    cleanup_candidate_workspace_outcome, invalidate_approved_evaluation,
    load_provider_exchange_record, load_provider_exchange_request,
    load_verified_final_evaluation_authority, load_verified_recovery_authority_kind,
    persist_provider_exchange_record_reference, promote_evaluated_candidate,
    rerun_invalidated_evaluation, revise_provider_step, stage_provider_exchange_record,
    verify_candidate_patch_evidence, write_provider_exchange_request,
    AuthoritativeRunInputSnapshots, CommandOutput, ContextLimits, ContextPackRequest,
    InitializedLoopRun, LoopRunner, LoopRunnerConfig, LoopWorkspace, PatchCommand,
    PatchCommandRunner, PatchGateError, PreparedLoopRun, ProviderExchangeCoordinates,
    ProviderExchangeResponseAudit, ProviderPatchGateConfig, ProviderStepRunner, StepRunner,
    TestingEvidence, PROVIDER_EXCHANGE_SCHEMA_VERSION,
};
use seaf_models::{FakeProvider, ModelResponse};
use sha2::{Digest, Sha256};

const RUN_TREE_BYTE_CAP: u64 = 32 * 1024 * 1024;
const RUN_TREE_ENTRY_CAP: usize = 4096;
const EVALUATION_EVIDENCE_CAP: u64 = 2 * 1024 * 1024;

#[test]
fn secret_free_v2_indexed_final_authority_remains_readable() {
    let (fixture, approved) = approved_fixture("v2-final-compatibility");
    let run = publish_indexed_final_eval_artifacts_v2(&fixture.workspace, &approved, true);
    load_verified_final_evaluation_authority(&fixture.workspace, &run)
        .expect("secret-free V2 final authority remains compatible");
    fixture.cleanup();
}

#[test]
fn final_authority_rejects_ordered_testing_name_substitution() {
    let (fixture, approved) = approved_fixture("testing-name-substitution");
    let mut run = publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    let testing_path = "artifacts/07-testing.attempt-001.json";
    let mut testing: TestingEvidence = serde_json::from_slice(
        &fs::read(fixture.workspace.run_directory().join(testing_path)).unwrap(),
    )
    .unwrap();
    testing.checks[0].name = "substituted".to_string();
    let bytes = canonical_json_bytes(&testing).unwrap();
    write_private_run_fixture(&fixture.workspace, testing_path, &bytes);
    let testing_step = run
        .steps
        .iter_mut()
        .find(|step| step.name == LoopStepName::Testing)
        .unwrap();
    testing_step.artifact_digest = Some(format!("{:x}", Sha256::digest(&bytes)));

    let error = load_verified_final_evaluation_authority(&fixture.workspace, &run)
        .expect_err("substituted Testing name must fail exact intent binding");

    assert!(error.to_string().contains("ordered evaluation intent"));
    fixture.cleanup();
}

#[test]
fn structural_secret_rejection_precedes_intent_and_command_side_effects() {
    let (fixture, _approved) = approved_fixture_with_eval_mode(
        "evaluation-structural-secret",
        FixtureEvalMode::StructuralSecret,
    );
    let marker = fixture._temp.path().join("eval-spawned.marker");

    let error = seaf_loop::execute_approved_evaluation(&fixture.workspace, &fixture.source)
        .expect_err("secret-bearing planned command must fail closed");

    assert!(error.to_string().contains("prohibited credential material"));
    assert!(!error.to_string().contains(PROVIDER_SECRET_FIXTURE));
    assert!(!marker.exists());
    assert!(!fixture
        .workspace
        .run_directory()
        .join("artifacts/07-testing.attempt-001.execution-intent.json")
        .exists());
    fixture.cleanup();
}

#[test]
fn configured_secret_is_identical_and_safe_in_provider_call_and_request_audit() {
    let Fixture {
        _temp,
        runs_root,
        source,
        candidate,
        ticket,
        policy,
        prepared,
    } = fixture_with_eval_mode(
        "provider-request-redaction",
        FixtureEvalMode::ProviderRequestSecret,
    );
    let provider = FakeProvider::new(candidate_responses(true));
    let mut patch_runner = RecordingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&candidate, &ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(&candidate, &ticket, policy, true),
            &mut patch_runner,
        );
    let mut runner = LoopRunner::start_initialized(prepared, &mut step_runner).unwrap();
    runner.run_next_step().unwrap();
    let run_directory = runs_root.join("provider-request-redaction");
    let sent = provider.requests().unwrap().remove(0);
    let sent_bytes = serde_json::to_vec_pretty(&sent).unwrap();
    assert!(!sent_bytes
        .windows(PROVIDER_SECRET_FIXTURE.len())
        .any(|part| { part == PROVIDER_SECRET_FIXTURE.as_bytes() }));
    let role_input: serde_json::Value = serde_json::from_str(&sent.messages[0].content).unwrap();
    let context_authority = &role_input["repository_context_authority"];
    assert!(context_authority["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning
            .as_str()
            .is_some_and(|warning| warning.contains(
                "excluded src/provider-context.txt because it contains prohibited credential material"
            ))));
    assert!(!context_authority["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"] == "src/provider-context.txt"));
    let record_reference = runner.run().provider_exchange_records[0].clone();
    let record = load_provider_exchange_record(&run_directory, &record_reference).unwrap();
    let audited = load_provider_exchange_request(&run_directory, &record.request).unwrap();
    assert_eq!(audited, sent_bytes);
    let secret_digest = hex::encode(Sha256::digest(PROVIDER_SECRET_FIXTURE.as_bytes()));
    for (path, bytes) in directory_snapshot(&run_directory) {
        if path == Path::new("inputs/eval-config.json") {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(PROVIDER_SECRET_FIXTURE),
            "{}",
            path.display()
        );
        assert!(!text.contains(&secret_digest), "{}", path.display());
    }
    drop(runner);
    drop(step_runner);
    remove_candidate(&source, &candidate);
}

#[test]
fn provider_exact_record_and_future_run_envelopes_fail_before_unsafe_publication() {
    for (case, secret, expected_provider_calls) in [
        ("running-run", "\"status\": \"running\"", 0),
        ("request-record", "\"phase\": \"request\"", 0),
        ("request-run", "\"provider_exchange_records\": [\n    {", 0),
        ("response-record", "\"phase\": \"response\"", 1),
        ("response-run", "\n    },\n    {\n      \"run_id\"", 1),
    ] {
        let Fixture {
            _temp,
            runs_root,
            source,
            candidate,
            ticket,
            policy,
            prepared,
        } = fixture_with_eval_mode(
            &format!("provider-envelope-{case}"),
            FixtureEvalMode::ProviderEnvelopeSecret(secret),
        );
        let provider = FakeProvider::new(candidate_responses(true));
        let mut patch_runner = RecordingPatchRunner::default();
        let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_context_pack_request(context_request(&candidate, &ticket))
            .with_patch_gate(
                ProviderPatchGateConfig::for_ticket(&candidate, &ticket, policy, true),
                &mut patch_runner,
            );
        let mut runner = LoopRunner::start_initialized(prepared, &mut step_runner).unwrap();
        let run_directory = runs_root.join(format!("provider-envelope-{case}"));
        let raw_digest = hex::encode(Sha256::digest(secret.as_bytes()));

        let error = runner
            .run_next_step()
            .expect_err("an unsafe exact provider envelope must fail closed");

        assert!(
            error.to_string().contains("prohibited credential material"),
            "{case}: {error}"
        );
        assert!(!error.to_string().contains(secret), "{case}: {error}");
        assert!(!error.to_string().contains(&raw_digest), "{case}: {error}");
        assert_eq!(
            provider.requests().unwrap().len(),
            expected_provider_calls,
            "{case}"
        );
        let persisted = seaf_loop::state::load_run(
            &LoopWorkspace::open(&runs_root, &format!("provider-envelope-{case}")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            persisted.provider_exchange_records.len(),
            expected_provider_calls,
            "{case}"
        );
        let stem = "01-research.attempt-001.exchange-001.initial";
        if expected_provider_calls == 0 {
            assert!(!run_directory
                .join(format!("prompts/{stem}.request.md"))
                .exists());
            assert!(!run_directory
                .join(format!("artifacts/{stem}.request.record.json"))
                .exists());
        } else {
            assert!(!run_directory
                .join(format!("artifacts/{stem}.response.record.json"))
                .exists());
        }
        for (path, bytes) in directory_snapshot(&run_directory) {
            if path == Path::new("inputs/eval-config.json") {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains(secret), "{case}: {}", path.display());
            assert!(!text.contains(&raw_digest), "{case}: {}", path.display());
        }
        drop(runner);
        drop(step_runner);
        remove_candidate(&source, &candidate);
    }
}

#[test]
fn provider_fixed_log_line_rejects_before_the_paired_run_mutation() {
    let secret = "started step Research\n";
    let Fixture {
        _temp,
        runs_root,
        source,
        candidate,
        ticket,
        policy,
        prepared,
    } = fixture_with_eval_mode(
        "provider-started-log-envelope",
        FixtureEvalMode::ProviderEnvelopeSecret(secret),
    );
    let provider = FakeProvider::new(candidate_responses(true));
    let mut patch_runner = RecordingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&candidate, &ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(&candidate, &ticket, policy, true),
            &mut patch_runner,
        );
    let mut runner = LoopRunner::start_initialized(prepared, &mut step_runner).unwrap();
    let run_directory = runs_root.join("provider-started-log-envelope");
    let before_run = runner.run().clone();
    let before_tree = directory_snapshot(&run_directory);

    let error = runner
        .run_next_step()
        .expect_err("unsafe fixed log line must reject before starting the step");

    assert!(
        error.to_string().contains("prohibited credential material"),
        "{error}"
    );
    assert!(!error.to_string().contains(secret));
    assert_eq!(runner.run(), &before_run);
    assert_eq!(directory_snapshot(&run_directory), before_tree);
    assert!(provider.requests().unwrap().is_empty());
    drop(runner);
    drop(step_runner);
    remove_candidate(&source, &candidate);
}

#[test]
fn configured_secret_provider_response_persists_only_fixed_safe_failure() {
    let Fixture {
        _temp,
        runs_root,
        source,
        candidate,
        ticket,
        policy,
        prepared,
        ..
    } = fixture_with_eval_mode(
        "provider-response-redaction",
        FixtureEvalMode::ProviderSecret,
    );
    let raw = format!("invalid {PROVIDER_SECRET_FIXTURE}");
    let raw_digest = hex::encode(Sha256::digest(raw.as_bytes()));
    let secret_digest = hex::encode(Sha256::digest(PROVIDER_SECRET_FIXTURE.as_bytes()));
    let raw_response = ModelResponse {
        content: raw,
        latency_ms: 1,
        raw_provider_metadata: serde_json::json!({"provider": "fake"}),
    };
    let raw_audit_digest = canonical_sha256_digest(&ProviderExchangeResponseAudit::ModelResponse {
        response: raw_response.clone(),
    })
    .unwrap();
    let provider = FakeProvider::new(vec![Ok(raw_response)]);
    let mut patch_runner = RecordingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&candidate, &ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(&candidate, &ticket, policy, true),
            &mut patch_runner,
        );
    let mut runner = LoopRunner::start_initialized(prepared, &mut step_runner).unwrap();
    runner.run_next_step().unwrap();
    let run_directory = runs_root.join("provider-response-redaction");
    for (path, bytes) in directory_snapshot(&run_directory) {
        if matches!(path.to_string_lossy().as_ref(), "inputs/eval-config.json") {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(PROVIDER_SECRET_FIXTURE),
            "{}",
            path.display()
        );
        assert!(!text.contains(&raw_digest), "{}", path.display());
        assert!(!text.contains(&secret_digest), "{}", path.display());
        assert!(!text.contains(&raw_audit_digest), "{}", path.display());
    }
    let response = fs::read_to_string(
        run_directory.join("responses/01-research.attempt-001.exchange-001.initial.response.json"),
    )
    .unwrap();
    assert!(response.contains("provider_response_contains_secret"));
    assert_eq!(provider.requests().unwrap().len(), 1);
    drop(runner);
    drop(step_runner);
    remove_candidate(&source, &candidate);
}

#[test]
fn evaluation_intent_capacity_boundaries_refuse_before_intent_or_child_process() {
    for (label, extra, should_succeed) in [("byte-exact", 0_u64, true), ("byte-short", 1, false)] {
        let (fixture, approved) = approved_fixture_with_eval_mode(
            &format!("evaluation-capacity-{label}"),
            FixtureEvalMode::Marker,
        );
        let intent_size = intended_evaluation_intent_size(&fixture, &approved);
        let commitment = 2 * 64 * 1024 + 5 * EVALUATION_EVIDENCE_CAP;
        let current = run_tree_usage(fixture.workspace.run_directory()).0;
        let target = RUN_TREE_BYTE_CAP
            .checked_sub(current)
            .and_then(|bytes| bytes.checked_sub(u64::try_from(intent_size).unwrap()))
            .and_then(|bytes| bytes.checked_sub(commitment))
            .unwrap()
            + extra;
        fill_run_bytes(fixture.workspace.run_directory(), target, label);
        let marker = fixture.source.parent().unwrap().join("eval-spawned.marker");
        let result = seaf_loop::execute_approved_evaluation(&fixture.workspace, &fixture.source);
        assert_eq!(result.is_ok(), should_succeed, "{result:?}");
        assert_eq!(marker.exists(), should_succeed);
        assert_eq!(
            fixture
                .workspace
                .run_directory()
                .join("artifacts/07-testing.attempt-001.execution-intent.json")
                .exists(),
            should_succeed
        );
        fixture.cleanup();
    }

    for (label, free_entries, should_succeed) in [
        ("entry-exact", 8_usize, true),
        ("entry-short", 7_usize, false),
    ] {
        let (fixture, _approved) = approved_fixture_with_eval_mode(
            &format!("evaluation-capacity-{label}"),
            FixtureEvalMode::Marker,
        );
        let current = run_tree_usage(fixture.workspace.run_directory()).1;
        fill_run_entries(
            fixture.workspace.run_directory(),
            RUN_TREE_ENTRY_CAP - current - free_entries,
            label,
        );
        let marker = fixture.source.parent().unwrap().join("eval-spawned.marker");
        let result = seaf_loop::execute_approved_evaluation(&fixture.workspace, &fixture.source);
        assert_eq!(result.is_ok(), should_succeed, "{result:?}");
        assert_eq!(marker.exists(), should_succeed);
        assert_eq!(
            fixture
                .workspace
                .run_directory()
                .join("artifacts/07-testing.attempt-001.execution-intent.json")
                .exists(),
            should_succeed
        );
        fixture.cleanup();
    }
}

#[test]
fn wrong_same_name_log_retains_residual_and_cannot_fund_a_generic_writer() {
    let (fixture, approved) =
        approved_fixture_with_eval_mode("evaluation-wrong-log-residual", FixtureEvalMode::Marker);
    let intent = intended_evaluation_intent_bytes(&fixture, &approved);
    write_private_run_fixture(
        &fixture.workspace,
        "artifacts/07-testing.attempt-001.execution-intent.json",
        &intent,
    );
    let commitment = 2 * 64 * 1024 + 5 * EVALUATION_EVIDENCE_CAP;
    let current = run_tree_usage(fixture.workspace.run_directory()).0;
    fill_run_bytes(
        fixture.workspace.run_directory(),
        RUN_TREE_BYTE_CAP - current - commitment,
        "wrong-log-residual",
    );
    let wrong_log = fixture
        .workspace
        .run_directory()
        .join("artifacts/07-testing.attempt-001.check-001.stdout.log");
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    std::io::Write::write_all(&mut options.open(&wrong_log).unwrap(), b"x").unwrap();

    let error = write_step_request(&fixture.workspace, LoopStepName::Testing, 99, "unrelated")
        .expect_err("a one-byte wrong log must not release the other 65,535 reserved bytes");

    assert!(error.to_string().contains("commitment"), "{error}");
    assert!(!fixture
        .workspace
        .run_directory()
        .join("prompts/07-testing.attempt-099.prompt.md")
        .exists());
    fixture.cleanup();
}

#[test]
fn hard_linked_same_name_log_cannot_release_capacity() {
    let (fixture, approved) = approved_fixture_with_eval_mode(
        "evaluation-hardlinked-log-residual",
        FixtureEvalMode::Marker,
    );
    let intent = intended_evaluation_intent_bytes(&fixture, &approved);
    write_private_run_fixture(
        &fixture.workspace,
        "artifacts/07-testing.attempt-001.execution-intent.json",
        &intent,
    );
    let source = fixture
        .workspace
        .run_directory()
        .join("artifacts/hardlink-source.log");
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    std::io::Write::write_all(&mut options.open(&source).unwrap(), b"x").unwrap();
    let wrong_log = fixture
        .workspace
        .run_directory()
        .join("artifacts/07-testing.attempt-001.check-001.stdout.log");
    fs::hard_link(&source, &wrong_log).unwrap();

    let error = write_step_request(
        &fixture.workspace,
        LoopStepName::Testing,
        99,
        "must not publish",
    )
    .expect_err("a hard-linked canonical log name cannot consume an evaluation slot");

    assert!(
        error.to_string().contains("single-link") || error.to_string().contains("link"),
        "{error}"
    );
    assert!(!fixture
        .workspace
        .run_directory()
        .join("prompts/07-testing.attempt-099.prompt.md")
        .exists());
    fixture.cleanup();
}

#[test]
fn malformed_or_substituted_testing_cannot_release_capacity() {
    let (fixture, approved) = approved_fixture_with_eval_mode(
        "evaluation-malformed-testing-residual",
        FixtureEvalMode::Marker,
    );
    let intent = intended_evaluation_intent_bytes(&fixture, &approved);
    write_private_run_fixture(
        &fixture.workspace,
        "artifacts/07-testing.attempt-001.execution-intent.json",
        &intent,
    );
    write_private_run_fixture(
        &fixture.workspace,
        "artifacts/07-testing.attempt-001.json",
        b"{}\n",
    );
    let error = write_step_request(
        &fixture.workspace,
        LoopStepName::Testing,
        99,
        "must not publish",
    )
    .expect_err("malformed Testing bytes cannot release log or Testing slots");
    assert!(error.to_string().contains("Testing") || error.to_string().contains("missing field"));
    assert!(!fixture
        .workspace
        .run_directory()
        .join("prompts/07-testing.attempt-099.prompt.md")
        .exists());
    fixture.cleanup();

    let (fixture, approved) = approved_fixture("evaluation-substituted-testing-residual");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    fs::remove_file(
        fixture
            .workspace
            .run_directory()
            .join("artifacts/08-eval-report.attempt-001.json"),
    )
    .unwrap();
    let testing_path = "artifacts/07-testing.attempt-001.json";
    let mut testing: TestingEvidence = serde_json::from_slice(
        &fs::read(fixture.workspace.run_directory().join(testing_path)).unwrap(),
    )
    .unwrap();
    testing.run_id = "substituted-run".into();
    let bytes = canonical_json_bytes(&testing).unwrap();
    write_private_run_fixture(&fixture.workspace, testing_path, &bytes);
    let error = write_step_request(
        &fixture.workspace,
        LoopStepName::Testing,
        99,
        "must not publish",
    )
    .expect_err("substituted Testing authority cannot release its slot");
    assert!(error.to_string().contains("Testing"), "{error}");
    assert!(!fixture
        .workspace
        .run_directory()
        .join("prompts/07-testing.attempt-099.prompt.md")
        .exists());
    fixture.cleanup();
}

#[test]
fn forged_staged_evaluation_sources_cannot_release_capacity_or_fund_generic_writes() {
    let (fixture, approved) = approved_fixture("forged-staged-adoption-source");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    fs::remove_file(
        fixture
            .workspace
            .run_directory()
            .join("artifacts/08-eval-report.attempt-001.json"),
    )
    .unwrap();
    let adopted = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "stage adoption authority",
    )
    .unwrap();
    write_raw_run(&fixture.workspace, &approved);
    fs::remove_file(
        fixture
            .workspace
            .run_directory()
            .join(&adopted.reference.artifact.path),
    )
    .unwrap();
    fs::remove_file(
        fixture
            .workspace
            .run_directory()
            .join("artifacts/08-eval-report.attempt-001.json"),
    )
    .unwrap();
    let source_path = format!(
        "artifacts/recovery-{:03}.source-run.json",
        adopted.reference.recovery_id
    );
    let mut source: EvaluationRecoverySourceRunV2 = serde_json::from_slice(
        &fs::read(fixture.workspace.run_directory().join(&source_path)).unwrap(),
    )
    .unwrap();
    source.evaluation_prefix.execution_intent.digest = "0".repeat(64);
    let source_bytes = canonical_json_bytes(&source).unwrap();
    write_private_run_fixture(&fixture.workspace, &source_path, &source_bytes);

    let error = write_step_request(
        &fixture.workspace,
        LoopStepName::Testing,
        99,
        "must not publish",
    )
    .expect_err("a forged canonical v2 source cannot release its source/report slots");
    assert!(error.to_string().contains("source"), "{error}");
    assert!(!fixture
        .workspace
        .run_directory()
        .join("prompts/07-testing.attempt-099.prompt.md")
        .exists());
    fixture.cleanup();

    let (fixture, approved) = approved_fixture("forged-staged-adoption-report");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    let adopted = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "stage report-bound adoption authority",
    )
    .unwrap();
    write_raw_run(&fixture.workspace, &approved);
    fs::remove_file(
        fixture
            .workspace
            .run_directory()
            .join(&adopted.reference.artifact.path),
    )
    .unwrap();
    let report_path = "artifacts/08-eval-report.attempt-001.json";
    let mut report: EvalReport = serde_json::from_slice(
        &fs::read(fixture.workspace.run_directory().join(report_path)).unwrap(),
    )
    .unwrap();
    report.checks[0].summary = Some("substituted report semantics".into());
    let report_bytes = canonical_json_bytes(&report).unwrap();
    write_private_run_fixture(&fixture.workspace, report_path, &report_bytes);
    let source_path = format!(
        "artifacts/recovery-{:03}.source-run.json",
        adopted.reference.recovery_id
    );
    let mut source: EvaluationRecoverySourceRunV2 = serde_json::from_slice(
        &fs::read(fixture.workspace.run_directory().join(&source_path)).unwrap(),
    )
    .unwrap();
    source
        .evaluation_prefix
        .eval_report
        .as_mut()
        .unwrap()
        .digest = format!("{:x}", Sha256::digest(&report_bytes));
    let source_bytes = canonical_json_bytes(&source).unwrap();
    write_private_run_fixture(&fixture.workspace, &source_path, &source_bytes);

    let error = write_step_request(
        &fixture.workspace,
        LoopStepName::Testing,
        99,
        "must not publish",
    )
    .expect_err("an unbound staged report cannot release its report slot");
    assert!(error.to_string().contains("EvalReport"), "{error}");
    assert!(!fixture
        .workspace
        .run_directory()
        .join("prompts/07-testing.attempt-099.prompt.md")
        .exists());
    fixture.cleanup();

    let (fixture, approved) = approved_fixture("forged-staged-invalidation-source");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    for path in [
        "artifacts/07-testing.attempt-001.check-001.stdout.log",
        "artifacts/07-testing.attempt-001.check-001.stderr.log",
        "artifacts/07-testing.attempt-001.json",
        "artifacts/08-eval-report.attempt-001.json",
    ] {
        fs::remove_file(fixture.workspace.run_directory().join(path)).unwrap();
    }
    let invalidated = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "stage invalidation authority",
    )
    .unwrap();
    write_raw_run(&fixture.workspace, &approved);
    fs::remove_file(
        fixture
            .workspace
            .run_directory()
            .join(&invalidated.reference.artifact.path),
    )
    .unwrap();
    let source_path = format!(
        "artifacts/recovery-{:03}.source-run.json",
        invalidated.reference.recovery_id
    );
    let mut source: EvaluationInvalidationSourceRunV3 = serde_json::from_slice(
        &fs::read(fixture.workspace.run_directory().join(&source_path)).unwrap(),
    )
    .unwrap();
    source.evaluation_prefix.present_artifacts[0].digest = "0".repeat(64);
    let source_bytes = canonical_json_bytes(&source).unwrap();
    write_private_run_fixture(&fixture.workspace, &source_path, &source_bytes);

    let error = write_step_request(
        &fixture.workspace,
        LoopStepName::Testing,
        99,
        "must not publish",
    )
    .expect_err("a forged canonical v3 source cannot release its source slot");
    assert!(error.to_string().contains("source"), "{error}");
    assert!(!fixture
        .workspace
        .run_directory()
        .join("prompts/07-testing.attempt-099.prompt.md")
        .exists());
    fixture.cleanup();
}

#[test]
fn forged_invalidation_reference_cannot_supersede_an_active_prefix() {
    let (fixture, approved) = approved_fixture("forged-invalidation-supersession");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    for path in [
        "artifacts/07-testing.attempt-001.check-001.stdout.log",
        "artifacts/07-testing.attempt-001.check-001.stderr.log",
        "artifacts/07-testing.attempt-001.json",
        "artifacts/08-eval-report.attempt-001.json",
    ] {
        fs::remove_file(fixture.workspace.run_directory().join(path)).unwrap();
    }
    let invalidated = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "publish valid invalidation",
    )
    .unwrap();
    let recovery_path = invalidated.reference.artifact.path.clone();
    let mut recovery: EvaluationInvalidationAttemptV3 = serde_json::from_slice(
        &fs::read(fixture.workspace.run_directory().join(&recovery_path)).unwrap(),
    )
    .unwrap();
    recovery.next_evaluation_attempt += 1;
    let recovery_bytes = canonical_json_bytes(&recovery).unwrap();
    write_private_run_fixture(&fixture.workspace, &recovery_path, &recovery_bytes);
    let mut forged_run = invalidated.run;
    forged_run.latest_recovery.as_mut().unwrap().artifact.digest =
        format!("{:x}", Sha256::digest(&recovery_bytes));
    write_raw_run(&fixture.workspace, &forged_run);

    let error = write_step_request(
        &fixture.workspace,
        LoopStepName::Testing,
        99,
        "must not publish",
    )
    .expect_err("a forged v3 reference cannot supersede the factual attempt prefix");
    assert!(error.to_string().contains("invalidation"), "{error}");
    assert!(!fixture
        .workspace
        .run_directory()
        .join("prompts/07-testing.attempt-099.prompt.md")
        .exists());
    fixture.cleanup();
}

#[test]
fn staged_invalidation_source_cannot_select_an_older_contiguous_attempt() {
    let (fixture, approved) = approved_fixture("staged-invalidation-older-attempt");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    for path in [
        "artifacts/07-testing.attempt-001.check-001.stdout.log",
        "artifacts/07-testing.attempt-001.check-001.stderr.log",
        "artifacts/07-testing.attempt-001.json",
        "artifacts/08-eval-report.attempt-001.json",
    ] {
        fs::remove_file(fixture.workspace.run_directory().join(path)).unwrap();
    }
    let invalidated = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "capture valid attempt-one invalidation source",
    )
    .unwrap();
    write_raw_run(&fixture.workspace, &approved);
    fs::remove_file(
        fixture
            .workspace
            .run_directory()
            .join(&invalidated.reference.artifact.path),
    )
    .unwrap();
    let first_intent = fixture
        .workspace
        .run_directory()
        .join("artifacts/07-testing.attempt-001.execution-intent.json");
    let mut second_intent: serde_json::Value =
        serde_json::from_slice(&fs::read(first_intent).unwrap()).unwrap();
    second_intent["evaluation_attempt"] = serde_json::json!(2);
    write_private_run_fixture(
        &fixture.workspace,
        "artifacts/07-testing.attempt-002.execution-intent.json",
        canonical_json_bytes(&second_intent).unwrap(),
    );
    let current = run_tree_usage(fixture.workspace.run_directory()).0;
    let newer_attempt_commitment = 2 * 64 * 1024 + 5 * EVALUATION_EVIDENCE_CAP;
    fill_run_bytes(
        fixture.workspace.run_directory(),
        RUN_TREE_BYTE_CAP - current - newer_attempt_commitment,
        "newer-attempt-commitment",
    );

    let error = write_step_request(
        &fixture.workspace,
        LoopStepName::Testing,
        99,
        "must not publish",
    )
    .expect_err("a staged v3 source for attempt one cannot release attempt two capacity");

    assert!(error.to_string().contains("active attempt"), "{error}");
    assert!(!fixture
        .workspace
        .run_directory()
        .join("prompts/07-testing.attempt-099.prompt.md")
        .exists());
    fixture.cleanup();
}

#[test]
fn create_missing_unbound_report_retains_residual_capacity_end_to_end() {
    let (fixture, approved) = approved_fixture("create-missing-unbound-report-residual");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    let report_path = "artifacts/08-eval-report.attempt-001.json";
    fs::remove_file(fixture.workspace.run_directory().join(report_path)).unwrap();
    let adopted = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "capture authentic CreateMissing source",
    )
    .unwrap();
    assert_eq!(
        adopted.recovery.report_disposition,
        EvaluationRecoveryReportDisposition::CreateMissing
    );
    write_raw_run(&fixture.workspace, &approved);
    fs::remove_file(
        fixture
            .workspace
            .run_directory()
            .join(&adopted.reference.artifact.path),
    )
    .unwrap();
    fs::remove_file(fixture.workspace.run_directory().join(report_path)).unwrap();
    let source: EvaluationRecoverySourceRunV2 = serde_json::from_slice(
        &fs::read(
            fixture
                .workspace
                .run_directory()
                .join(&adopted.recovery.source_run.path),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(source.evaluation_prefix.eval_report.is_none());
    let current = run_tree_usage(fixture.workspace.run_directory()).0;
    fill_run_bytes(
        fixture.workspace.run_directory(),
        RUN_TREE_BYTE_CAP - current - 3 * EVALUATION_EVIDENCE_CAP,
        "create-missing-residual",
    );
    write_private_run_fixture(&fixture.workspace, report_path, b"x");

    let error = write_step_request(
        &fixture.workspace,
        LoopStepName::Testing,
        99,
        "must not publish",
    )
    .expect_err("an unbound same-name report cannot finance one unrelated byte");

    assert!(error.to_string().contains("commitment"), "{error}");
    assert!(!fixture
        .workspace
        .run_directory()
        .join("prompts/07-testing.attempt-099.prompt.md")
        .exists());
    fixture.cleanup();
}

#[test]
fn evaluation_lost_capacity_after_check_one_prevents_check_two_and_testing_authority() {
    let (fixture, approved) = approved_fixture_with_eval_mode(
        "evaluation-capacity-between-checks",
        FixtureEvalMode::CapacityAfterFirstCheck,
    );
    let intent_size = intended_evaluation_intent_size(&fixture, &approved);
    let commitment = 4 * 64 * 1024 + 5 * EVALUATION_EVIDENCE_CAP;
    let current = run_tree_usage(fixture.workspace.run_directory()).0;
    let target = RUN_TREE_BYTE_CAP
        .checked_sub(current)
        .and_then(|bytes| bytes.checked_sub(u64::try_from(intent_size).unwrap()))
        .and_then(|bytes| bytes.checked_sub(commitment))
        .unwrap();
    fill_run_bytes(fixture.workspace.run_directory(), target, "between-checks");
    let root = fixture.source.parent().unwrap();

    let error = seaf_loop::execute_approved_evaluation(&fixture.workspace, &fixture.source)
        .expect_err("capacity lost by check one must abort before check two");

    assert!(error.to_string().contains("commitment"), "{error}");
    assert!(root.join("eval-first.marker").exists());
    assert!(!root.join("eval-second.marker").exists());
    assert!(fixture
        .workspace
        .run_directory()
        .join("artifacts/07-testing.attempt-001.execution-intent.json")
        .exists());
    assert!(!fixture
        .workspace
        .run_directory()
        .join("artifacts/07-testing.attempt-001.json")
        .exists());
    assert!(!fixture
        .workspace
        .run_directory()
        .join("artifacts/07-testing.attempt-001.check-001.stdout.log")
        .exists());
    assert!(!fixture
        .workspace
        .run_directory()
        .join("artifacts/07-testing.attempt-001.check-001.stderr.log")
        .exists());
    fixture.cleanup();
}

#[test]
fn evaluation_command_latency_does_not_hold_the_run_mutation_lock() {
    let (fixture, approved) = approved_fixture_with_eval_mode(
        "evaluation-command-lock-release",
        FixtureEvalMode::Blocking,
    );
    let workspace = fixture.workspace.clone();
    let source = fixture.source.clone();
    let worker =
        std::thread::spawn(move || seaf_loop::execute_approved_evaluation(&workspace, &source));
    let root = fixture.source.parent().unwrap();
    let started = root.join("eval-blocking.started");
    let release = root.join("eval-blocking.release");
    for _ in 0..500 {
        if started.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !started.exists() {
        let release_result = fs::write(&release, b"release");
        let evaluation_result = worker.join();
        fixture.cleanup();
        release_result.expect("release a blocking command that missed its start marker");
        panic!("blocking evaluation command never started: {evaluation_result:?}");
    }

    let (sender, receiver) = std::sync::mpsc::channel();
    let lock_workspace = fixture.workspace.clone();
    let exact = approved.clone();
    let lock_worker = std::thread::spawn(move || {
        sender
            .send(seaf_loop::state::save_run(&lock_workspace, &exact))
            .unwrap();
    });
    let lock_result = receiver.recv_timeout(std::time::Duration::from_secs(2));
    let release_result = fs::write(&release, b"release");
    lock_worker.join().unwrap();
    let evaluation_result = worker.join().unwrap();
    release_result.expect("release blocking evaluation command");
    lock_result
        .expect("idempotent run-lock operation must finish during command latency")
        .expect("idempotent run save");
    let final_run = evaluation_result.expect("evaluation completes");
    assert_eq!(final_run.status, seaf_core::LoopStatus::EvalPassed);
    fixture.cleanup();
}

#[test]
fn evaluation_final_reopen_reclaims_orphan_run_temp_before_recovery_publication() {
    let (fixture, approved) = approved_fixture("evaluation-final-orphan-temp");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    let current = run_tree_usage(fixture.workspace.run_directory()).0;
    let active_complete_prefix = 3 * EVALUATION_EVIDENCE_CAP;
    fill_run_bytes(
        fixture.workspace.run_directory(),
        RUN_TREE_BYTE_CAP - current - active_complete_prefix,
        "final-orphan",
    );
    let stale = fixture
        .workspace
        .run_directory()
        .join(".run.json.run-state.tmp-999999-1");
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&stale)
        .unwrap();
    file.set_len(EVALUATION_EVIDENCE_CAP).unwrap();

    let adopted = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "recover final publication after dead run temp",
    )
    .expect("reopened final recovery must reclaim the prior owner's run temp");

    assert_eq!(adopted.run.status, seaf_core::LoopStatus::EvalPassed);
    assert!(!stale.exists());
    assert_eq!(
        seaf_loop::state::load_run(&fixture.workspace).unwrap(),
        adopted.run
    );
    fixture.cleanup();
}

#[test]
fn evaluation_invalidation_reopen_reclaims_orphan_run_temp_before_reset_cas() {
    let (fixture, approved) = approved_fixture("evaluation-invalidation-orphan-temp");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    for path in [
        "artifacts/07-testing.attempt-001.check-001.stdout.log",
        "artifacts/07-testing.attempt-001.check-001.stderr.log",
        "artifacts/07-testing.attempt-001.json",
        "artifacts/08-eval-report.attempt-001.json",
    ] {
        fs::remove_file(fixture.workspace.run_directory().join(path)).unwrap();
    }
    let current = run_tree_usage(fixture.workspace.run_directory()).0;
    let active_intent_prefix = 2 * 64 * 1024 + 5 * EVALUATION_EVIDENCE_CAP;
    fill_run_bytes(
        fixture.workspace.run_directory(),
        RUN_TREE_BYTE_CAP - current - active_intent_prefix,
        "invalidation-orphan",
    );
    let stale = fixture
        .workspace
        .run_directory()
        .join(".run.json.run-state.tmp-999999-2");
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&stale)
        .unwrap();
    file.set_len(EVALUATION_EVIDENCE_CAP).unwrap();

    let invalidated = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "recover invalidation after dead run temp",
    )
    .expect("reopened invalidation must reclaim the prior owner's run temp");

    assert_eq!(invalidated.run.status, seaf_core::LoopStatus::Approved);
    assert!(!stale.exists());
    assert_eq!(
        seaf_loop::state::load_run(&fixture.workspace).unwrap(),
        invalidated.run
    );
    fixture.cleanup();
}

#[test]
fn evaluation_invalidation_preserves_an_intent_only_prefix_and_resets_exact_approved_authority() {
    let (fixture, approved) = approved_fixture("evaluation-invalidate-intent-only");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    for path in [
        "artifacts/07-testing.attempt-001.check-001.stdout.log",
        "artifacts/07-testing.attempt-001.check-001.stderr.log",
        "artifacts/07-testing.attempt-001.json",
        "artifacts/08-eval-report.attempt-001.json",
    ] {
        fs::remove_file(fixture.workspace.run_directory().join(path)).unwrap();
    }
    let intent_path = "artifacts/07-testing.attempt-001.execution-intent.json";
    let intent_before = fs::read(fixture.workspace.run_directory().join(intent_path)).unwrap();
    let provider_before = approved.provider_exchange_records.clone();

    let outcome = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "invalidate interrupted evaluation",
    )
    .expect("intent-only evaluation prefix must be invalidatable");

    assert_eq!(outcome.run.status, seaf_core::LoopStatus::Approved);
    assert_eq!(outcome.run.current_step, LoopStepName::Testing);
    assert_eq!(outcome.run.provider_exchange_records, provider_before);
    assert_eq!(outcome.recovery.invalidated_attempt, 1);
    assert_eq!(outcome.recovery.next_evaluation_attempt, 2);
    assert_eq!(outcome.run.latest_recovery, Some(outcome.reference));
    assert_eq!(
        fs::read(fixture.workspace.run_directory().join(intent_path)).unwrap(),
        intent_before,
        "invalidation must preserve the interrupted attempt bytes"
    );
    fixture.cleanup();
}

#[test]
fn evaluation_invalidation_authorizes_exactly_one_fresh_indexed_attempt() {
    let (fixture, approved) =
        approved_fixture_with_eval_execution("evaluation-invalidation-rerun-once");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    for path in [
        "artifacts/07-testing.attempt-001.check-001.stdout.log",
        "artifacts/07-testing.attempt-001.check-001.stderr.log",
        "artifacts/07-testing.attempt-001.json",
        "artifacts/08-eval-report.attempt-001.json",
    ] {
        fs::remove_file(fixture.workspace.run_directory().join(path)).unwrap();
    }
    let invalidated = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "authorize one fresh evaluation",
    )
    .unwrap();

    let rerun = rerun_invalidated_evaluation(
        &fixture.workspace,
        &fixture.source,
        invalidated.reference.recovery_id,
    )
    .expect("exact invalidation must authorize its next indexed attempt");
    assert_eq!(rerun.status, seaf_core::LoopStatus::EvalPassed);
    let testing = rerun
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::Testing)
        .unwrap();
    assert_eq!(
        testing.artifact_path.as_deref(),
        Some("artifacts/07-testing.attempt-002.json")
    );
    let intent: serde_json::Value = serde_json::from_slice(
        &fs::read(
            fixture
                .workspace
                .run_directory()
                .join("artifacts/07-testing.attempt-002.execution-intent.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        intent["recovery"],
        serde_json::to_value(&invalidated.reference).unwrap()
    );

    let tree_after = directory_snapshot(fixture.workspace.run_directory());
    let retry = rerun_invalidated_evaluation(
        &fixture.workspace,
        &fixture.source,
        invalidated.reference.recovery_id,
    )
    .expect("exact post-final retry may return the already durable final authority");
    assert_eq!(retry, rerun);
    assert_eq!(
        directory_snapshot(fixture.workspace.run_directory()),
        tree_after
    );
    let candidate = rerun.candidate_workspace.as_ref().unwrap();
    let report_digest = rerun
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::EvalReport)
        .and_then(|step| step.artifact_digest.as_deref())
        .unwrap();
    let canonical_source = fixture.source.canonicalize().unwrap();
    let promoted = promote_evaluated_candidate(
        &fixture.workspace,
        &canonical_source,
        "operator@example.invalid",
        &candidate.candidate_diff_digest,
        report_digest,
        &git(&canonical_source, &["rev-parse", "HEAD"]),
    )
    .expect("passing V3 evaluation authority must remain promotable");
    assert_eq!(promoted.run.status, seaf_core::LoopStatus::Promoted);
    fixture.cleanup();
}

#[test]
fn evaluation_invalidation_resets_active_final_failure_to_its_exact_approved_predecessor() {
    let (fixture, approved) = approved_fixture("evaluation-invalidation-final-failed");
    let failed = publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, false);
    write_raw_run(&fixture.workspace, &failed);

    let outcome = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "retry the failed evaluation",
    )
    .expect("active approval-bound Failed evaluation must be invalidatable");

    assert_eq!(outcome.run.status, seaf_core::LoopStatus::Approved);
    assert_eq!(outcome.run.current_step, LoopStepName::Testing);
    assert!(outcome.run.eval_report_path.is_none());
    assert!(outcome
        .recovery
        .present_artifacts
        .iter()
        .any(|reference| { reference.path == "artifacts/08-eval-report.attempt-001.json" }));
    assert_eq!(outcome.recovery.next_evaluation_attempt, 2);
    fixture.cleanup();
}

#[test]
fn complete_recovered_attempt_before_final_cas_uses_zero_command_adoption() {
    let (fixture, approved) = approved_fixture_with_eval_execution("evaluation-recovered-adoption");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    for path in [
        "artifacts/07-testing.attempt-001.check-001.stdout.log",
        "artifacts/07-testing.attempt-001.check-001.stderr.log",
        "artifacts/07-testing.attempt-001.json",
        "artifacts/08-eval-report.attempt-001.json",
    ] {
        fs::remove_file(fixture.workspace.run_directory().join(path)).unwrap();
    }
    let invalidated = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "authorize recovered evaluation",
    )
    .unwrap();
    let completed = rerun_invalidated_evaluation(
        &fixture.workspace,
        &fixture.source,
        invalidated.reference.recovery_id,
    )
    .unwrap();
    assert_eq!(completed.status, seaf_core::LoopStatus::EvalPassed);
    write_raw_run(&fixture.workspace, &invalidated.run);
    let bytes_before = approved_eval_log_bytes(&fixture.workspace);

    let adopted = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "adopt complete recovered attempt",
    )
    .expect("complete attempt two must adopt through its consumed V3 predecessor");

    assert_eq!(adopted.recovery.evaluation_attempt, 2);
    assert_eq!(
        adopted.recovery.previous_recovery,
        Some(invalidated.reference)
    );
    assert_eq!(approved_eval_log_bytes(&fixture.workspace), bytes_before);
    load_verified_final_evaluation_authority(&fixture.workspace, &adopted.run).unwrap();
    fixture.cleanup();
}

#[test]
fn partial_recovered_attempt_requires_a_new_invalidation_before_attempt_three() {
    let (fixture, approved) =
        approved_fixture_with_eval_execution("evaluation-repeated-invalidation");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    for path in [
        "artifacts/07-testing.attempt-001.check-001.stdout.log",
        "artifacts/07-testing.attempt-001.check-001.stderr.log",
        "artifacts/07-testing.attempt-001.json",
        "artifacts/08-eval-report.attempt-001.json",
    ] {
        fs::remove_file(fixture.workspace.run_directory().join(path)).unwrap();
    }
    let first = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "authorize attempt two",
    )
    .unwrap();
    rerun_invalidated_evaluation(
        &fixture.workspace,
        &fixture.source,
        first.reference.recovery_id,
    )
    .unwrap();
    write_raw_run(&fixture.workspace, &first.run);
    for path in [
        "artifacts/07-testing.attempt-002.json",
        "artifacts/08-eval-report.attempt-002.json",
    ] {
        fs::remove_file(fixture.workspace.run_directory().join(path)).unwrap();
    }
    let partial_tree = directory_snapshot(fixture.workspace.run_directory());
    rerun_invalidated_evaluation(
        &fixture.workspace,
        &fixture.source,
        first.reference.recovery_id,
    )
    .expect_err("same recovery cannot replay after intent and log publication");
    assert_eq!(
        directory_snapshot(fixture.workspace.run_directory()),
        partial_tree
    );

    let second = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "attempt two was interrupted",
    )
    .expect("partial consumed attempt two requires a new invalidation");
    assert_eq!(second.recovery.invalidated_attempt, 2);
    assert_eq!(second.recovery.next_evaluation_attempt, 3);
    assert_eq!(second.recovery.previous_recovery, Some(first.reference));

    let final_run = rerun_invalidated_evaluation(
        &fixture.workspace,
        &fixture.source,
        second.reference.recovery_id,
    )
    .expect("second invalidation must authorize exactly attempt three");
    assert_eq!(
        final_run
            .steps
            .iter()
            .find(|step| step.name == LoopStepName::Testing)
            .unwrap()
            .artifact_path
            .as_deref(),
        Some("artifacts/07-testing.attempt-003.json")
    );
    load_verified_final_evaluation_authority(&fixture.workspace, &final_run).unwrap();
    fixture.cleanup();
}

#[test]
fn evaluation_invalidation_recovers_create_only_publication_cuts_and_exact_retry() {
    let (fixture, approved) = approved_fixture("evaluation-invalidation-crash-cuts");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    for path in [
        "artifacts/07-testing.attempt-001.check-001.stdout.log",
        "artifacts/07-testing.attempt-001.check-001.stderr.log",
        "artifacts/07-testing.attempt-001.json",
        "artifacts/08-eval-report.attempt-001.json",
    ] {
        fs::remove_file(fixture.workspace.run_directory().join(path)).unwrap();
    }
    let actor = "operator@example.invalid";
    let reason = "recover invalidation publication cuts";
    let first = invalidate_approved_evaluation(&fixture.workspace, actor, reason).unwrap();
    let source_path = first.recovery.source_run.path.clone();
    let recovery_path = first.reference.artifact.path.clone();
    let source_bytes = fs::read(fixture.workspace.run_directory().join(&source_path)).unwrap();
    let recovery_bytes = fs::read(fixture.workspace.run_directory().join(&recovery_path)).unwrap();

    write_raw_run(&fixture.workspace, &approved);
    fs::remove_file(fixture.workspace.run_directory().join(&source_path)).unwrap();
    fs::remove_file(fixture.workspace.run_directory().join(&recovery_path)).unwrap();
    write_private_run_fixture(&fixture.workspace, &source_path, &source_bytes);
    let after_source = invalidate_approved_evaluation(&fixture.workspace, actor, reason).unwrap();
    assert_eq!(after_source, first);

    write_raw_run(&fixture.workspace, &approved);
    let after_decision = invalidate_approved_evaluation(&fixture.workspace, actor, reason).unwrap();
    assert_eq!(after_decision, first);
    assert_eq!(
        fs::read(fixture.workspace.run_directory().join(&recovery_path)).unwrap(),
        recovery_bytes
    );

    let tree = directory_snapshot(fixture.workspace.run_directory());
    let after_cas = invalidate_approved_evaluation(&fixture.workspace, actor, reason).unwrap();
    assert_eq!(after_cas, first);
    assert_eq!(directory_snapshot(fixture.workspace.run_directory()), tree);
    fixture.cleanup();
}

#[test]
fn evaluation_rerun_rejects_authoritative_input_drift_before_attempt_publication() {
    for (label, path) in [
        ("ticket", "inputs/ticket.json"),
        ("provider-ticket", "ticket.snapshot.json"),
        ("policy", "inputs/policy.json"),
        ("config", "inputs/config.json"),
        ("repository", "inputs/repository.json"),
        ("eval", "inputs/eval-config.json"),
    ] {
        let (fixture, approved) =
            approved_fixture_with_eval_execution(&format!("evaluation-rerun-{label}-drift"));
        publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
        for path in [
            "artifacts/07-testing.attempt-001.check-001.stdout.log",
            "artifacts/07-testing.attempt-001.check-001.stderr.log",
            "artifacts/07-testing.attempt-001.json",
            "artifacts/08-eval-report.attempt-001.json",
        ] {
            fs::remove_file(fixture.workspace.run_directory().join(path)).unwrap();
        }
        let invalidated = invalidate_approved_evaluation(
            &fixture.workspace,
            "operator@example.invalid",
            "authorize drift-checked rerun",
        )
        .unwrap();
        write_private_run_fixture(&fixture.workspace, path, b"substituted");

        rerun_invalidated_evaluation(
            &fixture.workspace,
            &fixture.source,
            invalidated.reference.recovery_id,
        )
        .expect_err("every authoritative input drift must fail before attempt publication");
        assert!(!fixture
            .workspace
            .run_directory()
            .join("artifacts/07-testing.attempt-002.execution-intent.json")
            .exists());
        fixture.cleanup();
    }
}

#[test]
fn pre_spawn_authority_rejection_preserves_only_the_factual_partial_attempt() {
    let (fixture, approved) =
        approved_fixture_with_drifting_eval("evaluation-pre-spawn-authority-rejection");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    for path in [
        "artifacts/07-testing.attempt-001.check-001.stdout.log",
        "artifacts/07-testing.attempt-001.check-001.stderr.log",
        "artifacts/07-testing.attempt-001.json",
        "artifacts/08-eval-report.attempt-001.json",
    ] {
        fs::remove_file(fixture.workspace.run_directory().join(path)).unwrap();
    }
    let invalidated = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "authorize authority-checked rerun",
    )
    .unwrap();

    let error = rerun_invalidated_evaluation(
        &fixture.workspace,
        &fixture.source,
        invalidated.reference.recovery_id,
    )
    .expect_err("second check pre-spawn must abort after first check changes authority");
    assert!(
        error.to_string().contains("pre-spawn authority rejected"),
        "{error}"
    );
    let persisted = seaf_loop::state::load_run(&fixture.workspace).unwrap();
    assert_eq!(persisted, invalidated.run);
    for path in [
        "artifacts/07-testing.attempt-002.execution-intent.json",
        "artifacts/07-testing.attempt-002.check-001.stdout.log",
        "artifacts/07-testing.attempt-002.check-001.stderr.log",
    ] {
        assert!(
            fixture.workspace.run_directory().join(path).is_file(),
            "{path}"
        );
    }
    for path in [
        "artifacts/07-testing.attempt-002.check-002.stdout.log",
        "artifacts/07-testing.attempt-002.check-002.stderr.log",
        "artifacts/07-testing.attempt-002.json",
        "artifacts/08-eval-report.attempt-002.json",
    ] {
        assert!(
            !fixture.workspace.run_directory().join(path).exists(),
            "{path}"
        );
    }
    fixture.cleanup();
}

#[test]
fn evaluation_invalidation_keeps_eval_passed_and_promotion_intent_frozen() {
    let (complete_fixture, complete_approved) =
        approved_fixture("evaluation-invalidation-complete-adoption-only");
    publish_indexed_final_eval_artifacts(&complete_fixture.workspace, &complete_approved, true);
    let before = directory_snapshot(complete_fixture.workspace.run_directory());
    let error = invalidate_approved_evaluation(
        &complete_fixture.workspace,
        "operator@example.invalid",
        "must adopt complete evidence",
    )
    .expect_err("complete Testing evidence is adoption-only");
    assert!(error.to_string().contains("adoption"), "{error}");
    assert_eq!(
        directory_snapshot(complete_fixture.workspace.run_directory()),
        before
    );
    complete_fixture.cleanup();

    let (passed_fixture, passed_approved) =
        approved_fixture("evaluation-invalidation-frozen-passed");
    let passed =
        publish_indexed_final_eval_artifacts(&passed_fixture.workspace, &passed_approved, true);
    write_raw_run(&passed_fixture.workspace, &passed);
    let before = directory_snapshot(passed_fixture.workspace.run_directory());
    let error = invalidate_approved_evaluation(
        &passed_fixture.workspace,
        "operator@example.invalid",
        "must not invalidate passing authority",
    )
    .expect_err("EvalPassed must remain frozen");
    assert!(error.to_string().contains("immutable"), "{error}");
    assert_eq!(
        directory_snapshot(passed_fixture.workspace.run_directory()),
        before
    );
    passed_fixture.cleanup();

    let (intent_fixture, intent_approved) =
        approved_fixture("evaluation-invalidation-promotion-intent");
    publish_indexed_final_eval_artifacts(&intent_fixture.workspace, &intent_approved, true);
    for path in [
        "artifacts/07-testing.attempt-001.check-001.stdout.log",
        "artifacts/07-testing.attempt-001.check-001.stderr.log",
        "artifacts/07-testing.attempt-001.json",
        "artifacts/08-eval-report.attempt-001.json",
    ] {
        fs::remove_file(intent_fixture.workspace.run_directory().join(path)).unwrap();
    }
    write_private_run_fixture(
        &intent_fixture.workspace,
        "artifacts/09-promotion.intent.json",
        b"{}",
    );
    let before = directory_snapshot(intent_fixture.workspace.run_directory());
    let error = invalidate_approved_evaluation(
        &intent_fixture.workspace,
        "operator@example.invalid",
        "must not cross promotion intent",
    )
    .expect_err("promotion intent freezes evaluation invalidation");
    assert!(error.to_string().contains("promotion intent"), "{error}");
    assert_eq!(
        directory_snapshot(intent_fixture.workspace.run_directory()),
        before
    );
    intent_fixture.cleanup();
}

#[test]
fn failed_invalidation_source_remains_verifiable_after_the_fresh_attempt_exists() {
    let (fixture, approved) =
        approved_fixture_with_eval_execution("evaluation-failed-source-history");
    let failed = publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, false);
    write_raw_run(&fixture.workspace, &failed);
    let invalidated = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "retry exact failed authority",
    )
    .unwrap();

    let rerun = rerun_invalidated_evaluation(
        &fixture.workspace,
        &fixture.source,
        invalidated.reference.recovery_id,
    )
    .expect("later attempt artifacts must not invalidate historical Failed source");
    assert_eq!(rerun.status, seaf_core::LoopStatus::EvalPassed);
    load_verified_final_evaluation_authority(&fixture.workspace, &rerun).unwrap();
    fixture.cleanup();
}

#[test]
fn adopted_failed_v2_can_be_invalidated_and_remains_verified_after_attempt_two() {
    let (fixture, approved) =
        approved_fixture_with_eval_execution("evaluation-adopted-failed-v2-to-v3");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, false);
    let adopted = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "adopt failed attempt one",
    )
    .unwrap();
    assert_eq!(adopted.run.status, seaf_core::LoopStatus::Failed);
    let invalidated = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "invalidate adopted failure",
    )
    .expect("exact adopted Failed V2 must be eligible for V3 invalidation");
    assert_eq!(
        invalidated.recovery.previous_recovery,
        Some(adopted.reference)
    );

    let rerun = rerun_invalidated_evaluation(
        &fixture.workspace,
        &fixture.source,
        invalidated.reference.recovery_id,
    )
    .expect("V2-to-V3 lineage must remain verifiable after attempt two exists");
    load_verified_final_evaluation_authority(&fixture.workspace, &rerun).unwrap();
    fixture.cleanup();
}

#[test]
fn invalidation_v3_rejects_recomputed_prior_final_reference_substitution() {
    let (fixture, approved) = approved_fixture("evaluation-v3-prior-final-tamper");
    let failed = publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, false);
    write_raw_run(&fixture.workspace, &failed);
    let outcome = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "publish tamper target",
    )
    .unwrap();
    let mut source: EvaluationInvalidationSourceRunV3 = serde_json::from_slice(
        &fs::read(
            fixture
                .workspace
                .run_directory()
                .join(&outcome.recovery.source_run.path),
        )
        .unwrap(),
    )
    .unwrap();
    source.prior_final.as_mut().unwrap().testing_evidence =
        source.evaluation_prefix.present_artifacts[0].clone();
    let source_bytes = canonical_json_bytes(&source).unwrap();
    write_private_run_fixture(
        &fixture.workspace,
        &outcome.recovery.source_run.path,
        &source_bytes,
    );
    let mut recovery: EvaluationInvalidationAttemptV3 = outcome.recovery.clone();
    recovery.source_run.digest = format!("{:x}", Sha256::digest(&source_bytes));
    let recovery_bytes = canonical_json_bytes(&recovery).unwrap();
    write_private_run_fixture(
        &fixture.workspace,
        &outcome.reference.artifact.path,
        &recovery_bytes,
    );
    let mut tampered_run = outcome.run;
    tampered_run
        .latest_recovery
        .as_mut()
        .unwrap()
        .artifact
        .digest = format!("{:x}", Sha256::digest(&recovery_bytes));
    write_raw_run(&fixture.workspace, &tampered_run);

    let error = load_verified_recovery_authority_kind(
        &fixture.workspace,
        tampered_run.latest_recovery.as_ref().unwrap(),
    )
    .expect_err("recomputed V3 bytes cannot substitute prior_final Testing authority");
    assert!(error.to_string().contains("final references"), "{error}");
    fixture.cleanup();
}

#[test]
fn competing_evaluation_invalidators_choose_one_audited_winner() {
    let (fixture, approved) = approved_fixture("evaluation-v3-competing-invalidators");
    publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    for path in [
        "artifacts/07-testing.attempt-001.check-001.stdout.log",
        "artifacts/07-testing.attempt-001.check-001.stderr.log",
        "artifacts/07-testing.attempt-001.json",
        "artifacts/08-eval-report.attempt-001.json",
    ] {
        fs::remove_file(fixture.workspace.run_directory().join(path)).unwrap();
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for reason in ["competing invalidation A", "competing invalidation B"] {
        let workspace = fixture.workspace.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            invalidate_approved_evaluation(&workspace, "operator@example.invalid", reason)
        }));
    }
    barrier.wait();
    let outcomes = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_err()).count(),
        1
    );
    fixture.cleanup();
}

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
    let source_before_review = source_evidence(&fixture.source);
    let candidate_before_review = source_evidence(&fixture.candidate);
    assert!(loop_runner.run_next_step().expect("OutputReview"));
    assert_eq!(
        serde_json::to_value(loop_runner.run().status).unwrap(),
        serde_json::json!("awaiting_human_review"),
        "an approved isolated OutputReview must atomically stop before Testing"
    );
    assert_eq!(loop_runner.run().current_step, LoopStepName::Testing);
    assert!(
        seaf_core::validate_loop_run(loop_runner.run()).is_empty(),
        "the barrier must satisfy the public runtime contract"
    );
    let persisted_waiting = seaf_loop::state::load_run(&workspace).unwrap();
    assert_eq!(persisted_waiting, *loop_runner.run());

    let mut malformed = persisted_waiting.clone();
    malformed.execution_mode = seaf_core::LoopExecutionMode::LegacyProposalOnly;
    assert!(seaf_core::validate_loop_run(&malformed)
        .iter()
        .any(|error| error.field == "execution_mode"));
    let mut malformed = persisted_waiting.clone();
    malformed.current_step = LoopStepName::OutputReview;
    assert!(seaf_core::validate_loop_run(&malformed)
        .iter()
        .any(|error| error.field == "current_step"));
    let mut malformed = persisted_waiting.clone();
    malformed
        .steps
        .iter_mut()
        .find(|record| record.name == LoopStepName::Testing)
        .unwrap()
        .status = seaf_core::LoopStepStatus::Running;
    assert!(seaf_core::validate_loop_run(&malformed)
        .iter()
        .any(|error| error.field.ends_with(".status")));
    let mut malformed = persisted_waiting.clone();
    malformed.eval_report_path = Some("artifacts/08-eval-report.json".to_string());
    assert!(seaf_core::validate_loop_run(&malformed)
        .iter()
        .any(|error| error.field == "eval_report_path"));
    let mut malformed = persisted_waiting.clone();
    let mut duplicate = malformed
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::Testing)
        .unwrap()
        .clone();
    duplicate.status = seaf_core::LoopStepStatus::Completed;
    duplicate.artifact_path = Some("artifacts/duplicate-testing.md".to_string());
    duplicate.artifact_digest = Some("7".repeat(64));
    malformed.steps.push(duplicate);
    assert!(seaf_core::validate_loop_run(&malformed)
        .iter()
        .any(|error| error.field == "steps"));
    let mut malformed = persisted_waiting.clone();
    let output = malformed
        .steps
        .iter_mut()
        .find(|record| record.name == LoopStepName::OutputReview)
        .unwrap();
    output.status = seaf_core::LoopStepStatus::Blocked;
    output.artifact_path = None;
    output.artifact_digest = None;
    let malformed_fields = seaf_core::validate_loop_run(&malformed)
        .into_iter()
        .map(|error| error.field)
        .collect::<Vec<_>>();
    assert!(malformed_fields
        .iter()
        .any(|field| field.ends_with(".status")));
    assert!(malformed_fields
        .iter()
        .any(|field| field.ends_with(".artifact_path")));
    let mut malformed = persisted_waiting.clone();
    malformed.provider_exchange_records.clear();
    assert!(seaf_core::validate_loop_run(&malformed)
        .iter()
        .any(|error| error.field == "provider_exchange_records"));
    let mut malformed = persisted_waiting.clone();
    malformed.provider_exchange_records.pop();
    assert!(seaf_core::validate_loop_run(&malformed)
        .iter()
        .any(|error| {
            error.field == "provider_exchange_records"
                && error.message.contains("end in an OutputReview")
        }));
    let mut malformed = persisted_waiting.clone();
    let last_attempt = malformed
        .provider_exchange_records
        .last()
        .unwrap()
        .step_attempt;
    malformed.provider_exchange_records.retain(|reference| {
        !(reference.step == LoopStepName::OutputReview
            && reference.step_attempt == last_attempt
            && reference.kind == ProviderExchangeKind::Initial
            && reference.phase == ProviderExchangePhase::Request)
    });
    assert!(seaf_core::validate_loop_run(&malformed)
        .iter()
        .any(|error| {
            error.field == "provider_exchange_records" && error.message.contains("Initial request")
        }));
    assert!(!loop_runner
        .run_next_step()
        .expect("human-review state is terminal for ordinary execution"));
    for absent in [
        "prompts/07-testing.prompt.md",
        "responses/07-testing.raw.txt",
        "artifacts/07-testing.md",
        "prompts/08-eval-report.prompt.md",
        "responses/08-eval-report.raw.txt",
        "artifacts/08-eval-report.md",
    ] {
        assert!(
            !workspace.run_directory().join(absent).exists(),
            "{absent} must not exist before human approval"
        );
    }
    let log = fs::read_to_string(workspace.run_directory().join("log.md")).unwrap();
    assert!(!log.contains("started step Testing"));
    assert!(!log.contains("started step EvalReport"));
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
    assert_eq!(source_evidence(&fixture.source), source_before_review);
    assert_eq!(source_evidence(&fixture.candidate), candidate_before_review);
    remove_candidate(&fixture.source, &fixture.candidate);
}

#[test]
fn human_approval_binds_the_exact_reviewed_candidate_without_running_tests() {
    let fixture = fixture("exact-human-approval");
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
    let mut runner =
        LoopRunner::start_initialized(fixture.prepared, &mut step_runner).expect("start");
    for _ in 0..6 {
        assert!(runner.run_next_step().expect("through OutputReview"));
    }
    drop(runner);
    drop(step_runner);
    let workspace = LoopWorkspace::open(&fixture.runs_root, "exact-human-approval").unwrap();
    let waiting = seaf_loop::state::load_run(&workspace).unwrap();
    let candidate = waiting.candidate_workspace.as_ref().unwrap();

    let approved = approve_candidate_for_testing(
        &workspace,
        &fixture.source,
        "reviewer@example.invalid",
        &candidate.candidate_diff_digest,
        &candidate.starting_head,
    )
    .expect("exact approval");

    assert_eq!(approved.run.status, seaf_core::LoopStatus::Approved);
    assert_eq!(approved.run.current_step, LoopStepName::Testing);
    assert_eq!(
        approved
            .run
            .steps
            .iter()
            .find(|step| step.name == LoopStepName::Testing)
            .unwrap()
            .status,
        seaf_core::LoopStepStatus::Pending
    );
    assert!(approved.run.eval_report_path.is_none());
    assert_eq!(approved.evidence.schema_version, 1);
    assert_eq!(approved.evidence.run_id, approved.run.run_id);
    assert_eq!(approved.evidence.reviewer, "reviewer@example.invalid");
    assert!(!approved.evidence.approved_at.is_empty());
    assert_eq!(
        approved.evidence.candidate_diff.digest,
        candidate.candidate_diff_digest
    );
    assert_eq!(approved.evidence.starting_head, candidate.starting_head);
    let authoritative_policy = approved
        .run
        .policy_decisions
        .iter()
        .find(|decision| {
            decision.get("patch_id").and_then(serde_json::Value::as_str)
                == Some(approved.run.run_id.as_str())
        })
        .unwrap();
    assert_eq!(
        approved.evidence.policy_decision_digest,
        canonical_sha256_digest(authoritative_policy).unwrap()
    );
    let review = approved
        .run
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::OutputReview)
        .unwrap();
    assert_eq!(
        approved.evidence.output_review.path,
        review.artifact_path.clone().unwrap()
    );
    assert_eq!(
        approved.evidence.output_review.digest,
        review.artifact_digest.clone().unwrap()
    );
    assert!(approved
        .run
        .provider_exchange_records
        .contains(&approved.evidence.output_review_request));
    assert_eq!(
        approved.run.provider_exchange_records.last(),
        Some(&approved.evidence.output_review_response)
    );
    assert_eq!(
        approved.evidence.output_review_request.step_attempt,
        approved.evidence.output_review_response.step_attempt
    );
    let approved_snapshot = snapshot_files(workspace.run_directory());
    let append_error = persist_provider_exchange_record_reference(
        &workspace,
        approved.evidence.output_review_response.clone(),
    )
    .expect_err("Approved must freeze provider append");
    assert!(
        append_error.to_string().contains("frozen"),
        "{append_error}"
    );
    let cleanup_error = cleanup_candidate_workspace_outcome(&workspace, &fixture.source)
        .expect_err("Approved candidate remains active and non-cleanable");
    assert!(
        cleanup_error.to_string().contains("active run"),
        "{cleanup_error}"
    );
    let mut false_completed = approved.run.clone();
    false_completed.status = seaf_core::LoopStatus::Completed;
    false_completed.human_approval = None;
    let writer_error = seaf_loop::state::save_run(&workspace, &false_completed)
        .expect_err("public state writer cannot replace Approved");
    assert!(
        writer_error.to_string().contains("approved authority"),
        "{writer_error}"
    );
    let mut inert = UnauthenticatedOutputReview;
    let mut resumed = LoopRunner::resume(&fixture.runs_root, "exact-human-approval", &mut inert)
        .expect("Approved resumes as inert authority");
    assert!(!resumed
        .run_next_step()
        .expect("Approved has no runnable step"));
    assert_eq!(snapshot_files(workspace.run_directory()), approved_snapshot);
    remove_candidate(&fixture.source, &fixture.candidate);
}

#[test]
fn human_approval_rejects_stale_or_substituted_authority_without_further_mutation() {
    for mutation in [
        ApprovalMutation::DuplicatePolicy,
        ApprovalMutation::UnrelatedPolicy,
        ApprovalMutation::OutputReviewArtifact,
        ApprovalMutation::InitialProviderReference,
        ApprovalMutation::ProviderReference,
        ApprovalMutation::LaterReviewAttempt,
        ApprovalMutation::MovedSourceHead,
        ApprovalMutation::ChangedCandidate,
        ApprovalMutation::NonAwaitingStatus,
    ] {
        let run_id = format!("approval-{mutation:?}").to_ascii_lowercase();
        let fixture = awaiting_approval_fixture(&run_id);
        let mut waiting = seaf_loop::state::load_run(&fixture.workspace).unwrap();
        let candidate = waiting.candidate_workspace.as_ref().unwrap().clone();
        match mutation {
            ApprovalMutation::DuplicatePolicy => {
                waiting
                    .policy_decisions
                    .push(waiting.policy_decisions[0].clone());
                write_raw_run(&fixture.workspace, &waiting);
            }
            ApprovalMutation::UnrelatedPolicy => {
                waiting.policy_decisions[0]
                    .insert("patch_id".to_string(), serde_json::json!("another-run"));
                write_raw_run(&fixture.workspace, &waiting);
            }
            ApprovalMutation::OutputReviewArtifact => {
                let review = waiting
                    .steps
                    .iter_mut()
                    .find(|step| step.name == LoopStepName::OutputReview)
                    .unwrap();
                review.artifact_digest = Some("f".repeat(64));
                write_raw_run(&fixture.workspace, &waiting);
            }
            ApprovalMutation::ProviderReference => {
                waiting.provider_exchange_records.last_mut().unwrap().digest = "f".repeat(64);
                write_raw_run(&fixture.workspace, &waiting);
            }
            ApprovalMutation::InitialProviderReference => {
                waiting
                    .provider_exchange_records
                    .iter_mut()
                    .find(|reference| {
                        reference.step == LoopStepName::OutputReview
                            && reference.kind == ProviderExchangeKind::Initial
                            && reference.phase == ProviderExchangePhase::Request
                    })
                    .unwrap()
                    .digest = "f".repeat(64);
                write_raw_run(&fixture.workspace, &waiting);
            }
            ApprovalMutation::LaterReviewAttempt => {
                write_step_request(
                    &fixture.workspace,
                    LoopStepName::OutputReview,
                    2,
                    "unapproved later review attempt",
                )
                .unwrap();
            }
            ApprovalMutation::MovedSourceHead => {
                fs::write(fixture.source.join("moved.txt"), "moved\n").unwrap();
                git_ok(&fixture.source, &["add", "moved.txt"]);
                git_ok(&fixture.source, &["commit", "-qm", "move source"]);
            }
            ApprovalMutation::ChangedCandidate => {
                fs::write(fixture.candidate.join("src/new.rs"), "substituted\n").unwrap();
            }
            ApprovalMutation::NonAwaitingStatus => {
                waiting.status = seaf_core::LoopStatus::Blocked;
                write_raw_run(&fixture.workspace, &waiting);
            }
        }
        let run_before = snapshot_files(fixture.workspace.run_directory());
        let source_before = source_evidence(&fixture.source);
        let candidate_before = source_evidence(&fixture.candidate);

        let error = approve_candidate_for_testing(
            &fixture.workspace,
            &fixture.source,
            "reviewer@example.invalid",
            &candidate.candidate_diff_digest,
            &candidate.starting_head,
        )
        .expect_err("stale or substituted approval authority must fail closed");

        assert!(!error.to_string().is_empty(), "{mutation:?}");
        assert_eq!(
            snapshot_files(fixture.workspace.run_directory()),
            run_before
        );
        assert_eq!(source_evidence(&fixture.source), source_before);
        assert_eq!(source_evidence(&fixture.candidate), candidate_before);
        fixture.cleanup();
    }
}

#[test]
fn eval_passed_authority_is_inert_frozen_and_non_cleanable() {
    let fixture = final_eval_fixture("eval-passed-frozen", true);
    let run = seaf_loop::state::load_run(&fixture.workspace).unwrap();
    let before = snapshot_files(fixture.workspace.run_directory());

    let mut replacement = run.clone();
    replacement.status = seaf_core::LoopStatus::Completed;
    replacement.human_approval = None;
    let writer_error = seaf_loop::state::save_run(&fixture.workspace, &replacement)
        .expect_err("public writer must not replace EvalPassed");
    assert!(writer_error.to_string().contains("final evaluation"));

    let append_error = persist_provider_exchange_record_reference(
        &fixture.workspace,
        run.provider_exchange_records.last().unwrap().clone(),
    )
    .expect_err("provider append must remain frozen");
    assert!(append_error.to_string().contains("frozen"));

    let cleanup_error = cleanup_candidate_workspace_outcome(&fixture.workspace, &fixture.source)
        .expect_err("EvalPassed must remain non-cleanable until promotion");
    assert!(cleanup_error.to_string().contains("active run"));

    let mut inert = UnauthenticatedOutputReview;
    let runs_root = fixture.workspace.run_directory().parent().unwrap();
    let mut resumed = LoopRunner::resume(runs_root, "eval-passed-frozen", &mut inert)
        .expect("EvalPassed resumes inertly");
    assert!(!resumed.run_next_step().unwrap());
    assert_eq!(snapshot_files(fixture.workspace.run_directory()), before);
    fixture.cleanup();
}

#[test]
fn approval_bound_reported_failure_freezes_execution_but_allows_terminal_cleanup() {
    let fixture = final_eval_fixture("eval-failed-frozen", false);
    let run = seaf_loop::state::load_run(&fixture.workspace).unwrap();
    let original_authority =
        load_verified_final_evaluation_authority(&fixture.workspace, &run).unwrap();

    let append_error = persist_provider_exchange_record_reference(
        &fixture.workspace,
        run.provider_exchange_records.last().unwrap().clone(),
    )
    .expect_err("reported failure must freeze provider append");
    assert!(append_error.to_string().contains("frozen"));

    let outcome = cleanup_candidate_workspace_outcome(&fixture.workspace, &fixture.source)
        .expect("reported evaluation failure remains cleanable");
    assert_eq!(outcome.status, seaf_core::LoopStatus::Failed);
    assert_eq!(
        outcome.candidate.lifecycle,
        seaf_core::CandidateWorkspaceLifecycle::Cleaned
    );
    let cleaned = seaf_loop::state::load_run(&fixture.workspace).unwrap();
    assert!(cleaned.human_approval.is_some());
    assert_eq!(cleaned.status, seaf_core::LoopStatus::Failed);
    let cleaned_authority = load_verified_final_evaluation_authority(&fixture.workspace, &cleaned)
        .expect("cleaned reported failure retains verifiable final authority");
    assert_eq!(
        cleaned_authority.testing_evidence(),
        original_authority.testing_evidence()
    );
    assert_eq!(
        cleaned_authority.eval_report(),
        original_authority.eval_report()
    );
}

#[test]
fn public_writers_cannot_mint_final_evaluation_authority_from_approved() {
    for passed in [true, false] {
        let run_id = if passed {
            "public-mint-eval-passed"
        } else {
            "public-mint-eval-failed"
        };
        let (fixture, approved) = approved_fixture(run_id);
        let final_run = publish_final_eval_artifacts(&fixture.workspace, &approved, passed);
        load_verified_final_evaluation_authority(&fixture.workspace, &final_run)
            .expect("otherwise valid final authority fixture");
        let before = fs::read(fixture.workspace.run_file()).unwrap();

        let save_error = seaf_loop::state::save_run(&fixture.workspace, &final_run)
            .expect_err("public save cannot mint final evaluation authority");
        let write_error =
            seaf_loop::state::write_run_file(&fixture.workspace.run_file(), &final_run)
                .expect_err("public writer cannot mint final evaluation authority");

        assert!(save_error.to_string().contains("final evaluation"));
        assert!(write_error.to_string().contains("final evaluation"));
        assert_eq!(fs::read(fixture.workspace.run_file()).unwrap(), before);
        fixture.cleanup();
    }
}

#[test]
fn evaluation_recovery_v2_reconstructs_exact_approved_for_pass_and_failure() {
    for passed in [true, false] {
        let run_id = if passed {
            "eval-recovery-v2-pass"
        } else {
            "eval-recovery-v2-fail"
        };
        let (fixture, approved) = approved_fixture(run_id);
        let direct = publish_final_eval_artifacts(&fixture.workspace, &approved, passed);
        let recovered = publish_evaluation_recovery_v2(&fixture.workspace, &approved, direct);

        let authority = load_verified_final_evaluation_authority(&fixture.workspace, &recovered)
            .expect("evaluation-v2 final must reconstruct its exact Approved source");

        assert_eq!(authority.approved_run(), &approved);
        fixture.cleanup();
    }
}

#[test]
fn evaluation_adoption_finalizes_complete_prefix_without_command_execution_and_retries_inertly() {
    for (indexed, report_present, passed) in [
        (false, true, true),
        (false, true, false),
        (false, false, true),
        (false, false, false),
        (true, true, true),
        (true, true, false),
        (true, false, true),
        (true, false, false),
    ] {
        let run_id = format!(
            "evaluation-adoption-{}-{}-{}",
            if indexed { "indexed" } else { "fixed" },
            if report_present {
                "existing"
            } else {
                "missing"
            },
            if passed { "pass" } else { "fail" },
        );
        let (fixture, approved) = approved_fixture(&run_id);
        let final_shape = if indexed {
            publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, passed)
        } else {
            publish_final_eval_artifacts(&fixture.workspace, &approved, passed)
        };
        let report_path = final_shape.eval_report_path.unwrap();
        if !report_present {
            fs::remove_file(fixture.workspace.run_directory().join(&report_path)).unwrap();
        }
        let run_before = fs::read(fixture.workspace.run_file()).unwrap();
        let provider_before = approved
            .provider_exchange_records
            .iter()
            .map(|reference| {
                fs::read(fixture.workspace.run_directory().join(&reference.path)).unwrap()
            })
            .collect::<Vec<_>>();
        let logs_before = approved_eval_log_bytes(&fixture.workspace);
        let candidate_before = source_evidence(&fixture.candidate);

        let adopted = adopt_approved_evaluation(
            &fixture.workspace,
            "operator@example.invalid",
            "adopt complete interrupted evaluation",
        )
        .expect("a complete exact evaluation prefix must be adoptable");

        assert_eq!(
            adopted.run.status == seaf_core::LoopStatus::EvalPassed,
            passed
        );
        assert_eq!(adopted.run.updated_at, adopted.recovery.created_at);
        assert_eq!(adopted.run.latest_recovery, Some(adopted.reference.clone()));
        assert_eq!(
            adopted.recovery.report_disposition,
            if report_present {
                EvaluationRecoveryReportDisposition::VerifyExisting
            } else {
                EvaluationRecoveryReportDisposition::CreateMissing
            }
        );
        assert_eq!(
            fs::read(fixture.workspace.run_directory().join(&report_path)).unwrap(),
            canonical_json_bytes(
                load_verified_final_evaluation_authority(&fixture.workspace, &adopted.run)
                    .unwrap()
                    .eval_report()
            )
            .unwrap()
        );
        load_verified_final_evaluation_authority(&fixture.workspace, &adopted.run)
            .expect("adopted final authority must verify");
        assert_eq!(
            approved
                .provider_exchange_records
                .iter()
                .map(|reference| {
                    fs::read(fixture.workspace.run_directory().join(&reference.path)).unwrap()
                })
                .collect::<Vec<_>>(),
            provider_before,
            "adoption must not call the provider or change its ledger"
        );
        assert_eq!(approved_eval_log_bytes(&fixture.workspace), logs_before);
        assert_eq!(source_evidence(&fixture.candidate), candidate_before);
        assert_ne!(fs::read(fixture.workspace.run_file()).unwrap(), run_before);
        let tree_after = directory_snapshot(fixture.workspace.run_directory());

        let retry = adopt_approved_evaluation(
            &fixture.workspace,
            "operator@example.invalid",
            "adopt complete interrupted evaluation",
        )
        .expect("an exact post-CAS retry must be inert");
        assert_eq!(retry, adopted);
        assert_eq!(
            directory_snapshot(fixture.workspace.run_directory()),
            tree_after
        );
        for (actor, reason) in [
            (
                "another@example.invalid",
                "adopt complete interrupted evaluation",
            ),
            ("operator@example.invalid", "different adoption reason"),
        ] {
            let rejected = adopt_approved_evaluation(&fixture.workspace, actor, reason)
                .expect_err("retry audit substitution must fail");
            assert!(rejected.to_string().contains("retry"), "{rejected}");
            assert_eq!(
                directory_snapshot(fixture.workspace.run_directory()),
                tree_after
            );
        }
        fixture.cleanup();
    }
}

#[test]
fn evaluation_adoption_rejects_report_credentials_before_any_recovery_publication() {
    for (intent_schema, report_present, secret) in [
        (1, false, "eval_report_id"),
        (1, true, "prefix[REDACTED]suffix"),
        (2, false, "\"passed\": true"),
        (2, true, "eval_report_id"),
        (3, false, "eval_report_id"),
        (3, true, "\"passed\": true"),
        (3, true, "prefix[REDACTED]suffix"),
    ] {
        let run_id = format!(
            "adoption-report-secret-v{intent_schema}-{}-{}",
            if report_present {
                "existing"
            } else {
                "missing"
            },
            if secret == "eval_report_id" {
                "key"
            } else if secret.contains("passed") {
                "boundary"
            } else {
                "marker"
            }
        );
        let (fixture, approved) = approved_fixture_with_eval_mode(
            &run_id,
            FixtureEvalMode::EvaluationReportEnvelopeSecret(secret),
        );
        let final_shape = match intent_schema {
            1 => publish_final_eval_artifacts(&fixture.workspace, &approved, true),
            2 => publish_indexed_final_eval_artifacts_v2(&fixture.workspace, &approved, true),
            3 => publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true),
            _ => unreachable!(),
        };
        let report_path = final_shape.eval_report_path.unwrap();
        if report_present && secret == "prefix[REDACTED]suffix" {
            let path = fixture.workspace.run_directory().join(&report_path);
            let mut report: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            report["summary"] = serde_json::Value::String(secret.to_string());
            write_private_run_fixture(
                &fixture.workspace,
                &report_path,
                canonical_json_bytes(&report).unwrap(),
            );
        } else if !report_present {
            fs::remove_file(fixture.workspace.run_directory().join(&report_path)).unwrap();
        }
        let before = directory_snapshot(fixture.workspace.run_directory());

        let error = adopt_approved_evaluation(
            &fixture.workspace,
            "operator@example.invalid",
            "reject report credential material",
        )
        .expect_err("report credential material must reject before publication");

        // The exact-prefix preflight now scans every schema's raw intent, log, and optional
        // report bytes before any typed legacy parsing or recovery publication.
        let expected_error =
            "audited recovery failed: operator evidence contains prohibited credential material";
        assert_eq!(
            error.to_string(),
            expected_error,
            "schema v{intent_schema}, report_present={report_present}, secret={secret}"
        );
        assert!(!error.to_string().contains(secret));
        assert_eq!(
            directory_snapshot(fixture.workspace.run_directory()),
            before,
            "schema v{intent_schema}, report_present={report_present}, secret={secret}"
        );
        assert!(!fixture
            .workspace
            .run_directory()
            .join("artifacts/recovery-001.source-run.json")
            .exists());
        assert!(!fixture
            .workspace
            .run_directory()
            .join("artifacts/recovery-001.json")
            .exists());
        fixture.cleanup();
    }
}

#[test]
fn evaluation_recovery_rejects_secret_bearing_prefix_before_any_publication() {
    let (fixture, approved) = approved_fixture_with_eval_mode(
        "adoption-secret-bearing-prefix",
        FixtureEvalMode::ProviderSecret,
    );
    let final_shape = publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    let report_path = final_shape.eval_report_path.unwrap();
    fs::remove_file(fixture.workspace.run_directory().join(&report_path)).unwrap();
    let stdout_path = "artifacts/07-testing.attempt-001.check-001.stdout.log";
    write_private_run_fixture(
        &fixture.workspace,
        stdout_path,
        PROVIDER_SECRET_FIXTURE.as_bytes(),
    );
    let testing_path = "artifacts/07-testing.attempt-001.json";
    let mut testing: TestingEvidence = serde_json::from_slice(
        &fs::read(fixture.workspace.run_directory().join(testing_path)).unwrap(),
    )
    .unwrap();
    testing.checks[0].stdout_digest = Some(hex::encode(Sha256::digest(
        PROVIDER_SECRET_FIXTURE.as_bytes(),
    )));
    write_private_run_fixture(
        &fixture.workspace,
        testing_path,
        testing.canonical_bytes().unwrap(),
    );
    let before = directory_snapshot(fixture.workspace.run_directory());
    let raw_digest = hex::encode(Sha256::digest(PROVIDER_SECRET_FIXTURE.as_bytes()));

    let error = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "reject secret-bearing prefix",
    )
    .expect_err("a secret-bearing log must reject adoption before publication");

    assert!(
        error.to_string().contains("prohibited credential material"),
        "{error}"
    );
    assert!(!error.to_string().contains(PROVIDER_SECRET_FIXTURE));
    assert!(!error.to_string().contains(&raw_digest));
    assert_eq!(
        directory_snapshot(fixture.workspace.run_directory()),
        before
    );
    assert!(!fixture
        .workspace
        .run_directory()
        .join("artifacts/recovery-001.source-run.json")
        .exists());
    assert!(!fixture
        .workspace
        .run_directory()
        .join("artifacts/recovery-001.json")
        .exists());
    assert!(!fixture.workspace.run_directory().join(report_path).exists());
    fixture.cleanup();

    let (fixture, approved) = approved_fixture_with_eval_mode(
        "invalidation-secret-bearing-prefix",
        FixtureEvalMode::ProviderSecret,
    );
    let final_shape = publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    fs::remove_file(
        fixture
            .workspace
            .run_directory()
            .join("artifacts/07-testing.attempt-001.json"),
    )
    .unwrap();
    fs::remove_file(
        fixture
            .workspace
            .run_directory()
            .join(final_shape.eval_report_path.unwrap()),
    )
    .unwrap();
    write_private_run_fixture(
        &fixture.workspace,
        stdout_path,
        PROVIDER_SECRET_FIXTURE.as_bytes(),
    );
    let before = directory_snapshot(fixture.workspace.run_directory());

    let error = invalidate_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "reject secret-bearing prefix",
    )
    .expect_err("a secret-bearing log must reject invalidation before publication");

    assert!(
        error.to_string().contains("prohibited credential material"),
        "{error}"
    );
    assert!(!error.to_string().contains(PROVIDER_SECRET_FIXTURE));
    assert!(!error.to_string().contains(&raw_digest));
    assert_eq!(
        directory_snapshot(fixture.workspace.run_directory()),
        before
    );
    assert!(!fixture
        .workspace
        .run_directory()
        .join("artifacts/recovery-001.source-run.json")
        .exists());
    assert!(!fixture
        .workspace
        .run_directory()
        .join("artifacts/recovery-001.json")
        .exists());
    fixture.cleanup();
}

#[test]
fn evaluation_adoption_rejects_input_and_report_drift_before_recovery_publication() {
    for target in ["input", "report", "attempt2"] {
        let (fixture, approved) = approved_fixture(&format!("adoption-preflight-{target}"));
        let final_shape = publish_final_eval_artifacts(&fixture.workspace, &approved, true);
        let path = match target {
            "input" => "inputs/ticket.json".to_string(),
            "report" => final_shape.eval_report_path.unwrap(),
            "attempt2" => "artifacts/07-testing.attempt-002.execution-intent.json".to_string(),
            _ => unreachable!(),
        };
        write_private_run_fixture(&fixture.workspace, &path, b"substituted");
        let before = directory_snapshot(fixture.workspace.run_directory());

        let error = adopt_approved_evaluation(
            &fixture.workspace,
            "operator@example.invalid",
            "drift must fail before publication",
        )
        .expect_err("bound input or report drift must reject adoption");

        assert!(!error.to_string().is_empty());
        assert_eq!(
            directory_snapshot(fixture.workspace.run_directory()),
            before
        );
        assert!(!fixture
            .workspace
            .run_directory()
            .join("artifacts/recovery-001.source-run.json")
            .exists());
        assert!(!fixture
            .workspace
            .run_directory()
            .join("artifacts/recovery-001.json")
            .exists());
        fixture.cleanup();
    }
}

#[test]
fn evaluation_adoption_rejects_impossible_or_conflicting_recovery_orphans_without_writes() {
    let (fixture, approved) = approved_fixture("adoption-source-collision");
    let final_shape = publish_final_eval_artifacts(&fixture.workspace, &approved, true);
    let report_path = final_shape.eval_report_path.unwrap();
    fs::remove_file(fixture.workspace.run_directory().join(&report_path)).unwrap();
    let source_path = fixture
        .workspace
        .run_directory()
        .join("artifacts/recovery-001.source-run.json");
    fs::write(&source_path, b"not an adoption source").unwrap();
    let before = directory_snapshot(fixture.workspace.run_directory());

    adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "collision must reject",
    )
    .expect_err("source collision must reject before recovery or report creation");
    assert_eq!(
        directory_snapshot(fixture.workspace.run_directory()),
        before
    );
    assert!(!fixture
        .workspace
        .run_directory()
        .join("artifacts/recovery-001.json")
        .exists());
    assert!(!fixture.workspace.run_directory().join(report_path).exists());
    fixture.cleanup();

    let (fixture, approved) = approved_fixture("adoption-recovery-without-source");
    let direct = publish_final_eval_artifacts(&fixture.workspace, &approved, true);
    let recovered = publish_evaluation_recovery_v2(&fixture.workspace, &approved, direct);
    let source = recovered
        .latest_recovery
        .as_ref()
        .map(|reference| {
            let recovery: EvaluationRecoveryAttemptV2 = serde_json::from_slice(
                &fs::read(
                    fixture
                        .workspace
                        .run_directory()
                        .join(&reference.artifact.path),
                )
                .unwrap(),
            )
            .unwrap();
            recovery.source_run.path
        })
        .unwrap();
    fs::remove_file(fixture.workspace.run_directory().join(source)).unwrap();
    let before = directory_snapshot(fixture.workspace.run_directory());
    adopt_approved_evaluation(
        &fixture.workspace,
        "reviewer@example.invalid",
        "adopt complete interrupted evaluation",
    )
    .expect_err("recovery without its source snapshot is impossible");
    assert_eq!(
        directory_snapshot(fixture.workspace.run_directory()),
        before
    );
    fixture.cleanup();
}

#[test]
fn evaluation_adoption_rejects_noncanonical_source_orphan_timestamp_before_later_writes() {
    let (fixture, approved) = approved_fixture("adoption-invalid-orphan-time");
    let final_shape = publish_final_eval_artifacts(&fixture.workspace, &approved, true);
    let report_path = final_shape.eval_report_path.unwrap();
    fs::remove_file(fixture.workspace.run_directory().join(&report_path)).unwrap();
    let adopted = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "invalid source orphan timestamp",
    )
    .unwrap();
    write_raw_run(&fixture.workspace, &approved);
    fs::remove_file(
        fixture
            .workspace
            .run_directory()
            .join(&adopted.reference.artifact.path),
    )
    .unwrap();
    fs::remove_file(fixture.workspace.run_directory().join(&report_path)).unwrap();
    let source_path = adopted.recovery.source_run.path;
    let mut source: EvaluationRecoverySourceRunV2 = serde_json::from_slice(
        &fs::read(fixture.workspace.run_directory().join(&source_path)).unwrap(),
    )
    .unwrap();
    source.created_at = "01".to_string();
    write_private_run_fixture(
        &fixture.workspace,
        &source_path,
        canonical_json_bytes(&source).unwrap(),
    );
    let before = directory_snapshot(fixture.workspace.run_directory());

    let error = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "invalid source orphan timestamp",
    )
    .expect_err("noncanonical orphan timestamp must fail during preflight");

    assert!(error.to_string().contains("source orphan"), "{error}");
    assert_eq!(
        directory_snapshot(fixture.workspace.run_directory()),
        before
    );
    assert!(!fixture
        .workspace
        .run_directory()
        .join("artifacts/recovery-001.json")
        .exists());
    assert!(!fixture.workspace.run_directory().join(report_path).exists());
    fixture.cleanup();
}

#[test]
fn evaluation_adoption_resumes_source_recovery_and_report_crash_cuts_exactly() {
    let (fixture, approved) = approved_fixture("adoption-crash-cuts");
    let final_shape = publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    let report_path = final_shape.eval_report_path.unwrap();
    fs::remove_file(fixture.workspace.run_directory().join(&report_path)).unwrap();
    let first = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "resume every adoption crash cut",
    )
    .unwrap();
    assert_eq!(
        first.recovery.report_disposition,
        EvaluationRecoveryReportDisposition::CreateMissing
    );
    let source_path = first.recovery.source_run.path.clone();
    let recovery_path = first.reference.artifact.path.clone();
    let source_bytes = fs::read(fixture.workspace.run_directory().join(&source_path)).unwrap();
    let recovery_bytes = fs::read(fixture.workspace.run_directory().join(&recovery_path)).unwrap();
    let report_bytes = fs::read(fixture.workspace.run_directory().join(&report_path)).unwrap();

    // Source published, then interrupted.
    write_raw_run(&fixture.workspace, &approved);
    fs::remove_file(fixture.workspace.run_directory().join(&recovery_path)).unwrap();
    fs::remove_file(fixture.workspace.run_directory().join(&report_path)).unwrap();
    let after_source = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "resume every adoption crash cut",
    )
    .unwrap();
    assert_eq!(after_source.recovery.created_at, first.recovery.created_at);
    assert_eq!(
        after_source.recovery.report_disposition,
        first.recovery.report_disposition
    );
    assert_eq!(
        fs::read(fixture.workspace.run_directory().join(&source_path)).unwrap(),
        source_bytes
    );
    assert_eq!(
        fs::read(fixture.workspace.run_directory().join(&recovery_path)).unwrap(),
        recovery_bytes
    );
    assert_eq!(
        fs::read(fixture.workspace.run_directory().join(&report_path)).unwrap(),
        report_bytes
    );

    // Recovery published, then interrupted before missing report.
    write_raw_run(&fixture.workspace, &approved);
    fs::remove_file(fixture.workspace.run_directory().join(&report_path)).unwrap();
    let after_recovery = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "resume every adoption crash cut",
    )
    .unwrap();
    assert_eq!(after_recovery, first);

    // Report published, then interrupted before CAS.
    write_raw_run(&fixture.workspace, &approved);
    let after_report = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "resume every adoption crash cut",
    )
    .unwrap();
    assert_eq!(after_report, first);

    // Successful CAS, then caller interruption.
    let after_cas = adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "resume every adoption crash cut",
    )
    .unwrap();
    assert_eq!(after_cas, first);
    fixture.cleanup();
}

#[test]
fn evaluation_adoption_rejects_unrelated_terminal_promotion_and_retry_drift() {
    let (fixture, approved) = approved_fixture("adoption-terminal-rejections");
    let direct = publish_final_eval_artifacts(&fixture.workspace, &approved, true);
    write_raw_run(&fixture.workspace, &direct);
    adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "unrelated final",
    )
    .expect_err("ordinary terminal evaluation cannot be relabelled adopted");
    write_raw_run(&fixture.workspace, &approved);
    write_private_run_fixture(
        &fixture.workspace,
        "artifacts/09-promotion.intent.json",
        b"promotion intent",
    );
    let before = directory_snapshot(fixture.workspace.run_directory());
    adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "promotion intent must freeze adoption",
    )
    .expect_err("promotion intent must reject fresh adoption");
    assert_eq!(
        directory_snapshot(fixture.workspace.run_directory()),
        before
    );
    fixture.cleanup();
}

#[test]
fn evaluation_adoption_exact_retry_rejects_post_cas_input_promotion_and_attempt_drift() {
    for drift in ["input", "promotion", "future-attempt"] {
        let (fixture, approved) = approved_fixture(&format!("adoption-retry-{drift}"));
        publish_final_eval_artifacts(&fixture.workspace, &approved, true);
        let adopted = adopt_approved_evaluation(
            &fixture.workspace,
            "operator@example.invalid",
            "exact retry drift test",
        )
        .unwrap();
        match drift {
            "input" => write_private_run_fixture(
                &fixture.workspace,
                "inputs/ticket.json",
                b"substituted ticket",
            ),
            "promotion" => write_private_run_fixture(
                &fixture.workspace,
                "artifacts/09-promotion.intent.json",
                b"promotion intent",
            ),
            "future-attempt" => write_private_run_fixture(
                &fixture.workspace,
                "artifacts/07-testing.attempt-002.execution-intent.json",
                b"future attempt",
            ),
            _ => unreachable!(),
        }
        let before = directory_snapshot(fixture.workspace.run_directory());
        adopt_approved_evaluation(
            &fixture.workspace,
            "operator@example.invalid",
            "exact retry drift test",
        )
        .expect_err("post-CAS drift must reject exact retry");
        assert_eq!(
            directory_snapshot(fixture.workspace.run_directory()),
            before
        );
        let mut expected_run = serde_json::to_vec_pretty(&adopted.run).unwrap();
        expected_run.push(b'\n');
        assert_eq!(
            fs::read(fixture.workspace.run_file()).unwrap(),
            expected_run
        );
        fixture.cleanup();
    }
}

#[test]
fn evaluation_adoption_rejects_pending_provider_and_active_evaluation_recovery_lineage() {
    let (fixture, approved) = approved_fixture("adoption-pending-provider");
    let pending = revise_provider_step(
        &fixture.workspace,
        LoopStepName::OutputReview,
        "operator@example.invalid",
        "pending provider recovery",
    )
    .unwrap();
    publish_final_eval_artifacts(&fixture.workspace, &approved, true);
    let mut grafted = approved.clone();
    grafted.latest_recovery = Some(pending.reference);
    write_raw_run(&fixture.workspace, &grafted);
    let before = directory_snapshot(fixture.workspace.run_directory());
    adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "pending lineage must reject",
    )
    .expect_err("unconsumed provider recovery must reject adoption");
    assert_eq!(
        directory_snapshot(fixture.workspace.run_directory()),
        before
    );
    fixture.cleanup();

    let (fixture, approved) = approved_fixture("adoption-active-evaluation");
    let direct = publish_final_eval_artifacts(&fixture.workspace, &approved, true);
    let recovered = publish_evaluation_recovery_v2(&fixture.workspace, &approved, direct);
    let mut grafted = approved;
    grafted.latest_recovery = recovered.latest_recovery;
    write_raw_run(&fixture.workspace, &grafted);
    let before = directory_snapshot(fixture.workspace.run_directory());
    adopt_approved_evaluation(
        &fixture.workspace,
        "operator@example.invalid",
        "active evaluation recovery must reject",
    )
    .expect_err("evaluation-v2 cannot be adopted again from Approved");
    assert_eq!(
        directory_snapshot(fixture.workspace.run_directory()),
        before
    );
    fixture.cleanup();
}

#[test]
fn competing_evaluation_adoptions_converge_for_exact_retry_and_choose_one_audit_winner() {
    let run_competition = |run_id: &str, reasons: [&'static str; 2]| {
        let (fixture, approved) = approved_fixture(run_id);
        publish_final_eval_artifacts(&fixture.workspace, &approved, true);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for reason in reasons {
            let workspace = fixture.workspace.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                adopt_approved_evaluation(&workspace, "operator@example.invalid", reason)
            }));
        }
        barrier.wait();
        let outcomes = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        (fixture, outcomes)
    };

    let (fixture, exact) = run_competition(
        "adoption-concurrent-exact",
        ["same adoption audit", "same adoption audit"],
    );
    let first = exact[0].as_ref().expect("one exact caller");
    let second = exact[1].as_ref().expect("other exact caller");
    assert_eq!(
        first, second,
        "same-request callers must converge byte-exactly"
    );
    fixture.cleanup();

    let (fixture, competing) = run_competition(
        "adoption-concurrent-different",
        ["competing adoption A", "competing adoption B"],
    );
    assert_eq!(
        competing.iter().filter(|outcome| outcome.is_ok()).count(),
        1
    );
    assert_eq!(
        competing.iter().filter(|outcome| outcome.is_err()).count(),
        1
    );
    let winner = competing
        .iter()
        .find_map(|outcome| outcome.as_ref().ok())
        .unwrap();
    assert!(matches!(
        winner.recovery.reason.as_str(),
        "competing adoption A" | "competing adoption B"
    ));
    let persisted = seaf_loop::state::load_run(&fixture.workspace).unwrap();
    assert_eq!(persisted, winner.run);
    assert!(fixture
        .workspace
        .run_directory()
        .join("artifacts/recovery-001.source-run.json")
        .is_file());
    assert!(fixture
        .workspace
        .run_directory()
        .join("artifacts/recovery-001.json")
        .is_file());
    assert!(!fixture
        .workspace
        .run_directory()
        .join("artifacts/recovery-002.source-run.json")
        .exists());
    assert!(!fixture
        .workspace
        .run_directory()
        .join("artifacts/recovery-002.json")
        .exists());
    fixture.cleanup();
}

#[test]
fn evaluation_recovery_v2_authenticates_indexed_v2_prefix() {
    let (fixture, approved) = approved_fixture("eval-recovery-indexed-v2");
    let direct = publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    let recovered = publish_evaluation_recovery_v2(&fixture.workspace, &approved, direct);

    let authority = load_verified_final_evaluation_authority(&fixture.workspace, &recovered)
        .expect("indexed-v2 adoption authority must verify");

    assert_eq!(authority.approved_run(), &approved);
    assert_eq!(authority.testing_evidence().schema_version, 2);
    fixture.cleanup();
}

#[test]
fn evaluation_recovery_create_missing_accepts_the_exact_created_report() {
    let (fixture, approved) = approved_fixture("eval-recovery-create-missing");
    let direct = publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    let mut recovered = publish_evaluation_recovery_v2(&fixture.workspace, &approved, direct);
    rewrite_evaluation_recovery_source(&fixture.workspace, &mut recovered, |source| {
        source.evaluation_prefix.eval_report = None;
    });
    rewrite_evaluation_recovery(&fixture.workspace, &mut recovered, |recovery| {
        recovery.report_disposition = EvaluationRecoveryReportDisposition::CreateMissing;
    });

    load_verified_final_evaluation_authority(&fixture.workspace, &recovered)
        .expect("CreateMissing must accept only its exact deterministic report after publication");
    fixture.cleanup();
}

#[test]
fn evaluation_recovery_v2_rejects_pending_provider_v1_graft() {
    let (fixture, approved) = approved_fixture("eval-recovery-mixed-lineage");
    let provider_recovery = revise_provider_step(
        &fixture.workspace,
        LoopStepName::OutputReview,
        "reviewer@example.invalid",
        "revise output review before evaluation",
    )
    .expect("publish valid provider-v1 recovery authority");
    let mut mixed_approved = approved;
    mixed_approved.latest_recovery = Some(provider_recovery.reference);
    let direct = publish_indexed_final_eval_artifacts(&fixture.workspace, &mixed_approved, true);
    let recovered = publish_evaluation_recovery_v2(&fixture.workspace, &mixed_approved, direct);

    let error = load_verified_final_evaluation_authority(&fixture.workspace, &recovered)
        .expect_err("evaluation recovery cannot graft an unconsumed provider recovery");

    assert!(
        error
            .to_string()
            .contains("predecessor is not demonstrably consumed"),
        "{error}"
    );
    fixture.cleanup();
}

#[test]
fn evaluation_recovery_v2_accepts_consumed_provider_v1_then_evaluation_v2_lineage() {
    let (fixture, approved) = approved_fixture("eval-recovery-consumed-lineage");
    let provider_recovery = revise_provider_step(
        &fixture.workspace,
        LoopStepName::OutputReview,
        "reviewer@example.invalid",
        "revise output review before evaluation",
    )
    .expect("publish valid provider-v1 recovery authority");
    let mixed_approved = consume_output_review_recovery_and_reapprove(
        &fixture,
        provider_recovery.recovery.next_step_attempt,
    );
    assert_eq!(
        mixed_approved.latest_recovery,
        Some(provider_recovery.reference)
    );
    assert!(
        mixed_approved.provider_exchange_records.len() > approved.provider_exchange_records.len()
    );
    let direct = publish_indexed_final_eval_artifacts(&fixture.workspace, &mixed_approved, true);
    let recovered = publish_evaluation_recovery_v2(&fixture.workspace, &mixed_approved, direct);

    let authority = load_verified_final_evaluation_authority(&fixture.workspace, &recovered)
        .expect("consumed provider-v1 may precede evaluation-v2 recovery");

    assert_eq!(authority.approved_run(), &mixed_approved);
    fixture.cleanup();
}

#[test]
fn evaluation_recovery_v2_rejects_grafted_prior_evaluation_authority() {
    let (fixture, approved) = approved_fixture("eval-recovery-prior-eval-v2");
    let direct = publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
    let first = publish_evaluation_recovery_v2(&fixture.workspace, &approved, direct);
    let mut grafted_approved = approved;
    grafted_approved.latest_recovery = first.latest_recovery.clone();
    let second = publish_evaluation_recovery_v2(&fixture.workspace, &grafted_approved, first);

    let error = load_verified_final_evaluation_authority(&fixture.workspace, &second)
        .expect_err("adoption cannot descend from prior evaluation-v2 recovery");

    assert!(
        error
            .to_string()
            .contains("Testing evidence bindings do not match exact Approved authority"),
        "{error}"
    );
    fixture.cleanup();
}

#[test]
fn evaluation_recovery_v2_rejects_source_prefix_disposition_projection_and_descendant_tamper() {
    for mutation in [
        "source",
        "prefix",
        "disposition",
        "projection",
        "reference",
        "log",
        "provider",
        "descendant",
    ] {
        let run_id = format!("eval-recovery-tamper-{mutation}");
        let (fixture, approved) = approved_fixture(&run_id);
        let direct = publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true);
        let mut recovered = publish_evaluation_recovery_v2(&fixture.workspace, &approved, direct);
        match mutation {
            "source" => {
                rewrite_evaluation_recovery_source(&fixture.workspace, &mut recovered, |source| {
                    source
                        .run
                        .candidate_workspace
                        .as_mut()
                        .unwrap()
                        .candidate_head = "b".repeat(40);
                })
            }
            "prefix" => {
                rewrite_evaluation_recovery_source(&fixture.workspace, &mut recovered, |source| {
                    source.evaluation_prefix.spelling = EvaluationPrefixSpellingV1::FixedV1;
                })
            }
            "disposition" => {
                rewrite_evaluation_recovery(&fixture.workspace, &mut recovered, |recovery| {
                    recovery.report_disposition =
                        EvaluationRecoveryReportDisposition::CreateMissing;
                })
            }
            "projection" => {
                rewrite_evaluation_recovery(&fixture.workspace, &mut recovered, |recovery| {
                    recovery.expected_final_projection_digest = "b".repeat(64);
                })
            }
            "reference" => {
                rewrite_evaluation_recovery(&fixture.workspace, &mut recovered, |recovery| {
                    recovery.testing_evidence.digest = "b".repeat(64);
                })
            }
            "log" => {
                write_private_run_fixture(
                    &fixture.workspace,
                    "artifacts/07-testing.attempt-001.check-001.stdout.log",
                    b"substituted log\n",
                );
            }
            "provider" => {
                let path = &approved.provider_exchange_records.last().unwrap().path;
                write_private_run_fixture(&fixture.workspace, path, b"substituted");
            }
            "descendant" => recovered.updated_at = "99".to_string(),
            _ => unreachable!(),
        }

        let error = load_verified_final_evaluation_authority(&fixture.workspace, &recovered)
            .expect_err("evaluation recovery tamper must fail closed");
        assert!(!error.to_string().is_empty(), "{mutation}");
        fixture.cleanup();
    }
}

#[test]
fn evaluation_recovery_v2_accepts_only_monotonic_failed_cleanup_descendants() {
    let (fixture, approved) = approved_fixture("eval-recovery-v2-cleanup");
    let direct = publish_final_eval_artifacts(&fixture.workspace, &approved, false);
    let recovered = publish_evaluation_recovery_v2(&fixture.workspace, &approved, direct);
    let base_time = recovered.updated_at.parse::<u64>().unwrap();
    let started_at = base_time.checked_add(1).unwrap().to_string();
    let cleaned_at = base_time.checked_add(2).unwrap().to_string();

    let mut cleaning = recovered.clone();
    let candidate = cleaning.candidate_workspace.as_mut().unwrap();
    candidate.lifecycle = seaf_core::CandidateWorkspaceLifecycle::Cleaning;
    candidate.cleanup_started_at = Some(started_at.clone());
    cleaning.updated_at = started_at;
    load_verified_final_evaluation_authority(&fixture.workspace, &cleaning)
        .expect("monotonic Cleaning descendant remains verifiable");

    let mut cleaned = cleaning;
    let candidate = cleaned.candidate_workspace.as_mut().unwrap();
    candidate.lifecycle = seaf_core::CandidateWorkspaceLifecycle::Cleaned;
    candidate.cleaned_at = Some(cleaned_at.clone());
    cleaned.updated_at = cleaned_at;
    load_verified_final_evaluation_authority(&fixture.workspace, &cleaned)
        .expect("monotonic Cleaned descendant remains verifiable");

    let mut arbitrary = recovered;
    arbitrary.updated_at = "99".to_string();
    let error = load_verified_final_evaluation_authority(&fixture.workspace, &arbitrary)
        .expect_err("an arbitrary Failed timestamp is not cleanup authority");
    assert!(error.to_string().contains("cleanup"), "{error}");
    fixture.cleanup();
}

#[test]
fn evaluation_recovery_v2_rejects_eval_passed_cleanup_descendants() {
    for lifecycle in [
        seaf_core::CandidateWorkspaceLifecycle::Cleaning,
        seaf_core::CandidateWorkspaceLifecycle::Cleaned,
    ] {
        let suffix = match lifecycle {
            seaf_core::CandidateWorkspaceLifecycle::Cleaning => "cleaning",
            seaf_core::CandidateWorkspaceLifecycle::Cleaned => "cleaned",
            _ => unreachable!(),
        };
        let (fixture, approved) = approved_fixture(&format!("eval-passed-{suffix}"));
        let direct = publish_final_eval_artifacts(&fixture.workspace, &approved, true);
        let mut recovered = publish_evaluation_recovery_v2(&fixture.workspace, &approved, direct);
        let base_time = recovered.updated_at.parse::<u64>().unwrap();
        let started_at = base_time.checked_add(1).unwrap().to_string();
        let cleaned_at = base_time.checked_add(2).unwrap().to_string();
        let candidate = recovered.candidate_workspace.as_mut().unwrap();
        candidate.lifecycle = lifecycle;
        candidate.cleanup_started_at = Some(started_at.clone());
        recovered.updated_at = started_at;
        if lifecycle == seaf_core::CandidateWorkspaceLifecycle::Cleaned {
            candidate.cleaned_at = Some(cleaned_at.clone());
            recovered.updated_at = cleaned_at;
        }

        let error = load_verified_final_evaluation_authority(&fixture.workspace, &recovered)
            .expect_err("EvalPassed recovery authority must remain frozen and non-cleanable");
        assert!(
            error.to_string().contains("cleanup") || error.to_string().contains("active"),
            "{error}"
        );
        fixture.cleanup();
    }
}

#[test]
fn testing_evidence_binding_rejects_every_approved_authority_substitution() {
    let (fixture, approved) = approved_fixture("testing-binding-substitution");
    let approved_at = approved
        .human_approval
        .as_ref()
        .unwrap()
        .approved_at
        .clone();
    let exact = TestingEvidence::create(
        &approved,
        approved_at.clone(),
        approved_at,
        vec![EvalCheck {
            name: "unit".to_string(),
            status: CheckStatus::Passed,
            duration_ms: Some(1),
            stdout_path: Some("artifacts/eval/unit.stdout.log".to_string()),
            stdout_digest: Some("a".repeat(64)),
            stderr_path: Some("artifacts/eval/unit.stderr.log".to_string()),
            stderr_digest: Some("b".repeat(64)),
            summary: Some("passed".to_string()),
        }],
    )
    .unwrap();
    let reference = ArtifactReference {
        path: "artifacts/testing-binding.json".to_string(),
        digest: exact.artifact_digest().unwrap(),
    };
    write_private_run_fixture(
        &fixture.workspace,
        &reference.path,
        exact.canonical_bytes().unwrap(),
    );
    assert_eq!(
        TestingEvidence::load_for_approved_run(&fixture.workspace, &reference, &approved).unwrap(),
        exact
    );

    let mut substitutions = Vec::new();
    let mut value = exact.clone();
    value.run_id = "other-run".to_string();
    substitutions.push(("run_id", value));
    let mut value = exact.clone();
    value.ticket_id = "other-ticket".to_string();
    substitutions.push(("ticket_id", value));
    let mut value = exact.clone();
    value.goal_id = "other-goal".to_string();
    substitutions.push(("goal_id", value));
    let mut value = exact.clone();
    value.eval_config.digest = "c".repeat(64);
    substitutions.push(("eval_config", value));
    let mut value = exact.clone();
    value.candidate_diff.digest = "d".repeat(64);
    substitutions.push(("candidate_diff", value));
    let mut value = exact.clone();
    value.starting_head = "a".repeat(40);
    substitutions.push(("starting_head", value));
    let mut value = exact.clone();
    value.human_approval_digest = "e".repeat(64);
    substitutions.push(("human_approval_digest", value));
    let mut value = exact.clone();
    value.policy_decision_digest = "f".repeat(64);
    substitutions.push(("policy_decision_digest", value));
    let mut value = exact;
    value.approved_run_digest = "0".repeat(64);
    substitutions.push(("approved_run_digest", value));

    for (field, substitution) in substitutions {
        let error = substitution
            .validate_against_approved_run(&approved)
            .expect_err("substituted Approved binding must fail");
        assert!(error.to_string().contains("bindings"), "{field}: {error}");
    }
    fixture.cleanup();
}

#[test]
fn testing_evidence_cannot_start_before_canonical_human_approval() {
    let (fixture, approved) = approved_fixture("testing-after-approval");
    let approved_at = approved
        .human_approval
        .as_ref()
        .unwrap()
        .approved_at
        .parse::<u64>()
        .unwrap();
    let checks = vec![EvalCheck {
        name: "unit".to_string(),
        status: CheckStatus::Passed,
        duration_ms: Some(1),
        stdout_path: Some("artifacts/eval/unit.stdout.log".to_string()),
        stdout_digest: Some("a".repeat(64)),
        stderr_path: Some("artifacts/eval/unit.stderr.log".to_string()),
        stderr_digest: Some("b".repeat(64)),
        summary: Some("passed".to_string()),
    }];

    let error = TestingEvidence::create(
        &approved,
        approved_at.saturating_sub(1).to_string(),
        approved_at.to_string(),
        checks.clone(),
    )
    .expect_err("Testing cannot begin before approval");
    assert!(error.to_string().contains("approved_at"), "{error}");

    let exact = TestingEvidence::create(
        &approved,
        approved_at.to_string(),
        approved_at.to_string(),
        checks,
    )
    .unwrap();
    let mut malformed_approval = approved.clone();
    malformed_approval
        .human_approval
        .as_mut()
        .unwrap()
        .approved_at = "not-unix-seconds".to_string();
    malformed_approval.updated_at = "not-unix-seconds".to_string();
    let error = exact
        .validate_against_approved_run(&malformed_approval)
        .expect_err("integrated evidence requires canonical approval time");
    assert!(error.to_string().contains("approved_at"), "{error}");
    fixture.cleanup();
}

#[test]
fn final_authority_loader_rejects_standalone_substituted_and_noncanonical_artifacts() {
    let (fixture, approved) = approved_fixture("final-loader-substitutions");
    let final_run = publish_final_eval_artifacts(&fixture.workspace, &approved, true);
    let verified = load_verified_final_evaluation_authority(&fixture.workspace, &final_run)
        .expect("exact final authority");
    let exact_report = verified.eval_report().clone();

    let mut variants = Vec::new();
    let mut report = exact_report.clone();
    report.loop_evidence = None;
    variants.push(("standalone", report));
    let mut report = exact_report.clone();
    report.patch_id = "other-run".to_string();
    report.loop_evidence.as_mut().unwrap().run_id = "other-run".to_string();
    variants.push(("wrong-run", report));
    let mut report = exact_report.clone();
    report.goal_id = "other-goal".to_string();
    variants.push(("wrong-goal", report));
    let mut report = exact_report.clone();
    report.loop_evidence.as_mut().unwrap().ticket_digest = "c".repeat(64);
    variants.push(("substituted-ticket", report));
    let mut report = exact_report.clone();
    report.loop_evidence.as_mut().unwrap().candidate_diff.digest = "d".repeat(64);
    variants.push(("substituted-candidate", report));
    let mut report = exact_report.clone();
    report.checks[0].summary = Some("substituted check".to_string());
    variants.push(("substituted-check", report));
    let mut report = exact_report.clone();
    report.checks[0].stdout_digest = None;
    variants.push(("incomplete-log", report));
    let mut report = exact_report.clone();
    report.decision = EvalDecision::Reject;
    variants.push(("rejecting-pass", report));

    for (label, report) in variants {
        let run = publish_report_variant(&fixture.workspace, &final_run, &report, label, true);
        let error = load_verified_final_evaluation_authority(&fixture.workspace, &run)
            .expect_err("substituted final report must fail");
        assert!(!error.to_string().is_empty(), "{label}");
        fs::remove_file(
            fixture
                .workspace
                .run_directory()
                .join(run.steps[7].artifact_path.as_ref().unwrap()),
        )
        .unwrap();
    }

    let noncanonical = publish_report_variant(
        &fixture.workspace,
        &final_run,
        &exact_report,
        "noncanonical",
        false,
    );
    let error = load_verified_final_evaluation_authority(&fixture.workspace, &noncanonical)
        .expect_err("noncanonical report must fail");
    assert!(error.to_string().contains("canonical"), "{error}");
    fs::remove_file(
        fixture
            .workspace
            .run_directory()
            .join(noncanonical.steps[7].artifact_path.as_ref().unwrap()),
    )
    .unwrap();

    let mut mismatched_digest = final_run.clone();
    mismatched_digest.steps[7].artifact_digest = Some("0".repeat(64));
    let error = load_verified_final_evaluation_authority(&fixture.workspace, &mismatched_digest)
        .expect_err("report digest mismatch must fail");
    assert!(error.to_string().contains("digest mismatch"), "{error}");

    let mut missing = final_run;
    missing.steps[7].artifact_path = Some("artifacts/missing-eval-report.json".to_string());
    missing.eval_report_path = missing.steps[7].artifact_path.clone();
    let error = load_verified_final_evaluation_authority(&fixture.workspace, &missing)
        .expect_err("missing report must fail");
    assert!(error.to_string().contains("canonical"), "{error}");
    fixture.cleanup();
}

#[test]
fn final_authority_loader_rejects_log_incomplete_testing_evidence() {
    let (fixture, approved) = approved_fixture("final-loader-testing-log");
    let final_run = publish_final_eval_artifacts(&fixture.workspace, &approved, true);
    let verified = load_verified_final_evaluation_authority(&fixture.workspace, &final_run)
        .expect("exact final authority");
    let mut testing = serde_json::to_value(verified.testing_evidence()).unwrap();
    testing["checks"][0]
        .as_object_mut()
        .unwrap()
        .remove("stdout_digest");
    let testing_reference = ArtifactReference {
        path: "artifacts/07-testing.json".to_string(),
        digest: canonical_sha256_digest(&testing).unwrap(),
    };
    write_private_run_fixture(
        &fixture.workspace,
        &testing_reference.path,
        canonical_json_bytes(&testing).unwrap(),
    );
    let mut report = verified.eval_report().clone();
    report.loop_evidence.as_mut().unwrap().testing_evidence = testing_reference.clone();
    let mut run = final_run;
    run.steps[6].artifact_path = Some(testing_reference.path);
    run.steps[6].artifact_digest = Some(testing_reference.digest);
    let report_path = "artifacts/08-eval-report.json";
    write_private_run_fixture(
        &fixture.workspace,
        report_path,
        canonical_json_bytes(&report).unwrap(),
    );
    run.steps[7].artifact_path = Some(report_path.to_string());
    run.steps[7].artifact_digest = Some(canonical_sha256_digest(&report).unwrap());
    run.eval_report_path = Some(report_path.to_string());

    let error = load_verified_final_evaluation_authority(&fixture.workspace, &run)
        .expect_err("log-incomplete Testing evidence must fail");
    assert!(error.to_string().contains("stdout"), "{error}");
    fixture.cleanup();
}

#[test]
fn final_authority_v1_v2_v3_loaders_raw_scan_marker_spanning_history() {
    const SECRET: &str = "prefix[REDACTED]suffix";
    for schema in [1, 2, 3] {
        let (fixture, approved) = approved_fixture_with_eval_mode(
            &format!("final-loader-marker-envelope-v{schema}"),
            FixtureEvalMode::EvaluationMarkerEnvelopeSecret,
        );
        let mut final_run = match schema {
            1 => publish_final_eval_artifacts(&fixture.workspace, &approved, true),
            2 => publish_indexed_final_eval_artifacts_v2(&fixture.workspace, &approved, true),
            3 => publish_indexed_final_eval_artifacts(&fixture.workspace, &approved, true),
            _ => unreachable!(),
        };
        replace_testing_summary(&fixture.workspace, &mut final_run, SECRET);

        let error = load_verified_final_evaluation_authority(&fixture.workspace, &final_run)
            .expect_err("marker-spanning Testing bytes must fail closed");

        assert_eq!(
            error.to_string(),
            "derived evaluation artifact contains prohibited credential material",
            "schema v{schema}"
        );
        fixture.cleanup();
    }
}

#[test]
fn reported_eval_failure_requires_a_rejecting_report() {
    let (fixture, approved) = approved_fixture("final-loader-failed-decision");
    let final_run = publish_final_eval_artifacts(&fixture.workspace, &approved, false);
    let verified = load_verified_final_evaluation_authority(&fixture.workspace, &final_run)
        .expect("exact reported failure");
    let mut report = verified.eval_report().clone();
    report.decision = EvalDecision::ApproveForHumanReview;
    let run = publish_report_variant(&fixture.workspace, &final_run, &report, "nonreject", true);

    let error = load_verified_final_evaluation_authority(&fixture.workspace, &run)
        .expect_err("reported failure with non-Reject report must fail");

    assert!(error.to_string().contains("reject"), "{error}");
    fixture.cleanup();
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
fn historical_isolated_testing_or_eval_prefix_rejects_before_recovery_mutation() {
    for next_step in [LoopStepName::Testing, LoopStepName::EvalReport] {
        let run_id = format!("historical-unapproved-{next_step:?}").to_ascii_lowercase();
        let fixture = fixture(&run_id);
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
        let mut runner =
            LoopRunner::start_initialized(fixture.prepared, &mut step_runner).expect("start");
        for _ in 0..6 {
            assert!(runner.run_next_step().expect("through OutputReview"));
        }
        let workspace = LoopWorkspace::open(&fixture.runs_root, &run_id).unwrap();
        let mut historical = runner.run().clone();
        historical.status = seaf_core::LoopStatus::Running;
        if next_step == LoopStepName::EvalReport {
            let testing = historical
                .steps
                .iter_mut()
                .find(|record| record.name == LoopStepName::Testing)
                .unwrap();
            testing.status = seaf_core::LoopStepStatus::Completed;
            testing.artifact_path = Some("artifacts/07-testing.md".to_string());
            testing.artifact_digest = Some("7".repeat(64));
            historical.current_step = LoopStepName::EvalReport;
        }
        drop(runner);
        drop(step_runner);
        let mut historical_bytes = serde_json::to_vec_pretty(&historical).unwrap();
        historical_bytes.push(b'\n');
        fs::write(workspace.run_file(), historical_bytes).unwrap();
        let before = snapshot_files(workspace.run_directory());
        let provider_calls_before = provider.requests().unwrap();

        let error = InitializedLoopRun::resume_isolated(&fixture.runs_root, historical)
            .expect_err("unapproved historical execution prefix must fail closed");

        assert!(error.to_string().contains("human approval"), "{error}");
        assert!(error.to_string().contains("start a new run"), "{error}");
        assert_eq!(snapshot_files(workspace.run_directory()), before);
        assert_eq!(provider.requests().unwrap(), provider_calls_before);
        remove_candidate(&fixture.source, &fixture.candidate);
    }
}

#[test]
fn awaiting_human_review_cleanup_refuses_before_repository_lock_or_evidence_mutation() {
    let fixture = fixture("awaiting-cleanup-refusal");
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
    let mut runner =
        LoopRunner::start_initialized(fixture.prepared, &mut step_runner).expect("start");
    for _ in 0..6 {
        assert!(runner.run_next_step().expect("through OutputReview"));
    }
    let workspace = LoopWorkspace::open(&fixture.runs_root, "awaiting-cleanup-refusal").unwrap();
    let run_bytes_before = fs::read(workspace.run_file()).unwrap();
    let source_before = source_evidence(&fixture.source);
    let candidate_before = source_evidence(&fixture.candidate);
    let mut false_completed = runner.run().clone();
    false_completed.status = seaf_core::LoopStatus::Completed;
    let error = seaf_loop::state::save_run(&workspace, &false_completed)
        .expect_err("public save cannot bypass the review barrier");
    assert!(
        error.to_string().contains("awaiting human review"),
        "{error}"
    );
    assert_eq!(fs::read(workspace.run_file()).unwrap(), run_bytes_before);
    let error = seaf_loop::state::write_run_file(&workspace.run_file(), &false_completed)
        .expect_err("public writer cannot bypass the review barrier");
    assert!(
        error.to_string().contains("awaiting human review"),
        "{error}"
    );
    assert_eq!(fs::read(workspace.run_file()).unwrap(), run_bytes_before);
    drop(runner);
    drop(step_runner);

    let error = cleanup_candidate_workspace_outcome(&workspace, &fixture.source)
        .expect_err("awaiting review candidate remains active and non-cleanable");

    assert!(error.to_string().contains("active run"), "{error}");
    assert_eq!(fs::read(workspace.run_file()).unwrap(), run_bytes_before);
    assert_eq!(source_evidence(&fixture.source), source_before);
    assert_eq!(source_evidence(&fixture.candidate), candidate_before);
    remove_candidate(&fixture.source, &fixture.candidate);
}

#[test]
fn unauthenticated_output_review_cannot_publish_the_human_review_barrier() {
    let fixture = fixture("unauthenticated-output-review");
    let provider = FakeProvider::new(candidate_responses(true).into_iter().take(5).collect());
    let mut patch_runner = RecordingPatchRunner::default();
    let mut provider_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
    let mut runner =
        LoopRunner::start_initialized(fixture.prepared, &mut provider_runner).expect("start");
    for _ in 0..5 {
        assert!(runner.run_next_step().expect("through Development"));
    }
    drop(runner);
    drop(provider_runner);
    let workspace = LoopWorkspace::open(&fixture.runs_root, "unauthenticated-output-review")
        .expect("workspace");
    let source_before = source_evidence(&fixture.source);
    let candidate_before = source_evidence(&fixture.candidate);
    let mut unauthenticated = UnauthenticatedOutputReview;
    let mut resumed = LoopRunner::resume(
        &fixture.runs_root,
        "unauthenticated-output-review",
        &mut unauthenticated,
    )
    .expect("resume applied candidate");

    let error = resumed
        .run_next_step()
        .expect_err("Passed review without an authenticated ledger cannot publish Awaiting");

    assert!(error.to_string().contains("OutputReview"), "{error}");
    drop(resumed);
    let persisted = seaf_loop::state::load_run(&workspace).unwrap();
    assert_ne!(persisted.status, seaf_core::LoopStatus::AwaitingHumanReview);
    assert_eq!(source_evidence(&fixture.source), source_before);
    assert_eq!(source_evidence(&fixture.candidate), candidate_before);
    remove_candidate(&fixture.source, &fixture.candidate);
}

#[test]
fn non_approving_authenticated_review_cannot_be_relabelled_passed_by_a_custom_runner() {
    let fixture = fixture("non-approving-review-custom-pass");
    let mut responses = candidate_responses(false);
    responses.push(response(
        r#"{"role":"output_reviewer","decision":"request_changes","summary":"Not approved.","blocking_issues":[{"summary":"Fix required","evidence":"candidate diff"}],"non_blocking_issues":[]}"#,
    ));
    let provider = FakeProvider::new(responses);
    let mut patch_runner = RecordingPatchRunner::default();
    let mut provider_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
    let mut runner =
        LoopRunner::start_initialized(fixture.prepared, &mut provider_runner).expect("start");
    for _ in 0..6 {
        assert!(runner.run_next_step().expect("through rejected review"));
    }
    assert_eq!(runner.run().status, seaf_core::LoopStatus::Blocked);
    let workspace =
        LoopWorkspace::open(&fixture.runs_root, "non-approving-review-custom-pass").unwrap();
    let blocked_bytes = fs::read(workspace.run_file()).unwrap();
    let mut forged = runner.run().clone();
    forged.status = seaf_core::LoopStatus::AwaitingHumanReview;
    forged.current_step = LoopStepName::Testing;
    forged
        .steps
        .iter_mut()
        .find(|record| record.name == LoopStepName::OutputReview)
        .unwrap()
        .status = seaf_core::LoopStepStatus::Passed;
    assert!(seaf_core::validate_loop_run(&forged).is_empty());
    let error = seaf_loop::state::save_run(&workspace, &forged)
        .expect_err("direct save cannot create Awaiting from RequestChanges history");
    assert!(error.to_string().contains("cannot create"), "{error}");
    assert_eq!(fs::read(workspace.run_file()).unwrap(), blocked_bytes);
    let error = seaf_loop::state::write_run_file(&workspace.run_file(), &forged)
        .expect_err("direct writer cannot create Awaiting from RequestChanges history");
    assert!(error.to_string().contains("cannot create"), "{error}");
    assert_eq!(fs::read(workspace.run_file()).unwrap(), blocked_bytes);
    drop(runner);
    drop(provider_runner);
    assert_ne!(
        seaf_loop::state::load_run(&workspace).unwrap().status,
        seaf_core::LoopStatus::AwaitingHumanReview
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
    write_raw_run(&workspace, &legacy);
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
fn non_completed_development_never_creates_a_transaction_or_calls_output_review() {
    enum Case {
        Rejected,
        Blocked,
        ProviderFailure,
        Timeout,
    }
    for (name, case) in [
        ("rejected", Case::Rejected),
        ("blocked", Case::Blocked),
        ("provider-failure", Case::ProviderFailure),
        ("timeout", Case::Timeout),
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
            Case::Timeout => Err(seaf_models::ModelError::timeout(
                "injected Development timeout",
                30_000,
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

#[derive(Debug, Clone, Copy)]
enum ApprovalMutation {
    DuplicatePolicy,
    UnrelatedPolicy,
    OutputReviewArtifact,
    InitialProviderReference,
    ProviderReference,
    LaterReviewAttempt,
    MovedSourceHead,
    ChangedCandidate,
    NonAwaitingStatus,
}

struct AwaitingApprovalFixture {
    _temp: tempfile::TempDir,
    source: PathBuf,
    candidate: PathBuf,
    ticket: TicketSpec,
    policy: Policy,
    workspace: LoopWorkspace,
}

impl AwaitingApprovalFixture {
    fn cleanup(self) {
        remove_candidate(&self.source, &self.candidate);
    }
}

fn awaiting_approval_fixture(run_id: &str) -> AwaitingApprovalFixture {
    awaiting_approval_fixture_with_eval_execution(run_id, false)
}

fn awaiting_approval_fixture_with_eval_execution(
    run_id: &str,
    allow_eval_execution: bool,
) -> AwaitingApprovalFixture {
    awaiting_approval_fixture_with_eval_mode(
        run_id,
        if allow_eval_execution {
            FixtureEvalMode::Cargo
        } else {
            FixtureEvalMode::Disabled
        },
    )
}

fn awaiting_approval_fixture_with_eval_mode(
    run_id: &str,
    eval_mode: FixtureEvalMode,
) -> AwaitingApprovalFixture {
    let Fixture {
        _temp,
        runs_root,
        source,
        candidate,
        ticket,
        policy,
        prepared,
    } = fixture_with_eval_mode(run_id, eval_mode);
    let provider = FakeProvider::new(candidate_responses(true));
    let mut patch_runner = RecordingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&candidate, &ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(&candidate, &ticket, policy.clone(), true),
            &mut patch_runner,
        );
    let mut runner = LoopRunner::start_initialized(prepared, &mut step_runner).expect("start");
    for _ in 0..6 {
        assert!(runner.run_next_step().expect("through OutputReview"));
    }
    drop(runner);
    drop(step_runner);
    let workspace = LoopWorkspace::open(&runs_root, run_id).unwrap();
    AwaitingApprovalFixture {
        _temp,
        source,
        candidate,
        ticket,
        policy,
        workspace,
    }
}

fn final_eval_fixture(run_id: &str, passed: bool) -> AwaitingApprovalFixture {
    let (fixture, approved) = approved_fixture(run_id);
    let run = publish_final_eval_artifacts(&fixture.workspace, &approved, passed);
    load_verified_final_evaluation_authority(&fixture.workspace, &run)
        .expect("private final fixture must pass combined authority validation");
    write_raw_run(&fixture.workspace, &run);
    fixture
}

fn approved_fixture(run_id: &str) -> (AwaitingApprovalFixture, seaf_core::LoopRun) {
    let fixture = awaiting_approval_fixture(run_id);
    let awaiting = seaf_loop::state::load_run(&fixture.workspace).unwrap();
    let candidate = awaiting.candidate_workspace.as_ref().unwrap();
    let approved = approve_candidate_for_testing(
        &fixture.workspace,
        &fixture.source,
        "reviewer@example.invalid",
        &candidate.candidate_diff_digest,
        &candidate.starting_head,
    )
    .unwrap()
    .run;
    (fixture, approved)
}

fn approved_fixture_with_eval_execution(
    run_id: &str,
) -> (AwaitingApprovalFixture, seaf_core::LoopRun) {
    let fixture = awaiting_approval_fixture_with_eval_execution(run_id, true);
    let awaiting = seaf_loop::state::load_run(&fixture.workspace).unwrap();
    let candidate = awaiting.candidate_workspace.as_ref().unwrap();
    let approved = approve_candidate_for_testing(
        &fixture.workspace,
        &fixture.source,
        "reviewer@example.invalid",
        &candidate.candidate_diff_digest,
        &candidate.starting_head,
    )
    .unwrap()
    .run;
    (fixture, approved)
}

fn approved_fixture_with_eval_mode(
    run_id: &str,
    mode: FixtureEvalMode,
) -> (AwaitingApprovalFixture, seaf_core::LoopRun) {
    let fixture = awaiting_approval_fixture_with_eval_mode(run_id, mode);
    let awaiting = seaf_loop::state::load_run(&fixture.workspace).unwrap();
    let candidate = awaiting.candidate_workspace.as_ref().unwrap();
    let approved = approve_candidate_for_testing(
        &fixture.workspace,
        &fixture.source,
        "reviewer@example.invalid",
        &candidate.candidate_diff_digest,
        &candidate.starting_head,
    )
    .unwrap()
    .run;
    (fixture, approved)
}

fn intended_evaluation_intent_size(
    fixture: &AwaitingApprovalFixture,
    approved: &seaf_core::LoopRun,
) -> usize {
    intended_evaluation_intent_bytes(fixture, approved).len()
}

fn intended_evaluation_intent_bytes(
    fixture: &AwaitingApprovalFixture,
    approved: &seaf_core::LoopRun,
) -> Vec<u8> {
    let candidate = approved.candidate_workspace.as_ref().unwrap();
    let eval_config: serde_json::Value = serde_json::from_slice(
        &fs::read(
            fixture
                .workspace
                .run_directory()
                .join("inputs/eval-config.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let intent = serde_json::json!({
        "schema_version": 3,
        "evaluation_attempt": 1,
        "run_id": approved.run_id,
        "approved_run_digest": canonical_sha256_digest(approved).unwrap(),
        "input_digests": approved.input_digests,
        "ticket": {
            "path": "inputs/ticket.json",
            "digest": approved.input_digests.ticket,
        },
        "eval_config": {
            "path": "inputs/eval-config.json",
            "digest": approved.input_digests.eval_config,
        },
        "candidate_state_digest": canonical_sha256_digest(candidate).unwrap(),
        "candidate_diff": approved.human_approval.as_ref().unwrap().candidate_diff,
        "source_worktree_state_digest": source_worktree_authority_digest(approved),
        "recovery": null,
        "planned_checks": planned_check_projections(&eval_config),
    });
    canonical_json_bytes(&intent).unwrap()
}

fn planned_check_projections(eval_config: &serde_json::Value) -> Vec<serde_json::Value> {
    eval_config["evals"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| {
            let mut env_names = check["env"]
                .as_object()
                .map(|env| env.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            env_names.sort();
            serde_json::json!({
                "name": check["name"],
                "command": check["command"],
                "cwd": check.get("cwd").cloned().unwrap_or(serde_json::Value::Null),
                "env_names": env_names,
                "timeout_ms": check.get("timeout_ms").cloned().unwrap_or(serde_json::Value::Null),
                "max_output_bytes": check.get("max_output_bytes").cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect()
}

fn run_tree_usage(root: &Path) -> (u64, usize) {
    fn walk(path: &Path, identities: &mut BTreeSet<(u64, u64)>) -> (u64, usize) {
        use std::os::unix::fs::MetadataExt;
        let mut bytes = 0_u64;
        let mut entries = 0_usize;
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            entries += 1;
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            if metadata.is_dir() {
                let nested = walk(&entry.path(), identities);
                bytes += nested.0;
                entries += nested.1;
            } else if metadata.is_file() && identities.insert((metadata.dev(), metadata.ino())) {
                bytes += metadata.len();
            }
        }
        (bytes, entries)
    }
    walk(root, &mut BTreeSet::new())
}

fn fill_run_bytes(root: &Path, mut bytes: u64, label: &str) {
    let mut index = 0;
    while bytes > 0 {
        let size = bytes.min(EVALUATION_EVIDENCE_CAP);
        let path = root.join(format!("{label}-byte-filler-{index:03}"));
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.set_len(size).unwrap();
        bytes -= size;
        index += 1;
    }
}

fn fill_run_entries(root: &Path, count: usize, label: &str) {
    for index in 0..count {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join(format!("{label}-entry-filler-{index:04}")))
            .unwrap();
    }
}

fn approved_fixture_with_drifting_eval(
    run_id: &str,
) -> (AwaitingApprovalFixture, seaf_core::LoopRun) {
    let fixture =
        awaiting_approval_fixture_with_eval_mode(run_id, FixtureEvalMode::DriftAfterFirstCheck);
    let awaiting = seaf_loop::state::load_run(&fixture.workspace).unwrap();
    let candidate = awaiting.candidate_workspace.as_ref().unwrap();
    let approved = approve_candidate_for_testing(
        &fixture.workspace,
        &fixture.source,
        "reviewer@example.invalid",
        &candidate.candidate_diff_digest,
        &candidate.starting_head,
    )
    .unwrap()
    .run;
    (fixture, approved)
}

fn consume_output_review_recovery_and_reapprove(
    fixture: &AwaitingApprovalFixture,
    attempt: u32,
) -> seaf_core::LoopRun {
    let reset = seaf_loop::state::load_run(&fixture.workspace).unwrap();
    let runs_root = fixture.workspace.run_directory().parent().unwrap();
    let initialized =
        InitializedLoopRun::resume_isolated_for_rerun(runs_root, reset, LoopStepName::OutputReview)
            .unwrap();
    let run_directory = fixture.workspace.run_directory();
    let read = |path: &str| fs::read(run_directory.join(path)).unwrap();
    let prepared = initialized
        .scaffold()
        .unwrap()
        .publish_authoritative_inputs(AuthoritativeRunInputSnapshots {
            ticket: read("inputs/ticket.json"),
            provider_ticket: read("ticket.snapshot.json"),
            policy: read("inputs/policy.json"),
            config: read("inputs/config.json"),
            repository: read("inputs/repository.json"),
            eval_config: read("inputs/eval-config.json"),
        })
        .unwrap();
    let provider = FakeProvider::new(vec![response(
        r#"{"role":"output_reviewer","decision":"approve_for_tests","summary":"The revised candidate still matches the approved spec.","blocking_issues":[],"non_blocking_issues":[]}"#,
    )]);
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
        )
        .with_recovery_attempt(LoopStepName::OutputReview, attempt);
    let mut runner = LoopRunner::resume_initialized(prepared, &mut step_runner).unwrap();
    runner.run_to_completion().unwrap();
    let awaiting = runner.run().clone();
    drop(runner);
    drop(step_runner);
    assert_eq!(awaiting.status, seaf_core::LoopStatus::AwaitingHumanReview);
    let candidate = awaiting.candidate_workspace.as_ref().unwrap();
    approve_candidate_for_testing(
        &fixture.workspace,
        &fixture.source,
        "reviewer@example.invalid",
        &candidate.candidate_diff_digest,
        &candidate.starting_head,
    )
    .unwrap()
    .run
}

fn publish_final_eval_artifacts(
    workspace: &LoopWorkspace,
    approved: &seaf_core::LoopRun,
    passed: bool,
) -> seaf_core::LoopRun {
    let eval_config: serde_json::Value = serde_json::from_slice(
        &fs::read(workspace.run_directory().join("inputs/eval-config.json")).unwrap(),
    )
    .unwrap();
    let check_name = eval_config["evals"]["required"][0]["name"]
        .as_str()
        .unwrap()
        .to_string();
    let stdout = b"fixture stdout\n";
    let stderr = b"fixture stderr\n";
    let stdout_path = "artifacts/07-testing.check-001.stdout.log";
    let stderr_path = "artifacts/07-testing.check-001.stderr.log";
    write_private_run_fixture(workspace, stdout_path, stdout);
    write_private_run_fixture(workspace, stderr_path, stderr);
    let check = EvalCheck {
        name: check_name,
        status: if passed {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
        duration_ms: Some(1),
        stdout_path: Some(stdout_path.to_string()),
        stdout_digest: Some(hex::encode(Sha256::digest(stdout))),
        stderr_path: Some(stderr_path.to_string()),
        stderr_digest: Some(hex::encode(Sha256::digest(stderr))),
        summary: Some(if passed { "passed" } else { "failed" }.to_string()),
    };
    let approved_at = approved
        .human_approval
        .as_ref()
        .unwrap()
        .approved_at
        .clone();
    let testing_evidence = TestingEvidence::create(
        approved,
        approved_at.clone(),
        approved_at,
        vec![check.clone()],
    )
    .unwrap();
    let intent = serde_json::json!({
        "schema_version": 1,
        "run_id": approved.run_id,
        "approved_run_digest": canonical_sha256_digest(approved).unwrap(),
        "ticket": {
            "path": "inputs/ticket.json",
            "digest": approved.input_digests.ticket,
        },
        "eval_config": {
            "path": "inputs/eval-config.json",
            "digest": approved.input_digests.eval_config,
        },
        "candidate_diff": approved.human_approval.as_ref().unwrap().candidate_diff,
        "planned_checks": eval_config["evals"]["required"],
    });
    write_private_run_fixture(
        workspace,
        "artifacts/07-testing.execution-intent.json",
        canonical_json_bytes(&intent).unwrap(),
    );
    let testing_reference = ArtifactReference {
        path: "artifacts/07-testing.json".to_string(),
        digest: testing_evidence.artifact_digest().unwrap(),
    };
    write_private_run_fixture(
        workspace,
        &testing_reference.path,
        testing_evidence.canonical_bytes().unwrap(),
    );

    let approval = approved.human_approval.as_ref().unwrap();
    let eval_config_digest = approved.input_digests.eval_config.as_ref().unwrap();
    let report = EvalReport {
        eval_report_id: format!("eval_{}", approved.run_id),
        patch_id: approved.run_id.clone(),
        goal_id: approved.goal_id.clone(),
        passed,
        summary: if passed {
            "Approved candidate passed all required local checks.".to_string()
        } else {
            "Approved candidate failed one or more required local checks.".to_string()
        },
        checks: vec![check],
        score_delta_estimate: None,
        risk_level: if passed {
            RiskLevel::Low
        } else {
            RiskLevel::High
        },
        decision: if passed {
            EvalDecision::ApproveForHumanReview
        } else {
            EvalDecision::Reject
        },
        loop_evidence: Some(EvalLoopEvidence {
            schema_version: 1,
            run_id: approved.run_id.clone(),
            ticket_id: approved.ticket_id.clone(),
            ticket_digest: approved.input_digests.ticket.clone(),
            eval_config: ArtifactReference {
                path: "inputs/eval-config.json".to_string(),
                digest: eval_config_digest.clone(),
            },
            candidate_diff: approval.candidate_diff.clone(),
            starting_head: approval.starting_head.clone(),
            human_approval_digest: canonical_sha256_digest(approval).unwrap(),
            policy_decision_digest: approval.policy_decision_digest.clone(),
            testing_evidence: testing_reference.clone(),
        }),
    };
    let report_reference = ArtifactReference {
        path: "artifacts/08-eval-report.json".to_string(),
        digest: canonical_sha256_digest(&report).unwrap(),
    };
    write_private_run_fixture(
        workspace,
        &report_reference.path,
        canonical_json_bytes(&report).unwrap(),
    );

    let mut run = approved.clone();
    run.status = if passed {
        seaf_core::LoopStatus::EvalPassed
    } else {
        seaf_core::LoopStatus::Failed
    };
    run.current_step = LoopStepName::EvalReport;
    let terminal_status = if passed {
        seaf_core::LoopStepStatus::Passed
    } else {
        seaf_core::LoopStepStatus::Failed
    };
    let testing = run
        .steps
        .iter_mut()
        .find(|step| step.name == LoopStepName::Testing)
        .unwrap();
    testing.status = terminal_status;
    testing.artifact_path = Some(testing_reference.path);
    testing.artifact_digest = Some(testing_reference.digest);
    let report = run
        .steps
        .iter_mut()
        .find(|step| step.name == LoopStepName::EvalReport)
        .unwrap();
    report.status = terminal_status;
    report.artifact_path = Some(report_reference.path);
    report.artifact_digest = Some(report_reference.digest);
    run.eval_report_path = report.artifact_path.clone();
    run.updated_at = approved
        .human_approval
        .as_ref()
        .unwrap()
        .approved_at
        .clone();
    run
}

fn publish_evaluation_recovery_v2(
    workspace: &LoopWorkspace,
    approved: &seaf_core::LoopRun,
    mut final_run: seaf_core::LoopRun,
) -> seaf_core::LoopRun {
    let recovery_id = approved
        .latest_recovery
        .as_ref()
        .map_or(1, |reference| reference.recovery_id + 1);
    let actor = "reviewer@example.invalid".to_string();
    let reason = "adopt complete interrupted evaluation".to_string();
    let created_at = approved
        .human_approval
        .as_ref()
        .unwrap()
        .approved_at
        .clone();
    let testing = final_run
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::Testing)
        .unwrap();
    let testing_reference = ArtifactReference {
        path: testing.artifact_path.clone().unwrap(),
        digest: testing.artifact_digest.clone().unwrap(),
    };
    let testing_evidence: TestingEvidence = serde_json::from_slice(
        &fs::read(workspace.run_directory().join(&testing_reference.path)).unwrap(),
    )
    .unwrap();
    let intent_path = testing_evidence
        .execution_intent
        .as_ref()
        .map_or("artifacts/07-testing.execution-intent.json", |value| {
            value.path.as_str()
        });
    let intent_reference = ArtifactReference {
        path: intent_path.to_string(),
        digest: format!(
            "{:x}",
            Sha256::digest(fs::read(workspace.run_directory().join(intent_path)).unwrap())
        ),
    };
    let intent_value: serde_json::Value =
        serde_json::from_slice(&fs::read(workspace.run_directory().join(intent_path)).unwrap())
            .unwrap();
    let source_worktree_state_digest = intent_value
        .get("source_worktree_state_digest")
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| "a".repeat(64), str::to_string);
    let report = final_run
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::EvalReport)
        .unwrap();
    let report_reference = ArtifactReference {
        path: report.artifact_path.clone().unwrap(),
        digest: report.artifact_digest.clone().unwrap(),
    };
    let source = EvaluationRecoverySourceRunV2 {
        schema_version: EVALUATION_RECOVERY_SCHEMA_VERSION,
        recovery_id,
        actor: actor.clone(),
        reason: reason.clone(),
        created_at: created_at.clone(),
        run: approved.clone(),
        evaluation_prefix: EvaluationPrefixAuthorityV1 {
            evaluation_attempt: 1,
            spelling: if testing_evidence.schema_version == 1 {
                EvaluationPrefixSpellingV1::FixedV1
            } else {
                EvaluationPrefixSpellingV1::IndexedV2
            },
            execution_intent: intent_reference.clone(),
            testing_evidence: testing_reference.clone(),
            eval_report: Some(report_reference.clone()),
        },
    };
    let source_path = format!("artifacts/recovery-{recovery_id:03}.source-run.json");
    let source_bytes = canonical_json_bytes(&source).unwrap();
    let source_reference = ArtifactReference {
        path: source_path.clone(),
        digest: format!("{:x}", Sha256::digest(&source_bytes)),
    };
    write_private_run_fixture(workspace, source_path, source_bytes);
    let recovery_path = format!("artifacts/recovery-{recovery_id:03}.json");
    let zero_reference = RecoveryReference {
        recovery_id,
        artifact: ArtifactReference {
            path: recovery_path.clone(),
            digest: "0".repeat(64),
        },
    };
    final_run.latest_recovery = Some(zero_reference);
    let projection_digest = canonical_sha256_digest(&final_run).unwrap();
    let candidate = approved.candidate_workspace.as_ref().unwrap();
    let recovery = EvaluationRecoveryAttemptV2 {
        schema_version: EVALUATION_RECOVERY_SCHEMA_VERSION,
        recovery_id,
        run_id: approved.run_id.clone(),
        action: EvaluationRecoveryAction::AdoptApprovedEvaluation,
        step: LoopStepName::Testing,
        actor,
        reason,
        created_at,
        source_run: source_reference,
        source_run_digest: canonical_sha256_digest(approved).unwrap(),
        input_digests: approved.input_digests.clone(),
        candidate_state_digest: canonical_sha256_digest(candidate).unwrap(),
        candidate_head: candidate.candidate_head.clone(),
        candidate_tree: candidate.candidate_tree.clone(),
        candidate_diff_digest: candidate.candidate_diff_digest.clone(),
        source_worktree_state_digest,
        evaluation_attempt: 1,
        execution_intent: intent_reference,
        testing_evidence: testing_reference,
        eval_report: report_reference,
        report_disposition: EvaluationRecoveryReportDisposition::VerifyExisting,
        previous_recovery: approved.latest_recovery.clone(),
        previous_provider_head: approved.provider_exchange_records.last().cloned(),
        expected_final_projection_digest: projection_digest,
    };
    let recovery_bytes = canonical_json_bytes(&recovery).unwrap();
    let recovery_reference = RecoveryReference {
        recovery_id,
        artifact: ArtifactReference {
            path: recovery_path.clone(),
            digest: format!("{:x}", Sha256::digest(&recovery_bytes)),
        },
    };
    write_private_run_fixture(workspace, recovery_path, recovery_bytes);
    final_run.latest_recovery = Some(recovery_reference);
    final_run
}

fn publish_indexed_final_eval_artifacts(
    workspace: &LoopWorkspace,
    approved: &seaf_core::LoopRun,
    passed: bool,
) -> seaf_core::LoopRun {
    publish_indexed_final_eval_artifacts_schema(workspace, approved, passed, 3)
}

fn publish_indexed_final_eval_artifacts_v2(
    workspace: &LoopWorkspace,
    approved: &seaf_core::LoopRun,
    passed: bool,
) -> seaf_core::LoopRun {
    publish_indexed_final_eval_artifacts_schema(workspace, approved, passed, 2)
}

fn publish_indexed_final_eval_artifacts_schema(
    workspace: &LoopWorkspace,
    approved: &seaf_core::LoopRun,
    passed: bool,
    intent_schema: u32,
) -> seaf_core::LoopRun {
    let fixed = publish_final_eval_artifacts(workspace, approved, passed);
    let fixed_testing_path = "artifacts/07-testing.json";
    let fixed_report_path = "artifacts/08-eval-report.json";
    let mut testing: TestingEvidence = serde_json::from_slice(
        &fs::read(workspace.run_directory().join(fixed_testing_path)).unwrap(),
    )
    .unwrap();
    let mut report: EvalReport = serde_json::from_slice(
        &fs::read(workspace.run_directory().join(fixed_report_path)).unwrap(),
    )
    .unwrap();
    let stdout_path = "artifacts/07-testing.attempt-001.check-001.stdout.log";
    let stderr_path = "artifacts/07-testing.attempt-001.check-001.stderr.log";
    fs::rename(
        workspace
            .run_directory()
            .join("artifacts/07-testing.check-001.stdout.log"),
        workspace.run_directory().join(stdout_path),
    )
    .unwrap();
    fs::rename(
        workspace
            .run_directory()
            .join("artifacts/07-testing.check-001.stderr.log"),
        workspace.run_directory().join(stderr_path),
    )
    .unwrap();
    for check in [&mut testing.checks[0], &mut report.checks[0]] {
        check.stdout_path = Some(stdout_path.to_string());
        check.stderr_path = Some(stderr_path.to_string());
    }
    let candidate = approved.candidate_workspace.as_ref().unwrap();
    let eval_config: serde_json::Value = serde_json::from_slice(
        &fs::read(workspace.run_directory().join("inputs/eval-config.json")).unwrap(),
    )
    .unwrap();
    let expected_name = eval_config["evals"]["required"][0]["name"]
        .as_str()
        .unwrap()
        .to_string();
    testing.checks[0].name = expected_name.clone();
    report.checks[0].name = expected_name;
    let planned_checks = if intent_schema == 2 {
        eval_config["evals"]["required"].clone()
    } else {
        serde_json::Value::Array(planned_check_projections(&eval_config))
    };
    let intent = serde_json::json!({
        "schema_version": intent_schema,
        "evaluation_attempt": 1,
        "run_id": approved.run_id,
        "approved_run_digest": canonical_sha256_digest(approved).unwrap(),
        "input_digests": approved.input_digests,
        "ticket": {
            "path": "inputs/ticket.json",
            "digest": approved.input_digests.ticket,
        },
        "eval_config": {
            "path": "inputs/eval-config.json",
            "digest": approved.input_digests.eval_config,
        },
        "candidate_state_digest": canonical_sha256_digest(candidate).unwrap(),
        "candidate_diff": approved.human_approval.as_ref().unwrap().candidate_diff,
        "source_worktree_state_digest": source_worktree_authority_digest(approved),
        "recovery": null,
        "planned_checks": planned_checks,
    });
    let intent_path = "artifacts/07-testing.attempt-001.execution-intent.json";
    let intent_bytes = canonical_json_bytes(&intent).unwrap();
    write_private_run_fixture(workspace, intent_path, &intent_bytes);
    let intent_reference = ArtifactReference {
        path: intent_path.to_string(),
        digest: format!("{:x}", Sha256::digest(&intent_bytes)),
    };
    testing.schema_version = 2;
    testing.evaluation_attempt = Some(1);
    testing.recovery = Some(None);
    testing.execution_intent = Some(intent_reference);
    let testing_path = "artifacts/07-testing.attempt-001.json";
    let testing_bytes = canonical_json_bytes(&testing).unwrap();
    write_private_run_fixture(workspace, testing_path, &testing_bytes);
    let testing_reference = ArtifactReference {
        path: testing_path.to_string(),
        digest: format!("{:x}", Sha256::digest(&testing_bytes)),
    };
    report.loop_evidence.as_mut().unwrap().testing_evidence = testing_reference.clone();
    let report_path = "artifacts/08-eval-report.attempt-001.json";
    let report_bytes = canonical_json_bytes(&report).unwrap();
    write_private_run_fixture(workspace, report_path, &report_bytes);
    let report_reference = ArtifactReference {
        path: report_path.to_string(),
        digest: format!("{:x}", Sha256::digest(&report_bytes)),
    };
    for path in [
        "artifacts/07-testing.execution-intent.json",
        fixed_testing_path,
        fixed_report_path,
    ] {
        fs::remove_file(workspace.run_directory().join(path)).unwrap();
    }
    let mut run = fixed;
    let testing_step = run
        .steps
        .iter_mut()
        .find(|step| step.name == LoopStepName::Testing)
        .unwrap();
    testing_step.artifact_path = Some(testing_reference.path);
    testing_step.artifact_digest = Some(testing_reference.digest);
    let report_step = run
        .steps
        .iter_mut()
        .find(|step| step.name == LoopStepName::EvalReport)
        .unwrap();
    report_step.artifact_path = Some(report_reference.path.clone());
    report_step.artifact_digest = Some(report_reference.digest);
    run.eval_report_path = Some(report_reference.path);
    run
}

fn rewrite_evaluation_recovery(
    workspace: &LoopWorkspace,
    run: &mut seaf_core::LoopRun,
    mutate: impl FnOnce(&mut EvaluationRecoveryAttemptV2),
) {
    let reference = run.latest_recovery.as_ref().unwrap();
    let path = reference.artifact.path.clone();
    let mut recovery: EvaluationRecoveryAttemptV2 =
        serde_json::from_slice(&fs::read(workspace.run_directory().join(&path)).unwrap()).unwrap();
    mutate(&mut recovery);
    let bytes = canonical_json_bytes(&recovery).unwrap();
    write_private_run_fixture(workspace, &path, &bytes);
    run.latest_recovery.as_mut().unwrap().artifact.digest = format!("{:x}", Sha256::digest(&bytes));
}

fn rewrite_evaluation_recovery_source(
    workspace: &LoopWorkspace,
    run: &mut seaf_core::LoopRun,
    mutate: impl FnOnce(&mut EvaluationRecoverySourceRunV2),
) {
    let recovery_reference = run.latest_recovery.as_ref().unwrap();
    let recovery_path = recovery_reference.artifact.path.clone();
    let mut recovery: EvaluationRecoveryAttemptV2 =
        serde_json::from_slice(&fs::read(workspace.run_directory().join(&recovery_path)).unwrap())
            .unwrap();
    let mut source: EvaluationRecoverySourceRunV2 = serde_json::from_slice(
        &fs::read(workspace.run_directory().join(&recovery.source_run.path)).unwrap(),
    )
    .unwrap();
    mutate(&mut source);
    let source_bytes = canonical_json_bytes(&source).unwrap();
    write_private_run_fixture(workspace, &recovery.source_run.path, &source_bytes);
    recovery.source_run.digest = format!("{:x}", Sha256::digest(&source_bytes));
    recovery.source_run_digest = canonical_sha256_digest(&source.run).unwrap();
    let recovery_bytes = canonical_json_bytes(&recovery).unwrap();
    write_private_run_fixture(workspace, recovery_path, &recovery_bytes);
    run.latest_recovery.as_mut().unwrap().artifact.digest =
        format!("{:x}", Sha256::digest(&recovery_bytes));
}

fn publish_report_variant(
    workspace: &LoopWorkspace,
    base_run: &seaf_core::LoopRun,
    report: &EvalReport,
    label: &str,
    canonical: bool,
) -> seaf_core::LoopRun {
    let path = format!("artifacts/08-eval-report-{label}.json");
    let bytes = if canonical {
        canonical_json_bytes(report).unwrap()
    } else {
        let mut bytes = serde_json::to_vec_pretty(report).unwrap();
        bytes.push(b'\n');
        bytes
    };
    let digest = if canonical {
        canonical_sha256_digest(report).unwrap()
    } else {
        format!("{:x}", Sha256::digest(&bytes))
    };
    write_private_run_fixture(workspace, &path, bytes);
    let mut run = base_run.clone();
    let record = run
        .steps
        .iter_mut()
        .find(|record| record.name == LoopStepName::EvalReport)
        .unwrap();
    record.artifact_path = Some(path.clone());
    record.artifact_digest = Some(digest);
    run.eval_report_path = Some(path);
    run
}

fn replace_testing_summary(workspace: &LoopWorkspace, run: &mut seaf_core::LoopRun, summary: &str) {
    let testing_step = run
        .steps
        .iter_mut()
        .find(|step| step.name == LoopStepName::Testing)
        .unwrap();
    let testing_path = testing_step.artifact_path.clone().unwrap();
    let mut testing: TestingEvidence =
        serde_json::from_slice(&fs::read(workspace.run_directory().join(&testing_path)).unwrap())
            .unwrap();
    testing.checks[0].summary = Some(summary.to_string());
    let testing_bytes = canonical_json_bytes(&testing).unwrap();
    write_private_run_fixture(workspace, &testing_path, &testing_bytes);
    let testing_digest = format!("{:x}", Sha256::digest(&testing_bytes));
    testing_step.artifact_digest = Some(testing_digest.clone());

    let report_step = run
        .steps
        .iter_mut()
        .find(|step| step.name == LoopStepName::EvalReport)
        .unwrap();
    let report_path = report_step.artifact_path.clone().unwrap();
    let mut report: EvalReport =
        serde_json::from_slice(&fs::read(workspace.run_directory().join(&report_path)).unwrap())
            .unwrap();
    report.checks[0].summary = Some(summary.to_string());
    report
        .loop_evidence
        .as_mut()
        .unwrap()
        .testing_evidence
        .digest = testing_digest;
    let report_bytes = canonical_json_bytes(&report).unwrap();
    write_private_run_fixture(workspace, &report_path, &report_bytes);
    report_step.artifact_digest = Some(format!("{:x}", Sha256::digest(&report_bytes)));
}

fn write_raw_run(workspace: &LoopWorkspace, run: &seaf_core::LoopRun) {
    let mut bytes = serde_json::to_vec_pretty(run).unwrap();
    bytes.push(b'\n');
    fs::write(workspace.run_file(), bytes).unwrap();
}

fn directory_snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

fn approved_eval_log_bytes(workspace: &LoopWorkspace) -> Vec<(String, Vec<u8>)> {
    let mut logs = fs::read_dir(workspace.run_directory().join("artifacts"))
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            (name.starts_with("07-testing") && name.ends_with(".log"))
                .then(|| (name, fs::read(entry.path()).unwrap()))
        })
        .collect::<Vec<_>>();
    logs.sort_by(|left, right| left.0.cmp(&right.0));
    logs
}

fn source_worktree_authority_digest(run: &seaf_core::LoopRun) -> String {
    let root = PathBuf::from(
        &run.candidate_workspace
            .as_ref()
            .unwrap()
            .source_worktree_root,
    );
    let git_bytes = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    let authority = serde_json::json!({
        "canonical_root": fs::canonicalize(&root).unwrap(),
        "head": git(&root, &["rev-parse", "HEAD"]),
        "staged_diff_digest": hex::encode(Sha256::digest(git_bytes(&[
            "diff", "--cached", "--binary", "--full-index", "--no-ext-diff",
            "--no-textconv", "HEAD", "--",
        ]))),
        "tracked_worktree_diff_digest": hex::encode(Sha256::digest(git_bytes(&[
            "diff", "--binary", "--full-index", "--no-ext-diff", "--no-textconv", "--",
        ]))),
        "untracked": [],
    });
    canonical_sha256_digest(&authority).unwrap()
}

fn fixture(run_id: &str) -> Fixture {
    fixture_with_eval_mode(run_id, FixtureEvalMode::Disabled)
}

#[derive(Clone, Copy)]
enum FixtureEvalMode {
    Disabled,
    Cargo,
    DriftAfterFirstCheck,
    Marker,
    CapacityAfterFirstCheck,
    Blocking,
    ProviderSecret,
    ProviderRequestSecret,
    ProviderEnvelopeSecret(&'static str),
    StructuralSecret,
    EvaluationMarkerEnvelopeSecret,
    EvaluationReportEnvelopeSecret(&'static str),
}

const PROVIDER_SECRET_FIXTURE: &str = "configured-provider-secret-value";

fn fixture_with_eval_mode(run_id: &str, eval_mode: FixtureEvalMode) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let runs_root = temp.path().join("runs");
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    git_ok(&source, &["init", "-q"]);
    git_ok(&source, &["config", "user.email", "test@example.com"]);
    git_ok(&source, &["config", "user.name", "SEAF Test"]);
    fs::create_dir(source.join("src")).unwrap();
    fs::write(source.join("src/lib.rs"), "pub fn existing() {}\n").unwrap();
    if matches!(eval_mode, FixtureEvalMode::ProviderRequestSecret) {
        fs::write(
            source.join("src/provider-context.txt"),
            PROVIDER_SECRET_FIXTURE,
        )
        .unwrap();
    }
    if matches!(eval_mode, FixtureEvalMode::DriftAfterFirstCheck) {
        fs::write(
            source.join("drift-authority.sh"),
            "#!/bin/sh\nprintf substituted > \"$1\"\n",
        )
        .unwrap();
    }
    if matches!(
        eval_mode,
        FixtureEvalMode::Marker | FixtureEvalMode::StructuralSecret
    ) {
        fs::write(
            source.join("eval-marker.sh"),
            "#!/bin/sh\nprintf spawned > \"$1\"\n",
        )
        .unwrap();
    }
    if matches!(eval_mode, FixtureEvalMode::CapacityAfterFirstCheck) {
        fs::write(
            source.join("eval-capacity-first.sh"),
            "#!/bin/sh\nprintf first > \"$1\"\numask 077\ntruncate -s 1 \"$2\"\nchmod 600 \"$2\"\n",
        )
        .unwrap();
        fs::write(
            source.join("eval-capacity-second.sh"),
            "#!/bin/sh\nprintf second > \"$1\"\n",
        )
        .unwrap();
    }
    if matches!(eval_mode, FixtureEvalMode::Blocking) {
        fs::write(
            source.join("eval-blocking.sh"),
            "#!/bin/sh\nprintf started > \"$1\"\nwhile [ ! -f \"$2\" ]; do sleep 0.01; done\n",
        )
        .unwrap();
    }
    git_ok(&source, &["add", "."]);
    git_ok(&source, &["commit", "-qm", "initial"]);
    let mut ticket = ticket();
    match eval_mode {
        FixtureEvalMode::Disabled => {}
        FixtureEvalMode::Cargo => {
            ticket.autonomy.allow_shell_commands = vec!["true".into()];
        }
        FixtureEvalMode::DriftAfterFirstCheck => {
            ticket.autonomy.allow_shell_commands = vec!["sh".into(), "true".into()];
        }
        FixtureEvalMode::Marker => {
            ticket.autonomy.allow_shell_commands = vec!["sh".into()];
        }
        FixtureEvalMode::CapacityAfterFirstCheck => {
            ticket.autonomy.allow_shell_commands = vec!["sh".into()];
        }
        FixtureEvalMode::Blocking => {
            ticket.autonomy.allow_shell_commands = vec!["sh".into()];
        }
        FixtureEvalMode::ProviderSecret => {
            ticket.autonomy.allow_shell_commands = vec!["true".into()];
        }
        FixtureEvalMode::ProviderRequestSecret => {
            ticket.autonomy.allow_shell_commands = vec!["true".into()];
            ticket
                .context
                .relevant_files
                .push("src/provider-context.txt".to_string());
        }
        FixtureEvalMode::ProviderEnvelopeSecret(_) => {
            ticket.autonomy.allow_shell_commands = vec!["true".into()];
        }
        FixtureEvalMode::StructuralSecret => {
            ticket.autonomy.allow_shell_commands = vec!["sh".into()];
        }
        FixtureEvalMode::EvaluationMarkerEnvelopeSecret => {
            ticket.autonomy.allow_shell_commands = vec!["true".into()];
        }
        FixtureEvalMode::EvaluationReportEnvelopeSecret(_) => {
            ticket.autonomy.allow_shell_commands = vec!["true".into()];
        }
    }
    let policy = policy();
    let config = serde_json::json!({"policy_path":"seaf.policy.json"});
    let repository = serde_json::json!({"source":source.canonicalize().unwrap()});
    let drift_command = format!(
        "sh drift-authority.sh {}",
        runs_root.join(run_id).join("inputs/policy.json").display()
    );
    let eval_yaml = match eval_mode {
        FixtureEvalMode::Disabled => {
            "evals:\n  allow_commands: []\n  required:\n    - name: tests\n      command: cargo test\n"
                .to_string()
        }
        FixtureEvalMode::Cargo => {
            "evals:\n  allow_commands: [true]\n  required:\n    - name: tests\n      command: true\n"
                .to_string()
        }
        FixtureEvalMode::DriftAfterFirstCheck => format!(
            "evals:\n  allow_commands: [sh, true]\n  required:\n    - name: mutate_authority\n      command: {drift_command}\n    - name: must_not_spawn\n      command: true\n"
        ),
        FixtureEvalMode::Marker => format!(
            "evals:\n  allow_commands: [sh]\n  required:\n    - name: marker\n      command: sh eval-marker.sh {}\n",
            temp.path().join("eval-spawned.marker").display()
        ),
        FixtureEvalMode::CapacityAfterFirstCheck => format!(
            "evals:\n  allow_commands: [sh]\n  required:\n    - name: consume_capacity\n      command: sh eval-capacity-first.sh {} {}\n    - name: must_not_spawn\n      command: sh eval-capacity-second.sh {}\n",
            temp.path().join("eval-first.marker").display(),
            runs_root.join(run_id).join("lost-capacity.raw").display(),
            temp.path().join("eval-second.marker").display(),
        ),
        FixtureEvalMode::Blocking => format!(
            "evals:\n  allow_commands: [sh]\n  required:\n    - name: blocking\n      command: sh eval-blocking.sh {} {}\n      timeout_ms: 10000\n",
            temp.path().join("eval-blocking.started").display(),
            temp.path().join("eval-blocking.release").display(),
        ),
        FixtureEvalMode::ProviderSecret | FixtureEvalMode::ProviderRequestSecret => format!(
            "evals:\n  allow_commands: [true]\n  required:\n    - name: tests\n      command: true\n      env:\n        API_TOKEN: {PROVIDER_SECRET_FIXTURE}\n"
        ),
        FixtureEvalMode::ProviderEnvelopeSecret(secret) => format!(
            "evals:\n  allow_commands: [true]\n  required:\n    - name: tests\n      command: true\n      env:\n        API_TOKEN: {secret:?}\n"
        ),
        FixtureEvalMode::StructuralSecret => format!(
            "evals:\n  allow_commands: [sh]\n  required:\n    - name: structural\n      command: sh eval-marker.sh {} {PROVIDER_SECRET_FIXTURE}\n      env:\n        API_TOKEN: {PROVIDER_SECRET_FIXTURE}\n",
            temp.path().join("eval-spawned.marker").display(),
        ),
        FixtureEvalMode::EvaluationMarkerEnvelopeSecret => {
            "evals:\n  allow_commands: [true]\n  required:\n    - name: tests\n      command: true\n      env:\n        API_TOKEN: prefix[REDACTED]suffix\n"
                .to_string()
        }
        FixtureEvalMode::EvaluationReportEnvelopeSecret(secret) => format!(
            "evals:\n  allow_commands: [true]\n  required:\n    - name: tests\n      command: true\n      env:\n        API_TOKEN: '{secret}'\n"
        ),
    };
    let eval_config = seaf_core::parse_eval_config(&eval_yaml).unwrap();
    let ticket_bytes = canonical_json_bytes(&ticket).unwrap();
    let policy_bytes = canonical_json_bytes(&policy).unwrap();
    let config_bytes = canonical_json_bytes(&config).unwrap();
    let repository_bytes = canonical_json_bytes(&repository).unwrap();
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
        &AuthoritativeRunInputSnapshots {
            ticket: ticket_bytes.clone(),
            provider_ticket: ticket_bytes.clone(),
            policy: policy_bytes.clone(),
            config: config_bytes.clone(),
            repository: repository_bytes.clone(),
            eval_config: canonical_json_bytes(&eval_config).unwrap(),
        },
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
            eval_config: canonical_json_bytes(&eval_config).unwrap(),
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

struct UnauthenticatedOutputReview;

impl StepRunner for UnauthenticatedOutputReview {
    fn step_request(&mut self, step: LoopStepName) -> Result<String, seaf_loop::RunnerError> {
        assert_eq!(step, LoopStepName::OutputReview);
        Ok("unauthenticated review".to_string())
    }

    fn run_step(
        &mut self,
        step: LoopStepName,
        _request: &str,
    ) -> Result<seaf_loop::StepOutput, seaf_loop::RunnerError> {
        assert_eq!(step, LoopStepName::OutputReview);
        let mut output = seaf_loop::StepOutput::completed("unauthenticated pass")
            .with_artifact(seaf_loop::ArtifactContent::markdown("not authenticated"));
        output.status = seaf_core::LoopStepStatus::Passed;
        Ok(output)
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

#[cfg(unix)]
fn write_private_run_fixture(
    workspace: &LoopWorkspace,
    relative: impl AsRef<Path>,
    bytes: impl AsRef<[u8]>,
) {
    use std::io::Write;

    let path = workspace.run_directory().join(relative);
    let bytes = bytes.as_ref();
    if path.exists() {
        fs::write(path, bytes).unwrap();
        return;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(path).unwrap();
    file.write_all(bytes).unwrap();
}

#[cfg(not(unix))]
fn write_private_run_fixture(
    _workspace: &LoopWorkspace,
    _relative: impl AsRef<Path>,
    _bytes: impl AsRef<[u8]>,
) {
    panic!("private loop workspace tests require Unix")
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
