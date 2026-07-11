use std::{fs, path::Path};

#[cfg(unix)]
use std::os::unix::fs::symlink;

use seaf_core::{
    LoopInputDigests, LoopRun, LoopStatus, LoopStepName, LoopStepStatus, ProviderExchangeKind,
    ProviderExchangePhase, ProviderExchangeRecord, ProviderRole, TicketContext, TicketSpec,
    TicketStatus,
};
use seaf_loop::{
    persist_provider_exchange_record_reference, stage_provider_exchange_record,
    state::{create_run, finish_step, NewLoopRun},
    write_provider_exchange_request, ArtifactContent, ContextManifest, LoopRunner,
    LoopRunnerConfig, LoopWorkspace, ProviderExchangeCoordinates, StepOutput, StepRunner,
    UNTRUSTED_CONTEXT_MARKER,
};

#[test]
fn state_save_with_stale_empty_exchange_vector_cannot_erase_first_concurrent_request() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    let ticket = ticket();
    let mut step_runner = FirstConcurrentExchangeRunner { workspace: None };
    let mut runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "first-exchange-race",
            &ticket,
            "fake",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start");

    let error = runner
        .run_next_step()
        .expect_err("stale empty state must not erase the first exchange");

    assert!(error.to_string().contains("exchange head changed"));
    let workspace = LoopWorkspace::open(&runs_root, "first-exchange-race").expect("workspace");
    let persisted = seaf_loop::state::load_run(&workspace).expect("persisted run");
    assert_eq!(persisted.provider_exchange_records.len(), 1);
    assert_eq!(
        persisted.provider_exchange_records[0].phase,
        ProviderExchangePhase::Request
    );
    assert_eq!(persisted.status, LoopStatus::Running);
}

struct FirstConcurrentExchangeRunner {
    workspace: Option<LoopWorkspace>,
}

impl StepRunner for FirstConcurrentExchangeRunner {
    fn prepare_run(
        &mut self,
        workspace: &LoopWorkspace,
        _run: &LoopRun,
    ) -> Result<(), seaf_loop::RunnerError> {
        self.workspace = Some(workspace.clone());
        Ok(())
    }

    fn step_request(&mut self, _step: LoopStepName) -> Result<String, seaf_loop::RunnerError> {
        Ok("ordinary step request".to_string())
    }

    fn run_step(
        &mut self,
        step: LoopStepName,
        _request: &str,
    ) -> Result<StepOutput, seaf_loop::RunnerError> {
        let workspace = self.workspace.as_ref().expect("prepared workspace");
        let coordinates = ProviderExchangeCoordinates {
            run_id: "first-exchange-race".to_string(),
            step,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
        };
        let request = write_provider_exchange_request(
            workspace.run_directory(),
            &coordinates,
            b"concurrent request",
        )
        .expect("concurrent request");
        let record = ProviderExchangeRecord {
            schema_version: 1,
            run_id: coordinates.run_id,
            step: coordinates.step,
            role: coordinates.role,
            step_attempt: coordinates.step_attempt,
            exchange_index: coordinates.exchange_index,
            kind: coordinates.kind,
            context_round: coordinates.context_round,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: None,
            request,
            response: None,
            expansion: None,
            outcome: None,
        };
        let reference = stage_provider_exchange_record(workspace.run_directory(), &record)
            .expect("stage concurrent request");
        persist_provider_exchange_record_reference(workspace, reference)
            .expect("append concurrent request");
        Ok(StepOutput::completed("ordinary step response"))
    }
}

#[test]
fn state_finish_step_rejects_unpaired_or_malformed_artifact_integrity() {
    let base = NewLoopRun {
        run_id: "artifact-integrity".to_string(),
        ticket_id: "P2-005".to_string(),
        goal_id: "phase-2".to_string(),
        provider: "fake-provider".to_string(),
        model: "fake-model".to_string(),
        input_digests: test_input_digests(),
    };
    let mut unpaired = create_run(base.clone());
    let error = finish_step(
        &mut unpaired,
        LoopStepName::Research,
        LoopStepStatus::Completed,
        Some("artifacts/01-research.json".to_string()),
        None,
    )
    .expect_err("artifact path without digest must fail");
    assert!(error.to_string().contains("artifact path and digest"));

    let mut malformed = create_run(base);
    let error = finish_step(
        &mut malformed,
        LoopStepName::Research,
        LoopStepStatus::Completed,
        Some("artifacts/01-research.json".to_string()),
        Some("not-a-digest".to_string()),
    )
    .expect_err("malformed artifact digest must fail");
    assert!(error.to_string().contains("artifact digest"));
}

#[test]
fn state_creation_preserves_exact_effective_input_digests() {
    let input_digests = LoopInputDigests {
        ticket: "a".repeat(64),
        policy: "b".repeat(64),
        config: "c".repeat(64),
        repository: "d".repeat(64),
    };

    let run = create_run(NewLoopRun {
        run_id: "run-with-input-digests".to_string(),
        ticket_id: "T-LOCAL-001".to_string(),
        goal_id: "local_agent_loop_mvp".to_string(),
        provider: "fake-provider".to_string(),
        model: "fake-model".to_string(),
        input_digests: input_digests.clone(),
    });

    assert_eq!(run.input_digests, input_digests);
}

#[test]
fn state_resume_skips_completed_steps_after_interruption() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut first_runner = RecordingStepRunner::with_prefix("initial");
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-resume",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut first_runner,
    )
    .expect("start run");

    run.run_next_step().expect("run first step");
    drop(run);

    assert_eq!(first_runner.calls, vec![LoopStepName::Research]);

    let mut resumed_runner = RecordingStepRunner::with_prefix("resumed");
    let mut resumed =
        LoopRunner::resume(&runs_root, "run-resume", &mut resumed_runner).expect("resume run");

    resumed.run_to_completion().expect("complete resumed run");
    let final_status = resumed.run().status;
    drop(resumed);

    assert!(
        !resumed_runner.calls.contains(&LoopStepName::Research),
        "completed steps should not be rerun on resume"
    );
    assert_eq!(final_status, seaf_core::LoopStatus::Completed);
}

#[test]
fn state_start_rejects_duplicate_run_id_without_touching_audit_files() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut first_runner = RecordingStepRunner::with_prefix("first");
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-duplicate",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut first_runner,
    )
    .expect("start run");
    run.run_next_step().expect("run first step");
    drop(run);

    let run_dir = runs_root.join("run-duplicate");
    let original_run_json = std::fs::read_to_string(run_dir.join("run.json")).expect("run json");
    let original_prompt =
        std::fs::read_to_string(run_dir.join("prompts/01-research.prompt.md")).expect("prompt");

    let mut duplicate_runner = RecordingStepRunner::with_prefix("duplicate");
    let error = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-duplicate",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut duplicate_runner,
    )
    .expect_err("duplicate run id should fail");

    assert!(error.to_string().contains("already exists"));
    assert_eq!(
        std::fs::read_to_string(run_dir.join("run.json")).expect("run json"),
        original_run_json
    );
    assert_eq!(
        std::fs::read_to_string(run_dir.join("prompts/01-research.prompt.md")).expect("prompt"),
        original_prompt
    );
    assert!(
        !run_dir
            .join("prompts/01-research.attempt-002.prompt.md")
            .exists(),
        "duplicate start must not create a second prompt attempt"
    );
    assert!(duplicate_runner.calls.is_empty());
}

#[test]
fn state_passed_run_status_is_terminal_even_with_runnable_steps() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut setup_runner = RecordingStepRunner::new();
    let run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-passed-terminal",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut setup_runner,
    )
    .expect("start run");
    drop(run);

    let run_dir = runs_root.join("run-passed-terminal");
    let mut persisted = read_run(&run_dir);
    persisted.status = LoopStatus::Passed;
    persisted.current_step = LoopStepName::Research;
    persisted.steps[0].status = LoopStepStatus::Running;
    persisted.steps[1].status = LoopStepStatus::Pending;
    write_run(&run_dir, &persisted);

    let mut step_runner = RecordingStepRunner::with_prefix("resume");
    let mut resumed =
        LoopRunner::resume(&runs_root, "run-passed-terminal", &mut step_runner).expect("resume");

    let did_run = resumed.run_next_step().expect("run next step");
    drop(resumed);

    assert!(!did_run);
    assert!(step_runner.calls.is_empty());
    assert!(
        !run_dir.join("prompts/01-research.prompt.md").exists(),
        "terminal passed runs should not execute runnable steps"
    );
}

#[test]
fn state_resume_reruns_persisted_running_step() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut setup_runner = RecordingStepRunner::new();
    let run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-running-resume",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut setup_runner,
    )
    .expect("start run");
    drop(run);

    let run_dir = runs_root.join("run-running-resume");
    let mut persisted = read_run(&run_dir);
    persisted.status = LoopStatus::Running;
    persisted.current_step = LoopStepName::Research;
    persisted.steps[0].status = LoopStepStatus::Running;
    write_run(&run_dir, &persisted);

    let mut step_runner = RecordingStepRunner::with_prefix("resumed-running");
    let mut resumed =
        LoopRunner::resume(&runs_root, "run-running-resume", &mut step_runner).expect("resume");

    resumed.run_next_step().expect("run running step");
    drop(resumed);

    assert_eq!(step_runner.calls, vec![LoopStepName::Research]);
    assert_file_contains(
        &run_dir.join("prompts/01-research.prompt.md"),
        "resumed-running request for research",
    );
}

#[test]
fn state_blocked_step_updates_run_json_and_stops_execution() {
    assert_terminal_step_output_updates_run_and_stops(
        "run-blocked-terminal",
        LoopStepStatus::Blocked,
        LoopStatus::Blocked,
    );
}

#[test]
fn state_failed_step_updates_run_json_and_stops_execution() {
    assert_terminal_step_output_updates_run_and_stops(
        "run-failed-terminal",
        LoopStepStatus::Failed,
        LoopStatus::Failed,
    );
}

#[test]
fn state_rerun_from_repeats_selected_step_without_repeating_prior_steps() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut initial_runner = RecordingStepRunner::with_prefix("initial");
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-rerun",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut initial_runner,
    )
    .expect("start run");
    run.run_to_completion().expect("complete initial run");

    let run_dir = runs_root.join("run-rerun");
    assert_file_contains(
        &run_dir.join("prompts/03-spec.prompt.md"),
        "initial request for spec creation",
    );
    assert_file_contains(
        &run_dir.join("responses/03-spec.raw.txt"),
        "initial response for spec creation",
    );

    let mut rerun_runner = RecordingStepRunner::with_prefix("rerun");
    let mut rerun = LoopRunner::resume(&runs_root, "run-rerun", &mut rerun_runner)
        .expect("resume run")
        .rerun_from(LoopStepName::SpecCreation)
        .expect("reset from spec creation");

    rerun.run_to_completion().expect("complete rerun");
    drop(rerun);

    assert_eq!(
        rerun_runner.calls.first(),
        Some(&LoopStepName::SpecCreation),
        "rerun should restart at the requested step"
    );
    assert!(
        !rerun_runner.calls.contains(&LoopStepName::Research)
            && !rerun_runner.calls.contains(&LoopStepName::Analysis),
        "rerun should preserve completed steps before the requested step"
    );
    assert_file_contains(
        &run_dir.join("prompts/03-spec.prompt.md"),
        "initial request for spec creation",
    );
    assert_file_contains(
        &run_dir.join("responses/03-spec.raw.txt"),
        "initial response for spec creation",
    );
    assert_file_contains(
        &run_dir.join("prompts/03-spec.attempt-002.prompt.md"),
        "rerun request for spec creation",
    );
    assert_file_contains(
        &run_dir.join("responses/03-spec.attempt-002.raw.txt"),
        "rerun response for spec creation",
    );
}

#[test]
fn state_writes_run_workspace_prompt_response_artifact_and_log() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut step_runner = RecordingStepRunner::with_prefix("first");
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-artifacts",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start run");

    run.run_next_step().expect("run first step");

    let run_dir = runs_root.join("run-artifacts");
    assert!(run_dir.join("run.json").is_file());
    let manifest_json =
        std::fs::read_to_string(run_dir.join("context-manifest.json")).expect("manifest");
    let manifest: ContextManifest =
        serde_json::from_str(&manifest_json).expect("empty context manifest");
    assert_eq!(manifest.untrusted_context_marker, UNTRUSTED_CONTEXT_MARKER);
    assert_eq!(manifest.total_context_bytes, 0);
    assert!(manifest.files.is_empty());
    assert!(run_dir.join("prompts/01-research.prompt.md").is_file());
    assert!(run_dir.join("responses/01-research.raw.txt").is_file());
    assert!(run_dir.join("artifacts/01-research.md").is_file());
    assert!(run_dir.join("log.md").is_file());
    assert_file_contains(
        &run_dir.join("prompts/01-research.prompt.md"),
        "first request for research",
    );
    assert_file_contains(
        &run_dir.join("responses/01-research.raw.txt"),
        "first response for research",
    );

    let run_json = std::fs::read_to_string(run_dir.join("run.json")).expect("run json");
    let persisted: seaf_core::LoopRun = serde_json::from_str(&run_json).expect("run json");
    assert_eq!(persisted.status, seaf_core::LoopStatus::Running);
    assert_eq!(persisted.current_step, LoopStepName::Analysis);
    let research = persisted
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::Research)
        .expect("research step");
    assert_eq!(research.status, LoopStepStatus::Completed);
    assert_eq!(
        research.artifact_path.as_deref(),
        Some("artifacts/01-research.md")
    );
}

#[test]
fn state_persists_request_before_failed_step_execution() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut step_runner =
        RecordingStepRunner::with_prefix("failing").failing_on(LoopStepName::Research);
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-failed-step",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start run");

    let error = run.run_next_step().expect_err("step should fail");
    assert!(error.to_string().contains("forced failure"));

    let run_dir = runs_root.join("run-failed-step");
    assert_file_contains(
        &run_dir.join("prompts/01-research.prompt.md"),
        "failing request for research",
    );
    assert!(
        !run_dir.join("responses/01-research.raw.txt").exists(),
        "response should not be written when execution fails before producing one"
    );
}

#[test]
fn state_persists_response_before_rejecting_invalid_step_status() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut step_runner =
        RecordingStepRunner::with_prefix("invalid").non_terminal_on(LoopStepName::Research);
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-invalid-step",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start run");

    let error = run
        .run_next_step()
        .expect_err("non-terminal status should fail");
    assert!(error.to_string().contains("terminal"));

    let run_dir = runs_root.join("run-invalid-step");
    assert_file_contains(
        &run_dir.join("prompts/01-research.prompt.md"),
        "invalid request for research",
    );
    assert_file_contains(
        &run_dir.join("responses/01-research.raw.txt"),
        "invalid response for research",
    );
}

#[test]
fn state_artifact_extensions_fall_back_to_bin_when_invalid() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut step_runner = RecordingStepRunner::with_prefix("unsafe-extension")
        .with_artifact(ArtifactContent::new("../bad path", b"unsafe extension"));
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "run-artifact-extension",
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start run");

    run.run_next_step().expect("run first step");

    let run_dir = runs_root.join("run-artifact-extension");
    assert!(run_dir.join("artifacts/01-research.bin").is_file());
    let persisted = read_run(&run_dir);
    let research = persisted
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::Research)
        .expect("research step");
    assert_eq!(
        research.artifact_path.as_deref(),
        Some("artifacts/01-research.bin")
    );
}

#[test]
fn state_resume_missing_run_directory_reports_clear_error() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let mut step_runner = RecordingStepRunner::new();

    let error =
        LoopRunner::resume(&runs_root, "missing-run", &mut step_runner).expect_err("missing run");

    assert!(
        error.to_string().contains("run directory does not exist"),
        "error should identify the missing run directory, got: {error}"
    );
}

#[test]
fn state_resume_verified_uses_preflight_run_when_disk_run_changes() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "resume-verified-state";
    let mut initial_runner = RecordingStepRunner::with_prefix("initial");
    let mut runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            run_id,
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut initial_runner,
    )
    .expect("start run");
    runner.run_next_step().expect("finish research");
    drop(runner);

    let run_dir = runs_root.join(run_id);
    let verified = read_run(&run_dir);
    let mut replacement = verified.clone();
    replacement.ticket_id = "replacement-ticket".to_string();
    replacement.steps[0].status = LoopStepStatus::Pending;
    write_run(&run_dir, &replacement);

    let mut resumed_runner = RecordingStepRunner::with_prefix("resumed");
    let resumed = LoopRunner::resume_verified(&runs_root, verified.clone(), &mut resumed_runner)
        .expect("resume verified run");

    assert_eq!(resumed.run(), &verified);
    assert_eq!(resumed.run().ticket_id, ticket().ticket_id);
    assert_eq!(resumed.run().steps[0].status, LoopStepStatus::Completed);
}

#[cfg(unix)]
#[test]
fn state_resume_rejects_symlinked_run_directory_without_touching_external_target() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let external_runs = temp_dir.path().join("external-runs");
    let run_id = "symlinked-run-directory";
    create_test_run(&external_runs, run_id);
    fs::create_dir(&runs_root).expect("runs root");
    symlink(external_runs.join(run_id), runs_root.join(run_id)).expect("run directory symlink");
    let external_run = external_runs.join(run_id);
    let before = read_tree_bytes(&external_run);
    let mut step_runner = RecordingStepRunner::new();

    let result = LoopRunner::resume(&runs_root, run_id, &mut step_runner);

    let error = result.expect_err("symlinked run directory must fail closed");
    assert!(error.to_string().contains("symlink"), "{error}");
    assert_eq!(read_tree_bytes(&external_run), before);
    assert!(step_runner.calls.is_empty());
}

#[cfg(unix)]
#[test]
fn state_resume_rejects_symlinked_workspace_files_without_touching_external_targets() {
    for relative_path in ["run.json", "log.md", "context-manifest.json"] {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let runs_root = temp_dir.path().join("runs");
        let run_id = "symlinked-workspace-file";
        create_test_run(&runs_root, run_id);
        let run_dir = runs_root.join(run_id);
        let local_path = run_dir.join(relative_path);
        let external_path = temp_dir.path().join(format!("external-{}", relative_path));
        let original = fs::read(&local_path).expect("workspace file");
        fs::write(&external_path, &original).expect("external file");
        fs::remove_file(&local_path).expect("remove workspace file");
        symlink(&external_path, &local_path).expect("workspace file symlink");
        let mut step_runner = RecordingStepRunner::new();

        let result = LoopRunner::resume(&runs_root, run_id, &mut step_runner);

        let error = result.expect_err("symlinked workspace file must fail closed");
        assert!(error.to_string().contains("symlink"), "{error}");
        assert_eq!(fs::read(&external_path).expect("external bytes"), original);
        assert!(step_runner.calls.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn state_resume_rejects_symlinked_layout_directories_without_touching_external_targets() {
    for directory in ["prompts", "responses", "artifacts"] {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let runs_root = temp_dir.path().join("runs");
        let run_id = "symlinked-layout-directory";
        create_test_run(&runs_root, run_id);
        let run_dir = runs_root.join(run_id);
        let external_dir = temp_dir.path().join(format!("external-{directory}"));
        fs::create_dir(&external_dir).expect("external layout dir");
        fs::write(external_dir.join("sentinel"), b"unchanged").expect("external sentinel");
        fs::remove_dir(run_dir.join(directory)).expect("remove empty layout dir");
        symlink(&external_dir, run_dir.join(directory)).expect("layout directory symlink");
        let mut step_runner = RecordingStepRunner::new();

        let result = LoopRunner::resume(&runs_root, run_id, &mut step_runner);

        let error = result.expect_err("symlinked layout directory must fail closed");
        assert!(error.to_string().contains("symlink"), "{error}");
        assert_eq!(
            fs::read(external_dir.join("sentinel")).expect("external sentinel"),
            b"unchanged"
        );
        assert!(step_runner.calls.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn state_resume_verified_rejects_symlinked_prompt_child_before_prepare() {
    assert_resume_verified_child_symlink_is_rejected("prompts/01-research.prompt.md");
}

#[cfg(unix)]
#[test]
fn state_resume_verified_rejects_symlinked_response_child_before_prepare() {
    assert_resume_verified_child_symlink_is_rejected("responses/01-research.raw.txt");
}

#[cfg(unix)]
#[test]
fn state_resume_verified_rejects_symlinked_artifact_child_before_prepare() {
    assert_resume_verified_child_symlink_is_rejected("artifacts/01-research.md");
}

#[test]
fn state_step_rejects_exhausted_prompt_attempts_before_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "exhausted-prompt-attempts";
    let mut step_runner = RecordingStepRunner::with_prefix("must-not-run");
    let mut runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            run_id,
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start run");
    let run_dir = runs_root.join(run_id);
    fs::write(
        run_dir.join("prompts/01-research.attempt-4294967295.prompt.md"),
        b"highest possible prompt attempt",
    )
    .expect("maximum prompt attempt");
    let before = read_tree_bytes(&run_dir);

    let error = runner
        .run_next_step()
        .expect_err("exhausted prompt attempts must fail closed");

    assert!(
        error.to_string().contains("prompt attempt") && error.to_string().contains("exhausted"),
        "overflow error must be actionable, got {error}"
    );
    assert_eq!(read_tree_bytes(&run_dir), before);
    drop(runner);
    assert!(step_runner.request_calls.is_empty());
    assert!(step_runner.calls.is_empty());
}

#[test]
fn state_resume_verified_rejects_exhausted_next_attempt_before_prepare_or_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "resume-exhausted-prompt-attempts";
    create_test_run(&runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    fs::write(
        run_dir.join("prompts/01-research.attempt-4294967295.prompt.md"),
        b"highest possible prompt attempt",
    )
    .expect("maximum prompt attempt");
    let verified = read_run(&run_dir);
    let before = read_tree_bytes(&run_dir);
    let mut step_runner = RecordingStepRunner::with_prefix("must-not-prepare");

    let error = LoopRunner::resume_verified(&runs_root, verified, &mut step_runner)
        .expect_err("resume must preflight exhausted attempts");

    assert!(
        error.to_string().contains("prompt attempt") && error.to_string().contains("exhausted"),
        "overflow error must be actionable, got {error}"
    );
    assert_eq!(read_tree_bytes(&run_dir), before);
    assert_eq!(step_runner.prepare_calls, 0);
    assert!(step_runner.request_calls.is_empty());
    assert!(step_runner.calls.is_empty());
}

struct RecordingStepRunner {
    prepare_calls: usize,
    calls: Vec<LoopStepName>,
    request_calls: Vec<LoopStepName>,
    prefix: &'static str,
    fail_on: Option<LoopStepName>,
    non_terminal_on: Option<LoopStepName>,
    terminal_status: Option<LoopStepStatus>,
    artifact: Option<ArtifactContent>,
}

impl RecordingStepRunner {
    fn new() -> Self {
        Self::with_prefix("recorded")
    }

    fn with_prefix(prefix: &'static str) -> Self {
        Self {
            prepare_calls: 0,
            calls: Vec::new(),
            request_calls: Vec::new(),
            prefix,
            fail_on: None,
            non_terminal_on: None,
            terminal_status: None,
            artifact: None,
        }
    }

    fn failing_on(mut self, step: LoopStepName) -> Self {
        self.fail_on = Some(step);
        self
    }

    fn non_terminal_on(mut self, step: LoopStepName) -> Self {
        self.non_terminal_on = Some(step);
        self
    }

    fn returning_status(mut self, status: LoopStepStatus) -> Self {
        self.terminal_status = Some(status);
        self
    }

    fn with_artifact(mut self, artifact: ArtifactContent) -> Self {
        self.artifact = Some(artifact);
        self
    }
}

impl StepRunner for RecordingStepRunner {
    fn prepare_run(
        &mut self,
        _workspace: &LoopWorkspace,
        _run: &LoopRun,
    ) -> Result<(), seaf_loop::RunnerError> {
        self.prepare_calls += 1;
        Ok(())
    }

    fn step_request(&mut self, step: LoopStepName) -> Result<String, seaf_loop::RunnerError> {
        self.request_calls.push(step);
        Ok(format!("{} request for {}", self.prefix, step_label(step)))
    }

    fn run_step(
        &mut self,
        step: LoopStepName,
        request: &str,
    ) -> Result<StepOutput, seaf_loop::RunnerError> {
        self.calls.push(step);
        if self.fail_on == Some(step) {
            return Err(seaf_loop::RunnerError::Step(format!(
                "forced failure after {request}"
            )));
        }

        let artifact = self.artifact.clone().unwrap_or_else(|| {
            ArtifactContent::markdown(format!("{} artifact for {}", self.prefix, step_label(step)))
        });
        let mut output =
            StepOutput::completed(format!("{} response for {}", self.prefix, step_label(step)))
                .with_artifact(artifact);
        if self.non_terminal_on == Some(step) {
            output.status = LoopStepStatus::Running;
        } else if let Some(status) = self.terminal_status {
            output.status = status;
        }
        Ok(output)
    }
}

fn assert_terminal_step_output_updates_run_and_stops(
    run_id: &str,
    step_status: LoopStepStatus,
    run_status: LoopStatus,
) {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let ticket = ticket();

    let mut step_runner =
        RecordingStepRunner::with_prefix("terminal").returning_status(step_status);
    let mut run = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            run_id,
            &ticket,
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("start run");

    assert!(run.run_next_step().expect("run terminal step"));
    assert!(!run.run_next_step().expect("terminal run should stop"));
    drop(run);

    assert_eq!(step_runner.calls, vec![LoopStepName::Research]);

    let run_dir = runs_root.join(run_id);
    let persisted = read_run(&run_dir);
    assert_eq!(persisted.status, run_status);
    let research = persisted
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::Research)
        .expect("research step");
    assert_eq!(research.status, step_status);
    assert_file_contains(
        &run_dir.join("responses/01-research.raw.txt"),
        "terminal response for research",
    );
}

fn ticket() -> TicketSpec {
    TicketSpec {
        ticket_id: "P2-005".to_string(),
        goal_id: "phase-2".to_string(),
        title: "Add loop workspace and state machine".to_string(),
        status: TicketStatus::Ready,
        priority: seaf_core::TicketPriority::P2,
        problem: "Loop runs must be restartable and auditable.".to_string(),
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
        acceptance_criteria: vec![
            "A run can be resumed after interruption.".to_string(),
            "Every model request and response is stored.".to_string(),
        ],
        eval: None,
    }
}

fn test_input_digests() -> LoopInputDigests {
    LoopInputDigests {
        ticket: "a".repeat(64),
        policy: "b".repeat(64),
        config: "c".repeat(64),
        repository: "d".repeat(64),
    }
}

fn create_test_run(runs_root: &Path, run_id: &str) {
    let mut step_runner = RecordingStepRunner::new();
    let runner = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            runs_root,
            run_id,
            &ticket(),
            "fake-provider",
            "fake-model",
            test_input_digests(),
        ),
        &mut step_runner,
    )
    .expect("create test run");
    drop(runner);
}

fn read_tree_bytes(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
        let mut entries = fs::read_dir(current)
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
                    fs::read(path).expect("tree file"),
                ));
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, &mut files);
    files
}

#[cfg(unix)]
fn assert_resume_verified_child_symlink_is_rejected(relative_path: &str) {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "symlinked-step-child";
    create_test_run(&runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    let external_path = temp_dir.path().join("external-target");
    let original = b"external target must stay unchanged";
    fs::write(&external_path, original).expect("external target");
    symlink(&external_path, run_dir.join(relative_path)).expect("step child symlink");
    let verified = read_run(&run_dir);
    let before = read_tree_bytes(&run_dir);
    let mut step_runner = RecordingStepRunner::with_prefix("symlink-guard");

    let error = LoopRunner::resume_verified(&runs_root, verified, &mut step_runner)
        .expect_err("resume must reject symlinked child before prepare");

    assert!(error.to_string().contains("symlink"), "{error}");
    assert_eq!(read_tree_bytes(&run_dir), before);
    assert_eq!(
        fs::read(&external_path).expect("external target bytes"),
        original,
        "{relative_path} must not be followed"
    );
    assert_eq!(step_runner.prepare_calls, 0);
    assert!(step_runner.request_calls.is_empty());
    assert!(step_runner.calls.is_empty());
}

fn assert_file_contains(path: &Path, expected: &str) {
    let content = std::fs::read_to_string(path).expect("read file");
    assert!(
        content.contains(expected),
        "{path:?} should contain {expected:?}; got {content:?}"
    );
}

fn read_run(run_dir: &Path) -> LoopRun {
    let run_json = std::fs::read_to_string(run_dir.join("run.json")).expect("run json");
    serde_json::from_str(&run_json).expect("run json")
}

fn write_run(run_dir: &Path, run: &LoopRun) {
    let mut json = serde_json::to_vec_pretty(run).expect("run json");
    json.push(b'\n');
    std::fs::write(run_dir.join("run.json"), json).expect("write run json");
}

fn step_label(step: LoopStepName) -> &'static str {
    match step {
        LoopStepName::Research => "research",
        LoopStepName::Analysis => "analysis",
        LoopStepName::SpecCreation => "spec creation",
        LoopStepName::SpecReview => "spec review",
        LoopStepName::Development => "development",
        LoopStepName::OutputReview => "output review",
        LoopStepName::Testing => "testing",
        LoopStepName::EvalReport => "eval report",
    }
}
