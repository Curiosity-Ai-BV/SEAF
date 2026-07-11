use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use seaf_core::{
    validate_loop_run, LoopInputDigests, LoopRun, LoopStatus, LoopStepName, LoopStepRecord,
    LoopStepStatus,
};
use serde_json::Value;

use crate::workspace::LoopWorkspace;

pub const LOOP_STEPS: [LoopStepName; 8] = [
    LoopStepName::Research,
    LoopStepName::Analysis,
    LoopStepName::SpecCreation,
    LoopStepName::SpecReview,
    LoopStepName::Development,
    LoopStepName::OutputReview,
    LoopStepName::Testing,
    LoopStepName::EvalReport,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLoopRun {
    pub run_id: String,
    pub ticket_id: String,
    pub goal_id: String,
    pub provider: String,
    pub model: String,
    pub input_digests: LoopInputDigests,
}

pub fn create_run(config: NewLoopRun) -> LoopRun {
    let now = now_timestamp();
    LoopRun {
        run_id: config.run_id,
        ticket_id: config.ticket_id,
        goal_id: config.goal_id,
        provider: config.provider,
        model: config.model,
        input_digests: config.input_digests,
        status: LoopStatus::Pending,
        current_step: LOOP_STEPS[0],
        started_at: now.clone(),
        updated_at: now,
        steps: LOOP_STEPS
            .iter()
            .copied()
            .map(|name| LoopStepRecord {
                name,
                status: LoopStepStatus::Pending,
                artifact_path: None,
                artifact_digest: None,
            })
            .collect(),
        policy_decisions: Vec::<std::collections::BTreeMap<String, Value>>::new(),
        provider_exchange_records: Vec::new(),
        eval_report_path: None,
    }
}

pub fn load_run(workspace: &LoopWorkspace) -> Result<LoopRun, StateError> {
    let path = workspace.run_file();
    if !path.is_file() {
        return Err(StateError::MissingRunFile(path));
    }

    let content = fs::read_to_string(&path)?;
    let run = serde_json::from_str(&content)?;
    validate_run_integrity(&run)?;
    crate::provider_exchange::validate_authoritative_provider_exchange_records(workspace, &run)
        .map_err(|error| StateError::InvalidRun(error.to_string()))?;
    Ok(run)
}

pub fn save_run(workspace: &LoopWorkspace, run: &LoopRun) -> Result<(), StateError> {
    write_run_file(&workspace.run_file(), run)
}

pub fn write_run_file(path: &Path, run: &LoopRun) -> Result<(), StateError> {
    validate_run_integrity(run)?;
    let mut json = serde_json::to_vec_pretty(run)?;
    json.push(b'\n');
    fs::write(path, json)?;
    Ok(())
}

pub fn next_runnable_step(run: &LoopRun) -> Option<LoopStepName> {
    run.steps
        .iter()
        .find(|step| {
            matches!(
                step.status,
                LoopStepStatus::Pending | LoopStepStatus::Running
            )
        })
        .map(|step| step.name)
}

pub fn mark_step_running(run: &mut LoopRun, step: LoopStepName) -> Result<(), StateError> {
    let record = step_record_mut(run, step)?;
    record.status = LoopStepStatus::Running;
    run.current_step = step;
    run.status = LoopStatus::Running;
    touch(run);
    Ok(())
}

pub fn finish_step(
    run: &mut LoopRun,
    step: LoopStepName,
    status: LoopStepStatus,
    artifact_path: Option<String>,
    artifact_digest: Option<String>,
) -> Result<(), StateError> {
    if !is_terminal_step_status(status) {
        return Err(StateError::NonTerminalStepStatus(status));
    }
    if artifact_path.is_some() != artifact_digest.is_some() {
        return Err(StateError::InvalidRun(
            "artifact path and digest must either both be present or both be absent".to_string(),
        ));
    }
    if let Some(digest) = &artifact_digest {
        if digest.len() != 64
            || !digest
                .chars()
                .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
        {
            return Err(StateError::InvalidRun(
                "artifact digest must be a lowercase 64-character SHA-256 digest".to_string(),
            ));
        }
    }

    let record = step_record_mut(run, step)?;
    record.status = status;
    record.artifact_path = artifact_path;
    record.artifact_digest = artifact_digest;
    run.current_step = step;

    match status {
        LoopStepStatus::Completed | LoopStepStatus::Passed => {
            if let Some(next_step) = next_runnable_step(run) {
                run.current_step = next_step;
                run.status = LoopStatus::Running;
            } else {
                run.status = LoopStatus::Completed;
            }
        }
        LoopStepStatus::Blocked => run.status = LoopStatus::Blocked,
        LoopStepStatus::Failed => run.status = LoopStatus::Failed,
        LoopStepStatus::Pending | LoopStepStatus::Running => {
            return Err(StateError::NonTerminalStepStatus(status));
        }
    }

    touch(run);
    Ok(())
}

pub fn reset_from_step(run: &mut LoopRun, step: LoopStepName) -> Result<(), StateError> {
    let start = step_index(step)?;
    for record in run.steps.iter_mut().skip(start) {
        record.status = LoopStepStatus::Pending;
        record.artifact_path = None;
        record.artifact_digest = None;
    }
    run.current_step = step;
    run.status = LoopStatus::Pending;
    touch(run);
    Ok(())
}

pub fn step_index(step: LoopStepName) -> Result<usize, StateError> {
    LOOP_STEPS
        .iter()
        .position(|candidate| *candidate == step)
        .ok_or(StateError::UnknownStep(step))
}

pub fn step_file_stem(step: LoopStepName) -> String {
    let index = step_index(step).expect("known loop step") + 1;
    format!("{index:02}-{}", step_slug(step))
}

pub fn step_slug(step: LoopStepName) -> &'static str {
    match step {
        LoopStepName::Research => "research",
        LoopStepName::Analysis => "analysis",
        LoopStepName::SpecCreation => "spec",
        LoopStepName::SpecReview => "spec-review",
        LoopStepName::Development => "development",
        LoopStepName::OutputReview => "output-review",
        LoopStepName::Testing => "testing",
        LoopStepName::EvalReport => "eval-report",
    }
}

fn step_record_mut(
    run: &mut LoopRun,
    step: LoopStepName,
) -> Result<&mut LoopStepRecord, StateError> {
    run.steps
        .iter_mut()
        .find(|record| record.name == step)
        .ok_or(StateError::UnknownStep(step))
}

fn is_terminal_step_status(status: LoopStepStatus) -> bool {
    matches!(
        status,
        LoopStepStatus::Completed
            | LoopStepStatus::Passed
            | LoopStepStatus::Blocked
            | LoopStepStatus::Failed
    )
}

fn touch(run: &mut LoopRun) {
    run.updated_at = now_timestamp();
}

fn now_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

fn validate_run_integrity(run: &LoopRun) -> Result<(), StateError> {
    let errors = validate_loop_run(run);
    if errors.is_empty() {
        return Ok(());
    }
    let details = errors
        .into_iter()
        .map(|error| format!("{}: {}", error.field, error.message))
        .collect::<Vec<_>>()
        .join("; ");
    Err(StateError::InvalidRun(details))
}

#[derive(Debug)]
pub enum StateError {
    MissingRunFile(PathBuf),
    UnknownStep(LoopStepName),
    NonTerminalStepStatus(LoopStepStatus),
    InvalidRun(String),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for StateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRunFile(path) => {
                write!(formatter, "run.json does not exist: {}", path.display())
            }
            Self::UnknownStep(step) => write!(formatter, "unknown loop step: {step:?}"),
            Self::NonTerminalStepStatus(status) => {
                write!(formatter, "step result must be terminal, got {status:?}")
            }
            Self::InvalidRun(message) => write!(formatter, "invalid loop run state: {message}"),
            Self::Io(error) => write!(formatter, "run state I/O error: {error}"),
            Self::Json(error) => write!(formatter, "run state JSON error: {error}"),
        }
    }
}

impl Error for StateError {}

impl From<std::io::Error> for StateError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for StateError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
