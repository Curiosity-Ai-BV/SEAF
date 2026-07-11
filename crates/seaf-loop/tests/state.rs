use std::path::Path;

use seaf_core::{
    LoopInputDigests, LoopRun, LoopStatus, LoopStepName, LoopStepStatus, TicketContext, TicketSpec,
    TicketStatus,
};
use seaf_loop::{
    state::{create_run, NewLoopRun},
    ArtifactContent, ContextManifest, LoopRunner, LoopRunnerConfig, StepOutput, StepRunner,
    UNTRUSTED_CONTEXT_MARKER,
};

#[test]
fn state_creation_preserves_exact_effective_input_digests() {
    let input_digests = LoopInputDigests {
        ticket: "a".repeat(64),
        policy: "b".repeat(64),
        config: "c".repeat(64),
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

struct RecordingStepRunner {
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
    }
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
