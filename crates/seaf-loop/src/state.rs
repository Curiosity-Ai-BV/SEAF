use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use seaf_core::{
    validate_loop_run, LoopExecutionMode, LoopInputDigests, LoopRun, LoopStatus, LoopStepName,
    LoopStepRecord, LoopStepStatus,
};
use serde_json::Value;

use crate::{
    run_persistence::{self, RunMutationGuard},
    workspace::LoopWorkspace,
};

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
        execution_mode: LoopExecutionMode::LegacyProposalOnly,
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
        candidate_workspace: None,
        human_approval: None,
        eval_report_path: None,
        promotion: None,
        latest_recovery: None,
    }
}

pub fn load_run(workspace: &LoopWorkspace) -> Result<LoopRun, StateError> {
    let run = load_run_before_provider_reconciliation(workspace)?;
    crate::provider_exchange::validate_authoritative_provider_exchange_records(workspace, &run)
        .map_err(|error| StateError::InvalidRun(error.to_string()))?;
    Ok(run)
}

pub(crate) fn load_run_before_provider_reconciliation(
    workspace: &LoopWorkspace,
) -> Result<LoopRun, StateError> {
    let path = workspace.run_file();
    if !path.is_file() {
        return Err(StateError::MissingRunFile(path));
    }

    let content = run_persistence::read_regular_file(&path)?;
    let run = serde_json::from_slice(&content)?;
    validate_run_integrity(&run)?;
    Ok(run)
}

pub fn save_run(workspace: &LoopWorkspace, run: &LoopRun) -> Result<(), StateError> {
    write_run_file(&workspace.run_file(), run)
}

pub fn write_run_file(path: &Path, run: &LoopRun) -> Result<(), StateError> {
    validate_run_integrity(run)?;
    let json = run_file_bytes(run)?;
    let run_directory = path
        .parent()
        .ok_or_else(|| StateError::InvalidRun("run file has no parent directory".to_string()))?;
    let lock = RunMutationGuard::acquire(run_directory)?;
    match fs::symlink_metadata(path) {
        Ok(_) => {
            let current_bytes = run_persistence::read_regular_file(path)?;
            let current = serde_json::from_slice::<LoopRun>(&current_bytes).ok();
            let authenticated_current = current
                .as_ref()
                .filter(|current| validate_run_integrity(current).is_ok());
            guard_frozen_authority_direct_write(authenticated_current, run)?;
            if current.as_ref() != Some(run) || current_bytes != json {
                return Err(StateError::InvalidRun(
                    "public state writer only permits an exact idempotent retry for an existing run file"
                        .to_string(),
                ));
            }
            run_persistence::sync_existing(&lock, path)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if run.execution_mode == seaf_core::LoopExecutionMode::IsolatedCandidate {
                return Err(StateError::InvalidRun(
                    "public state writer cannot provision a new isolated candidate run".to_string(),
                ));
            }
            guard_frozen_authority_direct_write(None, run)?;
            run_persistence::publish_create_only(&lock, path, &json)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn publish_prevalidated_isolated_run(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    expected_bytes: &[u8],
) -> Result<(), StateError> {
    validate_run_integrity(run)?;
    if run.execution_mode != seaf_core::LoopExecutionMode::IsolatedCandidate
        || run.candidate_workspace.as_ref().is_none_or(|candidate| {
            candidate.lifecycle != seaf_core::CandidateWorkspaceLifecycle::Provisioning
        })
        || run_file_bytes(run)? != expected_bytes
    {
        return Err(StateError::InvalidRun(
            "isolated provisioning requires exact prevalidated run bytes".to_string(),
        ));
    }
    let path = workspace.run_file();
    let lock = RunMutationGuard::acquire(workspace.run_directory())?;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            guard_frozen_authority_direct_write(None, run)?;
            run_persistence::publish_create_only(&lock, &path, expected_bytes)?;
            Ok(())
        }
        Ok(_) => Err(StateError::InvalidRun(
            "isolated provisioning run already exists".to_string(),
        )),
        Err(error) => Err(error.into()),
    }
}

/// Publishes a legitimate state transition only when `expected` is still authoritative.
pub(crate) fn compare_and_swap_run(
    workspace: &LoopWorkspace,
    expected: &LoopRun,
    intended: &LoopRun,
) -> Result<(), StateError> {
    crate::provider_exchange::persist_ordinary_run_with_full_compare(workspace, expected, intended)
        .map_err(|error| StateError::InvalidRun(error.to_string()))
}

/// Re-authenticates an exact persisted run and closes a prior directory-sync uncertainty.
pub(crate) fn resync_exact_run(
    workspace: &LoopWorkspace,
    expected: &LoopRun,
) -> Result<(), StateError> {
    validate_run_integrity(expected)?;
    let expected_bytes = run_file_bytes(expected)?;
    let lock = RunMutationGuard::acquire(workspace.run_directory())?;
    let current = load_run(workspace)?;
    let current_bytes = run_persistence::read_regular_file(&workspace.run_file())?;
    if current != *expected || current_bytes != expected_bytes {
        return Err(StateError::InvalidRun(
            "exact run resync requires byte-identical canonical authority".to_string(),
        ));
    }
    run_persistence::sync_existing(&lock, &workspace.run_file())?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn write_raw_canonical_run_fixture(
    path: &Path,
    run: &LoopRun,
) -> Result<(), StateError> {
    validate_run_integrity(run)?;
    let bytes = run_file_bytes(run)?;
    crate::artifact_safety::write_private_fixture(path, bytes)?;
    Ok(())
}

pub(crate) fn run_file_bytes(run: &LoopRun) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec_pretty(run)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn guard_frozen_authority_direct_write(
    current: Option<&LoopRun>,
    intended: &LoopRun,
) -> Result<bool, StateError> {
    match current {
        Some(current) if current.latest_recovery != intended.latest_recovery => {
            return Err(StateError::InvalidRun(
                "public state writer cannot mint, replace, or clear recovery authority".to_string(),
            ));
        }
        None if intended.latest_recovery.is_some() => {
            return Err(StateError::InvalidRun(
                "a new run file cannot begin with recovery authority".to_string(),
            ));
        }
        _ => {}
    }
    if is_frozen_review_or_evaluation_authority(intended) {
        if current == Some(intended) {
            return Ok(true);
        }
        return Err(StateError::InvalidRun(
            "public state writer cannot create awaiting human review, approved authority, or final evaluation authority"
                .to_string(),
        ));
    }
    if current.is_some_and(is_frozen_review_or_evaluation_authority) {
        return Err(StateError::InvalidRun(
            "public state writer cannot replace awaiting human review, approved authority, or final evaluation authority"
                .to_string(),
        ));
    }
    Ok(false)
}

pub(crate) fn is_frozen_review_or_evaluation_authority(run: &LoopRun) -> bool {
    matches!(
        run.status,
        LoopStatus::AwaitingHumanReview
            | LoopStatus::Approved
            | LoopStatus::EvalPassed
            | LoopStatus::Promoted
    ) || (run.status == LoopStatus::Failed
        && run.current_step == LoopStepName::EvalReport
        && run.human_approval.is_some()
        && run.eval_report_path.is_some())
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
                run.status = if run.execution_mode == LoopExecutionMode::IsolatedCandidate
                    && step == LoopStepName::OutputReview
                    && status == LoopStepStatus::Passed
                {
                    LoopStatus::AwaitingHumanReview
                } else {
                    LoopStatus::Running
                };
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

impl From<run_persistence::RunPersistenceError> for StateError {
    fn from(error: run_persistence::RunPersistenceError) -> Self {
        Self::InvalidRun(error.to_string())
    }
}

#[cfg(test)]
mod recovery_authority_tests {
    use super::*;
    use seaf_core::{
        ArtifactReference, CandidateWorkspaceLifecycle, CandidateWorkspaceState, LoopExecutionMode,
        RecoveryReference,
    };
    use std::sync::{Arc, Barrier};

    fn run_with_candidate() -> LoopRun {
        let mut run = create_run(NewLoopRun {
            run_id: "direct-writer-recovery".to_string(),
            ticket_id: "T-1".to_string(),
            goal_id: "G-1".to_string(),
            provider: "fake".to_string(),
            model: "fake".to_string(),
            input_digests: LoopInputDigests {
                ticket: "a".repeat(64),
                policy: "b".repeat(64),
                config: "c".repeat(64),
                repository: "d".repeat(64),
                eval_config: None,
            },
        });
        run.execution_mode = LoopExecutionMode::IsolatedCandidate;
        run.candidate_workspace = Some(CandidateWorkspaceState {
            schema_version: 2,
            run_directory_digest: Some("1".repeat(64)),
            path: "/tmp/candidate".to_string(),
            source_worktree_root: "/tmp/source".to_string(),
            git_common_dir: "/tmp/source/.git".to_string(),
            repository_identity_digest: "d".repeat(64),
            starting_head: "3".repeat(40),
            starting_tree: "4".repeat(40),
            candidate_head: "3".repeat(40),
            candidate_tree: "4".repeat(40),
            candidate_diff_digest:
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            patch_transaction: None,
            lifecycle: CandidateWorkspaceLifecycle::Active,
            cleanup_started_at: None,
            cleaned_at: None,
        });
        run
    }

    fn recovery(id: u32, digest: char) -> RecoveryReference {
        RecoveryReference {
            recovery_id: id,
            artifact: ArtifactReference {
                path: format!("artifacts/recovery-{id:03}.json"),
                digest: digest.to_string().repeat(64),
            },
        }
    }

    #[test]
    fn public_direct_writer_cannot_mint_change_or_clear_recovery_authority() {
        let temp = tempfile::tempdir().unwrap();
        crate::artifact_safety::make_private_directory_fixture(temp.path()).unwrap();
        let path = temp.path().join("run.json");
        let run = run_with_candidate();
        write_raw_canonical_run_fixture(&path, &run).unwrap();

        let mut minted = run.clone();
        minted.latest_recovery = Some(recovery(1, '6'));
        let error = write_run_file(&path, &minted).unwrap_err();
        assert!(error.to_string().contains("recovery authority"), "{error}");

        let mut authoritative_bytes = serde_json::to_vec_pretty(&minted).unwrap();
        authoritative_bytes.push(b'\n');
        fs::write(&path, authoritative_bytes).unwrap();
        write_run_file(&path, &minted).unwrap();

        let mut changed = minted.clone();
        changed.latest_recovery = Some(recovery(2, '7'));
        let error = write_run_file(&path, &changed).unwrap_err();
        assert!(error.to_string().contains("recovery authority"), "{error}");

        let mut cleared = minted;
        cleared.latest_recovery = None;
        let error = write_run_file(&path, &cleared).unwrap_err();
        assert!(error.to_string().contains("recovery authority"), "{error}");
    }

    #[test]
    fn new_isolated_run_file_cannot_bypass_provisioning_with_recovery_authority() {
        let temp = tempfile::tempdir().unwrap();
        crate::artifact_safety::make_private_directory_fixture(temp.path()).unwrap();
        let path = temp.path().join("run.json");
        let mut run = run_with_candidate();
        run.latest_recovery = Some(recovery(1, '6'));
        let error = write_run_file(&path, &run).unwrap_err();
        assert!(error.to_string().contains("cannot provision"), "{error}");
        assert!(!path.exists());
    }

    #[test]
    fn sparse_oversized_run_rejects_direct_write_before_allocation_or_mutation() {
        let temp = tempfile::tempdir().unwrap();
        crate::artifact_safety::make_private_directory_fixture(temp.path()).unwrap();
        let path = temp.path().join("run.json");
        let run = run_with_candidate();
        write_raw_canonical_run_fixture(&path, &run).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(2 * 1024 * 1024 + 1)
            .unwrap();

        let error = write_run_file(&path, &run)
            .expect_err("oversized existing run authority must fail from metadata");

        assert!(error.to_string().contains("byte cap"), "{error}");
        assert_eq!(fs::metadata(path).unwrap().len(), 2 * 1024 * 1024 + 1);
    }

    #[test]
    fn public_direct_writer_only_allows_exact_idempotent_existing_state() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        let workspace = LoopWorkspace::create(&runs_root, "direct-writer-cas").unwrap();
        let run = run_with_candidate();
        let mut run = LoopRun {
            run_id: "direct-writer-cas".to_string(),
            ..run
        };
        write_raw_canonical_run_fixture(&workspace.run_file(), &run).unwrap();
        let original = fs::read(workspace.run_file()).unwrap();

        save_run(&workspace, &run).expect("an exact retry is idempotent");
        assert_eq!(fs::read(workspace.run_file()).unwrap(), original);

        fs::write(
            workspace.run_file(),
            serde_json::to_vec(&run).expect("same struct with noncanonical bytes"),
        )
        .unwrap();
        let noncanonical = fs::read(workspace.run_file()).unwrap();
        let error = save_run(&workspace, &run)
            .expect_err("semantic equality cannot replace noncanonical existing bytes");
        assert!(error.to_string().contains("exact idempotent"), "{error}");
        assert_eq!(fs::read(workspace.run_file()).unwrap(), noncanonical);
        fs::write(workspace.run_file(), &original).unwrap();

        run.updated_at = "different-state".to_string();
        let error = save_run(&workspace, &run)
            .expect_err("a public writer must not replace an existing state");
        assert!(error.to_string().contains("exact idempotent"), "{error}");
        assert_eq!(fs::read(workspace.run_file()).unwrap(), original);
    }

    #[test]
    fn competing_ordinary_full_compare_allows_exactly_one_transition() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        let workspace = LoopWorkspace::create(&runs_root, "ordinary-cas-race").unwrap();
        let mut expected = run_with_candidate();
        expected.run_id = "ordinary-cas-race".to_string();
        write_raw_canonical_run_fixture(&workspace.run_file(), &expected).unwrap();
        let mut left_intended = expected.clone();
        left_intended.updated_at = "left-transition".to_string();
        let mut right_intended = expected.clone();
        right_intended.updated_at = "right-transition".to_string();
        let barrier = Arc::new(Barrier::new(3));

        let left_workspace = workspace.clone();
        let left_expected = expected.clone();
        let left_barrier = Arc::clone(&barrier);
        let left = std::thread::spawn(move || {
            left_barrier.wait();
            compare_and_swap_run(&left_workspace, &left_expected, &left_intended)
        });
        let right_workspace = workspace.clone();
        let right_expected = expected.clone();
        let right_barrier = Arc::clone(&barrier);
        let right = std::thread::spawn(move || {
            right_barrier.wait();
            compare_and_swap_run(&right_workspace, &right_expected, &right_intended)
        });
        barrier.wait();
        let results = [left.join().unwrap(), right.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);

        let persisted = load_run(&workspace).unwrap();
        assert!(matches!(
            persisted.updated_at.as_str(),
            "left-transition" | "right-transition"
        ));
        compare_and_swap_run(&workspace, &expected, &persisted)
            .expect("exact intended retry reauthenticates and resyncs");
    }

    #[test]
    fn exact_resync_rejects_semantically_equal_noncanonical_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), "resync-bytes").unwrap();
        let mut run = run_with_candidate();
        run.run_id = "resync-bytes".to_string();
        write_raw_canonical_run_fixture(&workspace.run_file(), &run).unwrap();
        resync_exact_run(&workspace, &run).expect("canonical authority resyncs");
        fs::write(workspace.run_file(), serde_json::to_vec(&run).unwrap()).unwrap();
        let error = resync_exact_run(&workspace, &run).unwrap_err();
        assert!(error.to_string().contains("byte-identical"), "{error}");
    }
}
