use std::path::Path;

use seaf_core::{LoopStepName, LoopStepStatus, TicketContext, TicketSpec, TicketStatus};
use seaf_loop::{LoopRunner, LoopRunnerConfig, ProviderStepRunner, Role, StepRunner};
use seaf_models::{FakeProvider, ModelError, ModelResponse};
use serde_json::json;

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
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000);
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "parse-failure-run",
            &ticket(),
            "fake-provider",
            "fake-model",
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
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000);
    let mut loop_runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "repair-failure-run",
            &ticket(),
            "fake-provider",
            "fake-model",
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

fn assert_file_contains(path: &Path, expected: &str) {
    let content = std::fs::read_to_string(path).expect("read file");
    assert!(
        content.contains(expected),
        "{path:?} should contain {expected:?}; got {content:?}"
    );
}
