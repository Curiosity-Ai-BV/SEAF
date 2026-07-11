use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};

use seaf_core::{
    canonical_sha256_digest, LoopInputDigests, LoopRun, LoopStepName, LoopStepStatus,
    ProviderExchangeKind, ProviderExchangeOutcome, ProviderExchangePhase, ProviderExchangeRecord,
    ProviderRole, TicketAutonomy, TicketContext, TicketPriority, TicketSpec, TicketStatus,
};
use seaf_loop::{
    load_provider_exchange_record, stage_provider_exchange_record,
    write_provider_exchange_response, ContextLimits, ContextPackRequest, LoopRunner,
    LoopRunnerConfig, ProviderExchangeCoordinates, ProviderExchangeResponseAudit,
    ProviderStepRunner, Role, ValidatedRoleArtifact, PROVIDER_EXCHANGE_SCHEMA_VERSION,
};
use seaf_models::{ModelError, ModelProvider, ModelRequest, ModelResponse};
use serde_json::json;

struct InspectingProvider {
    run_directory: PathBuf,
    expected_request_record_counts: Vec<usize>,
    script: Mutex<VecDeque<Result<ModelResponse, ModelError>>>,
    requests: Mutex<Vec<ModelRequest>>,
    collisions_before_return: Vec<Option<String>>,
    mutations_before_return: Vec<Vec<(PathBuf, String)>>,
    symlinks_before_return: Vec<Vec<(PathBuf, PathBuf)>>,
}

impl InspectingProvider {
    fn new(
        run_directory: PathBuf,
        expected_request_record_counts: Vec<usize>,
        script: Vec<Result<ModelResponse, ModelError>>,
    ) -> Self {
        let call_count = expected_request_record_counts.len();
        Self {
            run_directory,
            expected_request_record_counts,
            script: Mutex::new(script.into()),
            requests: Mutex::new(Vec::new()),
            collisions_before_return: vec![None; call_count],
            mutations_before_return: vec![Vec::new(); call_count],
            symlinks_before_return: vec![Vec::new(); call_count],
        }
    }

    fn with_collisions_before_return(mut self, collisions: Vec<Option<&str>>) -> Self {
        self.collisions_before_return = collisions
            .into_iter()
            .map(|path| path.map(str::to_string))
            .collect();
        self
    }

    fn with_mutations_before_return(mut self, mutations: Vec<Vec<(PathBuf, &str)>>) -> Self {
        self.mutations_before_return = mutations
            .into_iter()
            .map(|round| {
                round
                    .into_iter()
                    .map(|(path, content)| (path, content.to_string()))
                    .collect()
            })
            .collect();
        self
    }

    #[cfg(unix)]
    fn with_symlinks_before_return(mut self, symlinks: Vec<Vec<(PathBuf, PathBuf)>>) -> Self {
        self.symlinks_before_return = symlinks;
        self
    }

    fn requests(&self) -> Vec<ModelRequest> {
        lock(&self.requests).clone()
    }
}

impl ModelProvider for InspectingProvider {
    fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let call = lock(&self.requests).len();
        let run = read_run(&self.run_directory);
        assert_eq!(
            run.provider_exchange_records.len(),
            self.expected_request_record_counts[call],
            "provider call {call} must observe its durable request record"
        );
        let request_reference = run
            .provider_exchange_records
            .last()
            .expect("provider call must have an authoritative request reference");
        assert_eq!(request_reference.phase, ProviderExchangePhase::Request);
        assert!(self.run_directory.join(&request_reference.path).is_file());
        if let Some(path) = &self.collisions_before_return[call] {
            let path = self.run_directory.join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("collision parent");
            }
            std::fs::write(path, b"occupied durable target").expect("inject collision");
        }
        for (path, content) in &self.mutations_before_return[call] {
            std::fs::write(path, content).expect("mutate live repository source");
        }
        for (relative_link, target) in &self.symlinks_before_return[call] {
            let _ = std::fs::remove_file(self.run_directory.join(relative_link));
            symlink(target, self.run_directory.join(relative_link)).expect("inject symlink");
        }
        lock(&self.requests).push(request);
        lock(&self.script)
            .pop_front()
            .expect("provider script has one result per expected call")
    }
}

#[test]
#[cfg(unix)]
fn symlinked_expansion_publication_target_is_a_hard_stop_without_terminal_claim() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    std::fs::write(repository.join("additional.txt"), "additional\n").expect("context");
    let outside = temp.path().join("outside-artifact.json");
    std::fs::write(&outside, "outside unchanged").expect("outside");
    let runs_root = temp.path().join("runs");
    let run_id = "symlinked-expansion-target";
    let ticket = ticket(Vec::new());
    let expansion_path = PathBuf::from("artifacts/01-research.attempt-001.context-round-001.json");
    let provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1],
        vec![Ok(response(needs_context(&["additional.txt"])))],
    )
    .with_symlinks_before_return(vec![vec![(expansion_path, outside.clone())]]);
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut loop_runner =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut step_runner).expect("start");

    let error = loop_runner
        .run_next_step()
        .expect_err("unsafe publication must stop");

    assert!(error
        .to_string()
        .contains("durable context expansion write failed"));
    assert_eq!(provider.requests().len(), 1);
    assert_eq!(
        std::fs::read_to_string(&outside).expect("outside"),
        "outside unchanged"
    );
    let run = read_run(&runs_root.join(run_id));
    assert_eq!(run.provider_exchange_records.len(), 2);
    let research = run
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::Research)
        .expect("research");
    assert_eq!(research.status, LoopStepStatus::Running);
    assert!(research.artifact_path.is_none());
}

#[test]
fn retry_chain_uses_audited_initial_and_accepted_expansion_bytes_after_live_sources_change() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let initial = repository.join("initial.txt");
    let first = repository.join("first.txt");
    let second = repository.join("second.txt");
    std::fs::write(&initial, "initial accepted bytes").expect("initial");
    std::fs::write(&first, "first accepted bytes").expect("first");
    std::fs::write(&second, "second accepted bytes").expect("second");
    let runs_root = temp.path().join("runs");
    let run_id = "immutable-prompt-chain";
    let ticket = ticket(vec!["initial.txt"]);
    let provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1, 3, 5],
        vec![
            Ok(response(needs_context(&["first.txt"]))),
            Ok(response(needs_context(&["second.txt"]))),
            Ok(response(passed())),
        ],
    )
    .with_mutations_before_return(vec![
        vec![(initial.clone(), "initial changed live bytes")],
        vec![(first.clone(), "first changed live bytes")],
        Vec::new(),
    ]);
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut loop_runner =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut step_runner).expect("start");

    assert!(loop_runner.run_next_step().expect("two immutable rounds"));

    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    for retry in &requests[1..] {
        let audit = serde_json::to_string(retry).expect("request audit");
        assert!(audit.contains("initial accepted bytes"));
        assert!(!audit.contains("initial changed live bytes"));
        assert!(audit.contains("first accepted bytes"));
        assert!(!audit.contains("first changed live bytes"));
    }
    let second_retry = serde_json::to_string(&requests[2]).expect("second retry");
    assert!(second_retry.contains("second accepted bytes"));
}

#[test]
fn third_context_request_is_denied_after_exactly_two_accepted_expansions() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    for name in ["one.txt", "two.txt", "three.txt"] {
        std::fs::write(repository.join(name), format!("{name} authority\n")).expect("context");
    }
    let runs_root = temp.path().join("runs");
    let run_id = "context-cap";
    let ticket = ticket(Vec::new());
    let provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1, 3, 5],
        vec![
            Ok(response(needs_context(&["one.txt"]))),
            Ok(response(needs_context(&["two.txt"]))),
            Ok(response(needs_context(&["three.txt"]))),
        ],
    );
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut loop_runner =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut step_runner).expect("start");

    assert!(loop_runner.run_next_step().expect("cap denial"));

    let run_directory = runs_root.join(run_id);
    let run = read_run(&run_directory);
    assert_eq!(
        provider.requests().len(),
        3,
        "the denied round makes no call"
    );
    assert_eq!(run.provider_exchange_records.len(), 6);
    assert_eq!(
        step_status(&run, LoopStepName::Research),
        LoopStepStatus::Blocked
    );
    assert!(run_directory
        .join("artifacts/01-research.attempt-001.context-round-002.json")
        .is_file());
    assert!(!run_directory
        .join("artifacts/01-research.attempt-001.context-round-003.json")
        .exists());
    let denial = run
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::Research)
        .and_then(|record| record.artifact_path.as_ref())
        .map(|path| std::fs::read_to_string(run_directory.join(path)).expect("denial evidence"))
        .expect("denial artifact");
    assert!(denial.contains("cap is exhausted"));
}

#[test]
fn durable_exchange_write_collisions_stop_without_a_later_call_or_false_terminal_evidence() {
    for (case, collision, expected_records) in [
        (
            "response",
            "responses/01-research.attempt-001.exchange-001.initial.response.json",
            1,
        ),
        (
            "expansion",
            "artifacts/01-research.attempt-001.context-round-001.json",
            2,
        ),
        (
            "next-request",
            "prompts/01-research.attempt-001.exchange-002.context-retry.request.md",
            2,
        ),
    ] {
        let temp = tempfile::tempdir().expect("temp");
        let repository = temp.path().join("repository");
        std::fs::create_dir(&repository).expect("repository");
        std::fs::write(repository.join("additional.txt"), "additional\n").expect("context");
        let runs_root = temp.path().join("runs");
        let run_id = format!("write-collision-{case}");
        let ticket = ticket(Vec::new());
        let provider = InspectingProvider::new(
            runs_root.join(&run_id),
            vec![1],
            vec![Ok(response(needs_context(&["additional.txt"])))],
        )
        .with_collisions_before_return(vec![Some(collision)]);
        let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_context_pack_request(context_request(&repository, &ticket));
        let mut loop_runner =
            LoopRunner::start(config(&runs_root, &run_id, &ticket), &mut step_runner)
                .expect("start");

        let error = loop_runner
            .run_next_step()
            .expect_err("durable collision must stop orchestration");
        assert!(
            error.to_string().contains("durable"),
            "clear durable-write error for {case}: {error}"
        );
        assert_eq!(provider.requests().len(), 1, "no later call for {case}");
        let run = read_run(&runs_root.join(&run_id));
        assert_eq!(
            run.provider_exchange_records.len(),
            expected_records,
            "{case}"
        );
        let research = run
            .steps
            .iter()
            .find(|record| record.name == LoopStepName::Research)
            .expect("research");
        assert_eq!(research.status, LoopStepStatus::Running, "{case}");
        assert!(
            research.artifact_path.is_none(),
            "no false evidence for {case}"
        );
    }
}

#[test]
fn unsafe_unavailable_duplicate_only_and_excessive_requests_end_blocked_with_denial_evidence() {
    let mut cases = vec!["unsafe", "unavailable", "duplicate-only", "excessive"];
    #[cfg(unix)]
    cases.push("unreadable");
    for case in cases {
        let temp = tempfile::tempdir().expect("temp");
        let repository = temp.path().join("repository");
        std::fs::create_dir(&repository).expect("repository");
        std::fs::write(repository.join("initial.txt"), "12345678").expect("initial");
        std::fs::write(repository.join("additional.txt"), "additional").expect("additional");
        std::fs::write(repository.join("unreadable.txt"), "unreadable").expect("unreadable");
        let (relevant, requested, max_total) = match case {
            "unsafe" => (Vec::new(), ".env", 8_192),
            "unavailable" => (Vec::new(), "missing.txt", 8_192),
            "duplicate-only" => (vec!["initial.txt"], "initial.txt", 8_192),
            "excessive" => (vec!["initial.txt"], "additional.txt", 8),
            "unreadable" => (Vec::new(), "unreadable.txt", 8_192),
            _ => unreachable!(),
        };
        #[cfg(unix)]
        if case == "unreadable" {
            std::fs::set_permissions(
                repository.join("unreadable.txt"),
                std::fs::Permissions::from_mode(0o000),
            )
            .expect("remove read permission");
        }
        let runs_root = temp.path().join("runs");
        let run_id = format!("context-denial-{case}");
        let ticket = ticket(relevant);
        let provider = InspectingProvider::new(
            runs_root.join(&run_id),
            vec![1],
            vec![Ok(response(needs_context(&[requested])))],
        );
        let mut pack = context_request(&repository, &ticket);
        pack.limits.max_total_bytes = max_total;
        let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_context_pack_request(pack);
        let mut loop_runner =
            LoopRunner::start(config(&runs_root, &run_id, &ticket), &mut step_runner)
                .expect("start");

        assert!(loop_runner.run_next_step().expect("terminal denial"));

        assert_eq!(provider.requests().len(), 1, "no retry for {case}");
        let run_directory = runs_root.join(&run_id);
        let run = read_run(&run_directory);
        assert_eq!(run.provider_exchange_records.len(), 2, "{case}");
        assert_eq!(
            step_status(&run, LoopStepName::Research),
            LoopStepStatus::Blocked
        );
        let evidence_path = run
            .steps
            .iter()
            .find(|record| record.name == LoopStepName::Research)
            .and_then(|record| record.artifact_path.as_ref())
            .expect("denial evidence path");
        let evidence =
            std::fs::read_to_string(run_directory.join(evidence_path)).expect("denial evidence");
        assert!(evidence.contains("context_denied"), "{case}: {evidence}");
        #[cfg(unix)]
        if case == "unreadable" {
            std::fs::set_permissions(
                repository.join("unreadable.txt"),
                std::fs::Permissions::from_mode(0o600),
            )
            .expect("restore read permission");
        }
    }
}

#[test]
fn fresh_needs_context_round_is_fully_durable_before_retry_and_uses_one_prompt_chain() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    std::fs::write(repository.join("initial.txt"), "initial authority\n").expect("initial");
    std::fs::write(repository.join("additional.txt"), "additional authority\n")
        .expect("additional");
    let runs_root = temp.path().join("runs");
    let run_id = "live-context-ordering";
    let ticket = ticket(vec!["initial.txt"]);
    let provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1, 3],
        vec![
            Ok(response(needs_context(&["additional.txt"]))),
            Ok(response(passed())),
        ],
    );
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut loop_runner =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut step_runner).expect("start");

    assert!(loop_runner.run_next_step().expect("bounded context step"));

    let run = read_run(&runs_root.join(run_id));
    assert_eq!(run.provider_exchange_records.len(), 4);
    assert_eq!(
        run.provider_exchange_records
            .iter()
            .filter(|record| record.kind == ProviderExchangeKind::ContextRetry)
            .count(),
        2,
        "the context request and response records form one logical retry"
    );
    assert_eq!(
        step_status(&run, LoopStepName::Research),
        LoopStepStatus::Completed
    );
    let log = std::fs::read_to_string(runs_root.join(run_id).join("log.md")).expect("run log");
    assert_eq!(log.matches("finished step Research").count(), 1);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages[0], requests[0].messages[0]);
    let initial_input: serde_json::Value =
        serde_json::from_str(&requests[0].messages[0].content).expect("initial role input");
    assert_eq!(initial_input["ticket"], json!(ticket));
    let initial_repository_context = initial_input["repository_context"]
        .as_str()
        .expect("initial repository context");
    assert_eq!(
        initial_repository_context
            .matches("content:\ninitial authority\n")
            .count(),
        1,
        "the initial content-bearing file entry appears once"
    );
    let expansion_input: serde_json::Value =
        serde_json::from_str(&requests[1].messages[1].content).expect("expansion input");
    let expansions = expansion_input["context_expansions"]
        .as_array()
        .expect("expansion entries");
    assert_eq!(expansions.len(), 1);
    assert_eq!(expansions[0]["path"], "additional.txt");
    assert_eq!(expansions[0]["content"], "additional authority\n");
}

#[test]
fn schema_invalid_response_is_a_durable_failed_step_without_json_repair() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "schema-invalid";
    let ticket = ticket(Vec::new());
    let provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1],
        vec![Ok(response(
            json!({
                "role": "researcher",
                "summary": "missing status"
            })
            .to_string(),
        ))],
    );
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut loop_runner =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut step_runner).expect("start");

    assert!(loop_runner
        .run_next_step()
        .expect("terminal invalid response"));

    let run = read_run(&runs_root.join(run_id));
    assert_eq!(
        provider.requests().len(),
        1,
        "schema errors are not repaired"
    );
    assert_eq!(run.provider_exchange_records.len(), 2);
    assert_eq!(
        step_status(&run, LoopStepName::Research),
        LoopStepStatus::Failed
    );
    assert!(run
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::Research)
        .and_then(|record| record.artifact_path.as_ref())
        .is_some());
}

#[test]
fn reviewer_decision_for_the_wrong_reviewer_role_is_durably_invalid_and_failed() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "wrong-reviewer-decision";
    let ticket = ticket(Vec::new());
    let provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1, 3, 5, 7],
        vec![
            Ok(response(passed())),
            Ok(response(agent_passed("analyzer"))),
            Ok(response(agent_passed("spec_writer"))),
            Ok(response(
                json!({
                    "role": "spec_reviewer",
                    "decision": "approve_for_tests",
                    "summary": "Wrong approval for this reviewer.",
                    "blocking_issues": [],
                    "non_blocking_issues": []
                })
                .to_string(),
            )),
        ],
    );
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut loop_runner =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut step_runner).expect("start");

    for _ in 0..4 {
        assert!(loop_runner.run_next_step().expect("provider step"));
    }

    let run = read_run(&runs_root.join(run_id));
    assert_eq!(provider.requests().len(), 4);
    assert_eq!(run.provider_exchange_records.len(), 8);
    assert_eq!(
        step_status(&run, LoopStepName::SpecReview),
        LoopStepStatus::Failed
    );
}

#[test]
fn initial_request_collision_prevents_the_first_provider_call_and_terminal_claim() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "initial-request-collision";
    let ticket = ticket(Vec::new());
    let provider = InspectingProvider::new(runs_root.join(run_id), Vec::new(), Vec::new());
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut loop_runner =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut step_runner).expect("start");
    std::fs::write(
        runs_root
            .join(run_id)
            .join("prompts/01-research.attempt-001.exchange-001.initial.request.md"),
        "occupied",
    )
    .expect("collision");

    let error = loop_runner
        .run_next_step()
        .expect_err("initial durable request collision");

    assert!(error
        .to_string()
        .contains("durable provider exchange write failed"));
    assert!(provider.requests().is_empty());
    let run = read_run(&runs_root.join(run_id));
    assert!(run.provider_exchange_records.is_empty());
    let research = run
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::Research)
        .expect("research");
    assert_eq!(research.status, LoopStepStatus::Running);
    assert!(research.artifact_path.is_none());
}

#[test]
fn resume_reuses_attempt_one_after_conventional_prompt_crash_before_first_exchange() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "resume-pre-exchange-attempt-one";
    let ticket = ticket(Vec::new());
    let exchange_request = "prompts/01-research.attempt-001.exchange-001.initial.request.md";
    let first_provider = InspectingProvider::new(runs_root.join(run_id), Vec::new(), Vec::new());
    let mut first_runner = ProviderStepRunner::new(&first_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut first =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut first_runner).expect("start");
    std::fs::write(runs_root.join(run_id).join(exchange_request), "collision")
        .expect("inject collision");
    first.run_next_step().expect_err("pre-exchange crash cut");
    drop(first);
    let run_directory = runs_root.join(run_id);
    std::fs::remove_file(run_directory.join(exchange_request)).expect("remove injected cut");
    let conventional = std::fs::read(run_directory.join("prompts/01-research.prompt.md"))
        .expect("attempt one prompt");

    let provider =
        InspectingProvider::new(run_directory.clone(), vec![1], vec![Ok(response(passed()))]);
    let mut resumed_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut resumed =
        LoopRunner::resume(&runs_root, run_id, &mut resumed_runner).expect("resume same attempt");
    resumed.run_next_step().expect("finish attempt one");

    let run = read_run(&run_directory);
    assert_eq!(run.provider_exchange_records[0].step_attempt, 1);
    assert_eq!(
        std::fs::read(run_directory.join("prompts/01-research.prompt.md"))
            .expect("attempt one prompt"),
        conventional
    );
    assert!(!run_directory
        .join("prompts/01-research.attempt-002.prompt.md")
        .exists());
}

#[test]
fn resume_reuses_authorized_attempt_two_after_rerun_prompt_crash_before_exchange() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "resume-pre-exchange-attempt-two";
    let ticket = ticket(Vec::new());
    let initial_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1],
        vec![Ok(response(passed()))],
    );
    let mut initial_runner = ProviderStepRunner::new(&initial_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut initial =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut initial_runner).expect("start");
    initial.run_next_step().expect("attempt one");
    drop(initial);
    let run_directory = runs_root.join(run_id);
    let prior_records = read_run(&run_directory).provider_exchange_records.len();
    let exchange_request = "prompts/01-research.attempt-002.exchange-001.initial.request.md";
    let collision_provider = InspectingProvider::new(run_directory.clone(), Vec::new(), Vec::new());
    let mut collision_runner = ProviderStepRunner::new(&collision_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut rerun = LoopRunner::resume(&runs_root, run_id, &mut collision_runner)
        .expect("resume")
        .rerun_from(LoopStepName::Research)
        .expect("authorize attempt two");
    std::fs::write(run_directory.join(exchange_request), "collision").expect("inject collision");
    rerun.run_next_step().expect_err("attempt two prompt cut");
    drop(rerun);
    std::fs::remove_file(run_directory.join(exchange_request)).expect("remove cut");
    let conventional =
        std::fs::read(run_directory.join("prompts/01-research.attempt-002.prompt.md"))
            .expect("attempt two prompt");

    let provider = InspectingProvider::new(
        run_directory.clone(),
        vec![prior_records + 1],
        vec![Ok(response(passed()))],
    );
    let mut resumed_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut resumed = LoopRunner::resume(&runs_root, run_id, &mut resumed_runner)
        .expect("resume authorized attempt two");
    resumed.run_next_step().expect("finish attempt two");

    let run = read_run(&run_directory);
    assert_eq!(
        run.provider_exchange_records
            .last()
            .expect("response")
            .step_attempt,
        2
    );
    assert_eq!(
        std::fs::read(run_directory.join("prompts/01-research.attempt-002.prompt.md"))
            .expect("attempt two prompt"),
        conventional
    );
    assert!(!run_directory
        .join("prompts/01-research.attempt-003.prompt.md")
        .exists());
}

#[test]
fn resume_rejects_a_skipped_conventional_prompt_attempt_before_any_exchange_write() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "reject-skipped-conventional-attempt";
    let ticket = ticket(Vec::new());
    let exchange_request = "prompts/01-research.attempt-001.exchange-001.initial.request.md";
    let unused = InspectingProvider::new(runs_root.join(run_id), Vec::new(), Vec::new());
    let mut runner = ProviderStepRunner::new(&unused, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut loop_runner =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut runner).expect("start");
    std::fs::write(runs_root.join(run_id).join(exchange_request), "collision")
        .expect("inject collision");
    loop_runner.run_next_step().expect_err("prompt crash cut");
    drop(loop_runner);
    let run_directory = runs_root.join(run_id);
    std::fs::remove_file(run_directory.join(exchange_request)).expect("remove cut");
    let exact =
        std::fs::read(run_directory.join("prompts/01-research.prompt.md")).expect("attempt one");
    std::fs::write(
        run_directory.join("prompts/01-research.attempt-999.prompt.md"),
        exact,
    )
    .expect("inject skipped prompt");
    let before = snapshot_tree(&run_directory);

    let provider = InspectingProvider::new(run_directory.clone(), Vec::new(), Vec::new());
    let mut resumed_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let error = LoopRunner::resume(&runs_root, run_id, &mut resumed_runner)
        .expect_err("skipped attempt must fail preflight");

    assert!(
        error.to_string().contains("expected recovery attempt 1"),
        "{error}"
    );
    assert!(provider.requests().is_empty());
    assert_eq!(snapshot_tree(&run_directory), before);
    assert!(!run_directory
        .join("prompts/01-research.attempt-999.exchange-001.initial.request.md")
        .exists());
}

#[test]
fn recovery_rejects_a_staged_initial_request_without_its_exact_conventional_prompt() {
    for case in ["missing", "substituted"] {
        let temp = tempfile::tempdir().expect("temp");
        let repository = temp.path().join("repository");
        std::fs::create_dir(&repository).expect("repository");
        let runs_root = temp.path().join("runs");
        let run_id = format!("reject-staged-initial-prompt-{case}");
        let ticket = ticket(Vec::new());
        let unused = InspectingProvider::new(runs_root.join(&run_id), Vec::new(), Vec::new());
        let mut initial_runner = ProviderStepRunner::new(&unused, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_context_pack_request(context_request(&repository, &ticket));
        let initial = LoopRunner::start(config(&runs_root, &run_id, &ticket), &mut initial_runner)
            .expect("start");
        drop(initial);
        let run_directory = runs_root.join(&run_id);
        let coordinates = ProviderExchangeCoordinates {
            run_id: run_id.clone(),
            step: LoopStepName::Research,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
        };
        let request_bytes = b"exact staged initial request";
        let request =
            seaf_loop::write_provider_exchange_request(&run_directory, &coordinates, request_bytes)
                .expect("request");
        stage_provider_exchange_record(
            &run_directory,
            &ProviderExchangeRecord {
                schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
                run_id: run_id.clone(),
                step: LoopStepName::Research,
                role: ProviderRole::Researcher,
                step_attempt: 1,
                exchange_index: 1,
                kind: ProviderExchangeKind::Initial,
                context_round: None,
                phase: ProviderExchangePhase::Request,
                previous_record_digest: None,
                request,
                response: None,
                expansion: None,
                outcome: None,
            },
        )
        .expect("stage request");
        if case == "substituted" {
            std::fs::write(
                run_directory.join("prompts/01-research.prompt.md"),
                b"substituted conventional prompt",
            )
            .expect("substituted prompt");
        }
        std::fs::write(run_directory.join("provider-exchange.lock"), b"")
            .expect("stable lock file");
        let before = snapshot_tree(&run_directory);

        let provider = InspectingProvider::new(run_directory.clone(), Vec::new(), Vec::new());
        let mut resumed_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_context_pack_request(context_request(&repository, &ticket));
        let error = LoopRunner::resume(&runs_root, &run_id, &mut resumed_runner)
            .expect_err("invalid conventional prompt must reject before head adoption");

        assert!(
            error.to_string().contains("conventional provider prompt"),
            "{case}: {error}"
        );
        assert!(provider.requests().is_empty(), "{case}");
        assert_eq!(snapshot_tree(&run_directory), before, "{case}");
        assert!(read_run(&run_directory)
            .provider_exchange_records
            .is_empty());
    }
}

#[test]
fn resume_rejects_expected_attempt_two_without_matching_rerun_authorization() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "reject-unauthorized-conventional-attempt-two";
    let ticket = ticket(Vec::new());
    let initial_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1],
        vec![Ok(response(passed()))],
    );
    let mut initial_runner = ProviderStepRunner::new(&initial_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut initial =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut initial_runner).expect("start");
    initial.run_next_step().expect("attempt one");
    drop(initial);
    let run_directory = runs_root.join(run_id);
    let mut unauthorized = read_run(&run_directory);
    unauthorized.status = seaf_core::LoopStatus::Running;
    unauthorized.current_step = LoopStepName::Research;
    let research = unauthorized
        .steps
        .iter_mut()
        .find(|record| record.name == LoopStepName::Research)
        .expect("research");
    research.status = LoopStepStatus::Running;
    research.artifact_path = None;
    research.artifact_digest = None;
    seaf_loop::state::write_run_file(&run_directory.join("run.json"), &unauthorized)
        .expect("write unauthorized running state");
    let attempt_one =
        std::fs::read(run_directory.join("prompts/01-research.prompt.md")).expect("attempt one");
    std::fs::write(
        run_directory.join("prompts/01-research.attempt-002.prompt.md"),
        attempt_one,
    )
    .expect("unauthorized attempt two prompt");
    let before = snapshot_tree(&run_directory);

    let provider = InspectingProvider::new(run_directory.clone(), Vec::new(), Vec::new());
    let mut resumed_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let error = LoopRunner::resume(&runs_root, run_id, &mut resumed_runner)
        .expect_err("attempt two requires rerun authorization");

    assert!(error.to_string().contains("not authorized"), "{error}");
    assert!(provider.requests().is_empty());
    assert_eq!(snapshot_tree(&run_directory), before);
    assert!(!run_directory
        .join("prompts/01-research.attempt-002.exchange-001.initial.request.md")
        .exists());
}

#[test]
fn explicit_rerun_rejects_orphaned_skipped_conventional_history_before_authorization_or_reset() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "reject-orphaned-rerun-attempt";
    let ticket = ticket(Vec::new());
    let initial_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1],
        vec![Ok(response(passed()))],
    );
    let mut initial_runner = ProviderStepRunner::new(&initial_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut initial =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut initial_runner).expect("start");
    initial.run_next_step().expect("attempt one");
    drop(initial);
    let run_directory = runs_root.join(run_id);
    let attempt_one =
        std::fs::read(run_directory.join("prompts/01-research.prompt.md")).expect("attempt one");
    std::fs::write(
        run_directory.join("prompts/01-research.attempt-003.prompt.md"),
        attempt_one,
    )
    .expect("orphan attempt three");
    let provider = InspectingProvider::new(run_directory.clone(), Vec::new(), Vec::new());
    let mut runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let resumed = LoopRunner::resume(&runs_root, run_id, &mut runner).expect("resume");
    let before = snapshot_tree(&run_directory);

    let error = resumed
        .rerun_from(LoopStepName::Research)
        .expect_err("skipped rerun attempt must reject");

    assert!(
        error.to_string().contains("exact next durable attempt 2"),
        "{error}"
    );
    assert!(provider.requests().is_empty());
    assert_eq!(snapshot_tree(&run_directory), before);
    assert!(!run_directory
        .join("artifacts/01-research.attempt-004.rerun-authorization.json")
        .exists());
}

fn snapshot_tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    for directory in ["", "prompts", "responses", "artifacts"] {
        let path = if directory.is_empty() {
            root.to_path_buf()
        } else {
            root.join(directory)
        };
        for entry in std::fs::read_dir(path).expect("tree directory") {
            let entry = entry.expect("tree entry");
            if entry.file_type().expect("file type").is_file() {
                files.push((
                    entry
                        .path()
                        .strip_prefix(root)
                        .expect("relative")
                        .to_path_buf(),
                    std::fs::read(entry.path()).expect("tree bytes"),
                ));
            }
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

#[test]
fn malformed_json_repair_and_context_retry_each_publish_request_before_calling_provider() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    std::fs::write(repository.join("additional.txt"), "additional authority\n")
        .expect("additional");
    let runs_root = temp.path().join("runs");
    let run_id = "repair-context-ordering";
    let ticket = ticket(Vec::new());
    let provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1, 3, 5, 7],
        vec![
            Ok(response("not json".to_string())),
            Ok(response(needs_context(&["additional.txt"]))),
            Ok(response("still not json".to_string())),
            Ok(response(passed())),
        ],
    );
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut loop_runner =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut step_runner).expect("start");

    assert!(loop_runner
        .run_next_step()
        .expect("repair and context step"));

    let run = read_run(&runs_root.join(run_id));
    assert_eq!(run.provider_exchange_records.len(), 8);
    assert_eq!(
        step_status(&run, LoopStepName::Research),
        LoopStepStatus::Completed
    );
    assert_eq!(provider.requests().len(), 4);
}

#[test]
fn provider_failure_is_a_durable_terminal_failure_instead_of_an_unexplained_running_step() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "provider-failure";
    let ticket = ticket(Vec::new());
    let provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1],
        vec![Err(ModelError::provider(
            "provider unavailable",
            true,
            json!({"provider": "fake"}),
        ))],
    );
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut loop_runner =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut step_runner).expect("start");

    assert!(loop_runner
        .run_next_step()
        .expect("durable provider failure"));

    let run = read_run(&runs_root.join(run_id));
    assert_eq!(run.provider_exchange_records.len(), 2);
    assert_eq!(
        step_status(&run, LoopStepName::Research),
        LoopStepStatus::Failed
    );
    let artifact = run
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::Research)
        .and_then(|record| record.artifact_path.as_ref())
        .expect("failure evidence");
    assert!(runs_root.join(run_id).join(artifact).is_file());
}

#[test]
fn resume_continues_the_durable_retry_request_with_exact_audited_bytes() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    std::fs::write(
        repository.join("additional.txt"),
        "accepted before interruption\n",
    )
    .expect("context");
    let runs_root = temp.path().join("runs");
    let run_id = "resume-durable-context-request";
    let ticket = ticket(Vec::new());
    let response_path =
        "responses/01-research.attempt-001.exchange-002.context-retry.response.json";
    let first_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1, 3],
        vec![
            Ok(response(needs_context(&["additional.txt"]))),
            Ok(response(passed())),
        ],
    )
    .with_collisions_before_return(vec![None, Some(response_path)]);
    let mut first_step_runner = ProviderStepRunner::new(&first_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut first = LoopRunner::start(config(&runs_root, run_id, &ticket), &mut first_step_runner)
        .expect("start");
    first
        .run_next_step()
        .expect_err("response publication interruption");
    drop(first);
    let original_retry = first_provider.requests()[1].clone();
    std::fs::remove_file(runs_root.join(run_id).join(response_path))
        .expect("model an interruption before response publication");
    std::fs::write(
        repository.join("additional.txt"),
        "changed after interruption\n",
    )
    .expect("change live repository");

    let resumed_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![3],
        vec![Ok(response(passed()))],
    );
    let mut resumed_step_runner = ProviderStepRunner::new(&resumed_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut resumed = LoopRunner::resume(&runs_root, run_id, &mut resumed_step_runner)
        .expect("resume verified durable prefix");
    resumed.run_next_step().expect("finish resumed exchange");

    assert_eq!(resumed_provider.requests(), vec![original_retry]);
    let run = read_run(&runs_root.join(run_id));
    assert_eq!(run.provider_exchange_records.len(), 4);
    assert_eq!(
        step_status(&run, LoopStepName::Research),
        LoopStepStatus::Completed
    );
}

#[test]
fn resumed_second_round_uses_original_initial_metadata_and_prior_expansion_after_repo_changes() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let initial = repository.join("initial.txt");
    let first = repository.join("first.txt");
    let second = repository.join("second.txt");
    std::fs::write(&initial, "initial accepted bytes\n").expect("initial");
    std::fs::write(&first, "first accepted bytes\n").expect("first");
    std::fs::write(&second, "second accepted bytes\n").expect("second");
    let runs_root = temp.path().join("runs");
    let run_id = "resume-second-context-round";
    let ticket = ticket(vec!["initial.txt"]);
    let response_path =
        "responses/01-research.attempt-001.exchange-002.context-retry.response.json";
    let first_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1, 3],
        vec![
            Ok(response(needs_context(&["first.txt"]))),
            Ok(response(needs_context(&["second.txt"]))),
        ],
    )
    .with_collisions_before_return(vec![None, Some(response_path)]);
    let mut first_runner = ProviderStepRunner::new(&first_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut first_loop =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut first_runner).expect("start");
    first_loop
        .run_next_step()
        .expect_err("interrupt after durable round-one request");
    drop(first_loop);
    std::fs::remove_file(runs_root.join(run_id).join(response_path)).expect("remove injected cut");
    std::fs::write(&initial, "changed initial bytes that are much longer\n")
        .expect("mutate initial");
    std::fs::write(&first, "changed first bytes\n").expect("mutate first");

    let resumed_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![3, 5],
        vec![
            Ok(response(needs_context(&["second.txt"]))),
            Ok(response(passed())),
        ],
    );
    let mut resumed_runner = ProviderStepRunner::new(&resumed_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut resumed = LoopRunner::resume(&runs_root, run_id, &mut resumed_runner)
        .expect("resume verified context history");
    resumed.run_next_step().expect("complete second round");

    let requests = resumed_provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], first_provider.requests()[1]);
    let final_request = serde_json::to_string(&requests[1]).expect("final request");
    assert!(final_request.contains("initial accepted bytes"));
    assert!(final_request.contains("first accepted bytes"));
    assert!(final_request.contains("second accepted bytes"));
    assert!(!final_request.contains("changed initial bytes"));
    assert!(!final_request.contains("changed first bytes"));
    let run = read_run(&runs_root.join(run_id));
    assert_eq!(run.provider_exchange_records.len(), 6);
    assert_eq!(
        step_status(&run, LoopStepName::Research),
        LoopStepStatus::Completed
    );
}

#[test]
fn resume_after_initial_response_uses_structured_original_context_authority_for_first_round() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let initial = repository.join("initial.txt");
    std::fs::write(&initial, "original initial bytes\n").expect("initial");
    std::fs::write(repository.join("additional.txt"), "additional bytes\n").expect("additional");
    let runs_root = temp.path().join("runs");
    let run_id = "resume-first-context-round";
    let ticket = ticket(vec!["initial.txt"]);
    let expansion_path = "artifacts/01-research.attempt-001.context-round-001.json";
    let first_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1],
        vec![Ok(response(needs_context(&["additional.txt"])))],
    )
    .with_collisions_before_return(vec![Some(expansion_path)]);
    let mut first_runner = ProviderStepRunner::new(&first_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut first_loop =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut first_runner).expect("start");
    first_loop
        .run_next_step()
        .expect_err("interrupt before expansion publication");
    drop(first_loop);
    std::fs::remove_file(runs_root.join(run_id).join(expansion_path)).expect("remove injected cut");
    std::fs::write(&initial, "changed initial bytes with a different size\n")
        .expect("mutate initial");

    let resumed_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![3],
        vec![Ok(response(passed()))],
    );
    let mut resumed_runner = ProviderStepRunner::new(&resumed_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut resumed = LoopRunner::resume(&runs_root, run_id, &mut resumed_runner)
        .expect("recover initial response");
    resumed.run_next_step().expect("finish first retry");

    let request = serde_json::to_string(&resumed_provider.requests()[0]).expect("request");
    assert!(request.contains("original initial bytes"));
    assert!(!request.contains("changed initial bytes"));
    assert!(request.contains("additional bytes"));
}

#[test]
fn resume_rejects_standalone_or_tampered_expansions_before_provider_invocation() {
    for case in ["standalone", "tampered-referenced"] {
        let temp = tempfile::tempdir().expect("temp");
        let repository = temp.path().join("repository");
        std::fs::create_dir(&repository).expect("repository");
        std::fs::write(repository.join("additional.txt"), "additional\n").expect("context");
        let runs_root = temp.path().join("runs");
        let run_id = format!("resume-expansion-{case}");
        let ticket = ticket(Vec::new());
        let collision = if case == "standalone" {
            "artifacts/01-research.attempt-001.context-round-001.json"
        } else {
            "responses/01-research.attempt-001.exchange-002.context-retry.response.json"
        };
        let provider = InspectingProvider::new(
            runs_root.join(&run_id),
            if case == "standalone" {
                vec![1]
            } else {
                vec![1, 3]
            },
            if case == "standalone" {
                vec![Ok(response(needs_context(&["additional.txt"])))]
            } else {
                vec![
                    Ok(response(needs_context(&["additional.txt"]))),
                    Ok(response(passed())),
                ]
            },
        )
        .with_collisions_before_return(if case == "standalone" {
            vec![Some(collision)]
        } else {
            vec![None, Some(collision)]
        });
        let mut runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_context_pack_request(context_request(&repository, &ticket));
        let mut loop_runner =
            LoopRunner::start(config(&runs_root, &run_id, &ticket), &mut runner).expect("start");
        loop_runner.run_next_step().expect_err("interrupt");
        drop(loop_runner);
        if case == "tampered-referenced" {
            std::fs::remove_file(runs_root.join(&run_id).join(collision))
                .expect("remove collision");
            std::fs::write(
                runs_root
                    .join(&run_id)
                    .join("artifacts/01-research.attempt-001.context-round-001.json"),
                "tampered",
            )
            .expect("tamper expansion");
        }
        let resumed_provider =
            InspectingProvider::new(runs_root.join(&run_id), Vec::new(), Vec::new());
        let mut resumed_runner = ProviderStepRunner::new(&resumed_provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_context_pack_request(context_request(&repository, &ticket));

        let error = LoopRunner::resume(&runs_root, &run_id, &mut resumed_runner)
            .expect_err("unsafe expansion must fail preflight");

        assert!(
            error.to_string().contains("orphan")
                || error.to_string().contains("digest")
                || error.to_string().contains("JSON"),
            "{case}: {error}"
        );
        assert!(resumed_provider.requests().is_empty());
    }
}

#[test]
fn resume_rejects_an_orphaned_exchange_response_before_provider_call_or_mutation() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "resume-orphan-response";
    let ticket = ticket(Vec::new());
    let response_path = "responses/01-research.attempt-001.exchange-001.initial.response.json";
    let first_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1],
        vec![Ok(response(passed()))],
    )
    .with_collisions_before_return(vec![Some(response_path)]);
    let mut first_step_runner = ProviderStepRunner::new(&first_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut first = LoopRunner::start(config(&runs_root, run_id, &ticket), &mut first_step_runner)
        .expect("start");
    first
        .run_next_step()
        .expect_err("response publication interruption");
    drop(first);
    let before = read_run(&runs_root.join(run_id));

    let resumed_provider = InspectingProvider::new(runs_root.join(run_id), Vec::new(), Vec::new());
    let mut resumed_step_runner = ProviderStepRunner::new(&resumed_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let error = LoopRunner::resume(&runs_root, run_id, &mut resumed_step_runner)
        .expect_err("orphan response must fail recovery preflight");

    assert!(error.to_string().contains("orphan"), "{error}");
    assert!(resumed_provider.requests().is_empty());
    assert_eq!(read_run(&runs_root.join(run_id)), before);
}

#[test]
fn resume_atomically_adopts_a_unique_linked_staged_response_record_without_recalling_provider() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "resume-staged-response-record";
    let ticket = ticket(Vec::new());
    let response_record_path =
        "artifacts/01-research.attempt-001.exchange-001.initial.response.record.json";
    let model_response = response(passed());
    let first_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1],
        vec![Ok(model_response.clone())],
    )
    .with_collisions_before_return(vec![Some(response_record_path)]);
    let mut first_step_runner = ProviderStepRunner::new(&first_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut first = LoopRunner::start(config(&runs_root, run_id, &ticket), &mut first_step_runner)
        .expect("start");
    first
        .run_next_step()
        .expect_err("record publication interruption");
    drop(first);
    let run_directory = runs_root.join(run_id);
    std::fs::remove_file(run_directory.join(response_record_path)).expect("remove injected cut");
    let interrupted = read_run(&run_directory);
    assert_eq!(interrupted.provider_exchange_records.len(), 1);
    let request_record =
        load_provider_exchange_record(&run_directory, &interrupted.provider_exchange_records[0])
            .expect("request record");
    let coordinates = ProviderExchangeCoordinates {
        run_id: run_id.to_string(),
        step: LoopStepName::Research,
        role: ProviderRole::Researcher,
        step_attempt: 1,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
    };
    let response_reference = write_provider_exchange_response(
        &run_directory,
        &coordinates,
        &ProviderExchangeResponseAudit::ModelResponse {
            response: model_response,
        },
    )
    .expect("idempotent response audit");
    stage_provider_exchange_record(
        &run_directory,
        &ProviderExchangeRecord {
            schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            step: LoopStepName::Research,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
            phase: ProviderExchangePhase::Response,
            previous_record_digest: Some(interrupted.provider_exchange_records[0].digest.clone()),
            request: request_record.request,
            response: Some(response_reference),
            expansion: None,
            outcome: Some(ProviderExchangeOutcome::Passed),
        },
    )
    .expect("stage linked response record");

    let resumed_provider = InspectingProvider::new(run_directory.clone(), Vec::new(), Vec::new());
    let mut resumed_step_runner = ProviderStepRunner::new(&resumed_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut resumed = LoopRunner::resume(&runs_root, run_id, &mut resumed_step_runner)
        .expect("adopt staged record");
    resumed.run_next_step().expect("close recovered response");

    assert!(resumed_provider.requests().is_empty());
    let run = read_run(&run_directory);
    assert_eq!(run.provider_exchange_records.len(), 2);
    assert_eq!(
        step_status(&run, LoopStepName::Research),
        LoopStepStatus::Completed
    );
}

#[test]
fn recovery_rejects_missing_reordered_and_substituted_staged_suffixes_without_head_mutation() {
    for case in ["missing", "reordered", "substituted", "digest-invalid"] {
        let temp = tempfile::tempdir().expect("temp");
        let repository = temp.path().join("repository");
        std::fs::create_dir(&repository).expect("repository");
        let runs_root = temp.path().join("runs");
        let run_id = format!("reject-staged-{case}");
        let ticket = ticket(Vec::new());
        let response_record_path =
            "artifacts/01-research.attempt-001.exchange-001.initial.response.record.json";
        let model_response = response(passed());
        let first_provider = InspectingProvider::new(
            runs_root.join(&run_id),
            vec![1],
            vec![Ok(model_response.clone())],
        )
        .with_collisions_before_return(vec![Some(response_record_path)]);
        let mut first_runner = ProviderStepRunner::new(&first_provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_context_pack_request(context_request(&repository, &ticket));
        let mut first = LoopRunner::start(config(&runs_root, &run_id, &ticket), &mut first_runner)
            .expect("start");
        first
            .run_next_step()
            .expect_err("interrupt response record");
        drop(first);
        let run_directory = runs_root.join(&run_id);
        std::fs::remove_file(run_directory.join(response_record_path)).expect("remove cut");
        let interrupted = read_run(&run_directory);
        let request_record = load_provider_exchange_record(
            &run_directory,
            interrupted
                .provider_exchange_records
                .last()
                .expect("request"),
        )
        .expect("request record");
        let coordinates = ProviderExchangeCoordinates {
            run_id: run_id.clone(),
            step: LoopStepName::Research,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
        };
        let response_reference = write_provider_exchange_response(
            &run_directory,
            &coordinates,
            &ProviderExchangeResponseAudit::ModelResponse {
                response: model_response,
            },
        )
        .expect("response audit");
        let mut staged = ProviderExchangeRecord {
            schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: run_id.clone(),
            step: LoopStepName::Research,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
            phase: ProviderExchangePhase::Response,
            previous_record_digest: Some(
                interrupted
                    .provider_exchange_records
                    .last()
                    .expect("head")
                    .digest
                    .clone(),
            ),
            request: request_record.request,
            response: Some(response_reference.clone()),
            expansion: None,
            outcome: Some(ProviderExchangeOutcome::Passed),
        };
        stage_provider_exchange_record(&run_directory, &staged).expect("stage response");
        match case {
            "missing" => {
                std::fs::remove_file(run_directory.join(&response_reference.path))
                    .expect("remove bound response");
            }
            "reordered" => {
                staged.previous_record_digest = Some("f".repeat(64));
                std::fs::write(
                    run_directory.join(response_record_path),
                    seaf_core::canonical_json_bytes(&staged).expect("record bytes"),
                )
                .expect("rewrite reordered record");
            }
            "substituted" => {
                staged.request.path =
                    "prompts/01-research.attempt-001.exchange-001.initial.request-substitute.md"
                        .to_string();
                std::fs::write(
                    run_directory.join(response_record_path),
                    seaf_core::canonical_json_bytes(&staged).expect("record bytes"),
                )
                .expect("rewrite substituted record");
            }
            "digest-invalid" => {
                staged.response.as_mut().expect("response").digest = "f".repeat(64);
                std::fs::write(
                    run_directory.join(response_record_path),
                    seaf_core::canonical_json_bytes(&staged).expect("record bytes"),
                )
                .expect("rewrite digest-invalid record");
            }
            _ => unreachable!(),
        }
        let run_before = std::fs::read(run_directory.join("run.json")).expect("run bytes");
        let provider = InspectingProvider::new(run_directory.clone(), Vec::new(), Vec::new());
        let mut resumed_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
            .with_ticket(ticket.clone())
            .with_context_pack_request(context_request(&repository, &ticket));
        let error = LoopRunner::resume(&runs_root, &run_id, &mut resumed_runner)
            .expect_err("invalid staged suffix must fail recovery");

        assert!(
            error.to_string().contains("recovery preflight")
                || error.to_string().contains("provider exchange"),
            "{case}: {error}"
        );
        assert!(provider.requests().is_empty(), "{case}");
        assert_eq!(
            std::fs::read(run_directory.join("run.json")).expect("run bytes"),
            run_before,
            "{case}"
        );
    }
}

#[test]
fn recovery_rejects_a_staged_next_role_attempt_999_without_advancing_the_head() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "reject-next-role-attempt-999";
    let ticket = ticket(Vec::new());
    let initial_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1],
        vec![Ok(response(passed()))],
    );
    let mut initial_runner = ProviderStepRunner::new(&initial_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut initial =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut initial_runner).expect("start");
    initial.run_next_step().expect("research");
    drop(initial);
    let run_directory = runs_root.join(run_id);
    let before = read_run(&run_directory);
    let coordinates = ProviderExchangeCoordinates {
        run_id: run_id.to_string(),
        step: LoopStepName::Analysis,
        role: ProviderRole::Analyzer,
        step_attempt: 999,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
    };
    let request = seaf_loop::write_provider_exchange_request(
        &run_directory,
        &coordinates,
        b"staged analysis attempt 999",
    )
    .expect("request");
    stage_provider_exchange_record(
        &run_directory,
        &ProviderExchangeRecord {
            schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            step: LoopStepName::Analysis,
            role: ProviderRole::Analyzer,
            step_attempt: 999,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: Some(
                before
                    .provider_exchange_records
                    .last()
                    .expect("head")
                    .digest
                    .clone(),
            ),
            request,
            response: None,
            expansion: None,
            outcome: None,
        },
    )
    .expect("stage attempt 999");
    let provider = InspectingProvider::new(run_directory.clone(), Vec::new(), Vec::new());
    let mut resumed_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let error = LoopRunner::resume(&runs_root, run_id, &mut resumed_runner)
        .expect_err("attempt 999 must reject");

    assert!(error.to_string().contains("orphaned"), "{error}");
    assert!(provider.requests().is_empty());
    assert_eq!(
        read_run(&run_directory).provider_exchange_records,
        before.provider_exchange_records
    );
}

#[test]
fn explicit_rerun_uses_a_new_audited_attempt_and_preserves_the_step_context_cap() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    for name in ["one.txt", "two.txt", "three.txt"] {
        std::fs::write(repository.join(name), name).expect("context");
    }
    let runs_root = temp.path().join("runs");
    let run_id = "rerun-preserves-context-cap";
    let ticket = ticket(Vec::new());
    let first_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1, 3, 5],
        vec![
            Ok(response(needs_context(&["one.txt"]))),
            Ok(response(needs_context(&["two.txt"]))),
            Ok(response(passed())),
        ],
    );
    let mut first_step_runner = ProviderStepRunner::new(&first_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut first = LoopRunner::start(config(&runs_root, run_id, &ticket), &mut first_step_runner)
        .expect("start");
    first.run_next_step().expect("first attempt");
    drop(first);

    let rerun_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![7],
        vec![Ok(response(needs_context(&["three.txt"])))],
    );
    let mut rerun_step_runner = ProviderStepRunner::new(&rerun_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut rerun = LoopRunner::resume(&runs_root, run_id, &mut rerun_step_runner)
        .expect("resume")
        .rerun_from(LoopStepName::Research)
        .expect("explicit rerun");
    rerun.run_next_step().expect("cap denial on rerun");

    let run = read_run(&runs_root.join(run_id));
    assert_eq!(rerun_provider.requests().len(), 1);
    assert_eq!(run.provider_exchange_records.len(), 8);
    assert_eq!(
        step_status(&run, LoopStepName::Research),
        LoopStepStatus::Blocked
    );
    assert!(run_directory_has_attempt(
        &runs_root.join(run_id),
        "attempt-002"
    ));
}

#[test]
fn legacy_terminal_needs_context_history_stays_inert_until_explicit_audited_rerun() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "legacy-needs-context-rerun";
    let ticket = ticket(Vec::new());
    let first_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1],
        vec![Ok(response(needs_context(&["missing.txt"])))],
    );
    let mut first_runner = ProviderStepRunner::new(&first_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut first =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut first_runner).expect("start");
    first.run_next_step().expect("terminal context denial");
    drop(first);
    let run_directory = runs_root.join(run_id);
    let mut legacy = read_run(&run_directory);
    legacy.provider_exchange_records.clear();
    let parsed = seaf_loop::parse_role_response(Role::Researcher, &needs_context(&["missing.txt"]))
        .expect("legacy needs_context response");
    let artifact =
        ValidatedRoleArtifact::new(run_id, LoopStepName::Research, Role::Researcher, parsed)
            .expect("legacy artifact");
    let research = legacy
        .steps
        .iter_mut()
        .find(|record| record.name == LoopStepName::Research)
        .expect("research");
    let artifact_path = research.artifact_path.clone().expect("artifact path");
    std::fs::write(
        run_directory.join(&artifact_path),
        artifact.canonical_bytes().expect("artifact bytes"),
    )
    .expect("write legacy artifact");
    research.artifact_digest = Some(artifact.artifact_digest().expect("artifact digest"));
    seaf_loop::state::write_run_file(&run_directory.join("run.json"), &legacy)
        .expect("write legacy run shape");
    remove_exchange_family_files(&run_directory);
    let original_prompt =
        std::fs::read(run_directory.join("prompts/01-research.prompt.md")).expect("legacy request");

    let inert_provider = InspectingProvider::new(run_directory.clone(), Vec::new(), Vec::new());
    let mut inert_runner = ProviderStepRunner::new(&inert_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut inert = LoopRunner::resume(&runs_root, run_id, &mut inert_runner).expect("resume");
    assert!(!inert.run_next_step().expect("terminal remains inert"));
    assert!(inert_provider.requests().is_empty());
    drop(inert);

    let rerun_provider =
        InspectingProvider::new(run_directory.clone(), vec![1], vec![Ok(response(passed()))]);
    let mut rerun_runner = ProviderStepRunner::new(&rerun_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut rerun = LoopRunner::resume(&runs_root, run_id, &mut rerun_runner)
        .expect("resume for explicit rerun")
        .rerun_from(LoopStepName::Research)
        .expect("authorize rerun");
    rerun.run_next_step().expect("audited rerun");

    assert_eq!(rerun_provider.requests().len(), 1);
    let run = read_run(&run_directory);
    assert_eq!(run.provider_exchange_records.len(), 2);
    assert_eq!(run.provider_exchange_records[0].step_attempt, 2);
    assert_eq!(
        std::fs::read(run_directory.join("prompts/01-research.prompt.md"))
            .expect("legacy request remains"),
        original_prompt
    );
    drop(rerun);
    std::fs::remove_file(
        run_directory.join("artifacts/01-research.attempt-002.rerun-authorization.json"),
    )
    .expect("remove rerun authorization");
    let rejected_provider = InspectingProvider::new(run_directory.clone(), Vec::new(), Vec::new());
    let mut rejected_runner = ProviderStepRunner::new(&rejected_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let error = LoopRunner::resume(&runs_root, run_id, &mut rejected_runner)
        .expect_err("legacy attempt-two history requires its rerun authorization");
    assert!(error.to_string().contains("rerun authorization"), "{error}");
    assert!(rejected_provider.requests().is_empty());
}

#[test]
fn incomplete_empty_ledger_resume_starts_with_an_audited_initial_request() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "resume-empty-ledger";
    let ticket = ticket(Vec::new());
    let unused_provider = InspectingProvider::new(runs_root.join(run_id), Vec::new(), Vec::new());
    let mut initial_runner = ProviderStepRunner::new(&unused_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let initial = LoopRunner::start(config(&runs_root, run_id, &ticket), &mut initial_runner)
        .expect("create run before first request");
    drop(initial);

    let provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1],
        vec![Ok(response(passed()))],
    );
    let mut resumed_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut resumed =
        LoopRunner::resume(&runs_root, run_id, &mut resumed_runner).expect("resume empty ledger");
    resumed.run_next_step().expect("audited first call");

    let run = read_run(&runs_root.join(run_id));
    assert_eq!(provider.requests().len(), 1);
    assert_eq!(run.provider_exchange_records.len(), 2);
    assert_eq!(
        run.provider_exchange_records[0].phase,
        ProviderExchangePhase::Request
    );
}

#[test]
fn empty_legacy_ledger_rejects_an_unauthorized_staged_attempt_two_request() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "reject-empty-ledger-staged-attempt-two";
    let ticket = ticket(Vec::new());
    let unused = InspectingProvider::new(runs_root.join(run_id), Vec::new(), Vec::new());
    let mut initial_runner = ProviderStepRunner::new(&unused, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let initial = LoopRunner::start(config(&runs_root, run_id, &ticket), &mut initial_runner)
        .expect("empty run");
    drop(initial);
    let run_directory = runs_root.join(run_id);
    let coordinates = ProviderExchangeCoordinates {
        run_id: run_id.to_string(),
        step: LoopStepName::Research,
        role: ProviderRole::Researcher,
        step_attempt: 2,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
    };
    let request = seaf_loop::write_provider_exchange_request(
        &run_directory,
        &coordinates,
        b"unauthorized attempt two",
    )
    .expect("request file");
    stage_provider_exchange_record(
        &run_directory,
        &ProviderExchangeRecord {
            schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            step: LoopStepName::Research,
            role: ProviderRole::Researcher,
            step_attempt: 2,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: None,
            request,
            response: None,
            expansion: None,
            outcome: None,
        },
    )
    .expect("stage unauthorized request");

    let provider = InspectingProvider::new(run_directory, Vec::new(), Vec::new());
    let mut resumed_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let error = LoopRunner::resume(&runs_root, run_id, &mut resumed_runner)
        .expect_err("unauthorized staged attempt must not be adopted");

    assert!(error.to_string().contains("orphaned"), "{error}");
    assert!(provider.requests().is_empty());
    assert!(read_run(&runs_root.join(run_id))
        .provider_exchange_records
        .is_empty());
}

#[test]
fn explicit_rerun_after_resume_preserves_the_run_wide_eight_expansion_cap() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    for index in 1..=9 {
        std::fs::write(
            repository.join(format!("context-{index}.txt")),
            format!("context {index}\n"),
        )
        .expect("context");
    }
    let runs_root = temp.path().join("runs");
    let run_id = "run-wide-cap-rerun";
    let ticket = ticket(Vec::new());
    let script = vec![
        Ok(response(agent_needs_context("researcher", "context-1.txt"))),
        Ok(response(agent_needs_context("researcher", "context-2.txt"))),
        Ok(response(agent_passed("researcher"))),
        Ok(response(agent_needs_context("analyzer", "context-3.txt"))),
        Ok(response(agent_needs_context("analyzer", "context-4.txt"))),
        Ok(response(agent_passed("analyzer"))),
        Ok(response(agent_needs_context(
            "spec_writer",
            "context-5.txt",
        ))),
        Ok(response(agent_needs_context(
            "spec_writer",
            "context-6.txt",
        ))),
        Ok(response(agent_passed("spec_writer"))),
        Ok(response(reviewer_approval("spec_reviewer", "approve_spec"))),
        Ok(response(developer_needs_context("context-7.txt"))),
        Ok(response(developer_needs_context("context-8.txt"))),
        Ok(response(developer_blocked())),
    ];
    let provider = InspectingProvider::new(
        runs_root.join(run_id),
        (0..script.len()).map(|index| index * 2 + 1).collect(),
        script,
    );
    let mut runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut loop_runner =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut runner).expect("start");
    loop_runner
        .run_to_completion()
        .expect("reach blocked development");
    drop(loop_runner);
    let before = read_run(&runs_root.join(run_id));
    assert_eq!(
        before
            .provider_exchange_records
            .iter()
            .filter(|record| {
                record.phase == ProviderExchangePhase::Request
                    && record.kind == ProviderExchangeKind::ContextRetry
            })
            .count(),
        8
    );

    let rerun_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![before.provider_exchange_records.len() + 1],
        vec![Ok(response(agent_needs_context(
            "researcher",
            "context-9.txt",
        )))],
    );
    let mut rerun_runner = ProviderStepRunner::new(&rerun_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut rerun = LoopRunner::resume(&runs_root, run_id, &mut rerun_runner)
        .expect("resume")
        .rerun_from(LoopStepName::Research)
        .expect("rerun");
    rerun.run_next_step().expect("run-wide cap denial");

    let after = read_run(&runs_root.join(run_id));
    assert_eq!(rerun_provider.requests().len(), 1);
    assert_eq!(
        after.provider_exchange_records.len(),
        before.provider_exchange_records.len() + 2
    );
    assert_eq!(
        step_status(&after, LoopStepName::Research),
        LoopStepStatus::Blocked
    );
    assert!(!runs_root
        .join(run_id)
        .join("artifacts/01-research.attempt-002.context-round-001.json")
        .exists());
}

#[test]
fn resume_continues_an_exact_malformed_json_repair_request() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "resume-json-repair-request";
    let ticket = ticket(Vec::new());
    let response_path = "responses/01-research.attempt-001.exchange-002.json-repair.response.json";
    let provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1, 3],
        vec![Ok(response("not json".to_string())), Ok(response(passed()))],
    )
    .with_collisions_before_return(vec![None, Some(response_path)]);
    let mut runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut loop_runner =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut runner).expect("start");
    loop_runner.run_next_step().expect_err("interrupt repair");
    drop(loop_runner);
    let exact_repair = provider.requests()[1].clone();
    std::fs::remove_file(runs_root.join(run_id).join(response_path)).expect("remove cut");

    let resumed_provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![3],
        vec![Ok(response(passed()))],
    );
    let mut resumed_runner = ProviderStepRunner::new(&resumed_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut resumed =
        LoopRunner::resume(&runs_root, run_id, &mut resumed_runner).expect("resume repair request");
    resumed.run_next_step().expect("finish repair");

    assert_eq!(resumed_provider.requests(), vec![exact_repair]);
    assert_eq!(
        read_run(&runs_root.join(run_id))
            .provider_exchange_records
            .len(),
        4
    );
}

#[test]
fn resume_adopts_a_staged_json_repair_response_without_recalling_provider() {
    let temp = tempfile::tempdir().expect("temp");
    let repository = temp.path().join("repository");
    std::fs::create_dir(&repository).expect("repository");
    let runs_root = temp.path().join("runs");
    let run_id = "resume-staged-json-repair-response";
    let ticket = ticket(Vec::new());
    let response_record_path =
        "artifacts/01-research.attempt-001.exchange-002.json-repair.response.record.json";
    let repaired = response(passed());
    let provider = InspectingProvider::new(
        runs_root.join(run_id),
        vec![1, 3],
        vec![Ok(response("not json".to_string())), Ok(repaired.clone())],
    )
    .with_collisions_before_return(vec![None, Some(response_record_path)]);
    let mut runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut loop_runner =
        LoopRunner::start(config(&runs_root, run_id, &ticket), &mut runner).expect("start");
    loop_runner
        .run_next_step()
        .expect_err("interrupt repair record");
    drop(loop_runner);
    let run_directory = runs_root.join(run_id);
    std::fs::remove_file(run_directory.join(response_record_path)).expect("remove cut");
    let interrupted = read_run(&run_directory);
    let request_record = load_provider_exchange_record(
        &run_directory,
        interrupted
            .provider_exchange_records
            .last()
            .expect("repair request"),
    )
    .expect("repair request record");
    let coordinates = ProviderExchangeCoordinates {
        run_id: run_id.to_string(),
        step: LoopStepName::Research,
        role: ProviderRole::Researcher,
        step_attempt: 1,
        exchange_index: 2,
        kind: ProviderExchangeKind::JsonRepair,
        context_round: None,
    };
    let response_reference = write_provider_exchange_response(
        &run_directory,
        &coordinates,
        &ProviderExchangeResponseAudit::ModelResponse { response: repaired },
    )
    .expect("response audit");
    stage_provider_exchange_record(
        &run_directory,
        &ProviderExchangeRecord {
            schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            step: LoopStepName::Research,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 2,
            kind: ProviderExchangeKind::JsonRepair,
            context_round: None,
            phase: ProviderExchangePhase::Response,
            previous_record_digest: Some(
                interrupted
                    .provider_exchange_records
                    .last()
                    .expect("head")
                    .digest
                    .clone(),
            ),
            request: request_record.request,
            response: Some(response_reference),
            expansion: None,
            outcome: Some(ProviderExchangeOutcome::Passed),
        },
    )
    .expect("stage repair response");

    let resumed_provider = InspectingProvider::new(run_directory.clone(), Vec::new(), Vec::new());
    let mut resumed_runner = ProviderStepRunner::new(&resumed_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&repository, &ticket));
    let mut resumed = LoopRunner::resume(&runs_root, run_id, &mut resumed_runner)
        .expect("adopt staged repair response");
    resumed.run_next_step().expect("close repair");

    assert!(resumed_provider.requests().is_empty());
    assert_eq!(read_run(&run_directory).provider_exchange_records.len(), 4);
}

fn remove_exchange_family_files(run_directory: &Path) {
    for directory in ["prompts", "responses", "artifacts"] {
        for entry in std::fs::read_dir(run_directory.join(directory)).expect("artifact directory") {
            let entry = entry.expect("artifact entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.contains(".exchange-") || name.contains(".context-round-") {
                std::fs::remove_file(entry.path()).expect("remove modern exchange artifact");
            }
        }
    }
}

fn run_directory_has_attempt(run_directory: &Path, attempt: &str) -> bool {
    ["prompts", "responses", "artifacts"]
        .into_iter()
        .any(|name| {
            std::fs::read_dir(run_directory.join(name))
                .expect("read artifact directory")
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().contains(attempt))
        })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("test mutex")
}

fn response(content: String) -> ModelResponse {
    ModelResponse {
        content,
        latency_ms: 7,
        raw_provider_metadata: json!({"provider": "fake"}),
    }
}

fn passed() -> String {
    json!({
        "role": "researcher",
        "status": "passed",
        "summary": "Enough evidence.",
        "findings": [],
        "risks": [],
        "next_step_recommendation": "Continue."
    })
    .to_string()
}

fn agent_passed(role: &str) -> String {
    json!({
        "role": role,
        "status": "passed",
        "summary": "Enough evidence.",
        "findings": [],
        "risks": [],
        "next_step_recommendation": "Continue."
    })
    .to_string()
}

fn needs_context(paths: &[&str]) -> String {
    json!({
        "role": "researcher",
        "status": "needs_context",
        "summary": "More evidence is required.",
        "findings": [],
        "risks": [],
        "next_step_recommendation": "Load the requested files.",
        "context_request": {
            "paths": paths,
            "reason": "The requested file is required to answer the ticket."
        }
    })
    .to_string()
}

fn agent_needs_context(role: &str, path: &str) -> String {
    json!({
        "role": role,
        "status": "needs_context",
        "summary": "More evidence is required.",
        "findings": [],
        "risks": [],
        "next_step_recommendation": "Load context.",
        "context_request": {
            "paths": [path],
            "reason": "This path is required for the current role."
        }
    })
    .to_string()
}

fn developer_needs_context(path: &str) -> String {
    json!({
        "role": "developer",
        "status": "needs_context",
        "summary": "More evidence is required.",
        "changed_files": [],
        "requires_human_review": false,
        "context_request": {
            "paths": [path],
            "reason": "This path is required to propose the patch."
        }
    })
    .to_string()
}

fn developer_blocked() -> String {
    json!({
        "role": "developer",
        "status": "blocked",
        "summary": "Stop after establishing the run-wide cap.",
        "changed_files": [],
        "requires_human_review": false
    })
    .to_string()
}

fn reviewer_approval(role: &str, decision: &str) -> String {
    json!({
        "role": role,
        "decision": decision,
        "summary": "Approved.",
        "blocking_issues": [],
        "non_blocking_issues": []
    })
    .to_string()
}

fn ticket(relevant_files: Vec<&str>) -> TicketSpec {
    TicketSpec {
        ticket_id: "M1-04b2b".to_string(),
        goal_id: "production-use".to_string(),
        title: "Execute bounded live context".to_string(),
        status: TicketStatus::Ready,
        priority: TicketPriority::P1,
        problem: "Provider context retries must be durable.".to_string(),
        research_questions: Vec::new(),
        context: TicketContext {
            relevant_files: relevant_files.into_iter().map(str::to_string).collect(),
            forbidden_files: Vec::new(),
        },
        autonomy: TicketAutonomy {
            level: 1,
            apply_patch: false,
            allow_shell_commands: Vec::new(),
        },
        acceptance_criteria: vec!["Durable bounded context retries.".to_string()],
        eval: None,
    }
}

fn config(runs_root: &Path, run_id: &str, ticket: &TicketSpec) -> LoopRunnerConfig {
    LoopRunnerConfig::for_ticket(
        runs_root,
        run_id,
        ticket,
        "fake",
        "fake-model",
        LoopInputDigests {
            ticket: canonical_sha256_digest(ticket).expect("ticket digest"),
            policy: "b".repeat(64),
            config: "c".repeat(64),
            repository: "d".repeat(64),
        },
    )
}

fn context_request(repository: &Path, ticket: &TicketSpec) -> ContextPackRequest {
    ContextPackRequest::for_ticket(
        repository,
        Path::new("workspace-selected-by-runner"),
        ticket,
        &[],
        ContextLimits {
            max_bytes_per_file: 1_024,
            max_total_bytes: 8_192,
        },
    )
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
