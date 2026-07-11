use std::path::Path;

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, LoopInputDigests, LoopRun, LoopStatus,
    LoopStepName, LoopStepStatus, Policy, TicketContext, TicketSpec, TicketStatus,
};
use seaf_loop::{
    CommandOutput, ContextLimits, ContextManifest, ContextPackRequest, LoopRunner,
    LoopRunnerConfig, PatchCommand, PatchCommandRunner, PatchDecisionKind, PatchGateError,
    PolicyDecision, ProviderPatchGateConfig, ProviderStepRunner, Role, StepRunner,
    UNTRUSTED_CONTEXT_MARKER,
};
use seaf_models::{FakeProvider, ModelError, ModelResponse};
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
fn provider_step_runner_sends_role_request_and_maps_common_passed_status() {
    let provider = FakeProvider::new(vec![Ok(model_response(fixture("research.valid.json")))]);
    let mut runner = ProviderStepRunner::new(&provider, "fake-model", 12_345);

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
        ProviderStepRunner::new(&provider, "fake-model", 30_000).with_ticket(ticket.clone());
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
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
        ProviderStepRunner::new(&provider, "fake-model", 30_000).with_ticket(ticket.clone());
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
        ProviderStepRunner::new(&start_provider, "fake-model", 30_000).with_ticket(ticket.clone());
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
        ProviderStepRunner::new(&resume_provider, "fake-model", 30_000).with_ticket(ticket.clone());
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
    let mut runner = ProviderStepRunner::new(&provider, "fake-model", 30_000);

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
    let mut runner = ProviderStepRunner::new(&provider, "fake-model", 30_000);

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
fn provider_step_runner_persists_provider_response_when_parse_failure_stops_loop() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let provider = FakeProvider::new(vec![Ok(model_response(fixture(
        "research.invalid_missing_status.json",
    )))]);
    let mut step_runner =
        ProviderStepRunner::new(&provider, "fake-model", 30_000).with_ticket(ticket());
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

    let error = loop_runner
        .run_next_step()
        .expect_err("parse failure should stop loop");

    assert!(
        error.to_string().contains("invalid role response"),
        "parse error should remain useful, got {error}"
    );
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
        ProviderStepRunner::new(&provider, "fake-model", 30_000).with_ticket(ticket());
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

    let error = loop_runner
        .run_next_step()
        .expect_err("repair provider failure should stop loop");

    assert!(
        error.to_string().contains("repair service failed"),
        "provider error should remain useful, got {error}"
    );
    let response_path = runs_root.join("repair-failure-run/responses/01-research.raw.txt");
    assert_file_contains(&response_path, "initial provider response");
    assert_file_contains(&response_path, "not json");
    assert_file_contains(&response_path, "repair provider request");
    assert_file_contains(&response_path, "repair provider error");
    assert_file_contains(&response_path, "repair service failed");
}

#[test]
fn provider_step_runner_surfaces_model_timeout_without_retrying_a_role_step() {
    let provider = FakeProvider::new(vec![Err(ModelError::timeout(
        "research model timed out",
        10,
        json!({ "provider": "fake" }),
    ))]);
    let mut runner = ProviderStepRunner::new(&provider, "fake-model", 10);

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
        ProviderStepRunner::new(&provider, "fake-model", 10).with_ticket(ticket());
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

    let error = loop_runner
        .run_next_step()
        .expect_err("timeout should stop the live loop step");

    assert!(
        error.to_string().contains("research model timed out"),
        "provider timeout should remain visible, got {error}"
    );
    let response_path = runs_root.join("timeout-artifact-run/responses/01-research.raw.txt");
    assert_file_contains(&response_path, "provider request failed for Research");
    assert_file_contains(&response_path, "\"kind\": \"timeout\"");
    assert_file_contains(&response_path, "research model timed out");
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
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
    let mut forbidden_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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

    let mut allowed_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
    let mut start_runner = ProviderStepRunner::new(&start_provider, "fake-model", 30_000)
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
    let mut resume_runner = ProviderStepRunner::new(&resume_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repo, &ticket, Vec::new()));
    let mut resumed =
        LoopRunner::resume(&runs_root, "resume-context-run", &mut resume_runner).expect("resume");

    resumed.run_next_step().expect("run analysis step");

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
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
    assert!(
        patch_runner.commands.is_empty(),
        "forbidden patches must not reach git apply --check"
    );
}

#[test]
fn provider_step_runner_replaces_stale_policy_decision_when_development_is_rerun() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    std::fs::create_dir_all(&repo).expect("repo dir");
    let rejected_patch = forbidden_patch();

    let provider = FakeProvider::new(vec![
        Ok(model_response(fixture("research.valid.json"))),
        Ok(model_response(fixture("analyzer.valid.json"))),
        Ok(model_response(&spec_writer_response())),
        Ok(model_response(&spec_review_approved_response())),
        Ok(model_response(&developer_response(&rejected_patch))),
        Ok(model_response(&developer_response(fixture(
            "allowed-doc.diff",
        )))),
    ]);
    let mut patch_runner = RecordingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket())
        .with_patch_gate(patch_gate_config(&repo, false, true), &mut patch_runner);
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "rerun-policy-run",
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
        .expect("first development attempt records rejection");
    assert_eq!(loop_runner.run().status, LoopStatus::Failed);

    let mut loop_runner = loop_runner
        .rerun_from(LoopStepName::Development)
        .expect("rerun development");
    loop_runner
        .run_next_step()
        .expect("second development attempt records replacement decision");
    drop(loop_runner);
    drop(step_runner);

    let persisted = read_run(&runs_root.join("rerun-policy-run"));
    let decision = single_policy_decision(&persisted);
    assert_eq!(decision.patch_id, "rerun-policy-run");
    assert_eq!(decision.decision, PatchDecisionKind::Allowed);
    assert_eq!(decision.changed_paths, vec!["docs/example.md"]);
}

#[test]
fn provider_step_runner_human_review_patch_persists_without_failing_or_applying() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let runs_root = temp_dir.path().join("runs");
    std::fs::create_dir_all(&repo).expect("repo dir");
    let patch = human_review_patch();

    let provider = provider_for_development_patch(&patch);
    let mut patch_runner = RecordingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
    drop(loop_runner);
    drop(step_runner);

    let persisted = read_run(&runs_root.join("review-patch-run"));
    assert_eq!(
        step_status(&persisted, LoopStepName::Development),
        LoopStepStatus::Completed
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
    let mut setup_step_runner = ProviderStepRunner::new(&setup_provider, "fake-model", 30_000)
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
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
fn provider_step_runner_only_attempts_apply_when_autonomy_and_clean_guard_allow() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let clean_runs_root = temp_dir.path().join("clean-runs");
    let dirty_runs_root = temp_dir.path().join("dirty-runs");
    std::fs::create_dir_all(&repo).expect("repo dir");
    let apply_ticket = ticket_with_apply(true);

    let clean_provider = provider_for_development_patch(fixture("allowed-doc.diff"));
    let mut clean_patch_runner = RecordingPatchRunner::default();
    let mut clean_step_runner = ProviderStepRunner::new(&clean_provider, "fake-model", 30_000)
        .with_ticket(apply_ticket.clone())
        .with_patch_gate(
            patch_gate_config(&repo, true, true),
            &mut clean_patch_runner,
        );
    let mut clean_loop = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &clean_runs_root,
            "clean-apply-run",
            &apply_ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&apply_ticket),
        ),
        &mut clean_step_runner,
    )
    .expect("start clean loop");
    finish_steps_before_development(&mut clean_loop);
    clean_loop.run_next_step().expect("run clean apply step");
    drop(clean_loop);
    drop(clean_step_runner);

    assert_eq!(
        clean_patch_runner.commands,
        vec![PatchCommand::GitApplyCheck, PatchCommand::GitApply],
        "clean apply-enabled patches should run check before apply"
    );
    let clean_decision =
        single_policy_decision(&read_run(&clean_runs_root.join("clean-apply-run")));
    assert!(clean_decision.apply_requested);
    assert!(clean_decision.applied);

    let dirty_provider = provider_for_development_patch(fixture("allowed-doc.diff"));
    let mut dirty_patch_runner = RecordingPatchRunner::default();
    let mut dirty_step_runner = ProviderStepRunner::new(&dirty_provider, "fake-model", 30_000)
        .with_ticket(apply_ticket.clone())
        .with_patch_gate(
            patch_gate_config(&repo, true, false),
            &mut dirty_patch_runner,
        );
    let mut dirty_loop = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &dirty_runs_root,
            "dirty-apply-run",
            &apply_ticket,
            "fake-provider",
            "fake-model",
            test_input_digests_for(&apply_ticket),
        ),
        &mut dirty_step_runner,
    )
    .expect("start dirty loop");
    finish_steps_before_development(&mut dirty_loop);
    dirty_loop.run_next_step().expect("run dirty apply step");
    drop(dirty_loop);
    drop(dirty_step_runner);

    let dirty_decision =
        single_policy_decision(&read_run(&dirty_runs_root.join("dirty-apply-run")));
    assert_eq!(dirty_decision.decision, PatchDecisionKind::Rejected);
    assert!(dirty_decision.apply_requested);
    assert!(!dirty_decision.applied);
    let dirty_reason = dirty_decision
        .reasons
        .iter()
        .find(|reason| reason.code == "git_apply_check_failed")
        .expect("dirty guard should be recorded as apply-check failure");
    assert!(dirty_reason
        .details
        .as_deref()
        .unwrap_or_default()
        .contains("worktree is not clean"));
    assert!(
        dirty_patch_runner.commands.is_empty(),
        "dirty worktree guard must not invoke the patch runner"
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
        ProviderStepRunner::new(&provider, "fake-model", 30_000).with_ticket(ticket());
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
        ProviderStepRunner::new(&provider, "fake-model", 30_000).with_ticket(ticket());
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
fn provider_step_runner_keeps_testing_and_eval_report_as_no_model_steps() {
    let provider = FakeProvider::new(Vec::new());
    let mut runner = ProviderStepRunner::new(&provider, "fake-model", 30_000);

    for step in [LoopStepName::Testing, LoopStepName::EvalReport] {
        let request = runner.step_request(step).expect("no-model request");
        let output = runner.run_step(step, &request).expect("no-model step");

        assert_eq!(output.status, LoopStepStatus::Completed);
        assert!(request.contains("no model provider call"));
        assert!(output.response.contains("no model provider call"));
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
    let mut runner = ProviderStepRunner::new(&provider, "fake-model", 30_000);
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
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
        ProviderStepRunner::new(&start_provider, "fake-model", 30_000).with_ticket(ticket.clone());
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

    let before = read_tree_bytes(&run_dir);
    let provider = FakeProvider::new(Vec::new());
    let mut resume_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
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
