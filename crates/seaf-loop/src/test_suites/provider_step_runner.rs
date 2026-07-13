use std::path::Path;

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, LoopInputDigests, LoopRun, LoopStatus,
    LoopStepName, LoopStepStatus, Policy, ProviderExchangeOutcome, ProviderExchangePhase,
    ProviderExchangeRecord, TicketContext, TicketSpec, TicketStatus,
};
use seaf_loop::{
    CommandOutput, ContextLimits, ContextManifest, ContextPackRequest, LoopRunner,
    LoopRunnerConfig, PatchCommand, PatchCommandRunner, PatchDecisionKind, PatchGateError,
    PolicyDecision, ProviderPatchGateConfig, ProviderStepRunner, Role, StepRunner,
    UNTRUSTED_CONTEXT_MARKER,
};
use seaf_models::{FakeProvider, ModelError, ModelProvider, ModelRequest, ModelResponse};
use serde_json::json;
use sha2::{Digest, Sha256};

fn fixture(name: &str) -> &'static str {
    match name {
        "research.valid.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/research.valid.json"
            ))
        }
        "research.invalid_missing_status.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/research.invalid_missing_status.json"
            ))
        }
        "analyzer.valid.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/analyzer.valid.json"
            ))
        }
        "allowed-doc.diff" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/patches/allowed-doc.diff"
            ))
        }
        _ => panic!("unknown fixture: {name}"),
    }
}

#[test]
fn fresh_provider_runner_cannot_bypass_isolated_candidate_initialization() {
    let temp = tempfile::tempdir().unwrap();
    let ticket = ticket();
    let provider = FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let mut runner =
        ProviderStepRunner::new(&provider, "fake-model", 30_000).with_ticket(ticket.clone());

    let error = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            temp.path().join("runs"),
            "legacy-provider-bypass",
            &ticket,
            "fake",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut runner,
    )
    .expect_err("fresh provider execution must use the isolated initializer");
    assert!(error.to_string().contains("start a new isolated run"), "{error}");
    assert!(provider.requests().unwrap().is_empty());
}

#[test]
fn terminal_legacy_provider_run_rejects_before_resume_or_rerun_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let runs_root = temp.path().join("runs");
    let ticket = ticket();
    let workspace = seaf_loop::LoopWorkspace::create(&runs_root, "terminal-legacy").unwrap();
    let mut run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "terminal-legacy".to_string(),
        ticket_id: ticket.ticket_id.clone(),
        goal_id: ticket.goal_id.clone(),
        provider: "fake".to_string(),
        model: "fake-model".to_string(),
        input_digests: test_input_digests_for(&ticket),
    });
    run.status = LoopStatus::Completed;
    for step in &mut run.steps {
        step.status = LoopStepStatus::Completed;
    }
    seaf_loop::state::save_run(&workspace, &run).unwrap();
    let before_run = std::fs::read(workspace.run_file()).unwrap();
    let before_log = std::fs::read(workspace.run_directory().join("log.md")).unwrap();
    let provider = FakeProvider::new(Vec::new());
    let mut runner =
        ProviderStepRunner::new(&provider, "fake-model", 30_000).with_ticket(ticket);

    let error = LoopRunner::resume(&runs_root, "terminal-legacy", &mut runner)
        .expect_err("terminal legacy provider history cannot enter resume/rerun");
    assert!(error.to_string().contains("start a new isolated run"), "{error}");
    assert_eq!(std::fs::read(workspace.run_file()).unwrap(), before_run);
    assert_eq!(
        std::fs::read(workspace.run_directory().join("log.md")).unwrap(),
        before_log
    );
    assert!(provider.requests().unwrap().is_empty());
}

#[test]
fn provider_step_runner_sends_role_request_and_maps_common_passed_status() {
    let provider = FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let mut runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 12_345);

    let request_audit = runner
        .step_request(LoopStepName::Research)
        .expect("research request");
    let output = runner
        .run_step(LoopStepName::Research, &request_audit)
        .expect("research step");

    assert_eq!(output.status, LoopStepStatus::Completed);
    assert_eq!(output.response, fixture("research.valid.json"));

    let requests = provider.requests().expect("provider requests");
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.model, "fake-model");
    assert_eq!(request.system, Role::Researcher.system_prompt());
    assert_eq!(
        request.response_schema,
        Some(Role::Researcher.response_schema())
    );
    assert_eq!(request.temperature, 0.0);
    assert_eq!(request.timeout_ms, 12_345);
    assert_eq!(request.messages.len(), 1);
    assert_eq!(
        request.messages[0].content,
        request_audit_user_prompt(&request_audit)
    );
}

#[test]
fn research_request_contains_exact_ticket_and_all_run_input_digests() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();
    let input_digests = test_input_digests();
    let provider = FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000).with_ticket(ticket.clone());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "exact-research-request",
            &ticket,
            "fake-provider",
            "fake-model",
            input_digests.clone(),
        ),
        &mut step_runner,
    )
    .expect("start loop");

    loop_runner.run_next_step().expect("run research");

    let requests = provider.requests().expect("provider requests");
    assert_eq!(
        requests.len(),
        1,
        "initial execution makes one provider call"
    );
    assert_eq!(
        loop_runner.run().provider_exchange_records.len(),
        2,
        "fresh live execution durably records the request and response"
    );
    let prompt: serde_json::Value =
        serde_json::from_str(&requests[0].messages[0].content).expect("structured role prompt");
    assert_eq!(prompt["run_id"], "exact-research-request");
    assert_eq!(prompt["input_digests"], json!(input_digests));
    assert_eq!(prompt["ticket"], json!(ticket));
    assert_eq!(prompt["prerequisites"], json!({}));
}

#[test]
fn prepared_provider_run_requires_effective_ticket_before_workspace_or_provider_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let repo = temp_dir.path().join("repo");
    std::fs::create_dir_all(&runs_root).expect("runs root");
    std::fs::create_dir_all(repo.join("src")).expect("source directory");
    std::fs::write(repo.join("src/lib.rs"), "pub fn missing_ticket() {}\n").expect("source file");
    let authoritative = ticket_with_context(vec!["src/lib.rs"], Vec::new());
    let before = read_tree_bytes(&runs_root);
    let provider = FakeProvider::new(Vec::new());
    let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_context_pack_request(context_request(&repo, &authoritative, Vec::new()));

    let error = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "missing-prepared-ticket",
            &authoritative,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&authoritative),
        ),
        &mut step_runner,
    )
    .expect_err("prepared provider run without ticket must fail closed");

    assert!(error.to_string().contains("effective ticket"), "{error}");
    assert_eq!(read_tree_bytes(&runs_root), before);
    assert!(provider.requests().expect("provider requests").is_empty());
}

#[test]
fn prepared_provider_run_rejects_substituted_ticket_authority_before_any_mutation() {
    for mismatch in [
        PreparedTicketMismatch::TicketId,
        PreparedTicketMismatch::GoalId,
        PreparedTicketMismatch::Digest,
    ] {
        assert_prepared_ticket_mismatch_fails_closed(mismatch);
    }
}

#[test]
fn early_role_requests_chain_only_exact_validated_prerequisites_and_persist_canonical_artifacts() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();
    let input_digests = test_input_digests();
    let responses = [
        fixture("research.valid.json").to_string(),
        fixture("analyzer.valid.json").to_string(),
        spec_writer_response(),
        spec_review_approved_response(),
    ];
    let provider = FakeProvider::new(
        responses
            .iter()
            .map(|response| Ok(model_response(response)))
            .collect(),
    );
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000).with_ticket(ticket.clone());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "exact-early-chain",
            &ticket,
            "fake-provider",
            "fake-model",
            input_digests.clone(),
        ),
        &mut step_runner,
    )
    .expect("start loop");

    for _ in 0..4 {
        assert!(loop_runner.run_next_step().expect("run early step"));
    }
    drop(loop_runner);

    let response_values: Vec<serde_json::Value> = responses
        .iter()
        .map(|response| serde_json::from_str(response).expect("response JSON"))
        .collect();
    let requests = provider.requests().expect("provider requests");
    assert_eq!(requests.len(), 4);
    let prompts: Vec<serde_json::Value> = requests
        .iter()
        .map(|request| serde_json::from_str(&request.messages[0].content).expect("role input JSON"))
        .collect();
    for prompt in &prompts {
        assert_eq!(prompt["run_id"], "exact-early-chain");
        assert_eq!(prompt["input_digests"], json!(input_digests));
        assert_eq!(prompt["ticket"], json!(ticket));
    }
    assert_eq!(prompts[0]["prerequisites"], json!({}));
    assert_eq!(
        prompts[1]["prerequisites"],
        json!({ "research": response_values[0] })
    );
    assert_eq!(
        prompts[2]["prerequisites"],
        json!({ "research": response_values[0], "analysis": response_values[1] })
    );
    assert_eq!(
        prompts[3]["prerequisites"],
        json!({ "proposed_spec": response_values[2] })
    );

    let run_dir = runs_root.join("exact-early-chain");
    let persisted = read_run(&run_dir);
    for (index, (step, role)) in [
        (LoopStepName::Research, Role::Researcher),
        (LoopStepName::Analysis, Role::Analyzer),
        (LoopStepName::SpecCreation, Role::SpecWriter),
        (LoopStepName::SpecReview, Role::SpecReviewer),
    ]
    .into_iter()
    .enumerate()
    {
        let record = persisted
            .steps
            .iter()
            .find(|record| record.name == step)
            .expect("step record");
        let path = record.artifact_path.as_deref().expect("artifact path");
        let bytes = std::fs::read(run_dir.join(path)).expect("artifact bytes");
        let artifact: serde_json::Value = serde_json::from_slice(&bytes).expect("artifact JSON");
        assert_eq!(artifact["run_id"], "exact-early-chain");
        assert_eq!(artifact["step"], json!(step));
        assert_eq!(artifact["role"], json!(role));
        assert_eq!(artifact["response"], response_values[index]);
        assert_eq!(
            artifact["response_digest"],
            canonical_sha256_digest(&response_values[index]).expect("response digest")
        );
        assert_eq!(
            bytes,
            canonical_json_bytes(&artifact).expect("canonical artifact bytes")
        );
        assert_eq!(
            record.artifact_digest.as_deref(),
            Some(
                canonical_sha256_digest(&artifact)
                    .expect("artifact digest")
                    .as_str()
            )
        );
    }
}

#[test]
fn resumed_analysis_reuses_verified_research_artifact_without_unrelated_prerequisites() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();
    let start_provider =
        FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let mut start_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&start_provider, "fake-model", 30_000).with_ticket(ticket.clone());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "resume-exact-chain",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut start_runner,
    )
    .expect("start loop");
    loop_runner.run_next_step().expect("research");
    drop(loop_runner);

    let resume_provider =
        FakeProvider::new(vec![Ok(model_response(fixture("analyzer.valid.json")))]);
    let mut resume_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&resume_provider, "fake-model", 30_000).with_ticket(ticket.clone());
    let mut resumed = LoopRunner::resume(&runs_root, "resume-exact-chain", &mut resume_runner)
        .expect("resume verified role chain");
    resumed.run_next_step().expect("analysis");

    let requests = resume_provider.requests().expect("provider requests");
    let prompt: serde_json::Value =
        serde_json::from_str(&requests[0].messages[0].content).expect("role input JSON");
    let research: serde_json::Value =
        serde_json::from_str(fixture("research.valid.json")).expect("research JSON");
    assert_eq!(prompt["ticket"], json!(ticket));
    assert_eq!(prompt["prerequisites"], json!({ "research": research }));
}

#[test]
fn development_request_uses_exact_approved_spec_and_only_developer_repository_context() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    let source_content = "pub fn development_context() -> &'static str { \"needed\" }\n";
    std::fs::create_dir_all(repo.join("src")).expect("source directory");
    std::fs::write(repo.join("src/lib.rs"), source_content).expect("source file");
    let ticket = ticket_with_context(vec!["src/lib.rs"], Vec::new());
    let input_digests = test_input_digests_for(&ticket);
    let developer_blocked = json!({
        "role": "developer",
        "status": "blocked",
        "summary": "No patch was proposed.",
        "changed_files": [],
        "requires_human_review": false
    })
    .to_string();
    let provider = FakeProvider::new(vec![
        Ok(model_response(fixture("research.valid.json"))),
        Ok(model_response(fixture("analyzer.valid.json"))),
        Ok(model_response(&spec_writer_response())),
        Ok(model_response(&spec_review_approved_response())),
        Ok(model_response(&developer_blocked)),
    ]);
    let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repo, &ticket, Vec::new()));
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "exact-development-request",
            &ticket,
            "fake-provider",
            "fake-model",
            input_digests.clone(),
        ),
        &mut step_runner,
    )
    .expect("start loop");
    finish_steps_before_development(&mut loop_runner);

    loop_runner.run_next_step().expect("run development");
    drop(loop_runner);

    let run_dir = runs_root.join("exact-development-request");
    let persisted = read_run(&run_dir);
    let spec = persisted_step_artifact(&run_dir, &persisted, LoopStepName::SpecCreation);
    let approval = persisted_step_artifact(&run_dir, &persisted, LoopStepName::SpecReview);
    let requests = provider.requests().expect("provider requests");
    assert_eq!(requests.len(), 5);
    let prompt: serde_json::Value =
        serde_json::from_str(&requests[4].messages[0].content).expect("development role input");

    assert_eq!(prompt["run_id"], "exact-development-request");
    assert_eq!(prompt["input_digests"], json!(input_digests));
    assert_eq!(prompt["approved_spec"]["spec_creation"]["artifact"], spec.2);
    assert_eq!(
        prompt["approved_spec"]["spec_creation"]["artifact_path"],
        spec.0
    );
    assert_eq!(
        prompt["approved_spec"]["spec_creation"]["artifact_digest"],
        spec.1
    );
    assert_eq!(
        prompt["approved_spec"]["spec_review"]["artifact"],
        approval.2
    );
    assert_eq!(
        prompt["approved_spec"]["spec_review"]["artifact_path"],
        approval.0
    );
    assert_eq!(
        prompt["approved_spec"]["spec_review"]["artifact_digest"],
        approval.1
    );
    assert_eq!(
        prompt["approved_spec"]["spec_review"]["artifact"]["response"]["decision"],
        "approve_spec"
    );
    assert!(prompt.get("ticket").is_none());
    assert!(prompt.get("prerequisites").is_none());
    assert!(!prompt
        .to_string()
        .contains("Repository inspection is required"));
    assert!(!prompt
        .to_string()
        .contains("Provider integration is feasible"));
    assert!(prompt["repository_context"]
        .as_str()
        .expect("developer repository context")
        .contains(source_content));
}

#[test]
fn wrong_spec_reviewer_approval_fails_spec_review_and_never_reaches_development() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();
    let wrong_approval = json!({
        "role": "spec_reviewer",
        "decision": "approve_for_tests",
        "summary": "This is not the required spec approval decision.",
        "blocking_issues": [],
        "non_blocking_issues": []
    })
    .to_string();
    let provider = FakeProvider::new(vec![
        Ok(model_response(fixture("research.valid.json"))),
        Ok(model_response(fixture("analyzer.valid.json"))),
        Ok(model_response(&spec_writer_response())),
        Ok(model_response(&wrong_approval)),
    ]);
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000).with_ticket(ticket.clone());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "wrong-spec-approval",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut step_runner,
    )
    .expect("start loop");
    finish_steps_before_development(&mut loop_runner);
    let run_dir = runs_root.join("wrong-spec-approval");
    assert_eq!(loop_runner.run().status, LoopStatus::Failed);
    assert_eq!(
        step_status(loop_runner.run(), LoopStepName::SpecReview),
        LoopStepStatus::Failed
    );
    assert!(run_dir.join("artifacts/04-spec-review.json").is_file());
    assert!(!run_dir.join("prompts/05-development.prompt.md").exists());
    let before = read_tree_bytes(&run_dir);

    assert!(!loop_runner
        .run_next_step()
        .expect("terminal failed review stops the loop"));
    assert_eq!(read_tree_bytes(&run_dir), before);
    assert_eq!(provider.requests().expect("provider requests").len(), 4);
}

#[test]
fn output_review_uses_only_canonical_policy_gated_development_evidence() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    let source_content = "pub fn reviewer_must_not_see_initial_context() {}\n";
    std::fs::create_dir_all(repo.join("src")).expect("source directory");
    std::fs::write(repo.join("src/lib.rs"), source_content).expect("source file");
    let ticket = ticket_with_context(vec!["src/lib.rs"], Vec::new());
    let input_digests = test_input_digests_for(&ticket);
    let patch = fixture("allowed-doc.diff");
    let provider = FakeProvider::new(vec![
        Ok(model_response(fixture("research.valid.json"))),
        Ok(model_response(fixture("analyzer.valid.json"))),
        Ok(model_response(&spec_writer_response())),
        Ok(model_response(&spec_review_approved_response())),
        Ok(model_response(&developer_response(patch))),
        Ok(model_response(&output_review_approved_response())),
    ]);
    let mut patch_runner = RecordingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repo, &ticket, Vec::new()))
        .with_patch_gate(patch_gate_config(&repo, false, true), &mut patch_runner);
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "exact-output-review",
            &ticket,
            "fake-provider",
            "fake-model",
            input_digests.clone(),
        ),
        &mut step_runner,
    )
    .expect("start loop");
    finish_steps_before_development(&mut loop_runner);
    loop_runner.run_next_step().expect("development");
    loop_runner.run_next_step().expect("output review");
    drop(loop_runner);
    drop(step_runner);

    let run_dir = runs_root.join("exact-output-review");
    let persisted = read_run(&run_dir);
    let development = persisted_step_artifact(&run_dir, &persisted, LoopStepName::Development);
    let output_review = persisted_step_artifact(&run_dir, &persisted, LoopStepName::OutputReview);
    let decision = single_policy_decision(&persisted);
    let developer_response: serde_json::Value =
        serde_json::from_str(&developer_response(patch)).expect("developer response JSON");

    assert_eq!(development.2["run_id"], "exact-output-review");
    assert_eq!(development.2["step"], json!(LoopStepName::Development));
    assert_eq!(development.2["role"], json!(Role::Developer));
    assert_eq!(development.2["developer_response"], developer_response);
    assert_eq!(development.2["patch"], patch);
    assert_eq!(development.2["patch_digest"], sha256(patch.as_bytes()));
    assert_eq!(development.2["changed_paths"], json!(["docs/example.md"]));
    assert_eq!(development.2["policy_decision"], json!(decision));
    assert_eq!(
        std::fs::read(run_dir.join(&development.0)).expect("development artifact bytes"),
        canonical_json_bytes(&development.2).expect("canonical development evidence")
    );
    assert_eq!(
        development.1,
        canonical_sha256_digest(&development.2).expect("development artifact digest")
    );

    let requests = provider.requests().expect("provider requests");
    assert_eq!(requests.len(), 6);
    let prompt: serde_json::Value =
        serde_json::from_str(&requests[5].messages[0].content).expect("output-review role input");
    assert_eq!(prompt["run_id"], "exact-output-review");
    assert_eq!(prompt["input_digests"], json!(input_digests));
    assert_eq!(
        prompt["development_evidence"]["artifact_path"],
        development.0
    );
    assert_eq!(
        prompt["development_evidence"]["artifact_digest"],
        development.1
    );
    assert_eq!(prompt["development_evidence"]["artifact"], development.2);
    assert_eq!(prompt["development_evidence"]["artifact"]["patch"], patch);
    assert_eq!(
        prompt["development_evidence"]["artifact"]["policy_decision"],
        json!(decision)
    );
    assert_eq!(
        prompt["approved_spec_identity"]["spec_creation"]["artifact_digest"],
        persisted_step_artifact(&run_dir, &persisted, LoopStepName::SpecCreation).1
    );
    assert_eq!(
        prompt["approved_spec_identity"]["spec_review"]["artifact_digest"],
        persisted_step_artifact(&run_dir, &persisted, LoopStepName::SpecReview).1
    );
    assert!(prompt.get("repository_context").is_none());
    assert!(prompt.get("ticket").is_none());
    assert!(!prompt.to_string().contains(source_content));

    assert_eq!(output_review.2["run_id"], "exact-output-review");
    assert_eq!(output_review.2["step"], json!(LoopStepName::OutputReview));
    assert_eq!(output_review.2["role"], json!(Role::OutputReviewer));
    assert_eq!(output_review.2["response"]["decision"], "approve_for_tests");
    assert_eq!(
        std::fs::read(run_dir.join(&output_review.0)).expect("review artifact bytes"),
        canonical_json_bytes(&output_review.2).expect("canonical review artifact")
    );
    assert_eq!(
        output_review.1,
        canonical_sha256_digest(&output_review.2).expect("review artifact digest")
    );
}

#[test]
fn patch_proposed_development_requires_one_authoritative_policy_gate() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();
    let provider = provider_for_development_patch(fixture("allowed-doc.diff"));
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000).with_ticket(ticket.clone());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "missing-development-gate",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut step_runner,
    )
    .expect("start loop");
    finish_steps_before_development(&mut loop_runner);
    assert!(loop_runner
        .run_next_step()
        .expect("missing patch gate becomes durable failed evidence"));
    assert_eq!(loop_runner.run().status, LoopStatus::Failed);
    assert_eq!(
        step_status(loop_runner.run(), LoopStepName::Development),
        LoopStepStatus::Failed
    );
    assert_eq!(provider.requests().expect("provider requests").len(), 5);
    let run_dir = runs_root.join("missing-development-gate");
    let persisted = read_run(&run_dir);
    assert_eq!(persisted.provider_exchange_records.len(), 10);
    let evidence = persisted_step_artifact(&run_dir, &persisted, LoopStepName::Development);
    assert_eq!(evidence.2["result"], "post_response_failure");
    assert!(evidence.2["reason"]
        .as_str()
        .expect("failure reason")
        .contains("patch gate"));
    assert!(!run_dir.join("prompts/06-output-review.prompt.md").exists());
    let terminal_tree = read_tree_bytes(&run_dir);
    assert!(!loop_runner
        .run_next_step()
        .expect("failed development remains terminal"));
    assert_eq!(read_tree_bytes(&run_dir), terminal_tree);
}

#[test]
fn resume_at_output_review_reuses_verified_evidence_without_rerunning_patch_gate() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    std::fs::create_dir_all(&repo).expect("repo directory");
    let ticket = ticket_with_apply(true);
    let start_provider = provider_for_development_patch(fixture("allowed-doc.diff"));
    let mut start_patch_runner = RecordingPatchRunner::default();
    let mut start_runner = ProviderStepRunner::new_legacy_unit_test_harness(&start_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(&repo, &ticket, policy(), true),
            &mut start_patch_runner,
        );
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "resume-exact-development-evidence",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut start_runner,
    )
    .expect("start loop");
    finish_steps_before_development(&mut loop_runner);
    loop_runner.run_next_step().expect("development");
    drop(loop_runner);
    drop(start_runner);
    assert_eq!(
        start_patch_runner.commands,
        vec![PatchCommand::GitApplyCheck],
        "provider development validates the proposal but never executes GitApply"
    );
    let initial_decision = single_policy_decision(&read_run(
        &runs_root.join("resume-exact-development-evidence"),
    ));
    assert!(initial_decision.apply_requested);
    assert!(!initial_decision.applied);
    assert_eq!(start_provider.requests().expect("start requests").len(), 5);

    let resume_provider =
        FakeProvider::new(vec![Ok(model_response(&output_review_approved_response()))]);
    let mut resume_patch_runner = RecordingPatchRunner::default();
    let mut resume_runner = ProviderStepRunner::new_legacy_unit_test_harness(&resume_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(&repo, &ticket, policy(), true),
            &mut resume_patch_runner,
        );
    let mut resumed = LoopRunner::resume(
        &runs_root,
        "resume-exact-development-evidence",
        &mut resume_runner,
    )
    .expect("resume verified evidence");
    resumed.run_next_step().expect("output review");
    drop(resumed);
    drop(resume_runner);

    assert!(
        resume_patch_runner.commands.is_empty(),
        "resume at OutputReview must not rerun the patch gate"
    );
    assert_eq!(
        resume_provider.requests().expect("resume requests").len(),
        1,
        "resume should contact only OutputReview"
    );
}

#[test]
fn resume_at_development_reuses_verified_approved_spec_with_one_provider_call() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();
    let start_provider = FakeProvider::new(vec![
        Ok(model_response(fixture("research.valid.json"))),
        Ok(model_response(fixture("analyzer.valid.json"))),
        Ok(model_response(&spec_writer_response())),
        Ok(model_response(&spec_review_approved_response())),
    ]);
    let mut start_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&start_provider, "fake-model", 30_000).with_ticket(ticket.clone());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "resume-at-development",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut start_runner,
    )
    .expect("start loop");
    finish_steps_before_development(&mut loop_runner);
    drop(loop_runner);
    assert_eq!(start_provider.requests().expect("start requests").len(), 4);

    let blocked = json!({
        "role": "developer",
        "status": "blocked",
        "summary": "No patch in this resume call-count test.",
        "changed_files": [],
        "requires_human_review": false
    })
    .to_string();
    let resume_provider = FakeProvider::new(vec![Ok(model_response(&blocked))]);
    let mut resume_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&resume_provider, "fake-model", 30_000).with_ticket(ticket.clone());
    let mut resumed = LoopRunner::resume(&runs_root, "resume-at-development", &mut resume_runner)
        .expect("resume at development");
    resumed.run_next_step().expect("development");

    let requests = resume_provider.requests().expect("resume requests");
    assert_eq!(requests.len(), 1);
    let prompt: serde_json::Value =
        serde_json::from_str(&requests[0].messages[0].content).expect("development prompt");
    assert_eq!(
        prompt["approved_spec"]["spec_review"]["artifact"]["response"]["decision"],
        "approve_spec"
    );
}

#[test]
fn resume_rejects_invalid_development_evidence_before_mutation_or_provider() {
    for mutation in [
        DownstreamArtifactMutation::Missing,
        DownstreamArtifactMutation::TamperedResponse,
        DownstreamArtifactMutation::NonCanonical,
        DownstreamArtifactMutation::WrongRun,
        DownstreamArtifactMutation::WrongRole,
        DownstreamArtifactMutation::WrongStep,
        DownstreamArtifactMutation::WrongDigest,
        DownstreamArtifactMutation::SubstitutedPatch,
        DownstreamArtifactMutation::CoordinatedSubstitutedPatch,
        DownstreamArtifactMutation::PolicyMismatch,
    ] {
        assert_downstream_artifact_rejected_without_mutation(
            DownstreamArtifactTarget::Development,
            mutation,
        );
    }
}

#[test]
fn resume_rejects_invalid_output_review_artifact_before_mutation_or_provider() {
    for mutation in [
        DownstreamArtifactMutation::Missing,
        DownstreamArtifactMutation::TamperedResponse,
        DownstreamArtifactMutation::NonCanonical,
        DownstreamArtifactMutation::WrongRun,
        DownstreamArtifactMutation::WrongRole,
        DownstreamArtifactMutation::WrongStep,
        DownstreamArtifactMutation::WrongDigest,
    ] {
        assert_downstream_artifact_rejected_without_mutation(
            DownstreamArtifactTarget::OutputReview,
            mutation,
        );
    }
}

#[test]
fn output_review_resume_rejects_canonical_non_approving_spec_review_before_mutation() {
    for decision in ["approve_for_tests", "request_changes", "reject"] {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let repo = temp_dir.path().join("repo");
        let runs_root = temp_dir.path().join("runs");
        std::fs::create_dir_all(&repo).expect("repo directory");
        let run_id = format!("output-review-wrong-spec-{decision}");
        let ticket = ticket();
        let provider = provider_for_development_patch(fixture("allowed-doc.diff"));
        let mut patch_runner = RecordingPatchRunner::default();
        let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_patch_gate(patch_gate_config(&repo, false, true), &mut patch_runner);
        let mut loop_runner = LoopRunner::start(
            LoopRunnerConfig::for_ticket(
                &runs_root,
                &run_id,
                &ticket,
                "fake-provider",
                "fake-model",
                test_input_digests_for(&ticket),
            ),
            &mut step_runner,
        )
        .expect("start loop");
        finish_steps_before_development(&mut loop_runner);
        loop_runner.run_next_step().expect("development");
        drop(loop_runner);
        drop(step_runner);

        let run_dir = runs_root.join(&run_id);
        let mut verified = read_run(&run_dir);
        let record = verified
            .steps
            .iter_mut()
            .find(|record| record.name == LoopStepName::SpecReview)
            .expect("spec review record");
        let artifact_path = record.artifact_path.clone().expect("artifact path");
        let absolute_artifact_path = run_dir.join(&artifact_path);
        let mut artifact: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&absolute_artifact_path).expect("artifact bytes"),
        )
        .expect("artifact JSON");
        artifact["response"]["decision"] = json!(decision);
        artifact["response_digest"] = json!(canonical_sha256_digest(&artifact["response"])
            .expect("changed spec-review response digest"));
        rewrite_artifact_and_verified_digest(&absolute_artifact_path, &artifact, record);
        persist_verified_resume_authority(&run_dir, &verified);

        let before = read_tree_bytes(&run_dir);
        let resume_provider = FakeProvider::new(Vec::new());
        let mut resume_patch_runner = RecordingPatchRunner::default();
        let mut resume_runner = ProviderStepRunner::new_legacy_unit_test_harness(&resume_provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_patch_gate(
                patch_gate_config(&repo, false, true),
                &mut resume_patch_runner,
            );
        let error = LoopRunner::resume_verified(&runs_root, verified, &mut resume_runner)
            .expect_err("OutputReview resume requires approve_spec evidence");

        assert!(
            error.to_string().contains("approving SpecReview"),
            "{decision}: {error}"
        );
        assert_eq!(read_tree_bytes(&run_dir), before, "{decision}");
        assert!(
            resume_provider
                .requests()
                .expect("provider requests")
                .is_empty(),
            "{decision} must fail before provider invocation"
        );
        assert!(
            resume_patch_runner.commands.is_empty(),
            "{decision} must fail before patch gating"
        );
    }
}

#[test]
fn blocked_and_needs_context_development_artifacts_persist_without_policy_evidence_or_advance() {
    for status in ["blocked", "needs_context"] {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let runs_root = temp_dir.path().join("runs");
        let run_id = format!("development-{status}");
        let mut developer_response = json!({
            "role": "developer",
            "status": status,
            "summary": "Development cannot safely propose a patch.",
            "changed_files": [],
            "requires_human_review": false
        });
        if status == "needs_context" {
            developer_response["context_request"] = json!({
                "paths": ["crates/seaf-loop/src/policy.rs"],
                "reason": "The path policy is required before proposing a patch."
            });
        }
        let developer_response = developer_response.to_string();
        let provider = FakeProvider::new(vec![
            Ok(model_response(fixture("research.valid.json"))),
            Ok(model_response(fixture("analyzer.valid.json"))),
            Ok(model_response(&spec_writer_response())),
            Ok(model_response(&spec_review_approved_response())),
            Ok(model_response(&developer_response)),
        ]);
        let ticket = ticket();
        let mut step_runner =
            ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000).with_ticket(ticket.clone());
        let mut loop_runner = LoopRunner::start(
            LoopRunnerConfig::for_ticket(
                &runs_root,
                &run_id,
                &ticket,
                "fake-provider",
                "fake-model",
                test_input_digests_for(&ticket),
            ),
            &mut step_runner,
        )
        .expect("start loop");
        finish_steps_before_development(&mut loop_runner);
        loop_runner.run_next_step().expect("blocked development");
        drop(loop_runner);

        let run_dir = runs_root.join(&run_id);
        let persisted = read_run(&run_dir);
        let artifact = persisted_step_artifact(&run_dir, &persisted, LoopStepName::Development);
        assert_eq!(persisted.status, LoopStatus::Blocked);
        assert_eq!(
            step_status(&persisted, LoopStepName::Development),
            LoopStepStatus::Blocked
        );
        assert!(persisted.policy_decisions.is_empty());
        if status == "needs_context" {
            assert_eq!(artifact.2["result"], "context_denied");
        } else {
            assert_eq!(artifact.2["response"]["status"], status);
        }
        assert!(artifact.2.get("policy_decision").is_none());
        assert!(artifact.2.get("patch_digest").is_none());
        assert!(!run_dir.join("prompts/06-output-review.prompt.md").exists());
        assert_eq!(provider.requests().expect("provider requests").len(), 5);
    }
}

#[test]
fn resume_rejects_invalid_early_role_artifacts_before_context_log_or_provider_mutation() {
    for mutation in [
        ResumeArtifactMutation::Missing,
        ResumeArtifactMutation::TamperedResponse,
        ResumeArtifactMutation::NonCanonical,
        ResumeArtifactMutation::WrongRun,
        ResumeArtifactMutation::WrongRole,
        ResumeArtifactMutation::WrongStep,
        ResumeArtifactMutation::WrongDigest,
    ] {
        assert_resume_artifact_rejected_without_mutation(mutation);
    }
}

#[test]
fn provider_step_runner_repairs_invalid_json_once_and_audits_both_responses() {
    let provider = FakeProvider::new(vec![
        Ok(model_response("not json")),
        Ok(model_response(fixture("research.valid.json"))),
    ]);
    let mut runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000);

    let request_audit = runner
        .step_request(LoopStepName::Research)
        .expect("research request");
    let output = runner
        .run_step(LoopStepName::Research, &request_audit)
        .expect("repair succeeds");

    assert_eq!(output.status, LoopStepStatus::Completed);
    assert!(output.response.contains("initial provider response"));
    assert!(output.response.contains("not json"));
    assert!(output.response.contains("repair provider request"));
    assert!(output.response.contains("Repair the invalid JSON"));
    assert!(output.response.contains("repair provider response"));
    assert!(output.response.contains(fixture("research.valid.json")));

    let requests = provider.requests().expect("provider requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages[0]
        .content
        .contains("Repair the invalid JSON"));
}

#[test]
fn provider_step_runner_does_not_repair_schema_invalid_json() {
    let provider = FakeProvider::new(vec![
        Ok(model_response(fixture(
            "research.invalid_missing_status.json",
        ))),
        Ok(model_response(fixture("research.valid.json"))),
    ]);
    let mut runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000);

    let request_audit = runner
        .step_request(LoopStepName::Research)
        .expect("research request");
    let error = runner
        .run_step(LoopStepName::Research, &request_audit)
        .expect_err("schema-invalid JSON should fail without repair");

    assert!(
        error.to_string().contains("invalid role response"),
        "parse error should be useful, got {error}"
    );
    assert_eq!(provider.requests().expect("provider requests").len(), 1);
}

#[test]
fn invalid_json_containing_an_obvious_secret_is_dropped_without_repair() {
    let provider = FakeProvider::new(vec![Ok(model_response(
        "not-json sk-proj-abcdefghijklmnop",
    ))]);
    let mut runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000);
    let request = runner
        .step_request(LoopStepName::Research)
        .expect("research request");

    let error = runner
        .run_step(LoopStepName::Research, &request)
        .expect_err("secret-bearing result must become a safe provider failure");

    assert!(error
        .to_string()
        .contains("provider response contained prohibited credential material"));
    assert_eq!(provider.requests().expect("provider requests").len(), 1);
}

#[test]
fn provider_step_runner_persists_provider_response_when_parse_failure_stops_loop() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let provider = FakeProvider::new(vec![Ok(model_response(fixture(
        "research.invalid_missing_status.json",
    )))]);
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000).with_ticket(ticket());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "parse-failure-run",
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start loop");

    assert!(loop_runner
        .run_next_step()
        .expect("parse failure becomes a durable terminal step"));
    assert_eq!(loop_runner.run().status, LoopStatus::Failed);
    assert_file_contains(
        &runs_root.join("parse-failure-run/responses/01-research.raw.txt"),
        fixture("research.invalid_missing_status.json"),
    );
}

#[test]
fn provider_step_runner_persists_repair_transcript_when_repair_failure_stops_loop() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let provider = FakeProvider::new(vec![
        Ok(model_response("not json")),
        Err(ModelError::provider(
            "repair service failed",
            false,
            json!({}),
        )),
    ]);
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000).with_ticket(ticket());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "repair-failure-run",
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start loop");

    assert!(loop_runner
        .run_next_step()
        .expect("repair provider failure becomes a durable terminal step"));
    assert_eq!(loop_runner.run().status, LoopStatus::Failed);
    let response_path = runs_root.join("repair-failure-run/responses/01-research.raw.txt");
    assert_file_contains(&response_path, "repair service failed");
    assert!(runs_root.join("repair-failure-run/responses/01-research.attempt-001.exchange-001.initial.response.json").is_file());
    assert!(runs_root.join("repair-failure-run/responses/01-research.attempt-001.exchange-002.json-repair.response.json").is_file());
}

#[test]
fn provider_step_runner_surfaces_model_timeout_without_retrying_a_role_step() {
    let provider = FakeProvider::new(vec![Err(ModelError::timeout(
        "research model timed out",
        10,
        json!({ "provider": "fake" }),
    ))]);
    let mut runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 10);

    let request_audit = runner
        .step_request(LoopStepName::Research)
        .expect("research request");
    let error = runner
        .run_step(LoopStepName::Research, &request_audit)
        .expect_err("timeout should fail the role step");

    assert!(
        error.to_string().contains("research model timed out"),
        "timeout should be surfaced in the step error, got {error}"
    );
    assert_eq!(
        provider.requests().expect("provider requests").len(),
        1,
        "timeouts must not be repaired as malformed role output"
    );
}

#[test]
fn provider_step_runner_persists_timeout_response_artifact_when_loop_step_fails() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let provider = FakeProvider::new(vec![Err(ModelError::timeout(
        "research model timed out",
        10,
        json!({ "provider": "fake" }),
    ))]);
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 10).with_ticket(ticket());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "timeout-artifact-run",
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start loop");

    assert!(loop_runner
        .run_next_step()
        .expect("timeout becomes a durable terminal live step"));
    assert_eq!(loop_runner.run().status, LoopStatus::Failed);
    let response_path = runs_root.join("timeout-artifact-run/responses/01-research.raw.txt");
    assert_file_contains(&response_path, "provider request failed for Research");
    assert_file_contains(&response_path, "\"kind\": \"timeout\"");
    assert_file_contains(&response_path, "research model timed out");
}

#[cfg(unix)]
#[test]
fn provider_request_capacity_denial_happens_before_exchange_activation_or_provider_call() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let provider = FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let ticket = ticket();
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "provider-capacity-denial",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut step_runner,
    )
    .expect("start loop");
    let run_directory = runs_root.join("provider-capacity-denial");
    let current = crate::artifact_storage::published_run_bytes(&run_directory).unwrap();
    let target = crate::artifact_storage::RUN_TREE_BYTE_CAP - 32 * 1024;
    let mut remaining = target.checked_sub(current).expect("room for filler");
    let mut index = 0;
    while remaining > 0 {
        let size = remaining.min(2 * 1024 * 1024);
        let path = run_directory
            .join("artifacts")
            .join(format!("capacity-filler-{index:02}.bin"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("create filler");
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .expect("private filler");
        file.set_len(size).expect("size filler");
        remaining -= size;
        index += 1;
    }

    let error = loop_runner
        .run_next_step()
        .expect_err("insufficient committed capacity must refuse the provider step");
    assert!(error.to_string().contains("capacity") || error.to_string().contains("cap"), "{error}");
    assert!(provider.requests().expect("provider requests").is_empty());
    let persisted = read_run(&run_directory);
    assert_eq!(persisted.status, LoopStatus::Running);
    assert_eq!(
        persisted
            .steps
            .iter()
            .find(|step| step.name == LoopStepName::Research)
            .expect("research step")
            .status,
        LoopStepStatus::Running
    );
    assert!(persisted.provider_exchange_records.is_empty());
    assert!(!run_directory
        .join("prompts/01-research.attempt-001.exchange-001.initial.request.md")
        .exists());
    assert!(!run_directory
        .join("artifacts/01-research.attempt-001.exchange-001.initial.request.record.json")
        .exists());
}

#[test]
fn response_audit_appearing_before_call_reauthentication_prevents_provider_side_effects() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();
    let provider = FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let observer = |workspace: &seaf_loop::LoopWorkspace,
                    _run: &LoopRun,
                    coordinates: &seaf_loop::ProviderExchangeCoordinates,
                    _request: &seaf_core::ArtifactReference| {
        seaf_loop::write_provider_exchange_response(
            workspace.run_directory(),
            coordinates,
            &seaf_loop::ProviderExchangeResponseAudit::ModelResponse {
                response: model_response(fixture("research.valid.json")),
            },
        )
        .expect("inject canonical response audit before reauthentication");
    };
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_before_provider_reauthentication_observer(&observer);
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "response-audit-pre-call-race",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut step_runner,
    )
    .expect("start loop");

    let error = loop_runner
        .run_next_step()
        .expect_err("a raced response audit must stop before provider invocation");
    assert!(error.to_string().contains("provider call"), "{error}");
    assert!(provider.requests().expect("provider requests").is_empty());
    let persisted = read_run(&runs_root.join("response-audit-pre-call-race"));
    assert_eq!(persisted.provider_exchange_records.len(), 1);
    assert_eq!(
        persisted.provider_exchange_records[0].phase,
        ProviderExchangePhase::Request
    );
    assert_ne!(persisted.status, LoopStatus::Completed);
    assert_ne!(persisted.status, LoopStatus::Failed);
}

#[test]
fn context_manifest_tamper_before_call_reauthentication_prevents_provider_side_effects() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    std::fs::create_dir_all(repo.join("src")).expect("src dir");
    std::fs::write(repo.join("src/lib.rs"), "pub fn live() {}\n").expect("source file");
    let ticket = ticket_with_context(vec!["src/lib.rs"], Vec::new());
    let provider = FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let observer = |workspace: &seaf_loop::LoopWorkspace,
                    _run: &LoopRun,
                    _coordinates: &seaf_loop::ProviderExchangeCoordinates,
                    _request: &seaf_core::ArtifactReference| {
        std::fs::write(
            workspace.run_directory().join("context-manifest.json"),
            b"not canonical context authority",
        )
        .expect("tamper context manifest before provider reauthentication");
    };
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_context_pack_request(context_request(&repo, &ticket, Vec::new()))
            .with_before_provider_reauthentication_observer(&observer);
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "context-manifest-pre-call-race",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut step_runner,
    )
    .expect("start loop");

    let error = loop_runner
        .run_next_step()
        .expect_err("manifest tamper must stop before provider invocation");

    assert!(error.to_string().contains("context manifest"), "{error}");
    assert!(provider.requests().expect("provider requests").is_empty());
}

#[test]
fn staged_response_record_appearing_before_call_reauthentication_prevents_provider_side_effects() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();
    let provider = FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let observer = |workspace: &seaf_loop::LoopWorkspace,
                    _run: &LoopRun,
                    coordinates: &seaf_loop::ProviderExchangeCoordinates,
                    request: &seaf_core::ArtifactReference| {
        let response = seaf_loop::write_provider_exchange_response(
            workspace.run_directory(),
            coordinates,
            &seaf_loop::ProviderExchangeResponseAudit::ModelResponse {
                response: model_response(fixture("research.valid.json")),
            },
        )
        .expect("inject canonical response audit before reauthentication");
        let record = ProviderExchangeRecord {
            schema_version: seaf_loop::PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: coordinates.run_id.clone(),
            step: coordinates.step,
            role: coordinates.role,
            step_attempt: coordinates.step_attempt,
            exchange_index: coordinates.exchange_index,
            kind: coordinates.kind,
            context_round: coordinates.context_round,
            phase: ProviderExchangePhase::Response,
            previous_record_digest: None,
            request: request.clone(),
            response: Some(response),
            expansion: None,
            outcome: Some(ProviderExchangeOutcome::Passed),
        };
        let request_head = read_run(workspace.run_directory())
            .provider_exchange_records
            .last()
            .expect("request head")
            .digest
            .clone();
        let mut record = record;
        record.previous_record_digest = Some(request_head);
        seaf_loop::stage_provider_exchange_record(workspace.run_directory(), &record)
            .expect("inject linked staged response record before reauthentication");
    };
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_before_provider_reauthentication_observer(&observer);
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "response-record-pre-call-race",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut step_runner,
    )
    .expect("start loop");

    let error = loop_runner
        .run_next_step()
        .expect_err("a raced staged response must stop before provider invocation");
    assert!(error.to_string().contains("provider call"), "{error}");
    assert!(provider.requests().expect("provider requests").is_empty());
    let persisted = read_run(&runs_root.join("response-record-pre-call-race"));
    assert_eq!(persisted.provider_exchange_records.len(), 1);
    assert_eq!(
        persisted.provider_exchange_records[0].phase,
        ProviderExchangePhase::Request
    );
    assert_ne!(persisted.status, LoopStatus::Completed);
    assert_ne!(persisted.status, LoopStatus::Failed);
}

struct PanicAfterRequestProvider;

impl ModelProvider for PanicAfterRequestProvider {
    fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        panic!("injected crash after durable request tail")
    }
}

struct RunLockCheckingProvider {
    run_directory: std::path::PathBuf,
    calls: std::sync::Mutex<usize>,
}

impl ModelProvider for RunLockCheckingProvider {
    fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let guard = crate::run_persistence::RunMutationGuard::acquire(&self.run_directory)
            .expect("provider callback must run after the run mutation lock is released");
        guard.unlock().expect("release callback proof lock");
        *self.calls.lock().expect("calls lock") += 1;
        Ok(model_response(fixture("research.valid.json")))
    }
}

#[test]
fn provider_invocation_does_not_hold_the_run_mutation_lock() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "provider-call-lock-release";
    let ticket = ticket();
    let provider = RunLockCheckingProvider {
        run_directory: runs_root.join(run_id),
        calls: std::sync::Mutex::new(0),
    };
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            run_id,
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut step_runner,
    )
    .expect("start loop");

    loop_runner
        .run_next_step()
        .expect("provider executes after the run lock is released");
    assert_eq!(*provider.calls.lock().expect("calls lock"), 1);
}

#[cfg(unix)]
#[test]
fn request_only_crash_replays_exact_request_once_when_commitment_remains_sufficient() {
    use std::{os::unix::fs::PermissionsExt, sync::{Arc, Barrier}, thread};
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();
    let crash_provider = PanicAfterRequestProvider;
    let mut crash_step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&crash_provider, "fake-model", 30_000)
            .with_ticket(ticket.clone());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "request-only-replay",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut crash_step_runner,
    )
    .expect("start loop");
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = loop_runner.run_next_step();
    }));
    assert!(crashed.is_err());
    drop(loop_runner);
    drop(crash_step_runner);
    let run_directory = runs_root.join("request-only-replay");
    assert_eq!(
        read_run(&run_directory)
            .provider_exchange_records
            .last()
            .expect("request tail")
            .phase,
        seaf_core::ProviderExchangePhase::Request
    );
    let commitment = crate::provider_exchange::derive_active_provider_storage_commitment(
        &run_directory,
    )
    .unwrap()
    .expect("request-tail commitment");
    let current = crate::artifact_storage::published_run_bytes(&run_directory).unwrap();
    let reserved = commitment.permanent_bytes + commitment.transient_bytes;
    let resume_log_slack = 4 * 1024;
    let mut remaining =
        (crate::artifact_storage::RUN_TREE_BYTE_CAP - reserved - resume_log_slack) - current;
    let mut index = 0;
    while remaining > 0 {
        let size = remaining.min(2 * 1024 * 1024);
        let path = run_directory
            .join("artifacts")
            .join(format!("exact-replay-filler-{index:02}.bin"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
        file.set_len(size).unwrap();
        remaining -= size;
        index += 1;
    }
    let barrier = Arc::new(Barrier::new(3));
    let mut writers = Vec::new();
    for name in ["concurrent-unrelated-a", "concurrent-unrelated-b"] {
        let barrier = Arc::clone(&barrier);
        let run_directory = run_directory.clone();
        writers.push(thread::spawn(move || {
            barrier.wait();
            crate::immutable_artifact::publish_create_only(
                &run_directory,
                &format!("artifacts/{name}.json"),
                &vec![b'x'; resume_log_slack as usize + 1],
            )
        }));
    }
    barrier.wait();
    assert!(writers
        .into_iter()
        .all(|writer| writer.join().unwrap().is_err()));

    let resume_provider =
        FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let mut resume_step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&resume_provider, "fake-model", 30_000)
            .with_ticket(ticket);
    let mut resumed = LoopRunner::resume(
        &runs_root,
        "request-only-replay",
        &mut resume_step_runner,
    )
    .expect("resume request-only prefix");
    resumed.run_next_step().expect("replay exact request once");
    assert_eq!(resume_provider.requests().expect("provider requests").len(), 1);
}

#[cfg(unix)]
#[test]
fn request_only_replay_with_lost_headroom_makes_zero_provider_calls() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();
    let crash_provider = PanicAfterRequestProvider;
    let mut crash_step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&crash_provider, "fake-model", 30_000)
            .with_ticket(ticket.clone());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "request-only-insufficient-replay",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut crash_step_runner,
    )
    .unwrap();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = loop_runner.run_next_step();
    }));
    drop(loop_runner);
    drop(crash_step_runner);
    let run_directory = runs_root.join("request-only-insufficient-replay");
    let current = crate::artifact_storage::published_run_bytes(&run_directory).unwrap();
    let mut remaining = (crate::artifact_storage::RUN_TREE_BYTE_CAP - 32 * 1024) - current;
    let mut index = 0;
    while remaining > 0 {
        let size = remaining.min(2 * 1024 * 1024);
        let path = run_directory
            .join("artifacts")
            .join(format!("replay-filler-{index:02}.bin"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
        file.set_len(size).unwrap();
        remaining -= size;
        index += 1;
    }

    let resume_provider =
        FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let mut resume_step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&resume_provider, "fake-model", 30_000)
            .with_ticket(ticket);
    let error = match LoopRunner::resume(
        &runs_root,
        "request-only-insufficient-replay",
        &mut resume_step_runner,
    ) {
        Ok(mut resumed) => resumed
            .run_next_step()
            .expect_err("lost commitment headroom must block replay"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("cap") || error.to_string().contains("commitment"), "{error}");
    assert!(resume_provider.requests().expect("provider requests").is_empty());
}

#[test]
fn oversized_provider_success_becomes_fixed_audited_failure_without_raw_bytes_or_digest() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let sentinel = format!("RAW_PROVIDER_SENTINEL_{}", "x".repeat(1024 * 1024 + 128));
    let raw_digest = sha256(sentinel.as_bytes());
    let provider = FakeProvider::new(vec![Ok(model_response(&sentinel))]);
    let ticket = ticket();
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "oversized-provider-success",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut step_runner,
    )
    .expect("start loop");

    assert!(loop_runner
        .run_next_step()
        .expect("oversize conversion is a durable terminal provider failure"));
    assert_eq!(provider.requests().expect("provider requests").len(), 1);
    let run_directory = runs_root.join("oversized-provider-success");
    let persisted = read_run(&run_directory);
    assert_eq!(persisted.status, LoopStatus::Failed);
    let response_audit = std::fs::read_to_string(
        run_directory
            .join("responses/01-research.attempt-001.exchange-001.initial.response.json"),
    )
    .expect("fixed response audit");
    assert!(response_audit.contains("provider_response_audit_too_large"));
    for (_, bytes) in read_tree_bytes(&run_directory) {
        let rendered = String::from_utf8_lossy(&bytes);
        assert!(!rendered.contains("RAW_PROVIDER_SENTINEL_"));
        assert!(!rendered.contains(&raw_digest));
    }
}

#[test]
fn provider_step_runner_packs_live_context_into_prompt_and_manifest_before_steps_run() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    let source_path = repo.join("src/lib.rs");
    let source_content = "pub fn live_context() -> &'static str { \"packed\" }\n";
    std::fs::create_dir_all(source_path.parent().expect("source parent")).expect("src dir");
    std::fs::write(&source_path, source_content).expect("source file");

    let ticket = ticket_with_context(vec!["src/lib.rs"], Vec::new());
    let provider = FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repo, &ticket, Vec::new()));
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "live-context-run",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut step_runner,
    )
    .expect("start loop");

    loop_runner.run_next_step().expect("run research step");

    let digest = sha256(source_content.as_bytes());
    let requests = provider.requests().expect("provider requests");
    assert_eq!(requests.len(), 1);
    let user_prompt = &requests[0].messages[0].content;
    let role_input: serde_json::Value =
        serde_json::from_str(user_prompt).expect("structured role input");
    let repository_context = role_input["repository_context"]
        .as_str()
        .expect("repository context");
    assert!(repository_context.contains(UNTRUSTED_CONTEXT_MARKER));
    assert!(repository_context.contains("path: src/lib.rs"));
    assert!(repository_context.contains(&format!("sha256: {digest}")));
    assert!(repository_context.contains(source_content));

    let prompt_audit =
        std::fs::read_to_string(runs_root.join("live-context-run/prompts/01-research.prompt.md"))
            .expect("prompt audit");
    assert!(prompt_audit.contains(UNTRUSTED_CONTEXT_MARKER));
    assert!(prompt_audit.contains(&digest));

    let manifest_json =
        std::fs::read_to_string(runs_root.join("live-context-run/context-manifest.json"))
            .expect("manifest");
    let manifest: ContextManifest = serde_json::from_str(&manifest_json).expect("manifest json");
    assert_eq!(manifest.untrusted_context_marker, UNTRUSTED_CONTEXT_MARKER);
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(manifest.files[0].path, "src/lib.rs");
    assert_eq!(manifest.files[0].sha256, digest);
    assert_eq!(manifest.files[0].source_bytes, source_content.len());
    assert_eq!(manifest.files[0].included_bytes, source_content.len());
    assert!(!manifest_json.contains(source_content));
}

#[test]
fn provider_step_runner_rejects_forbidden_live_context_before_provider_call() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    std::fs::create_dir_all(repo.join("src")).expect("src dir");
    std::fs::write(repo.join("src/lib.rs"), "pub fn forbidden() {}\n").expect("source file");

    let ticket = ticket_with_context(vec!["src/lib.rs"], Vec::new());
    let provider = FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repo, &ticket, vec!["src/**".to_string()]));

    let error = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "forbidden-context-run",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut step_runner,
    )
    .expect_err("forbidden live context should fail before loop starts");

    assert!(
        error.to_string().contains("forbidden live context path"),
        "error should name forbidden context, got {error}"
    );
    assert!(provider.requests().expect("provider requests").is_empty());
    assert!(!runs_root
        .join("forbidden-context-run/prompts/01-research.prompt.md")
        .exists());
    assert!(!runs_root
        .join("forbidden-context-run/responses/01-research.raw.txt")
        .exists());
}

#[test]
fn provider_step_runner_cleans_failed_prepare_workspace_so_same_run_id_can_retry() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    std::fs::create_dir_all(repo.join("src")).expect("src dir");
    std::fs::write(repo.join("src/lib.rs"), "pub fn retryable() {}\n").expect("source file");

    let ticket = ticket_with_context(vec!["src/lib.rs"], Vec::new());
    let provider = FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let mut forbidden_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repo, &ticket, vec!["src/**".to_string()]));

    let first_error = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "retryable-context-run",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut forbidden_runner,
    )
    .expect_err("forbidden context should fail before start completes");

    assert!(
        first_error
            .to_string()
            .contains("forbidden live context path"),
        "first error should name forbidden context, got {first_error}"
    );
    assert!(
        !runs_root.join("retryable-context-run").exists(),
        "failed prepare should not leave a partial run directory that blocks retry"
    );

    let mut allowed_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repo, &ticket, Vec::new()));

    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "retryable-context-run",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut allowed_runner,
    )
    .expect("same run id should be reusable after fixing context");

    loop_runner.run_next_step().expect("run research step");
    assert_eq!(provider.requests().expect("provider requests").len(), 1);
    assert!(runs_root
        .join("retryable-context-run/context-manifest.json")
        .exists());
}

#[test]
fn provider_step_runner_resume_with_fresh_runner_prepares_live_context_for_next_prompt() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    let source_path = repo.join("src/lib.rs");
    let source_content = "pub fn resumed_context() -> &'static str { \"fresh\" }\n";
    std::fs::create_dir_all(source_path.parent().expect("source parent")).expect("src dir");
    std::fs::write(&source_path, source_content).expect("source file");

    let ticket = ticket_with_context(vec!["src/lib.rs"], Vec::new());
    let start_provider =
        FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let mut start_runner = ProviderStepRunner::new_legacy_unit_test_harness(&start_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repo, &ticket, Vec::new()));
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "resume-context-run",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut start_runner,
    )
    .expect("start loop");
    loop_runner.run_next_step().expect("run research step");
    drop(loop_runner);

    let resume_provider =
        FakeProvider::new(vec![Ok(model_response(fixture("analyzer.valid.json")))]);
    let mut resume_runner = ProviderStepRunner::new_legacy_unit_test_harness(&resume_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repo, &ticket, Vec::new()));
    let mut resumed =
        LoopRunner::resume(&runs_root, "resume-context-run", &mut resume_runner).expect("resume");

    resumed.run_next_step().expect("run analysis step");
    assert_eq!(
        resumed.run().provider_exchange_records.len(),
        4,
        "M1-04b2c keeps resumed provider execution in the durable exchange ledger"
    );

    let digest = sha256(source_content.as_bytes());
    let requests = resume_provider
        .requests()
        .expect("resume provider requests");
    assert_eq!(requests.len(), 1);
    let user_prompt = &requests[0].messages[0].content;
    let role_input: serde_json::Value =
        serde_json::from_str(user_prompt).expect("structured role input");
    let repository_context = role_input["repository_context"]
        .as_str()
        .expect("repository context");
    assert!(repository_context.contains(UNTRUSTED_CONTEXT_MARKER));
    assert!(repository_context.contains("path: src/lib.rs"));
    assert!(repository_context.contains(&format!("sha256: {digest}")));
    assert!(repository_context.contains(source_content));

    let prompt_audit =
        std::fs::read_to_string(runs_root.join("resume-context-run/prompts/02-analysis.prompt.md"))
            .expect("analysis prompt audit");
    assert!(prompt_audit.contains(UNTRUSTED_CONTEXT_MARKER));
    assert!(prompt_audit.contains(&digest));
}

#[test]
fn provider_step_runner_persists_allowed_patch_policy_decision_without_apply() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    std::fs::create_dir_all(&repo).expect("repo dir");

    let provider = provider_for_development_patch(fixture("allowed-doc.diff"));
    let mut patch_runner = RecordingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(ticket())
        .with_patch_gate(patch_gate_config(&repo, false, true), &mut patch_runner);
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "allowed-patch-run",
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start loop");
    finish_steps_before_development(&mut loop_runner);

    loop_runner
        .run_next_step()
        .expect("run development step with patch gate");
    drop(loop_runner);
    drop(step_runner);

    let run_dir = runs_root.join("allowed-patch-run");
    let persisted = read_run(&run_dir);
    let decision = single_policy_decision(&persisted);
    assert_eq!(decision.patch_id, "allowed-patch-run");
    assert_eq!(
        decision.patch_sha256,
        sha256(fixture("allowed-doc.diff").as_bytes())
    );
    assert_eq!(decision.changed_paths, vec!["docs/example.md"]);
    assert_eq!(decision.decision, PatchDecisionKind::Allowed);
    assert!(!decision.requires_human_review);
    assert!(!decision.apply_requested);
    assert!(!decision.applied);
    assert!(
        patch_runner.commands.is_empty(),
        "apply-disabled gates must not invoke git apply checks"
    );
    assert_file_contains(
        &run_dir.join("artifacts/allowed-patch-run.diff"),
        fixture("allowed-doc.diff"),
    );
    assert!(run_dir
        .join("artifacts/allowed-patch-run.policy-decision.json")
        .is_file());
}

#[test]
fn provider_step_runner_rejected_patch_fails_development_and_never_applies() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    std::fs::create_dir_all(&repo).expect("repo dir");
    let patch = forbidden_patch();

    let provider = provider_for_development_patch(&patch);
    let mut patch_runner = RecordingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(ticket())
        .with_patch_gate(patch_gate_config(&repo, true, true), &mut patch_runner);
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "rejected-patch-run",
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start loop");
    finish_steps_before_development(&mut loop_runner);

    loop_runner
        .run_next_step()
        .expect("policy rejection is persisted as a failed step");
    drop(loop_runner);
    drop(step_runner);

    let persisted = read_run(&runs_root.join("rejected-patch-run"));
    assert_eq!(persisted.status, LoopStatus::Failed);
    assert_eq!(
        step_status(&persisted, LoopStepName::Development),
        LoopStepStatus::Failed
    );
    let decision = single_policy_decision(&persisted);
    assert_eq!(decision.decision, PatchDecisionKind::Rejected);
    assert!(decision.apply_requested);
    assert!(!decision.applied);
    assert!(decision
        .reasons
        .iter()
        .any(|reason| reason.code == "forbidden_path"));
    let run_dir = runs_root.join("rejected-patch-run");
    let evidence = persisted_step_artifact(&run_dir, &persisted, LoopStepName::Development);
    assert_eq!(evidence.2["policy_decision"], json!(decision));
    assert_eq!(evidence.2["patch"], patch);
    assert!(!run_dir.join("prompts/06-output-review.prompt.md").exists());
    assert!(
        patch_runner.commands.is_empty(),
        "forbidden patches must not reach git apply --check"
    );
}

#[test]
fn development_evidence_preserves_exact_malformed_and_binary_gate_rejections() {
    for (label, patch, expected_reason) in [
        ("malformed", malformed_patch(), "invalid_patch"),
        ("binary", binary_patch(), "binary_patch"),
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let repo = temp_dir.path().join("repo");
        let runs_root = temp_dir.path().join("runs");
        std::fs::create_dir_all(&repo).expect("repo directory");
        let run_id = format!("{label}-development-evidence");
        let ticket = ticket();
        let provider = provider_for_development_patch(&patch);
        let mut patch_runner = RecordingPatchRunner::default();
        let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_patch_gate(patch_gate_config(&repo, false, true), &mut patch_runner);
        let mut loop_runner = LoopRunner::start(
            LoopRunnerConfig::for_ticket(
                &runs_root,
                &run_id,
                &ticket,
                "fake-provider",
                "fake-model",
                test_input_digests_for(&ticket),
            ),
            &mut step_runner,
        )
        .expect("start loop");
        finish_steps_before_development(&mut loop_runner);
        loop_runner.run_next_step().expect("rejected development");
        drop(loop_runner);
        drop(step_runner);

        let run_dir = runs_root.join(&run_id);
        let persisted = read_run(&run_dir);
        let decision = single_policy_decision(&persisted);
        assert_eq!(decision.decision, PatchDecisionKind::Rejected);
        assert!(decision
            .reasons
            .iter()
            .any(|reason| reason.code == expected_reason));
        assert_eq!(
            persisted_step_artifact(&run_dir, &persisted, LoopStepName::Development).2["patch"],
            patch
        );

        let resume_provider = FakeProvider::new(Vec::new());
        let mut resume_patch_runner = RecordingPatchRunner::default();
        let mut resume_runner = ProviderStepRunner::new_legacy_unit_test_harness(&resume_provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_patch_gate(
                patch_gate_config(&repo, false, true),
                &mut resume_patch_runner,
            );
        LoopRunner::resume(&runs_root, &run_id, &mut resume_runner)
            .expect("exact rejected evidence remains resumable and verifiable");
        assert!(resume_provider
            .requests()
            .expect("provider requests")
            .is_empty());
        assert!(resume_patch_runner.commands.is_empty());
    }
}

#[test]
fn provider_step_runner_human_review_patch_may_reach_output_review_without_applying() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    std::fs::create_dir_all(&repo).expect("repo dir");
    let patch = human_review_patch();

    let provider = FakeProvider::new(vec![
        Ok(model_response(fixture("research.valid.json"))),
        Ok(model_response(fixture("analyzer.valid.json"))),
        Ok(model_response(&spec_writer_response())),
        Ok(model_response(&spec_review_approved_response())),
        Ok(model_response(&developer_response(&patch))),
        Ok(model_response(&output_review_approved_response())),
    ]);
    let mut patch_runner = RecordingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(ticket())
        .with_patch_gate(patch_gate_config(&repo, true, true), &mut patch_runner);
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "review-patch-run",
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start loop");
    finish_steps_before_development(&mut loop_runner);

    loop_runner
        .run_next_step()
        .expect("human review decision should not fail development");
    loop_runner
        .run_next_step()
        .expect("human review evidence may reach output review before M1-06");
    drop(loop_runner);
    drop(step_runner);

    let persisted = read_run(&runs_root.join("review-patch-run"));
    assert_eq!(
        step_status(&persisted, LoopStepName::Development),
        LoopStepStatus::Completed
    );
    assert_eq!(
        step_status(&persisted, LoopStepName::OutputReview),
        LoopStepStatus::Passed
    );
    let decision = single_policy_decision(&persisted);
    assert_eq!(decision.decision, PatchDecisionKind::RequiresHumanReview);
    assert!(decision.requires_human_review);
    assert!(decision.apply_requested);
    assert!(!decision.applied);
    assert!(
        patch_runner.commands.is_empty(),
        "human-review patches must not be applied automatically"
    );
}

#[test]
fn provider_step_runner_uses_persisted_run_id_for_patch_gate_patch_id() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    std::fs::create_dir_all(&repo).expect("repo dir");

    let setup_provider = provider_for_development_patch(fixture("allowed-doc.diff"));
    let mut setup_patch_runner = RecordingPatchRunner::default();
    let mut setup_step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&setup_provider, "fake-model", 30_000)
        .with_ticket(ticket())
        .with_patch_gate(
            patch_gate_config(&repo, false, true),
            &mut setup_patch_runner,
        );
    let setup_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "workspace-directory",
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut setup_step_runner,
    )
    .expect("start loop");
    drop(setup_runner);
    drop(setup_step_runner);

    let run_dir = runs_root.join("workspace-directory");
    let mut run = read_run(&run_dir);
    run.run_id = "authoritative-run-id".to_string();
    write_run(&run_dir, &run);

    let provider = provider_for_development_patch(fixture("allowed-doc.diff"));
    let mut patch_runner = RecordingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(ticket())
        .with_patch_gate(patch_gate_config(&repo, false, true), &mut patch_runner);
    let mut loop_runner =
        LoopRunner::resume(&runs_root, "workspace-directory", &mut step_runner).expect("resume");
    finish_steps_before_development(&mut loop_runner);
    loop_runner
        .run_next_step()
        .expect("development records policy evidence");
    drop(loop_runner);
    drop(step_runner);

    let persisted = read_run(&run_dir);
    let decision = single_policy_decision(&persisted);
    assert_eq!(decision.patch_id, "authoritative-run-id");
    assert!(run_dir
        .join("artifacts/authoritative-run-id.policy-decision.json")
        .is_file());
}

#[test]
fn provider_development_is_proposal_only_even_when_artifact_persistence_fails() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    let source = repo.join("docs/example.md");
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("repo docs");
    std::fs::write(&source, "old line\n").expect("source file");
    let apply_ticket = ticket_with_apply(true);
    let provider = provider_for_development_patch(fixture("allowed-doc.diff"));
    let mut patch_runner = MutatingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(apply_ticket.clone())
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(&repo, &apply_ticket, policy(), true),
            &mut patch_runner,
        );
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "proposal-only-persistence-failure",
            &apply_ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&apply_ticket),
        ),
        &mut step_runner,
    )
    .expect("start loop");
    finish_steps_before_development(&mut loop_runner);
    let run_dir = runs_root.join("proposal-only-persistence-failure");
    crate::artifact_safety::write_private_fixture(
        run_dir.join("artifacts/05-development.json"),
        b"occupied development artifact",
    )
    .expect("force Development artifact persistence failure");

    let error = loop_runner
        .run_next_step()
        .expect_err("artifact persistence must fail after policy gating");
    drop(loop_runner);
    drop(step_runner);

    assert!(error.to_string().contains("collision"), "{error}");
    let decision: PolicyDecision = serde_json::from_slice(
        &std::fs::read(
            run_dir.join("artifacts/proposal-only-persistence-failure.policy-decision.json"),
        )
        .expect("policy decision artifact"),
    )
    .expect("policy decision JSON");
    assert!(
        decision.apply_requested,
        "ticket apply intent remains auditable"
    );
    assert!(
        !decision.applied,
        "provider proposals must never be applied in place"
    );
    assert_eq!(
        patch_runner.commands,
        vec![PatchCommand::GitApplyCheck],
        "proposal validation may check applicability but must never execute GitApply"
    );
    assert_eq!(
        std::fs::read_to_string(&source).expect("source content"),
        "old line\n",
        "a later evidence persistence failure must leave the source untouched"
    );
}

#[test]
fn provider_step_runner_maps_developer_blocked_and_reviewer_failed_statuses() {
    let developer_blocked = r#"{
        "role": "developer",
        "status": "blocked",
        "summary": "Need the approved spec before proposing a patch.",
        "changed_files": [],
        "requires_human_review": true
    }"#;
    let developer = run_scripted_step(LoopStepName::Development, developer_blocked);
    assert_eq!(developer.status, LoopStepStatus::Blocked);

    let reviewer_rejected = r#"{
        "role": "output_reviewer",
        "decision": "reject",
        "summary": "The patch violates a forbidden path.",
        "blocking_issues": [
            {
                "summary": "Forbidden path changed.",
                "evidence": "diff --git a/forbidden b/forbidden"
            }
        ],
        "non_blocking_issues": []
    }"#;
    let reviewer = run_scripted_step(LoopStepName::OutputReview, reviewer_rejected);
    assert_eq!(reviewer.status, LoopStepStatus::Failed);

    let reviewer_requested_changes = r#"{
        "role": "spec_reviewer",
        "decision": "request_changes",
        "summary": "The spec needs narrower acceptance criteria.",
        "blocking_issues": [
            {
                "summary": "Acceptance criteria are too broad.",
                "evidence": "ticket acceptance criteria"
            }
        ],
        "non_blocking_issues": []
    }"#;
    let reviewer = run_scripted_step(LoopStepName::SpecReview, reviewer_requested_changes);
    assert_eq!(reviewer.status, LoopStepStatus::Blocked);
}

#[test]
fn provider_step_runner_persists_blocked_reviewer_state_in_live_loop() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let reviewer_requested_changes = r#"{
        "role": "spec_reviewer",
        "decision": "request_changes",
        "summary": "The spec needs narrower acceptance criteria.",
        "blocking_issues": [
            {
                "summary": "Acceptance criteria are too broad.",
                "evidence": "ticket acceptance criteria"
            }
        ],
        "non_blocking_issues": []
    }"#;
    let provider = FakeProvider::new(vec![
        Ok(model_response(fixture("research.valid.json"))),
        Ok(model_response(fixture("analyzer.valid.json"))),
        Ok(model_response(&spec_writer_response())),
        Ok(model_response(reviewer_requested_changes)),
    ]);
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000).with_ticket(ticket());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "blocked-reviewer-run",
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start loop");

    loop_runner
        .run_to_completion()
        .expect("blocked reviewer should stop cleanly");
    drop(loop_runner);

    let run_dir = runs_root.join("blocked-reviewer-run");
    let persisted = read_run(&run_dir);
    assert_eq!(persisted.status, LoopStatus::Blocked);
    assert_eq!(persisted.current_step, LoopStepName::SpecReview);
    assert_eq!(
        step_status(&persisted, LoopStepName::SpecReview),
        LoopStepStatus::Blocked
    );
    let spec_review = persisted
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::SpecReview)
        .expect("spec review record");
    assert!(spec_review.artifact_path.is_some());
    assert!(spec_review.artifact_digest.is_some());
    assert_file_contains(
        &run_dir.join("responses/04-spec-review.raw.txt"),
        "request_changes",
    );
    assert!(
        !run_dir.join("prompts/05-development.prompt.md").exists(),
        "blocked reviewer recovery state must not advance to development"
    );
    assert_file_contains(
        &run_dir.join("log.md"),
        "finished step SpecReview as Blocked",
    );
}

#[test]
fn provider_step_runner_persists_failed_early_role_artifact_without_advancing() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let rejected = json!({
        "role": "spec_reviewer",
        "decision": "reject",
        "summary": "The proposed spec violates the ticket boundary.",
        "blocking_issues": [{
            "summary": "Forbidden scope",
            "evidence": "The proposed spec changes a forbidden file."
        }],
        "non_blocking_issues": []
    })
    .to_string();
    let provider = FakeProvider::new(vec![
        Ok(model_response(fixture("research.valid.json"))),
        Ok(model_response(fixture("analyzer.valid.json"))),
        Ok(model_response(&spec_writer_response())),
        Ok(model_response(&rejected)),
    ]);
    let mut step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000).with_ticket(ticket());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "failed-early-artifact",
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start loop");

    loop_runner.run_to_completion().expect("run to rejection");
    drop(loop_runner);

    let persisted = read_run(&runs_root.join("failed-early-artifact"));
    let review = persisted
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::SpecReview)
        .expect("spec review record");
    assert_eq!(review.status, LoopStepStatus::Failed);
    assert!(review.artifact_path.is_some());
    assert!(review.artifact_digest.is_some());
    assert_eq!(persisted.status, LoopStatus::Failed);
    assert_eq!(provider.requests().expect("provider requests").len(), 4);
    assert!(!runs_root
        .join("failed-early-artifact/prompts/05-development.prompt.md")
        .exists());
}

#[test]
fn provider_step_runner_refuses_testing_and_eval_report_without_locked_evaluation_publisher() {
    let provider = FakeProvider::new(Vec::new());
    let mut runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000);

    for step in [LoopStepName::Testing, LoopStepName::EvalReport] {
        let request_error = runner
            .step_request(step)
            .expect_err("provider runner must not prepare evaluation");
        let run_error = runner
            .run_step(step, "")
            .expect_err("provider runner must not execute evaluation");

        assert!(request_error.to_string().contains("dedicated locked evaluation"));
        assert!(run_error.to_string().contains("dedicated locked evaluation"));
    }

    assert!(provider.requests().expect("provider requests").is_empty());
}

fn request_audit_user_prompt(request_audit: &str) -> String {
    let value: serde_json::Value =
        serde_json::from_str(request_audit).expect("request audit should be JSON");
    value["messages"][0]["content"]
        .as_str()
        .expect("user prompt")
        .to_string()
}

fn run_scripted_step(step: LoopStepName, content: &str) -> seaf_loop::StepOutput {
    let provider = FakeProvider::new(vec![Ok(model_response(content))]);
    let mut runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000);
    let request = runner.step_request(step).expect("step request");
    runner.run_step(step, &request).expect("step output")
}

fn model_response(content: &str) -> ModelResponse {
    ModelResponse {
        content: content.to_string(),
        latency_ms: 7,
        raw_provider_metadata: json!({ "provider": "fake" }),
    }
}

fn provider_for_development_patch(patch: &str) -> FakeProvider {
    FakeProvider::new(vec![
        Ok(model_response(fixture("research.valid.json"))),
        Ok(model_response(fixture("analyzer.valid.json"))),
        Ok(model_response(&spec_writer_response())),
        Ok(model_response(&spec_review_approved_response())),
        Ok(model_response(&developer_response(patch))),
    ])
}

fn spec_writer_response() -> String {
    json!({
        "role": "spec_writer",
        "status": "passed",
        "summary": "Implement the requested patch.",
        "findings": [],
        "risks": [],
        "next_step_recommendation": "Review the implementation plan."
    })
    .to_string()
}

fn spec_review_approved_response() -> String {
    json!({
        "role": "spec_reviewer",
        "decision": "approve_spec",
        "summary": "The spec is narrow enough to implement.",
        "blocking_issues": [],
        "non_blocking_issues": []
    })
    .to_string()
}

fn output_review_approved_response() -> String {
    json!({
        "role": "output_reviewer",
        "decision": "approve_for_tests",
        "summary": "The exact gated patch satisfies the approved spec.",
        "blocking_issues": [],
        "non_blocking_issues": []
    })
    .to_string()
}

fn ticket() -> TicketSpec {
    TicketSpec {
        ticket_id: "P3-006".to_string(),
        goal_id: "phase-1".to_string(),
        title: "Add provider-backed step runner".to_string(),
        status: TicketStatus::Ready,
        priority: seaf_core::TicketPriority::P3,
        problem: "Provider calls must be auditable.".to_string(),
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
        acceptance_criteria: vec!["Every provider request and response is persisted.".to_string()],
        eval: None,
    }
}

fn ticket_with_apply(apply_patch: bool) -> TicketSpec {
    let mut ticket = ticket();
    ticket.autonomy.apply_patch = apply_patch;
    ticket
}

fn ticket_with_context(relevant_files: Vec<&str>, forbidden_files: Vec<&str>) -> TicketSpec {
    TicketSpec {
        context: TicketContext {
            relevant_files: relevant_files.into_iter().map(str::to_string).collect(),
            forbidden_files: forbidden_files.into_iter().map(str::to_string).collect(),
        },
        ..ticket()
    }
}

fn patch_gate_config(
    repository_root: &Path,
    apply_patch: bool,
    worktree_clean: bool,
) -> ProviderPatchGateConfig {
    ProviderPatchGateConfig::for_ticket(
        repository_root,
        &ticket_with_apply(apply_patch),
        policy(),
        worktree_clean,
    )
}

fn policy() -> Policy {
    Policy {
        policy_id: "test-policy".to_string(),
        default_autonomy_level: 1,
        forbidden_paths: vec!["secrets/**".to_string()],
        requires_human_review: vec!["dependency_changes".to_string()],
        allowed_without_review: Vec::new(),
    }
}

fn developer_response(patch: &str) -> String {
    json!({
        "role": "developer",
        "status": "patch_proposed",
        "summary": "Proposed a focused patch.",
        "changed_files": ["docs/example.md"],
        "requires_human_review": false,
        "patch": patch
    })
    .to_string()
}

fn forbidden_patch() -> String {
    r#"diff --git a/secrets/token.txt b/secrets/token.txt
index 1111111..2222222 100644
--- a/secrets/token.txt
+++ b/secrets/token.txt
@@ -1 +1 @@
-old
+new
"#
    .to_string()
}

fn human_review_patch() -> String {
    r#"diff --git a/Cargo.lock b/Cargo.lock
index 1111111..2222222 100644
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1 +1 @@
-old
+new
"#
    .to_string()
}

fn malformed_patch() -> String {
    r#"diff --git a/docs/example.md b/docs/example.md unexpected-token
--- a/docs/example.md
+++ b/docs/example.md
@@ -1 +1 @@
-old line
+new line
"#
    .to_string()
}

fn binary_patch() -> String {
    r#"diff --git a/assets/image.bin b/assets/image.bin
new file mode 100644
Binary files /dev/null and b/assets/image.bin differ
"#
    .to_string()
}

fn finish_steps_before_development<R: StepRunner + ?Sized>(loop_runner: &mut LoopRunner<'_, R>) {
    for step in [
        LoopStepName::Research,
        LoopStepName::Analysis,
        LoopStepName::SpecCreation,
        LoopStepName::SpecReview,
    ] {
        assert!(loop_runner
            .run_next_step()
            .unwrap_or_else(|error| panic!("run {step:?}: {error}")));
    }
}

fn read_run(run_dir: &Path) -> LoopRun {
    let run_json = std::fs::read_to_string(run_dir.join("run.json")).expect("run json");
    serde_json::from_str(&run_json).expect("run json")
}

fn persisted_step_artifact(
    run_dir: &Path,
    run: &LoopRun,
    step: LoopStepName,
) -> (String, String, serde_json::Value) {
    let record = run
        .steps
        .iter()
        .find(|record| record.name == step)
        .expect("step record");
    let path = record.artifact_path.clone().expect("artifact path");
    let digest = record.artifact_digest.clone().expect("artifact digest");
    let artifact =
        serde_json::from_slice(&std::fs::read(run_dir.join(&path)).expect("artifact bytes"))
            .expect("artifact JSON");
    (path, digest, artifact)
}

fn write_run(run_dir: &Path, run: &LoopRun) {
    let mut run_json = serde_json::to_vec_pretty(run).expect("run json");
    run_json.push(b'\n');
    std::fs::write(run_dir.join("run.json"), run_json).expect("write run json");
}

fn single_policy_decision(run: &LoopRun) -> PolicyDecision {
    assert_eq!(
        run.policy_decisions.len(),
        1,
        "expected exactly one persisted policy decision"
    );
    serde_json::from_value(serde_json::to_value(&run.policy_decisions[0]).expect("decision value"))
        .expect("typed policy decision")
}

fn step_status(run: &LoopRun, step: LoopStepName) -> LoopStepStatus {
    run.steps
        .iter()
        .find(|record| record.name == step)
        .expect("step record")
        .status
}

fn context_request(
    repository_root: &Path,
    ticket: &TicketSpec,
    policy_forbidden_paths: Vec<String>,
) -> ContextPackRequest {
    ContextPackRequest::for_ticket(
        repository_root,
        Path::new("prepare-workspace-must-choose-run-directory"),
        ticket,
        &policy_forbidden_paths,
        ContextLimits {
            max_bytes_per_file: 1_024,
            max_total_bytes: 8_192,
        },
    )
}

fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn test_input_digests() -> LoopInputDigests {
    LoopInputDigests {
        ticket: canonical_sha256_digest(&ticket()).expect("default ticket digest"),
        policy: "b".repeat(64),
        config: "c".repeat(64),
        repository: "d".repeat(64),
        eval_config: None,
    }
}

fn test_input_digests_for(ticket: &TicketSpec) -> LoopInputDigests {
    LoopInputDigests {
        ticket: canonical_sha256_digest(ticket).expect("ticket digest"),
        ..test_input_digests()
    }
}

#[derive(Debug, Clone, Copy)]
enum ResumeArtifactMutation {
    Missing,
    TamperedResponse,
    NonCanonical,
    WrongRun,
    WrongRole,
    WrongStep,
    WrongDigest,
}

#[derive(Debug, Clone, Copy)]
enum DownstreamArtifactTarget {
    Development,
    OutputReview,
}

#[derive(Debug, Clone, Copy)]
enum DownstreamArtifactMutation {
    Missing,
    TamperedResponse,
    NonCanonical,
    WrongRun,
    WrongRole,
    WrongStep,
    WrongDigest,
    SubstitutedPatch,
    CoordinatedSubstitutedPatch,
    PolicyMismatch,
}

#[derive(Debug, Clone, Copy)]
enum PreparedTicketMismatch {
    TicketId,
    GoalId,
    Digest,
}

fn assert_prepared_ticket_mismatch_fails_closed(mismatch: PreparedTicketMismatch) {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let repo = temp_dir.path().join("repo");
    std::fs::create_dir_all(&runs_root).expect("runs root");
    std::fs::create_dir_all(repo.join("src")).expect("source directory");
    std::fs::write(repo.join("src/lib.rs"), "pub fn authority() {}\n").expect("source file");
    let authoritative = ticket_with_context(vec!["src/lib.rs"], Vec::new());
    let mut supplied = authoritative.clone();
    let mut input_digests = test_input_digests_for(&authoritative);
    match mismatch {
        PreparedTicketMismatch::TicketId => supplied.ticket_id = "substituted-ticket".to_string(),
        PreparedTicketMismatch::GoalId => supplied.goal_id = "substituted-goal".to_string(),
        PreparedTicketMismatch::Digest => input_digests.ticket = "f".repeat(64),
    }
    let before = read_tree_bytes(&runs_root);
    let provider = FakeProvider::new(Vec::new());
    let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(supplied.clone())
        .with_context_pack_request(context_request(&repo, &supplied, Vec::new()));
    let run_id = format!("prepared-ticket-{mismatch:?}").to_ascii_lowercase();

    let error = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            &run_id,
            &authoritative,
            "fake-provider",
            "fake-model",
            input_digests,
        ),
        &mut step_runner,
    )
    .expect_err("substituted prepared ticket must fail closed");

    let expected = match mismatch {
        PreparedTicketMismatch::TicketId => "ticket_id mismatch",
        PreparedTicketMismatch::GoalId => "goal_id mismatch",
        PreparedTicketMismatch::Digest => "ticket digest mismatch",
    };
    assert!(
        error.to_string().contains(expected),
        "{mismatch:?}: {error}"
    );
    assert_eq!(read_tree_bytes(&runs_root), before);
    assert!(provider.requests().expect("provider requests").is_empty());
}

fn assert_resume_artifact_rejected_without_mutation(mutation: ResumeArtifactMutation) {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let repo = temp_dir.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).expect("repo source directory");
    std::fs::write(repo.join("src/lib.rs"), "pub fn changed_context() {}\n")
        .expect("repository context");
    let ticket = ticket_with_context(vec!["src/lib.rs"], Vec::new());
    let run_id = format!("resume-invalid-{mutation:?}").to_ascii_lowercase();
    let start_provider =
        FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let mut start_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&start_provider, "fake-model", 30_000).with_ticket(ticket.clone());
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            &run_id,
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut start_runner,
    )
    .expect("start loop");
    loop_runner.run_next_step().expect("research");
    drop(loop_runner);

    let run_dir = runs_root.join(&run_id);
    let mut verified = read_run(&run_dir);
    let record = verified
        .steps
        .iter_mut()
        .find(|record| record.name == LoopStepName::Research)
        .expect("research record");
    let artifact_path = record.artifact_path.clone().expect("artifact path");
    let absolute_artifact_path = run_dir.join(&artifact_path);
    let mut artifact: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&absolute_artifact_path).expect("artifact bytes"))
            .expect("artifact JSON");

    match mutation {
        ResumeArtifactMutation::Missing => {
            std::fs::remove_file(&absolute_artifact_path).expect("remove artifact");
        }
        ResumeArtifactMutation::TamperedResponse => {
            artifact["response"]["summary"] = json!("tampered validated response");
            std::fs::write(
                &absolute_artifact_path,
                canonical_json_bytes(&artifact).expect("canonical artifact"),
            )
            .expect("write artifact");
            record.artifact_digest =
                Some(canonical_sha256_digest(&artifact).expect("rewritten outer artifact digest"));
        }
        ResumeArtifactMutation::NonCanonical => {
            let mut bytes = canonical_json_bytes(&artifact).expect("canonical artifact");
            bytes.push(b'\n');
            std::fs::write(&absolute_artifact_path, bytes).expect("write noncanonical artifact");
        }
        ResumeArtifactMutation::WrongRun => {
            artifact["run_id"] = json!("different-run");
            rewrite_artifact_and_verified_digest(&absolute_artifact_path, &artifact, record);
        }
        ResumeArtifactMutation::WrongRole => {
            artifact["role"] = json!(Role::Analyzer);
            rewrite_artifact_and_verified_digest(&absolute_artifact_path, &artifact, record);
        }
        ResumeArtifactMutation::WrongStep => {
            artifact["step"] = json!(LoopStepName::Analysis);
            rewrite_artifact_and_verified_digest(&absolute_artifact_path, &artifact, record);
        }
        ResumeArtifactMutation::WrongDigest => {
            record.artifact_digest = Some("f".repeat(64));
        }
    }
    persist_verified_resume_authority(&run_dir, &verified);

    let before = read_tree_bytes(&run_dir);
    let provider = FakeProvider::new(Vec::new());
    let mut resume_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repo, &ticket, Vec::new()));
    let error = LoopRunner::resume_verified(&runs_root, verified, &mut resume_runner)
        .expect_err("invalid role artifact must reject resume");

    assert!(
        error.to_string().contains("role artifact") || error.to_string().contains("artifact"),
        "{mutation:?} should report the artifact failure, got {error}"
    );
    assert_eq!(
        read_tree_bytes(&run_dir),
        before,
        "{mutation:?} must not mutate any loop workspace file"
    );
    assert!(
        provider.requests().expect("provider requests").is_empty(),
        "{mutation:?} must fail before provider invocation"
    );
}

fn assert_downstream_artifact_rejected_without_mutation(
    target: DownstreamArtifactTarget,
    mutation: DownstreamArtifactMutation,
) {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let repo = temp_dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo directory");
    let run_id = format!("invalid-{target:?}-{mutation:?}").to_ascii_lowercase();
    let mut responses = vec![
        Ok(model_response(fixture("research.valid.json"))),
        Ok(model_response(fixture("analyzer.valid.json"))),
        Ok(model_response(&spec_writer_response())),
        Ok(model_response(&spec_review_approved_response())),
        Ok(model_response(&developer_response(fixture(
            "allowed-doc.diff",
        )))),
    ];
    if matches!(target, DownstreamArtifactTarget::OutputReview) {
        responses.push(Ok(model_response(&output_review_approved_response())));
    }
    let provider = FakeProvider::new(responses);
    let mut patch_runner = RecordingPatchRunner::default();
    let ticket = ticket();
    let mut step_runner = ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_patch_gate(patch_gate_config(&repo, false, true), &mut patch_runner);
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            &run_id,
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&ticket),
        ),
        &mut step_runner,
    )
    .expect("start loop");
    finish_steps_before_development(&mut loop_runner);
    loop_runner.run_next_step().expect("development");
    if matches!(target, DownstreamArtifactTarget::OutputReview) {
        loop_runner.run_next_step().expect("output review");
    }
    drop(loop_runner);
    drop(step_runner);

    let run_dir = runs_root.join(&run_id);
    let mut verified = read_run(&run_dir);
    let target_step = match target {
        DownstreamArtifactTarget::Development => LoopStepName::Development,
        DownstreamArtifactTarget::OutputReview => LoopStepName::OutputReview,
    };
    let record = verified
        .steps
        .iter_mut()
        .find(|record| record.name == target_step)
        .expect("target record");
    let artifact_path = record.artifact_path.clone().expect("artifact path");
    let absolute_artifact_path = run_dir.join(&artifact_path);
    let mut artifact: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&absolute_artifact_path).expect("artifact bytes"))
            .expect("artifact JSON");

    match mutation {
        DownstreamArtifactMutation::Missing => {
            std::fs::remove_file(&absolute_artifact_path).expect("remove artifact");
        }
        DownstreamArtifactMutation::TamperedResponse => {
            let response_field = match target {
                DownstreamArtifactTarget::Development => "developer_response",
                DownstreamArtifactTarget::OutputReview => "response",
            };
            artifact[response_field]["summary"] = json!("tampered validated response");
            rewrite_artifact_and_verified_digest(&absolute_artifact_path, &artifact, record);
        }
        DownstreamArtifactMutation::NonCanonical => {
            let mut bytes = canonical_json_bytes(&artifact).expect("canonical artifact");
            bytes.push(b'\n');
            std::fs::write(&absolute_artifact_path, bytes).expect("write noncanonical artifact");
        }
        DownstreamArtifactMutation::WrongRun => {
            artifact["run_id"] = json!("different-run");
            rewrite_artifact_and_verified_digest(&absolute_artifact_path, &artifact, record);
        }
        DownstreamArtifactMutation::WrongRole => {
            artifact["role"] = json!(Role::Analyzer);
            rewrite_artifact_and_verified_digest(&absolute_artifact_path, &artifact, record);
        }
        DownstreamArtifactMutation::WrongStep => {
            artifact["step"] = json!(LoopStepName::Research);
            rewrite_artifact_and_verified_digest(&absolute_artifact_path, &artifact, record);
        }
        DownstreamArtifactMutation::WrongDigest => {
            record.artifact_digest = Some("f".repeat(64));
        }
        DownstreamArtifactMutation::SubstitutedPatch => {
            assert!(matches!(target, DownstreamArtifactTarget::Development));
            let patch = forbidden_patch();
            let digest = sha256(patch.as_bytes());
            artifact["patch"] = json!(patch);
            artifact["patch_digest"] = json!(digest);
            artifact["developer_response"]["patch"] = artifact["patch"].clone();
            artifact["developer_response_digest"] =
                json!(canonical_sha256_digest(&artifact["developer_response"])
                    .expect("substituted developer response digest"));
            artifact["policy_decision"]["patch_sha256"] = artifact["patch_digest"].clone();
            rewrite_artifact_and_verified_digest(&absolute_artifact_path, &artifact, record);
        }
        DownstreamArtifactMutation::CoordinatedSubstitutedPatch => {
            assert!(matches!(target, DownstreamArtifactTarget::Development));
            let patch = forbidden_patch();
            let digest = sha256(patch.as_bytes());
            artifact["patch"] = json!(patch);
            artifact["patch_digest"] = json!(digest);
            artifact["developer_response"]["patch"] = artifact["patch"].clone();
            artifact["developer_response_digest"] =
                json!(canonical_sha256_digest(&artifact["developer_response"])
                    .expect("substituted developer response digest"));
            artifact["changed_paths"] = json!(["docs/example.md"]);
            artifact["policy_decision"]["patch_sha256"] = artifact["patch_digest"].clone();
            artifact["policy_decision"]["changed_paths"] = artifact["changed_paths"].clone();
            verified.policy_decisions[0]
                .insert("patch_sha256".to_string(), artifact["patch_digest"].clone());
            verified.policy_decisions[0].insert(
                "changed_paths".to_string(),
                artifact["changed_paths"].clone(),
            );
            rewrite_artifact_and_verified_digest(&absolute_artifact_path, &artifact, record);
        }
        DownstreamArtifactMutation::PolicyMismatch => {
            assert!(matches!(target, DownstreamArtifactTarget::Development));
            verified.policy_decisions[0].insert("decision".to_string(), json!("rejected"));
        }
    }
    persist_verified_resume_authority(&run_dir, &verified);

    let before = read_tree_bytes(&run_dir);
    let resume_provider = FakeProvider::new(Vec::new());
    let mut resume_patch_runner = RecordingPatchRunner::default();
    let mut resume_runner = ProviderStepRunner::new_legacy_unit_test_harness(&resume_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_patch_gate(
            patch_gate_config(&repo, false, true),
            &mut resume_patch_runner,
        );
    let error = LoopRunner::resume_verified(&runs_root, verified, &mut resume_runner)
        .expect_err("invalid downstream artifact must fail closed");

    assert!(
        error.to_string().contains("artifact")
            || error.to_string().contains("evidence")
            || error.to_string().contains("policy decision"),
        "{target:?}/{mutation:?}: {error}"
    );
    assert_eq!(read_tree_bytes(&run_dir), before, "{target:?}/{mutation:?}");
    assert!(
        resume_provider
            .requests()
            .expect("provider requests")
            .is_empty(),
        "{target:?}/{mutation:?} must fail before provider invocation"
    );
    assert!(
        resume_patch_runner.commands.is_empty(),
        "{target:?}/{mutation:?} must fail before patch gating"
    );
}

fn rewrite_artifact_and_verified_digest(
    path: &Path,
    artifact: &serde_json::Value,
    record: &mut seaf_core::LoopStepRecord,
) {
    std::fs::write(
        path,
        canonical_json_bytes(artifact).expect("canonical artifact"),
    )
    .expect("write artifact");
    record.artifact_digest =
        Some(canonical_sha256_digest(artifact).expect("rewritten outer artifact digest"));
}

fn persist_verified_resume_authority(run_dir: &Path, verified: &LoopRun) {
    crate::state::write_raw_canonical_run_fixture(&run_dir.join("run.json"), verified)
        .expect("persist exact verified resume authority");
}

fn read_tree_bytes(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
        let mut entries = std::fs::read_dir(current)
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
                    std::fs::read(path).expect("tree file"),
                ));
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, &mut files);
    files
}

fn assert_file_contains(path: &Path, expected: &str) {
    let content = std::fs::read_to_string(path).expect("read file");
    assert!(
        content.contains(expected),
        "{path:?} should contain {expected:?}; got {content:?}"
    );
}

#[derive(Default)]
struct RecordingPatchRunner {
    commands: Vec<PatchCommand>,
}

#[derive(Default)]
struct MutatingPatchRunner {
    commands: Vec<PatchCommand>,
}

impl PatchCommandRunner for MutatingPatchRunner {
    fn run(
        &mut self,
        repo_root: &Path,
        command: PatchCommand,
        _patch: &str,
    ) -> Result<CommandOutput, PatchGateError> {
        self.commands.push(command);
        if command == PatchCommand::GitApply {
            std::fs::write(repo_root.join("docs/example.md"), "new line\n")
                .expect("simulate source mutation");
        }
        Ok(CommandOutput::success())
    }
}

impl PatchCommandRunner for RecordingPatchRunner {
    fn run(
        &mut self,
        _repo_root: &Path,
        command: PatchCommand,
        _patch: &str,
    ) -> Result<CommandOutput, PatchGateError> {
        self.commands.push(command);
        Ok(CommandOutput::success())
    }
}
