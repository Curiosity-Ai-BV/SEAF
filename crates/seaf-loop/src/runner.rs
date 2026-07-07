use std::{error::Error, fmt, path::PathBuf};

use seaf_core::{LoopRun, LoopStatus, LoopStepName, LoopStepStatus, TicketSpec};

use crate::{
    artifacts::{
        next_step_attempt, write_step_artifact, write_step_request, write_step_response,
        ArtifactContent,
    },
    state::{self, NewLoopRun},
    workspace::{LoopWorkspace, WorkspaceError},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopRunnerConfig {
    pub runs_root: PathBuf,
    pub run_id: String,
    pub ticket_id: String,
    pub goal_id: String,
    pub provider: String,
    pub model: String,
}

impl LoopRunnerConfig {
    pub fn for_ticket(
        runs_root: impl Into<PathBuf>,
        run_id: impl Into<String>,
        ticket: &TicketSpec,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            runs_root: runs_root.into(),
            run_id: run_id.into(),
            ticket_id: ticket.ticket_id.clone(),
            goal_id: ticket.goal_id.clone(),
            provider: provider.into(),
            model: model.into(),
        }
    }
}

pub trait StepRunner {
    fn step_request(&mut self, step: LoopStepName) -> Result<String, RunnerError>;

    fn run_step(&mut self, step: LoopStepName, request: &str) -> Result<StepOutput, RunnerError>;

    fn error_response(&self) -> Option<&str> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutput {
    pub response: String,
    pub artifact: Option<ArtifactContent>,
    pub status: LoopStepStatus,
}

impl StepOutput {
    pub fn completed(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
            artifact: None,
            status: LoopStepStatus::Completed,
        }
    }

    pub fn with_artifact(mut self, artifact: ArtifactContent) -> Self {
        self.artifact = Some(artifact);
        self
    }
}

pub struct LoopRunner<'a, R: StepRunner + ?Sized> {
    workspace: LoopWorkspace,
    run: LoopRun,
    step_runner: &'a mut R,
}

impl<'a, R: StepRunner + ?Sized> LoopRunner<'a, R> {
    pub fn start(config: LoopRunnerConfig, step_runner: &'a mut R) -> Result<Self, RunnerError> {
        let workspace = LoopWorkspace::create(&config.runs_root, &config.run_id)?;
        let run = state::create_run(NewLoopRun {
            run_id: config.run_id,
            ticket_id: config.ticket_id,
            goal_id: config.goal_id,
            provider: config.provider,
            model: config.model,
        });
        state::save_run(&workspace, &run)?;
        workspace.append_log("started run")?;

        Ok(Self {
            workspace,
            run,
            step_runner,
        })
    }

    pub fn resume(
        runs_root: impl Into<PathBuf>,
        run_id: &str,
        step_runner: &'a mut R,
    ) -> Result<Self, RunnerError> {
        let runs_root = runs_root.into();
        let workspace = LoopWorkspace::open(&runs_root, run_id)?;
        let run = state::load_run(&workspace)?;
        workspace.append_log("resumed run")?;

        Ok(Self {
            workspace,
            run,
            step_runner,
        })
    }

    pub fn rerun_from(mut self, step: LoopStepName) -> Result<Self, RunnerError> {
        state::reset_from_step(&mut self.run, step)?;
        state::save_run(&self.workspace, &self.run)?;
        self.workspace
            .append_log(&format!("reset run from {step:?}"))?;
        Ok(self)
    }

    pub fn run(&self) -> &LoopRun {
        &self.run
    }

    pub fn run_next_step(&mut self) -> Result<bool, RunnerError> {
        if matches!(
            self.run.status,
            LoopStatus::Blocked | LoopStatus::Failed | LoopStatus::Passed | LoopStatus::Completed
        ) {
            return Ok(false);
        }

        let Some(step) = state::next_runnable_step(&self.run) else {
            return Ok(false);
        };

        state::mark_step_running(&mut self.run, step)?;
        state::save_run(&self.workspace, &self.run)?;
        self.workspace
            .append_log(&format!("started step {step:?}"))?;

        let attempt = next_step_attempt(&self.workspace, step)?;
        let request = self.step_runner.step_request(step)?;
        write_step_request(&self.workspace, step, attempt, &request)?;

        let output = match self.step_runner.run_step(step, &request) {
            Ok(output) => output,
            Err(error) => {
                if let Some(response) = self.step_runner.error_response() {
                    write_step_response(&self.workspace, step, attempt, response)?;
                }
                return Err(error);
            }
        };
        write_step_response(&self.workspace, step, attempt, &output.response)?;
        validate_step_output(&output)?;
        let artifact_path = match &output.artifact {
            Some(artifact) => Some(write_step_artifact(&self.workspace, step, artifact)?),
            None => None,
        };

        state::finish_step(&mut self.run, step, output.status, artifact_path)?;
        state::save_run(&self.workspace, &self.run)?;
        self.workspace
            .append_log(&format!("finished step {step:?} as {:?}", output.status))?;

        Ok(true)
    }

    pub fn run_to_completion(&mut self) -> Result<&LoopRun, RunnerError> {
        while self.run_next_step()? {}
        Ok(&self.run)
    }
}

impl<R: StepRunner + ?Sized> fmt::Debug for LoopRunner<'_, R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoopRunner")
            .field("workspace", &self.workspace)
            .field("run", &self.run)
            .finish_non_exhaustive()
    }
}

fn validate_step_output(output: &StepOutput) -> Result<(), RunnerError> {
    match output.status {
        LoopStepStatus::Completed
        | LoopStepStatus::Passed
        | LoopStepStatus::Blocked
        | LoopStepStatus::Failed => Ok(()),
        LoopStepStatus::Pending | LoopStepStatus::Running => {
            Err(RunnerError::NonTerminalStepStatus(output.status))
        }
    }
}

#[derive(Debug)]
pub enum RunnerError {
    Workspace(WorkspaceError),
    State(state::StateError),
    NonTerminalStepStatus(LoopStepStatus),
    Step(String),
}

impl fmt::Display for RunnerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace(error) => write!(formatter, "{error}"),
            Self::State(error) => write!(formatter, "{error}"),
            Self::NonTerminalStepStatus(status) => {
                write!(formatter, "step result must be terminal, got {status:?}")
            }
            Self::Step(message) => write!(formatter, "step runner error: {message}"),
        }
    }
}

impl Error for RunnerError {}

impl From<WorkspaceError> for RunnerError {
    fn from(error: WorkspaceError) -> Self {
        Self::Workspace(error)
    }
}

impl From<state::StateError> for RunnerError {
    fn from(error: state::StateError) -> Self {
        Self::State(error)
    }
}
