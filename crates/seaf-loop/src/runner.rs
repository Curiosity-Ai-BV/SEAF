use std::path::Path;
use std::{error::Error, fmt, fs, path::PathBuf};

use seaf_core::{
    LoopInputDigests, LoopRun, LoopStatus, LoopStepName, LoopStepStatus,
    ProviderExchangeRecordReference, TicketSpec,
};

use crate::{
    artifacts::{
        next_step_attempt, write_step_artifact, write_step_request, write_step_response,
        ArtifactContent,
    },
    policy_gate::PolicyDecision,
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
    pub input_digests: LoopInputDigests,
}

impl LoopRunnerConfig {
    pub fn for_ticket(
        runs_root: impl Into<PathBuf>,
        run_id: impl Into<String>,
        ticket: &TicketSpec,
        provider: impl Into<String>,
        model: impl Into<String>,
        input_digests: LoopInputDigests,
    ) -> Self {
        Self {
            runs_root: runs_root.into(),
            run_id: run_id.into(),
            ticket_id: ticket.ticket_id.clone(),
            goal_id: ticket.goal_id.clone(),
            provider: provider.into(),
            model: model.into(),
            input_digests,
        }
    }
}

pub trait StepRunner {
    fn prepare_workspace(&mut self, _workspace: &LoopWorkspace) -> Result<(), RunnerError> {
        Ok(())
    }

    fn prepare_run(
        &mut self,
        workspace: &LoopWorkspace,
        _run: &LoopRun,
    ) -> Result<(), RunnerError> {
        self.prepare_workspace(workspace)
    }

    fn prepare_fresh_run(
        &mut self,
        workspace: &LoopWorkspace,
        run: &LoopRun,
    ) -> Result<(), RunnerError> {
        self.prepare_run(workspace, run)
    }

    fn prepare_step(
        &mut self,
        _workspace: &LoopWorkspace,
        _run: &LoopRun,
        _step: LoopStepName,
    ) -> Result<(), RunnerError> {
        Ok(())
    }

    fn prepare_step_attempt(
        &mut self,
        workspace: &LoopWorkspace,
        run: &LoopRun,
        step: LoopStepName,
        _attempt: u32,
    ) -> Result<(), RunnerError> {
        self.prepare_step(workspace, run, step)
    }

    fn recovered_step_attempt(&self, _step: LoopStepName) -> Option<u32> {
        None
    }

    fn step_request(&mut self, step: LoopStepName) -> Result<String, RunnerError>;

    fn run_step(&mut self, step: LoopStepName, request: &str) -> Result<StepOutput, RunnerError>;

    fn drain_policy_decisions(&mut self) -> Result<Vec<PolicyDecision>, RunnerError> {
        Ok(Vec::new())
    }

    fn error_response(&self) -> Option<&str> {
        None
    }

    fn take_durable_provider_exchange_records(
        &mut self,
    ) -> Option<Vec<ProviderExchangeRecordReference>> {
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
    next_attempt: Option<(LoopStepName, u32)>,
}

#[derive(Debug)]
pub struct InitializedLoopRun {
    workspace: LoopWorkspace,
    run: LoopRun,
}

#[derive(Debug)]
pub struct ScaffoldedLoopRun {
    workspace: LoopWorkspace,
    run: LoopRun,
}

#[derive(Debug)]
pub struct PreparedLoopRun {
    workspace: LoopWorkspace,
    run: LoopRun,
}

pub fn validate_rerun_eligibility(run: &LoopRun, step: LoopStepName) -> Result<(), RunnerError> {
    if state::is_frozen_review_or_evaluation_authority(run) {
        return Err(RunnerError::Step(
            "awaiting human review, approved authority, or final evaluation authority cannot be rerun without audited invalidation; start a new run"
                .to_string(),
        ));
    }
    if run.execution_mode != seaf_core::LoopExecutionMode::IsolatedCandidate {
        return Ok(());
    }
    let candidate = run
        .candidate_workspace
        .as_ref()
        .ok_or_else(|| RunnerError::Step("isolated rerun lost candidate authority".to_string()))?;
    match candidate
        .patch_transaction
        .as_ref()
        .map(|transaction| transaction.phase)
    {
        Some(seaf_core::CandidatePatchPhase::Applied) if step == LoopStepName::OutputReview => {
            Ok(())
        }
        None if step != LoopStepName::OutputReview => Ok(()),
        _ => Err(RunnerError::Step(
            "an isolated candidate permits only OutputReview rerun from exact Applied evidence; start a new run"
                .to_string(),
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoritativeRunInputSnapshots {
    pub ticket: Vec<u8>,
    pub policy: Vec<u8>,
    pub config: Vec<u8>,
    pub repository: Vec<u8>,
    pub eval_config: Vec<u8>,
    pub provider_ticket: Vec<u8>,
}

impl InitializedLoopRun {
    pub fn create_isolated(
        config: LoopRunnerConfig,
        source_worktree_root: &Path,
    ) -> Result<Self, RunnerError> {
        if config.input_digests.eval_config.is_none() {
            return Err(RunnerError::Step(
                "isolated provider run requires an authoritative eval config digest".to_string(),
            ));
        }
        let workspace = LoopWorkspace::create_minimal(&config.runs_root, &config.run_id)?;
        let result =
            Self::create_isolated_in_workspace(workspace.clone(), config, source_worktree_root);
        if result.is_err() && !workspace.run_file().exists() {
            let empty = fs::read_dir(workspace.run_directory())
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            if empty {
                fs::remove_dir(workspace.run_directory()).map_err(WorkspaceError::Io)?;
                fs::File::open(
                    workspace
                        .run_directory()
                        .parent()
                        .ok_or_else(|| RunnerError::Step("runs root is missing".to_string()))?,
                )
                .and_then(|directory| directory.sync_all())
                .map_err(WorkspaceError::Io)?;
            }
        }
        result
    }

    fn create_isolated_in_workspace(
        workspace: LoopWorkspace,
        config: LoopRunnerConfig,
        source_worktree_root: &Path,
    ) -> Result<Self, RunnerError> {
        let mut run = state::create_run(NewLoopRun {
            run_id: config.run_id,
            ticket_id: config.ticket_id,
            goal_id: config.goal_id,
            provider: config.provider,
            model: config.model,
            input_digests: config.input_digests,
        });
        run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
        run.candidate_workspace = Some(
            crate::candidate_workspace::plan_candidate_workspace(
                workspace.run_directory(),
                source_worktree_root,
                &run.input_digests.repository,
            )
            .map_err(|error| RunnerError::Step(error.to_string()))?,
        );
        let mut bytes = serde_json::to_vec_pretty(&run).map_err(|error| {
            RunnerError::Step(format!("failed to serialize provisioning run: {error}"))
        })?;
        bytes.push(b'\n');
        crate::immutable_artifact::publish_create_only(
            workspace.run_directory(),
            crate::workspace::RUN_FILE,
            &bytes,
        )
        .map_err(|error| {
            RunnerError::Step(format!("failed to publish provisioning run: {error}"))
        })?;
        crate::candidate_workspace::provision_candidate_workspace(&workspace)
            .map_err(|error| RunnerError::Step(error.to_string()))?;
        let run = state::load_run(&workspace)?;
        Ok(Self { workspace, run })
    }

    pub fn run(&self) -> &LoopRun {
        &self.run
    }

    pub fn resume_isolated(runs_root: &Path, verified_run: LoopRun) -> Result<Self, RunnerError> {
        if verified_run.execution_mode != seaf_core::LoopExecutionMode::IsolatedCandidate {
            return Err(RunnerError::Step(
                "incomplete legacy provider run cannot be resumed; start a new isolated run"
                    .to_string(),
            ));
        }
        if verified_run.input_digests.eval_config.is_none() {
            return Err(RunnerError::Step(
                "isolated provider run has no authoritative eval config; start a new run"
                    .to_string(),
            ));
        }
        validate_human_review_execution_barrier(&verified_run)?;
        let workspace = LoopWorkspace::open_minimal(runs_root, &verified_run.run_id)?;
        let persisted = state::load_run_before_provider_reconciliation(&workspace)?;
        if persisted != verified_run {
            return Err(RunnerError::Step(
                "persisted run changed before candidate recovery".to_string(),
            ));
        }
        preflight_persisted_authoritative_snapshot_prefix(&workspace, &persisted)?;
        let candidate = persisted.candidate_workspace.as_ref().ok_or_else(|| {
            RunnerError::Step("isolated run has no candidate authority".to_string())
        })?;
        let recovered = match candidate.lifecycle {
            seaf_core::CandidateWorkspaceLifecycle::Provisioning => {
                crate::candidate_workspace::provision_candidate_workspace(&workspace)
                    .map_err(|error| RunnerError::Step(error.to_string()))?;
                state::load_run(&workspace)?
            }
            seaf_core::CandidateWorkspaceLifecycle::Active => {
                if !candidate
                    .patch_transaction
                    .as_ref()
                    .is_some_and(|transaction| {
                        transaction.phase == seaf_core::CandidatePatchPhase::Applying
                    })
                {
                    crate::candidate_workspace::validate_candidate_workspace(
                        workspace.run_directory(),
                        Path::new(&candidate.source_worktree_root),
                        candidate,
                    )
                    .map_err(|error| RunnerError::Step(error.to_string()))?;
                }
                persisted
            }
            _ => {
                return Err(RunnerError::Step(
                    "candidate is not resumable from its persisted lifecycle".to_string(),
                ));
            }
        };
        let run = normalize_completed_development_candidate(&workspace, recovered)?;
        Ok(Self { workspace, run })
    }

    pub fn resume_isolated_for_rerun(
        runs_root: &Path,
        verified_run: LoopRun,
        step: LoopStepName,
    ) -> Result<Self, RunnerError> {
        validate_rerun_eligibility(&verified_run, step)?;
        Self::resume_isolated(runs_root, verified_run)
    }

    pub fn workspace(&self) -> &LoopWorkspace {
        &self.workspace
    }

    pub fn scaffold(self) -> Result<ScaffoldedLoopRun, RunnerError> {
        let candidate = self.run.candidate_workspace.as_ref().ok_or_else(|| {
            RunnerError::Step("initialized isolated run lost candidate authority".to_string())
        })?;
        crate::candidate_workspace::validate_candidate_workspace(
            self.workspace.run_directory(),
            Path::new(&candidate.source_worktree_root),
            candidate,
        )
        .map_err(|error| RunnerError::Step(error.to_string()))?;
        self.workspace.scaffold_runtime()?;
        Ok(ScaffoldedLoopRun {
            workspace: self.workspace,
            run: self.run,
        })
    }
}

fn normalize_completed_development_candidate(
    workspace: &LoopWorkspace,
    run: LoopRun,
) -> Result<LoopRun, RunnerError> {
    let development_completed = run.steps.iter().any(|record| {
        record.name == LoopStepName::Development && record.status == LoopStepStatus::Completed
    });
    let candidate = run.candidate_workspace.as_ref().ok_or_else(|| {
        RunnerError::Step("isolated run lost candidate authority during recovery".to_string())
    })?;
    let phase = candidate
        .patch_transaction
        .as_ref()
        .map(|transaction| transaction.phase);
    if !development_completed {
        if phase.is_some() {
            return Err(RunnerError::Step(
                "candidate patch transaction exists without completed Development".to_string(),
            ));
        }
        return Ok(run);
    }

    let prospective =
        crate::provider_exchange::preflight_provider_exchange_reconciliation(workspace, &run)
            .map_err(|error| {
                RunnerError::Step(format!(
            "provider exchange recovery preflight failed before candidate normalization: {error}"
        ))
            })?;
    let has_output_review_history = prospective
        .provider_exchange_records
        .iter()
        .any(|record| record.step == LoopStepName::OutputReview);
    let output_review_pending = run.steps.iter().any(|record| {
        record.name == LoopStepName::OutputReview && record.status == LoopStepStatus::Pending
    });

    match phase {
        None if !output_review_pending || has_output_review_history => {
            return Err(RunnerError::Step(
                "pre-B3 completed Development can migrate only while OutputReview is pending with no provider history; start a new run"
                    .to_string(),
            ));
        }
        Some(seaf_core::CandidatePatchPhase::Applying) if has_output_review_history => {
            return Err(RunnerError::Step(
                "Applying candidate cannot have OutputReview provider history; start a new run"
                    .to_string(),
            ));
        }
        None | Some(seaf_core::CandidatePatchPhase::Applying) => {
            if run.status != LoopStatus::Running {
                return Err(RunnerError::Step(
                    "incomplete candidate application can resume only on a running loop"
                        .to_string(),
                ));
            }
            crate::candidate_workspace::apply_candidate_development_evidence(
                workspace,
                Path::new(&candidate.source_worktree_root),
            )
            .map_err(|error| RunnerError::Step(error.to_string()))?;
        }
        Some(seaf_core::CandidatePatchPhase::Applied) => {}
    }

    let normalized = state::load_run(workspace)?;
    let normalized_candidate = normalized
        .candidate_workspace
        .as_ref()
        .ok_or_else(|| RunnerError::Step("normalized run lost candidate authority".to_string()))?;
    crate::candidate_workspace::verify_candidate_patch_evidence(
        workspace,
        Path::new(&normalized_candidate.source_worktree_root),
    )
    .map_err(|error| RunnerError::Step(error.to_string()))?;
    Ok(normalized)
}

impl ScaffoldedLoopRun {
    pub fn run(&self) -> &LoopRun {
        &self.run
    }

    pub fn workspace(&self) -> &LoopWorkspace {
        &self.workspace
    }

    pub fn publish_authoritative_inputs(
        self,
        snapshots: AuthoritativeRunInputSnapshots,
    ) -> Result<PreparedLoopRun, RunnerError> {
        let persisted = state::load_run_before_provider_reconciliation(&self.workspace)?;
        if persisted != self.run {
            return Err(RunnerError::Step(
                "initialized run changed before authoritative input publication".to_string(),
            ));
        }
        let candidate = persisted.candidate_workspace.as_ref().ok_or_else(|| {
            RunnerError::Step("isolated run lost candidate authority".to_string())
        })?;
        crate::candidate_workspace::validate_candidate_workspace(
            self.workspace.run_directory(),
            Path::new(&candidate.source_worktree_root),
            candidate,
        )
        .map_err(|error| RunnerError::Step(error.to_string()))?;
        ensure_authoritative_run_inputs(&self.workspace, &self.run, &snapshots)?;
        Ok(PreparedLoopRun {
            workspace: self.workspace,
            run: self.run,
        })
    }
}

impl PreparedLoopRun {
    pub fn run(&self) -> &LoopRun {
        &self.run
    }

    pub fn workspace(&self) -> &LoopWorkspace {
        &self.workspace
    }
}

fn ensure_authoritative_run_inputs(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    snapshots: &AuthoritativeRunInputSnapshots,
) -> Result<(), RunnerError> {
    validate_authoritative_run_input_payloads(run, snapshots)?;
    preflight_authoritative_snapshot_prefix(workspace, run, snapshots)?;

    let inputs = workspace.run_directory().join("inputs");
    if !inputs.exists() {
        fs::create_dir(&inputs).map_err(|error| RunnerError::Step(error.to_string()))?;
    }
    fs::File::open(workspace.run_directory())
        .and_then(|directory| directory.sync_all())
        .map_err(|error| RunnerError::Step(error.to_string()))?;
    for (name, bytes, _) in authoritative_run_input_entries(run, snapshots)? {
        crate::immutable_artifact::publish_create_only(workspace.run_directory(), name, bytes)
            .map_err(|error| {
                RunnerError::Step(format!("failed to publish authoritative {name}: {error}"))
            })?;
    }
    Ok(())
}

pub fn preflight_authoritative_run_inputs(
    runs_root: &Path,
    verified_run: &LoopRun,
    snapshots: &AuthoritativeRunInputSnapshots,
) -> Result<(), RunnerError> {
    let workspace = LoopWorkspace::open_minimal(runs_root, &verified_run.run_id)?;
    let persisted = state::load_run_before_provider_reconciliation(&workspace)?;
    if persisted != *verified_run {
        return Err(RunnerError::Step(
            "persisted run changed before authoritative input preflight".to_string(),
        ));
    }
    validate_authoritative_run_input_payloads(&persisted, snapshots)?;
    preflight_authoritative_snapshot_prefix(&workspace, &persisted, snapshots)
}

type AuthoritativeRunInputEntry<'a> = (&'static str, &'a Vec<u8>, &'a String);

fn authoritative_run_input_entries<'a>(
    run: &'a LoopRun,
    snapshots: &'a AuthoritativeRunInputSnapshots,
) -> Result<[AuthoritativeRunInputEntry<'a>; 6], RunnerError> {
    let eval_config_digest = run.input_digests.eval_config.as_ref().ok_or_else(|| {
        RunnerError::Step(
            "isolated provider run has no authoritative eval config digest".to_string(),
        )
    })?;
    Ok([
        (
            "inputs/ticket.json",
            &snapshots.ticket,
            &run.input_digests.ticket,
        ),
        (
            "inputs/policy.json",
            &snapshots.policy,
            &run.input_digests.policy,
        ),
        (
            "inputs/config.json",
            &snapshots.config,
            &run.input_digests.config,
        ),
        (
            "inputs/repository.json",
            &snapshots.repository,
            &run.input_digests.repository,
        ),
        (
            "inputs/eval-config.json",
            &snapshots.eval_config,
            eval_config_digest,
        ),
        (
            "ticket.snapshot.json",
            &snapshots.provider_ticket,
            &run.input_digests.ticket,
        ),
    ])
}

fn validate_authoritative_run_input_payloads(
    run: &LoopRun,
    snapshots: &AuthoritativeRunInputSnapshots,
) -> Result<(), RunnerError> {
    let eval_config: seaf_core::EvalConfig = serde_json::from_slice(&snapshots.eval_config)
        .map_err(|error| {
            RunnerError::Step(format!(
                "authoritative inputs/eval-config.json is not a typed eval config: {error}"
            ))
        })?;
    seaf_core::validate_eval_config(&eval_config).map_err(|error| {
        RunnerError::Step(format!(
            "authoritative inputs/eval-config.json is not a valid eval config: {error}"
        ))
    })?;
    let typed_bytes = seaf_core::canonical_json_bytes(&eval_config).map_err(|error| {
        RunnerError::Step(format!(
            "authoritative inputs/eval-config.json cannot be canonicalized: {error}"
        ))
    })?;
    if typed_bytes != snapshots.eval_config {
        return Err(RunnerError::Step(
            "authoritative inputs/eval-config.json bytes are not canonical typed eval config"
                .to_string(),
        ));
    }

    for (name, bytes, expected_digest) in authoritative_run_input_entries(run, snapshots)? {
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
            RunnerError::Step(format!("authoritative {name} is not JSON: {error}"))
        })?;
        let canonical = seaf_core::canonical_json_bytes(&value).map_err(|error| {
            RunnerError::Step(format!(
                "authoritative {name} cannot be canonicalized: {error}"
            ))
        })?;
        if canonical != *bytes {
            return Err(RunnerError::Step(format!(
                "authoritative {name} bytes are not canonical"
            )));
        }
        let digest = seaf_core::canonical_sha256_digest(&value).map_err(|error| {
            RunnerError::Step(format!("authoritative {name} cannot be digested: {error}"))
        })?;
        if &digest != expected_digest {
            return Err(RunnerError::Step(format!(
                "authoritative {name} digest differs from the provisioning run"
            )));
        }
    }
    if snapshots.provider_ticket != snapshots.ticket {
        return Err(RunnerError::Step(
            "provider ticket snapshot differs from the authoritative ticket".to_string(),
        ));
    }
    Ok(())
}

fn preflight_authoritative_snapshot_prefix(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    snapshots: &AuthoritativeRunInputSnapshots,
) -> Result<(), RunnerError> {
    let inputs = workspace.run_directory().join("inputs");
    match fs::symlink_metadata(&inputs) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(RunnerError::Step(
                "authoritative input directory is not a real directory".to_string(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(RunnerError::Step(error.to_string())),
    }
    let mut missing_suffix_started = false;
    for (name, bytes, _) in authoritative_run_input_entries(run, snapshots)? {
        let path = workspace.run_directory().join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(RunnerError::Step(format!(
                    "authoritative snapshot collision at {}",
                    path.display()
                )));
            }
            Ok(_) => {
                if missing_suffix_started {
                    return Err(RunnerError::Step(format!(
                        "authoritative snapshots are not an exact prefix: {} exists after a missing entry",
                        path.display()
                    )));
                }
                if fs::read(&path).map_err(|error| RunnerError::Step(error.to_string()))? != *bytes
                {
                    return Err(RunnerError::Step(format!(
                        "authoritative snapshot collision at {}",
                        path.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing_suffix_started = true;
            }
            Err(error) => return Err(RunnerError::Step(error.to_string())),
        }
    }
    Ok(())
}

fn preflight_persisted_authoritative_snapshot_prefix(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<(), RunnerError> {
    let eval_config_digest = run.input_digests.eval_config.as_ref().ok_or_else(|| {
        RunnerError::Step(
            "isolated provider run has no authoritative eval config digest".to_string(),
        )
    })?;
    let entries = [
        ("inputs/ticket.json", &run.input_digests.ticket),
        ("inputs/policy.json", &run.input_digests.policy),
        ("inputs/config.json", &run.input_digests.config),
        ("inputs/repository.json", &run.input_digests.repository),
        ("inputs/eval-config.json", eval_config_digest),
        ("ticket.snapshot.json", &run.input_digests.ticket),
    ];
    let inputs = workspace.run_directory().join("inputs");
    match fs::symlink_metadata(&inputs) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(RunnerError::Step(
                "authoritative input directory is not a real directory".to_string(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(RunnerError::Step(error.to_string())),
    }

    let mut missing_suffix_started = false;
    for (relative, expected_digest) in entries {
        let path = workspace.run_directory().join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(RunnerError::Step(format!(
                    "authoritative snapshot collision at {}",
                    path.display()
                )));
            }
            Ok(_) => {
                if missing_suffix_started {
                    return Err(RunnerError::Step(format!(
                        "authoritative snapshots are not an exact prefix: {} exists after a missing entry",
                        path.display()
                    )));
                }
                let bytes =
                    fs::read(&path).map_err(|error| RunnerError::Step(error.to_string()))?;
                let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
                    RunnerError::Step(format!("authoritative {relative} is not JSON: {error}"))
                })?;
                if seaf_core::canonical_json_bytes(&value)
                    .map_err(|error| RunnerError::Step(error.to_string()))?
                    != bytes
                    || seaf_core::canonical_sha256_digest(&value)
                        .map_err(|error| RunnerError::Step(error.to_string()))?
                        != *expected_digest
                {
                    return Err(RunnerError::Step(format!(
                        "authoritative snapshot collision at {}",
                        path.display()
                    )));
                }
                if relative == "inputs/eval-config.json" {
                    let eval_config: seaf_core::EvalConfig = serde_json::from_slice(&bytes)
                        .map_err(|error| {
                            RunnerError::Step(format!(
                                "authoritative eval config is not typed: {error}"
                            ))
                        })?;
                    seaf_core::validate_eval_config(&eval_config).map_err(|error| {
                        RunnerError::Step(format!("authoritative eval config is invalid: {error}"))
                    })?;
                    if seaf_core::canonical_json_bytes(&eval_config)
                        .map_err(|error| RunnerError::Step(error.to_string()))?
                        != bytes
                    {
                        return Err(RunnerError::Step(
                            "authoritative eval config is not canonical typed input".to_string(),
                        ));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing_suffix_started = true;
            }
            Err(error) => return Err(RunnerError::Step(error.to_string())),
        }
    }
    Ok(())
}

impl<'a, R: StepRunner + ?Sized> LoopRunner<'a, R> {
    pub fn start_initialized(
        initialized: PreparedLoopRun,
        step_runner: &'a mut R,
    ) -> Result<Self, RunnerError> {
        let persisted = state::load_run_before_provider_reconciliation(&initialized.workspace)?;
        if persisted != initialized.run {
            return Err(RunnerError::Step(
                "initialized run changed before provider preparation".to_string(),
            ));
        }
        let candidate = persisted.candidate_workspace.as_ref().ok_or_else(|| {
            RunnerError::Step("isolated run lost candidate authority".to_string())
        })?;
        crate::candidate_workspace::validate_candidate_workspace(
            initialized.workspace.run_directory(),
            Path::new(&candidate.source_worktree_root),
            candidate,
        )
        .map_err(|error| RunnerError::Step(error.to_string()))?;
        step_runner.prepare_fresh_run(&initialized.workspace, &initialized.run)?;
        initialized
            .workspace
            .append_log("started isolated provider run")?;
        Ok(Self {
            workspace: initialized.workspace,
            run: initialized.run,
            step_runner,
            next_attempt: None,
        })
    }

    pub fn resume_initialized(
        initialized: PreparedLoopRun,
        step_runner: &'a mut R,
    ) -> Result<Self, RunnerError> {
        let persisted = state::load_run_before_provider_reconciliation(&initialized.workspace)?;
        if persisted != initialized.run {
            return Err(RunnerError::Step(
                "initialized resume run changed before provider preparation".to_string(),
            ));
        }
        let candidate = persisted.candidate_workspace.as_ref().ok_or_else(|| {
            RunnerError::Step("isolated run lost candidate authority".to_string())
        })?;
        crate::candidate_workspace::validate_candidate_workspace(
            initialized.workspace.run_directory(),
            Path::new(&candidate.source_worktree_root),
            candidate,
        )
        .map_err(|error| RunnerError::Step(error.to_string()))?;
        Self::resume_with_workspace(initialized.workspace, initialized.run, step_runner)
    }

    pub fn start(config: LoopRunnerConfig, step_runner: &'a mut R) -> Result<Self, RunnerError> {
        let workspace = LoopWorkspace::create(&config.runs_root, &config.run_id)?;
        let run = state::create_run(NewLoopRun {
            run_id: config.run_id,
            ticket_id: config.ticket_id,
            goal_id: config.goal_id,
            provider: config.provider,
            model: config.model,
            input_digests: config.input_digests,
        });
        if let Err(error) = step_runner.prepare_fresh_run(&workspace, &run) {
            return Err(cleanup_failed_start_workspace(&workspace, error));
        }
        state::save_run(&workspace, &run)?;
        workspace.append_log("started run")?;

        Ok(Self {
            workspace,
            run,
            step_runner,
            next_attempt: None,
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
        Self::resume_with_workspace(workspace, run, step_runner)
    }

    pub fn resume_verified(
        runs_root: impl Into<PathBuf>,
        run: LoopRun,
        step_runner: &'a mut R,
    ) -> Result<Self, RunnerError> {
        let runs_root = runs_root.into();
        let workspace = LoopWorkspace::open(&runs_root, &run.run_id)?;
        Self::resume_with_workspace(workspace, run, step_runner)
    }

    fn resume_with_workspace(
        workspace: LoopWorkspace,
        run: LoopRun,
        step_runner: &'a mut R,
    ) -> Result<Self, RunnerError> {
        if state::is_frozen_review_or_evaluation_authority(&run) {
            return Ok(Self {
                workspace,
                run,
                step_runner,
                next_attempt: None,
            });
        }
        validate_human_review_execution_barrier(&run)?;
        let filesystem_next_attempt = state::next_runnable_step(&run)
            .map(|step| next_step_attempt(&workspace, step).map(|attempt| (step, attempt)))
            .transpose()?;
        step_runner.prepare_run(&workspace, &run)?;
        let mut runner = Self {
            workspace,
            run,
            step_runner,
            next_attempt: None,
        };
        runner.import_durable_provider_exchange_records()?;
        runner.next_attempt = state::next_runnable_step(&runner.run)
            .map(|step| {
                runner
                    .step_runner
                    .recovered_step_attempt(step)
                    .or_else(|| {
                        filesystem_next_attempt
                            .filter(|(candidate, _)| *candidate == step)
                            .map(|(_, attempt)| attempt)
                    })
                    .map_or_else(|| next_step_attempt(&runner.workspace, step), Ok)
                    .map(|attempt| (step, attempt))
            })
            .transpose()?;
        runner.workspace.append_log("resumed run")?;
        Ok(runner)
    }

    pub fn rerun_from(self, _step: LoopStepName) -> Result<Self, RunnerError> {
        Err(RunnerError::Step(
            "legacy rerun is retired; use audited revise and rerun recovery commands".to_string(),
        ))
    }

    pub fn run(&self) -> &LoopRun {
        &self.run
    }

    pub fn run_next_step(&mut self) -> Result<bool, RunnerError> {
        if matches!(
            self.run.status,
            LoopStatus::AwaitingHumanReview
                | LoopStatus::Approved
                | LoopStatus::EvalPassed
                | LoopStatus::Promoted
                | LoopStatus::Blocked
                | LoopStatus::Failed
                | LoopStatus::Passed
                | LoopStatus::Completed
        ) {
            return Ok(false);
        }

        validate_human_review_execution_barrier(&self.run)?;

        let Some(step) = state::next_runnable_step(&self.run) else {
            return Ok(false);
        };
        let attempt = match self.next_attempt.take() {
            Some((cached_step, attempt)) if cached_step == step => attempt,
            Some(_) | None => next_step_attempt(&self.workspace, step)?,
        };

        self.step_runner
            .prepare_step_attempt(&self.workspace, &self.run, step, attempt)?;

        state::mark_step_running(&mut self.run, step)?;
        self.persist_run_state()?;
        self.workspace
            .append_log(&format!("started step {step:?}"))?;

        let request = self.step_runner.step_request(step)?;
        write_step_request(&self.workspace, step, attempt, &request)?;

        let output = match self.step_runner.run_step(step, &request) {
            Ok(output) => output,
            Err(error) => {
                self.import_durable_provider_exchange_records()?;
                if let Some(response) = self.step_runner.error_response() {
                    write_step_response(&self.workspace, step, attempt, response)?;
                }
                return Err(error);
            }
        };
        self.import_durable_provider_exchange_records()?;
        write_step_response(&self.workspace, step, attempt, &output.response)?;
        validate_step_output(&output)?;
        append_policy_decisions(&mut self.run, self.step_runner.drain_policy_decisions()?)?;
        let (artifact_path, artifact_digest) = match &output.artifact {
            Some(artifact) => (
                Some(write_step_artifact(
                    &self.workspace,
                    step,
                    attempt,
                    artifact,
                )?),
                Some(artifact.digest()),
            ),
            None => (None, None),
        };

        state::finish_step(
            &mut self.run,
            step,
            output.status,
            artifact_path,
            artifact_digest,
        )?;
        self.persist_run_state()?;
        if self.run.execution_mode == seaf_core::LoopExecutionMode::IsolatedCandidate
            && step == LoopStepName::Development
            && output.status == LoopStepStatus::Completed
        {
            let source = self
                .run
                .candidate_workspace
                .as_ref()
                .ok_or_else(|| {
                    RunnerError::Step(
                        "completed isolated Development lost candidate authority".to_string(),
                    )
                })?
                .source_worktree_root
                .clone();
            crate::candidate_workspace::apply_candidate_development_evidence(
                &self.workspace,
                Path::new(&source),
            )
            .map_err(|error| RunnerError::Step(error.to_string()))?;
            let applied = state::load_run(&self.workspace)?;
            let phase = applied
                .candidate_workspace
                .as_ref()
                .and_then(|candidate| candidate.patch_transaction.as_ref())
                .map(|transaction| transaction.phase);
            if phase != Some(seaf_core::CandidatePatchPhase::Applied) {
                return Err(RunnerError::Step(
                    "completed isolated Development did not publish exact Applied candidate evidence"
                        .to_string(),
                ));
            }
            self.run = applied;
        }
        self.workspace
            .append_log(&format!("finished step {step:?} as {:?}", output.status))?;

        Ok(true)
    }

    pub fn run_to_completion(&mut self) -> Result<&LoopRun, RunnerError> {
        while self.run_next_step()? {}
        Ok(&self.run)
    }

    fn import_durable_provider_exchange_records(&mut self) -> Result<(), RunnerError> {
        let Some(records) = self.step_runner.take_durable_provider_exchange_records() else {
            return Ok(());
        };
        if !records.starts_with(&self.run.provider_exchange_records) {
            return Err(RunnerError::Step(
                "step runner attempted to replace the authoritative provider exchange prefix"
                    .to_string(),
            ));
        }
        let mut prospective = self.run.clone();
        prospective.provider_exchange_records = records;
        crate::provider_exchange::validate_authoritative_provider_exchange_records(
            &self.workspace,
            &prospective,
        )
        .map_err(|error| {
            RunnerError::Step(format!(
                "step runner supplied invalid durable provider exchange records: {error}"
            ))
        })?;
        self.run.provider_exchange_records = prospective.provider_exchange_records;
        Ok(())
    }

    fn persist_run_state(&self) -> Result<(), RunnerError> {
        crate::provider_exchange::persist_run_with_provider_exchange_compare(
            &self.workspace,
            &self.run,
        )
        .map_err(|error| RunnerError::Step(format!("failed to publish loop state: {error}")))?;
        Ok(())
    }
}

pub fn validate_human_review_execution_barrier(run: &LoopRun) -> Result<(), RunnerError> {
    if run.execution_mode == seaf_core::LoopExecutionMode::IsolatedCandidate
        && matches!(run.status, LoopStatus::Pending | LoopStatus::Running)
        && matches!(
            state::next_runnable_step(run),
            Some(LoopStepName::Testing | LoopStepName::EvalReport)
        )
    {
        return Err(RunnerError::Step(
            "isolated Testing and EvalReport require audited human approval; start a new run because this historical execution prefix has no approval barrier"
                .to_string(),
        ));
    }
    Ok(())
}

fn append_policy_decisions(
    run: &mut LoopRun,
    decisions: Vec<PolicyDecision>,
) -> Result<(), RunnerError> {
    for decision in decisions {
        let patch_id = decision.patch_id.clone();
        let value = serde_json::to_value(decision).map_err(|error| {
            RunnerError::Step(format!("failed to serialize policy decision: {error}"))
        })?;
        let entry = serde_json::from_value(value).map_err(|error| {
            RunnerError::Step(format!("failed to encode policy decision entry: {error}"))
        })?;
        run.policy_decisions
            .retain(|existing| policy_decision_patch_id(existing) != Some(patch_id.as_str()));
        run.policy_decisions.push(entry);
    }
    Ok(())
}

fn policy_decision_patch_id(
    decision: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Option<&str> {
    decision.get("patch_id").and_then(serde_json::Value::as_str)
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

fn cleanup_failed_start_workspace(workspace: &LoopWorkspace, error: RunnerError) -> RunnerError {
    if workspace.run_file().exists() {
        return error;
    }

    match fs::remove_dir_all(workspace.run_directory()) {
        Ok(()) => error,
        Err(cleanup_error) => RunnerError::Step(format!(
            "{error}; failed to clean partial run workspace {}: {cleanup_error}",
            workspace.run_directory().display()
        )),
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
