use super::*;

use std::{
    collections::{BTreeMap, VecDeque},
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::Mutex,
};

use seaf_core::{
    EvalCommandConfig, EvalConfig, EvalGroup, LoopInputDigests, LoopStepStatus,
    ProviderExchangeKind, ProviderExchangeOutcome, ProviderExchangePhase, ProviderExchangeRecord,
    ProviderRole, TicketAutonomy, TicketContext, TicketPriority, TicketStatus,
};
use seaf_models::{ModelError, ModelProvider, ModelRequest, ModelResponse};

use crate::runner::LoopRunner;

const RAW_SECRET: &str = "configured-provider-recovery-secret";
const SYNTHETIC_CODE_COLLISION: &str = "provider_response_contains_secret";
const REQUEST_PRETTY_BOUNDARY_SECRET: &str = "\",\n  \"messages\"";
const RESPONSE_PRETTY_BOUNDARY_SECRET: &str = "\",\n    \"latency_ms\"";
const REQUEST_RECORD_ONLY_SECRET: &str = "\"request\": {\n    \"digest\"";

struct PanicProvider;

impl ModelProvider for PanicProvider {
    fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        panic!("simulated crash after durable request publication")
    }
}

struct RecordingProvider {
    responses: Mutex<VecDeque<Result<ModelResponse, ModelError>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl RecordingProvider {
    fn new(responses: Vec<Result<ModelResponse, ModelError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl ModelProvider for RecordingProvider {
    fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.requests.lock().expect("requests lock").push(request);
        self.responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .expect("one response per expected provider call")
    }
}

struct RecoveryFixture {
    _temp: tempfile::TempDir,
    repository: PathBuf,
    runs_root: PathBuf,
    run_id: String,
    ticket: TicketSpec,
}

impl RecoveryFixture {
    fn run_directory(&self) -> PathBuf {
        self.runs_root.join(&self.run_id)
    }

    fn context_request(&self) -> ContextPackRequest {
        ContextPackRequest::for_ticket(
            &self.repository,
            Path::new("workspace-selected-by-runner"),
            &self.ticket,
            &[],
            ContextLimits {
                max_bytes_per_file: 1_024,
                max_total_bytes: 8_192,
            },
        )
    }
}

#[test]
fn configured_secret_request_crash_resumes_the_exact_sanitized_request_once() {
    let fixture = recovery_fixture(
        "provider-secret-request-crash",
        &[RAW_SECRET],
        &format!("Investigate {RAW_SECRET} without disclosure."),
    );
    let first_provider = PanicProvider;
    let mut first_step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&first_provider, "fake-model", 30_000)
            .with_ticket(fixture.ticket.clone())
            .with_context_pack_request(fixture.context_request());
    let mut first = LoopRunner::resume(&fixture.runs_root, &fixture.run_id, &mut first_step_runner)
        .expect("prepare first attempt");

    let crash = catch_unwind(AssertUnwindSafe(|| first.run_next_step()));
    assert!(
        crash.is_err(),
        "provider panic must model the request-only crash cut"
    );
    drop(first);

    let interrupted = read_run(&fixture.run_directory());
    assert_eq!(interrupted.provider_exchange_records.len(), 1);
    assert_eq!(
        interrupted.provider_exchange_records[0].phase,
        ProviderExchangePhase::Request
    );
    let request_record = load_provider_exchange_record(
        &fixture.run_directory(),
        &interrupted.provider_exchange_records[0],
    )
    .expect("durable request record");
    let request_bytes =
        load_provider_exchange_request(&fixture.run_directory(), &request_record.request)
            .expect("durable request bytes");
    assert!(!contains(&request_bytes, RAW_SECRET.as_bytes()));
    assert!(contains(
        &request_bytes,
        crate::secret_redaction::REDACTION_MARKER.as_bytes()
    ));
    let exact_request: ModelRequest =
        serde_json::from_slice(&request_bytes).expect("typed sanitized request");

    let resumed_provider = RecordingProvider::new(vec![Ok(response(passed()))]);
    let mut resumed_step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&resumed_provider, "fake-model", 30_000)
            .with_ticket(fixture.ticket.clone())
            .with_context_pack_request(fixture.context_request());
    let mut resumed = LoopRunner::resume(
        &fixture.runs_root,
        &fixture.run_id,
        &mut resumed_step_runner,
    )
    .expect("resume request-only prefix");
    resumed.run_next_step().expect("finish resumed request");

    assert_eq!(resumed_provider.requests(), vec![exact_request]);
    assert_eq!(
        read_run(&fixture.run_directory())
            .provider_exchange_records
            .len(),
        2
    );
}

#[test]
fn durable_raw_safe_secret_failure_with_colliding_corpus_resumes_without_provider_call() {
    let fixture = recovery_fixture(
        "provider-secret-fixed-response",
        &[RAW_SECRET, SYNTHETIC_CODE_COLLISION],
        "Exercise fixed provider failure recovery.",
    );
    let first_provider = RecordingProvider::new(vec![Ok(response(
        serde_json::json!({
            "role": "researcher",
            "status": "passed",
            "summary": format!("unsafe {RAW_SECRET}"),
            "findings": [],
            "risks": [],
            "next_step_recommendation": "Continue."
        })
        .to_string(),
    ))]);
    let observer = |_workspace: &LoopWorkspace,
                    durable: &LoopRun,
                    _coordinates: &ProviderExchangeCoordinates| {
        assert_eq!(durable.provider_exchange_records.len(), 2);
        panic!("simulated crash after durable synthetic response")
    };
    let mut first_step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&first_provider, "fake-model", 30_000)
            .with_ticket(fixture.ticket.clone())
            .with_context_pack_request(fixture.context_request())
            .with_after_response_persist_observer(&observer);
    let mut first = LoopRunner::resume(&fixture.runs_root, &fixture.run_id, &mut first_step_runner)
        .expect("prepare provider response crash");

    let crash = catch_unwind(AssertUnwindSafe(|| first.run_next_step()));
    assert!(
        crash.is_err(),
        "observer panic must leave the durable response cut"
    );
    drop(first);

    let interrupted = read_run(&fixture.run_directory());
    assert_eq!(interrupted.provider_exchange_records.len(), 2);
    let response_record = load_provider_exchange_record(
        &fixture.run_directory(),
        interrupted
            .provider_exchange_records
            .last()
            .expect("response head"),
    )
    .expect("durable response record");
    let audit = load_provider_exchange_response_audit(
        &fixture.run_directory(),
        response_record.response.as_ref().expect("response audit"),
    )
    .expect("load fixed response audit");
    let ProviderExchangeResponseAudit::ProviderFailure { error } = audit else {
        panic!("the unsafe response must become a fixed provider failure")
    };
    assert_eq!(error.metadata["code"], "credential_policy_rejection");

    let resumed_provider = RecordingProvider::new(Vec::new());
    let mut resumed_step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&resumed_provider, "fake-model", 30_000)
            .with_ticket(fixture.ticket.clone())
            .with_context_pack_request(fixture.context_request());
    let mut resumed = LoopRunner::resume(
        &fixture.runs_root,
        &fixture.run_id,
        &mut resumed_step_runner,
    )
    .expect("fixed synthetic response remains replay-safe");
    resumed
        .run_next_step()
        .expect("close recovered provider failure");

    assert!(resumed_provider.requests().is_empty());
    assert_eq!(
        step_status(&read_run(&fixture.run_directory()), LoopStepName::Research),
        LoopStepStatus::Failed
    );
}

#[test]
fn staged_raw_secret_request_or_response_history_rejects_before_call_without_head_change() {
    for case in ["request", "response"] {
        let fixture = recovery_fixture(
            &format!("provider-staged-secret-{case}"),
            &[RAW_SECRET],
            "Reject unsafe staged provider history.",
        );
        let run_directory = fixture.run_directory();
        let coordinates = coordinates(&fixture.run_id);
        let instructions = if case == "request" {
            format!("unsafe {RAW_SECRET}")
        } else {
            "safe staged request".to_string()
        };
        let request = request_with_content(
            serde_json::json!({
                "instructions": instructions,
                "repository_context": null,
                "repository_context_authority": null
            })
            .to_string(),
        );
        let request_bytes = serde_json::to_vec_pretty(&request).expect("request bytes");
        let workspace =
            LoopWorkspace::open(&fixture.runs_root, &fixture.run_id).expect("workspace");
        crate::artifacts::write_step_request(
            &workspace,
            LoopStepName::Research,
            1,
            std::str::from_utf8(&request_bytes).expect("request UTF-8"),
        )
        .expect("conventional prompt");
        let request_reference =
            write_provider_exchange_request(&run_directory, &coordinates, &request_bytes)
                .expect("staged request audit");
        let request_record = ProviderExchangeRecord {
            schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: fixture.run_id.clone(),
            step: LoopStepName::Research,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: None,
            request: request_reference.clone(),
            response: None,
            expansion: None,
            outcome: None,
        };
        let staged_request = stage_provider_exchange_record(&run_directory, &request_record)
            .expect("stage request record");
        if case == "response" {
            persist_provider_exchange_record_reference(&workspace, staged_request.clone())
                .expect("persist safe request head");
            let response_reference = crate::provider_exchange::write_provider_exchange_response(
                &run_directory,
                &coordinates,
                &ProviderExchangeResponseAudit::ModelResponse {
                    response: ModelResponse {
                        content: passed(),
                        latency_ms: 1,
                        raw_provider_metadata: serde_json::json!({"trace": RAW_SECRET}),
                    },
                },
            )
            .expect("raw staged response audit");
            stage_provider_exchange_record(
                &run_directory,
                &ProviderExchangeRecord {
                    schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
                    run_id: fixture.run_id.clone(),
                    step: LoopStepName::Research,
                    role: ProviderRole::Researcher,
                    step_attempt: 1,
                    exchange_index: 1,
                    kind: ProviderExchangeKind::Initial,
                    context_round: None,
                    phase: ProviderExchangePhase::Response,
                    previous_record_digest: Some(staged_request.digest.clone()),
                    request: request_reference,
                    response: Some(response_reference),
                    expansion: None,
                    outcome: Some(ProviderExchangeOutcome::Passed),
                },
            )
            .expect("stage response record");
        }
        let head_before = std::fs::read(run_directory.join("run.json")).expect("run bytes");

        let provider = RecordingProvider::new(Vec::new());
        let mut resumed_step_runner =
            ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
                .with_ticket(fixture.ticket.clone())
                .with_context_pack_request(fixture.context_request());
        let error = LoopRunner::resume(
            &fixture.runs_root,
            &fixture.run_id,
            &mut resumed_step_runner,
        )
        .expect_err("raw staged history must fail before adoption");

        assert!(
            error.to_string().contains("prohibited credential material"),
            "{case}: {error}"
        );
        assert!(provider.requests().is_empty(), "{case}");
        assert_eq!(
            std::fs::read(run_directory.join("run.json")).expect("run bytes"),
            head_before,
            "{case}"
        );
        let expected_head = if case == "response" {
            vec![staged_request]
        } else {
            Vec::new()
        };
        assert_eq!(
            read_run(&run_directory).provider_exchange_records,
            expected_head
        );
    }
}

#[test]
fn staged_request_record_envelope_rejects_before_reconciliation() {
    let fixture = recovery_fixture(
        "provider-staged-record-envelope",
        &[REQUEST_RECORD_ONLY_SECRET],
        "Reject unsafe staged provider record evidence.",
    );
    let run_directory = fixture.run_directory();
    let coordinates = coordinates(&fixture.run_id);
    let request = request_with_content(
        serde_json::json!({
            "instructions": "safe staged request",
            "repository_context": null,
            "repository_context_authority": null
        })
        .to_string(),
    );
    let request_bytes = serde_json::to_vec_pretty(&request).expect("request bytes");
    assert!(!contains(
        &request_bytes,
        REQUEST_RECORD_ONLY_SECRET.as_bytes()
    ));
    let workspace = LoopWorkspace::open(&fixture.runs_root, &fixture.run_id).expect("workspace");
    crate::artifacts::write_step_request(
        &workspace,
        LoopStepName::Research,
        1,
        std::str::from_utf8(&request_bytes).expect("request UTF-8"),
    )
    .expect("conventional prompt");
    let request_reference =
        write_provider_exchange_request(&run_directory, &coordinates, &request_bytes)
            .expect("staged request audit");
    let record = ProviderExchangeRecord {
        schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
        run_id: fixture.run_id.clone(),
        step: LoopStepName::Research,
        role: ProviderRole::Researcher,
        step_attempt: 1,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
        phase: ProviderExchangePhase::Request,
        previous_record_digest: None,
        request: request_reference,
        response: None,
        expansion: None,
        outcome: None,
    };
    let staged = stage_provider_exchange_record(&run_directory, &record)
        .expect("stage exact request record");
    let record_bytes = std::fs::read(run_directory.join(&staged.path)).expect("record bytes");
    assert!(contains(
        &record_bytes,
        REQUEST_RECORD_ONLY_SECRET.as_bytes()
    ));
    let head_before = std::fs::read(run_directory.join("run.json")).expect("run bytes");
    let provider = RecordingProvider::new(Vec::new());
    let mut resumed_step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
            .with_ticket(fixture.ticket.clone())
            .with_context_pack_request(fixture.context_request());

    let error = LoopRunner::resume(
        &fixture.runs_root,
        &fixture.run_id,
        &mut resumed_step_runner,
    )
    .expect_err("unsafe exact record history must reject before reconciliation");

    assert!(
        error.to_string().contains("prohibited credential material"),
        "{error}"
    );
    assert!(!error.to_string().contains(REQUEST_RECORD_ONLY_SECRET));
    assert!(provider.requests().is_empty());
    assert_eq!(
        std::fs::read(run_directory.join("run.json")).expect("run bytes"),
        head_before
    );
    assert!(read_run(&run_directory).provider_exchange_records.is_empty());
}

#[test]
fn pretty_boundary_secrets_never_reach_provider_artifacts_or_the_run_tree() {
    let request_fixture = recovery_fixture(
        "provider-pretty-request-boundary",
        &[REQUEST_PRETTY_BOUNDARY_SECRET],
        "Reject a pretty-only provider request boundary.",
    );
    let request_provider = RecordingProvider::new(vec![Ok(response(passed()))]);
    let mut request_step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&request_provider, "fake-model", 30_000)
            .with_ticket(request_fixture.ticket.clone())
            .with_context_pack_request(request_fixture.context_request());
    let mut request_run = LoopRunner::resume(
        &request_fixture.runs_root,
        &request_fixture.run_id,
        &mut request_step_runner,
    )
    .expect("prepare request-boundary run");
    let error = request_run
        .run_next_step()
        .expect_err("pretty-only request secret must reject before publication or call");
    assert!(
        error.to_string().contains("prohibited credential material"),
        "{error}"
    );
    drop(request_run);
    assert!(request_provider.requests().is_empty());
    assert!(read_run(&request_fixture.run_directory())
        .provider_exchange_records
        .is_empty());
    assert_run_tree_excludes_secret(
        &request_fixture.run_directory(),
        REQUEST_PRETTY_BOUNDARY_SECRET,
    );

    let response_fixture = recovery_fixture(
        "provider-pretty-response-boundary",
        &[RESPONSE_PRETTY_BOUNDARY_SECRET],
        "Replace a pretty-only provider response boundary.",
    );
    let response_provider = RecordingProvider::new(vec![Ok(response(passed()))]);
    let mut response_step_runner =
        ProviderStepRunner::new_legacy_unit_test_harness(&response_provider, "fake-model", 30_000)
            .with_ticket(response_fixture.ticket.clone())
            .with_context_pack_request(response_fixture.context_request());
    let mut response_run = LoopRunner::resume(
        &response_fixture.runs_root,
        &response_fixture.run_id,
        &mut response_step_runner,
    )
    .expect("prepare response-boundary run");
    assert!(response_run
        .run_next_step()
        .expect("the fixed safe provider failure should close the step durably"));
    drop(response_run);
    assert_eq!(response_provider.requests().len(), 1);
    let response_run_state = read_run(&response_fixture.run_directory());
    assert_eq!(response_run_state.provider_exchange_records.len(), 2);
    assert_eq!(
        step_status(&response_run_state, LoopStepName::Research),
        LoopStepStatus::Failed
    );
    let response_record = load_provider_exchange_record(
        &response_fixture.run_directory(),
        response_run_state
            .provider_exchange_records
            .last()
            .expect("fixed response head"),
    )
    .expect("fixed response record");
    let audit = load_provider_exchange_response_audit(
        &response_fixture.run_directory(),
        response_record
            .response
            .as_ref()
            .expect("fixed response audit"),
    )
    .expect("load fixed response audit");
    let ProviderExchangeResponseAudit::ProviderFailure { error } = audit else {
        panic!("the unsafe response must become a fixed provider failure")
    };
    assert_eq!(
        error.metadata["code"],
        "provider_response_contains_secret"
    );
    assert_run_tree_excludes_secret(
        &response_fixture.run_directory(),
        RESPONSE_PRETTY_BOUNDARY_SECRET,
    );
}

#[test]
fn staged_exact_pretty_boundary_history_rejects_before_replay() {
    for (case, secret) in [
        ("request", REQUEST_PRETTY_BOUNDARY_SECRET),
        ("response", RESPONSE_PRETTY_BOUNDARY_SECRET),
    ] {
        let fixture = recovery_fixture(
            &format!("provider-staged-pretty-boundary-{case}"),
            &[secret],
            "Reject an unsafe exact-format provider history boundary.",
        );
        let run_directory = fixture.run_directory();
        let coordinates = coordinates(&fixture.run_id);
        let request = request_with_content(
            serde_json::json!({
                "instructions": "safe staged request",
                "repository_context": null,
                "repository_context_authority": null
            })
            .to_string(),
        );
        let request_bytes = serde_json::to_vec_pretty(&request).expect("request bytes");
        assert_eq!(
            contains(&request_bytes, secret.as_bytes()),
            case == "request",
            "{case}: fixture must isolate the intended exact-format boundary"
        );
        let workspace =
            LoopWorkspace::open(&fixture.runs_root, &fixture.run_id).expect("workspace");
        crate::artifacts::write_step_request(
            &workspace,
            LoopStepName::Research,
            1,
            std::str::from_utf8(&request_bytes).expect("request UTF-8"),
        )
        .expect("conventional prompt");
        let request_reference =
            write_provider_exchange_request(&run_directory, &coordinates, &request_bytes)
                .expect("staged request audit");
        let request_record = ProviderExchangeRecord {
            schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: fixture.run_id.clone(),
            step: LoopStepName::Research,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: None,
            request: request_reference.clone(),
            response: None,
            expansion: None,
            outcome: None,
        };
        let staged_request = stage_provider_exchange_record(&run_directory, &request_record)
            .expect("stage request record");
        if case == "response" {
            persist_provider_exchange_record_reference(&workspace, staged_request.clone())
                .expect("persist safe request head");
            let response_reference = crate::provider_exchange::write_provider_exchange_response(
                &run_directory,
                &coordinates,
                &ProviderExchangeResponseAudit::ModelResponse {
                    response: response(passed()),
                },
            )
            .expect("raw response-boundary audit");
            let response_bytes = std::fs::read(run_directory.join(&response_reference.path))
                .expect("response bytes");
            assert!(contains(&response_bytes, secret.as_bytes()));
            stage_provider_exchange_record(
                &run_directory,
                &ProviderExchangeRecord {
                    schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
                    run_id: fixture.run_id.clone(),
                    step: LoopStepName::Research,
                    role: ProviderRole::Researcher,
                    step_attempt: 1,
                    exchange_index: 1,
                    kind: ProviderExchangeKind::Initial,
                    context_round: None,
                    phase: ProviderExchangePhase::Response,
                    previous_record_digest: Some(staged_request.digest.clone()),
                    request: request_reference,
                    response: Some(response_reference),
                    expansion: None,
                    outcome: Some(ProviderExchangeOutcome::Passed),
                },
            )
            .expect("stage response record");
        }
        let head_before = std::fs::read(run_directory.join("run.json")).expect("run bytes");
        let provider = RecordingProvider::new(Vec::new());
        let mut resumed_step_runner =
            ProviderStepRunner::new_legacy_unit_test_harness(&provider, "fake-model", 30_000)
                .with_ticket(fixture.ticket.clone())
                .with_context_pack_request(fixture.context_request());
        let error = LoopRunner::resume(
            &fixture.runs_root,
            &fixture.run_id,
            &mut resumed_step_runner,
        )
        .expect_err("unsafe exact-format history must reject before replay");
        assert!(
            error.to_string().contains("prohibited credential material"),
            "{case}: {error}"
        );
        assert!(provider.requests().is_empty(), "{case}");
        assert_eq!(
            std::fs::read(run_directory.join("run.json")).expect("run bytes"),
            head_before,
            "{case}"
        );
    }
}

fn recovery_fixture(run_id: &str, secrets: &[&str], problem: &str) -> RecoveryFixture {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let workspace = LoopWorkspace::create(&runs_root, run_id).expect("workspace");
    crate::artifact_safety::create_private_directory(&workspace.run_directory().join("inputs"))
        .expect("inputs directory");
    let eval_config = EvalConfig {
        evals: EvalGroup {
            allow_commands: vec!["true".to_string()],
            required: vec![EvalCommandConfig {
                name: "test".to_string(),
                command: "true".to_string(),
                cwd: None,
                env: secrets
                    .iter()
                    .enumerate()
                    .map(|(index, secret)| (format!("API_TOKEN_{index}"), (*secret).to_string()))
                    .collect::<BTreeMap<_, _>>(),
                timeout_ms: None,
                max_output_bytes: None,
            }],
        },
        thresholds: None,
    };
    let eval_bytes = canonical_json_bytes(&eval_config).expect("eval bytes");
    crate::artifact_safety::write_private_fixture(
        workspace.run_directory().join("inputs/eval-config.json"),
        &eval_bytes,
    )
    .expect("eval config");
    let ticket = ticket(problem);
    let run = crate::state::create_run(crate::state::NewLoopRun {
        run_id: run_id.to_string(),
        ticket_id: ticket.ticket_id.clone(),
        goal_id: ticket.goal_id.clone(),
        provider: "fake".to_string(),
        model: "fake-model".to_string(),
        input_digests: LoopInputDigests {
            ticket: canonical_sha256_digest(&ticket).expect("ticket digest"),
            policy: "b".repeat(64),
            config: "c".repeat(64),
            repository: "d".repeat(64),
            eval_config: Some(canonical_sha256_digest(&eval_config).expect("eval digest")),
        },
    });
    crate::state::save_run(&workspace, &run).expect("save run");

    RecoveryFixture {
        _temp: temp,
        repository,
        runs_root,
        run_id: run_id.to_string(),
        ticket,
    }
}

fn ticket(problem: &str) -> TicketSpec {
    TicketSpec {
        ticket_id: "M1-11c-provider-secret-recovery".to_string(),
        goal_id: "production-use".to_string(),
        title: "Recover secret-safe provider history".to_string(),
        status: TicketStatus::Ready,
        priority: TicketPriority::P1,
        problem: problem.to_string(),
        research_questions: Vec::new(),
        context: TicketContext {
            relevant_files: Vec::new(),
            forbidden_files: Vec::new(),
        },
        autonomy: TicketAutonomy {
            level: 1,
            apply_patch: false,
            allow_shell_commands: Vec::new(),
        },
        acceptance_criteria: vec!["Secret-safe recovery remains exact.".to_string()],
        eval: None,
    }
}

fn coordinates(run_id: &str) -> ProviderExchangeCoordinates {
    ProviderExchangeCoordinates {
        run_id: run_id.to_string(),
        step: LoopStepName::Research,
        role: ProviderRole::Researcher,
        step_attempt: 1,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
    }
}

fn request_with_content(content: String) -> ModelRequest {
    ModelRequest {
        model: "fake-model".to_string(),
        system: Role::Researcher.system_prompt().to_string(),
        messages: vec![ModelMessage {
            role: ModelMessageRole::User,
            content,
        }],
        response_schema: Some(Role::Researcher.response_schema()),
        temperature: 0.0,
        timeout_ms: 30_000,
    }
}

fn response(content: String) -> ModelResponse {
    ModelResponse {
        content,
        latency_ms: 1,
        raw_provider_metadata: serde_json::Value::Null,
    }
}

fn passed() -> String {
    serde_json::json!({
        "role": "researcher",
        "status": "passed",
        "summary": "Enough evidence.",
        "findings": [],
        "risks": [],
        "next_step_recommendation": "Continue."
    })
    .to_string()
}

fn read_run(run_directory: &Path) -> LoopRun {
    serde_json::from_slice(&std::fs::read(run_directory.join("run.json")).expect("run bytes"))
        .expect("run JSON")
}

fn step_status(run: &LoopRun, step: LoopStepName) -> LoopStepStatus {
    run.steps
        .iter()
        .find(|record| record.name == step)
        .expect("step record")
        .status
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|part| part == needle)
}

fn assert_run_tree_excludes_secret(run_directory: &Path, secret: &str) {
    fn visit(path: &Path, run_directory: &Path, secret: &[u8]) {
        for entry in std::fs::read_dir(path).expect("read run tree") {
            let entry = entry.expect("run tree entry");
            let path = entry.path();
            if path.is_dir() {
                visit(&path, run_directory, secret);
            } else if path != run_directory.join("inputs/eval-config.json") {
                let bytes = std::fs::read(&path).expect("read run artifact");
                assert!(
                    !contains(&bytes, secret),
                    "secret leaked into {}",
                    path.display()
                );
            }
        }
    }
    visit(run_directory, run_directory, secret.as_bytes());
}
