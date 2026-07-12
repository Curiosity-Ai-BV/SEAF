use std::{
    error::Error,
    fmt, fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, ArtifactReference, CandidatePatchPhase,
    CandidateWorkspaceLifecycle, CandidateWorkspaceState, LoopExecutionMode, LoopInputDigests,
    LoopRun, LoopStatus, LoopStepName, ProviderExchangeKind, ProviderExchangePhase,
    ProviderExchangeRecordReference, RecoveryReference,
};
use serde::{Deserialize, Serialize};

use crate::{
    artifacts::latest_step_attempt,
    candidate_workspace::{
        acquire_candidate_lock, capture_source_worktree_authority, validate_candidate_workspace,
        validate_source_worktree_authority, verify_candidate_patch_evidence_locked,
        CANDIDATE_WORKSPACE_SCHEMA_VERSION,
    },
    immutable_artifact::{publish_create_only, read_verified_regular_file},
    inspect::{inspect_loop_run, InspectionIntegrity},
    provider_exchange::{
        load_provider_exchange_record, persist_recovery_reset_with_full_compare_and_validator,
        preflight_provider_exchange_reconciliation,
        validate_authoritative_provider_exchange_records,
    },
    state::{self, step_index},
    LoopWorkspace,
};

pub const RECOVERY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    ReviseProviderStep,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverySourceRunV1 {
    pub schema_version: u32,
    pub recovery_id: u32,
    pub run: LoopRun,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryAttemptV1 {
    pub schema_version: u32,
    pub recovery_id: u32,
    pub run_id: String,
    pub action: RecoveryAction,
    pub step: LoopStepName,
    pub actor: String,
    pub reason: String,
    pub created_at: String,
    pub source_run: ArtifactReference,
    pub source_run_digest: String,
    pub input_digests: LoopInputDigests,
    pub candidate_state_digest: String,
    pub candidate_head: String,
    pub candidate_tree: String,
    pub candidate_diff_digest: String,
    pub source_worktree_state_digest: String,
    pub source_step_attempt: u32,
    pub next_step_attempt: u32,
    pub previous_recovery: Option<RecoveryReference>,
    pub previous_provider_head: Option<ProviderExchangeRecordReference>,
    pub expected_reset_projection_digest: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryRevisionOutcome {
    pub run: LoopRun,
    pub recovery: RecoveryAttemptV1,
    pub reference: RecoveryReference,
}

pub fn revise_provider_step(
    workspace: &LoopWorkspace,
    step: LoopStepName,
    actor: &str,
    reason: &str,
) -> Result<RecoveryRevisionOutcome, RecoveryError> {
    validate_note("actor", actor, 256)?;
    validate_note("reason", reason, 1024)?;
    let candidate_lock = acquire_candidate_lock(workspace).map_err(RecoveryError::wrapped)?;
    let result = revise_provider_step_locked(workspace, step, actor, reason);
    let unlock = candidate_lock.unlock();
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(RecoveryError::wrapped(error)),
        (Err(error), _) => Err(error),
    }
}

fn revise_provider_step_locked(
    workspace: &LoopWorkspace,
    step: LoopStepName,
    actor: &str,
    reason: &str,
) -> Result<RecoveryRevisionOutcome, RecoveryError> {
    let source = state::load_run(workspace).map_err(RecoveryError::wrapped)?;
    if source.status == LoopStatus::Pending && source.current_step == step {
        if let Some(reference) = source.latest_recovery.clone() {
            let recovery = load_verified_recovery(workspace, &source, &reference)?;
            if !recovery_is_consumed(workspace, &source, &reference)? {
                if recovery.step == step && recovery.actor == actor && recovery.reason == reason {
                    validate_recovery_namespace(
                        workspace,
                        reference.recovery_id,
                        reference.recovery_id,
                    )?;
                    validate_pending_adoption(workspace, &source, step, &recovery)?;
                    return Ok(RecoveryRevisionOutcome {
                        run: source,
                        recovery,
                        reference,
                    });
                }
                return Err(RecoveryError::invalid(
                    "pending recovery retry does not match its exact step, actor, and reason",
                ));
            }
        }
    }
    validate_creation_eligibility(workspace, &source, step)?;
    let candidate = source
        .candidate_workspace
        .as_ref()
        .expect("eligibility checked");
    let source_root = Path::new(&candidate.source_worktree_root);
    let source_authority =
        capture_source_worktree_authority(source_root, Some(workspace.run_directory()))
            .map_err(RecoveryError::wrapped)?;
    validate_physical_candidate_locked(workspace, &source, step)?;

    let recovery_id = source.latest_recovery.as_ref().map_or(Ok(1), |reference| {
        if !recovery_is_consumed(workspace, &source, reference)? {
            return Err(RecoveryError::invalid(
                "a prior recovery is still pending its exact first request",
            ));
        }
        reference
            .recovery_id
            .checked_add(1)
            .ok_or_else(|| RecoveryError::invalid("recovery ID sequence is exhausted"))
    })?;
    let source_step_attempt = authenticated_source_attempt(workspace, &source, step)?;
    let next_step_attempt = source_step_attempt
        .checked_add(1)
        .ok_or_else(|| RecoveryError::invalid("provider step attempt sequence is exhausted"))?;
    if crate::artifacts::next_step_attempt(workspace, step).map_err(RecoveryError::wrapped)?
        != next_step_attempt
    {
        return Err(RecoveryError::invalid(
            "prompt and authenticated provider attempt authority disagree",
        ));
    }
    validate_recovery_namespace(
        workspace,
        source
            .latest_recovery
            .as_ref()
            .map_or(0, |value| value.recovery_id),
        recovery_id,
    )?;

    let source_snapshot = RecoverySourceRunV1 {
        schema_version: RECOVERY_SCHEMA_VERSION,
        recovery_id,
        run: source.clone(),
    };
    let source_bytes = canonical_json_bytes(&source_snapshot).map_err(RecoveryError::wrapped)?;
    let source_path = recovery_source_path(recovery_id);
    publish_create_only(workspace.run_directory(), &source_path, &source_bytes)
        .map_err(RecoveryError::wrapped)?;
    let source_reference = ArtifactReference {
        path: source_path,
        digest: digest_bytes(&source_bytes),
    };

    let recovery_path = recovery_path(recovery_id);
    let created_at = existing_or_new_timestamp(workspace, &recovery_path)?;
    let mut projection = reset_run(&source, step, recovery_id, &recovery_path, &created_at)?;
    let projection_digest = canonical_sha256_digest(&projection).map_err(RecoveryError::wrapped)?;
    let recovery = RecoveryAttemptV1 {
        schema_version: RECOVERY_SCHEMA_VERSION,
        recovery_id,
        run_id: source.run_id.clone(),
        action: RecoveryAction::ReviseProviderStep,
        step,
        actor: actor.to_string(),
        reason: reason.to_string(),
        created_at: created_at.clone(),
        source_run: source_reference,
        source_run_digest: canonical_sha256_digest(&source).map_err(RecoveryError::wrapped)?,
        input_digests: source.input_digests.clone(),
        candidate_state_digest: canonical_sha256_digest(candidate)
            .map_err(RecoveryError::wrapped)?,
        candidate_head: candidate.candidate_head.clone(),
        candidate_tree: candidate.candidate_tree.clone(),
        candidate_diff_digest: candidate.candidate_diff_digest.clone(),
        source_worktree_state_digest: canonical_sha256_digest(&source_authority)
            .map_err(RecoveryError::wrapped)?,
        source_step_attempt,
        next_step_attempt,
        previous_recovery: source.latest_recovery.clone(),
        previous_provider_head: source.provider_exchange_records.last().cloned(),
        expected_reset_projection_digest: projection_digest,
    };
    validate_recovery_contract(&recovery)?;
    let recovery_bytes = canonical_json_bytes(&recovery).map_err(RecoveryError::wrapped)?;
    let recovery_digest = digest_bytes(&recovery_bytes);
    let reference = RecoveryReference {
        recovery_id,
        artifact: ArtifactReference {
            path: recovery_path.clone(),
            digest: recovery_digest.clone(),
        },
    };
    projection.latest_recovery = Some(reference.clone());
    publish_create_only(workspace.run_directory(), &recovery_path, &recovery_bytes)
        .map_err(RecoveryError::wrapped)?;

    let intended = projection;
    validate_reset_relation(&source, &intended, &recovery)?;
    persist_recovery_reset_with_full_compare_and_validator(
        workspace,
        &source,
        &intended,
        |current| {
            let result = (|| {
                if current != &source {
                    return Err(RecoveryError::invalid(
                        "source run changed before recovery CAS",
                    ));
                }
                validate_source_worktree_authority(
                    source_root,
                    Some(workspace.run_directory()),
                    &source_authority,
                )
                .map_err(RecoveryError::wrapped)?;
                validate_creation_eligibility(workspace, current, step)?;
                validate_authoritative_provider_exchange_records(workspace, current)
                    .map_err(RecoveryError::wrapped)?;
                if authenticated_source_attempt(workspace, current, step)? != source_step_attempt {
                    return Err(RecoveryError::invalid(
                        "provider attempt authority changed before recovery CAS",
                    ));
                }
                validate_physical_candidate_locked(workspace, current, step)?;
                load_verified_recovery(workspace, &intended, &reference)?;
                validate_reset_relation(current, &intended, &recovery)
            })();
            result.map_err(|error| {
                crate::provider_exchange::ProviderExchangeError::Invalid(error.to_string())
            })
        },
    )
    .map_err(RecoveryError::wrapped)?;

    Ok(RecoveryRevisionOutcome {
        run: intended,
        recovery,
        reference,
    })
}

fn validate_pending_adoption(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    step: LoopStepName,
    recovery: &RecoveryAttemptV1,
) -> Result<(), RecoveryError> {
    let candidate = run
        .candidate_workspace
        .as_ref()
        .ok_or_else(|| RecoveryError::invalid("pending recovery lost candidate authority"))?;
    if run.execution_mode != LoopExecutionMode::IsolatedCandidate
        || candidate.schema_version != CANDIDATE_WORKSPACE_SCHEMA_VERSION
        || candidate.lifecycle != CandidateWorkspaceLifecycle::Active
    {
        return Err(RecoveryError::invalid(
            "pending recovery no longer has active isolated candidate authority",
        ));
    }
    let inspection = inspect_loop_run(
        workspace
            .run_directory()
            .parent()
            .ok_or_else(|| RecoveryError::invalid("run has no runs root"))?,
        &run.run_id,
    )
    .map_err(RecoveryError::wrapped)?;
    if inspection.integrity != InspectionIntegrity::Verified
        || !inspection.evaluation_prefix.is_empty()
    {
        return Err(RecoveryError::invalid(
            "pending recovery adoption requires verified provider-only authority",
        ));
    }
    validate_authoritative_provider_exchange_records(workspace, run)
        .map_err(RecoveryError::wrapped)?;
    let source_authority = capture_source_worktree_authority(
        Path::new(&candidate.source_worktree_root),
        Some(workspace.run_directory()),
    )
    .map_err(RecoveryError::wrapped)?;
    if canonical_sha256_digest(&source_authority).map_err(RecoveryError::wrapped)?
        != recovery.source_worktree_state_digest
    {
        return Err(RecoveryError::invalid(
            "source worktree authority changed after recovery reset",
        ));
    }
    validate_physical_candidate_locked(workspace, run, step)
}

pub fn load_verified_latest_recovery(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<Option<RecoveryAttemptV1>, RecoveryError> {
    run.latest_recovery
        .as_ref()
        .map(|reference| load_verified_recovery(workspace, run, reference))
        .transpose()
}

pub fn ensure_no_pending_recovery(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<(), RecoveryError> {
    let Some(reference) = &run.latest_recovery else {
        return Ok(());
    };
    if recovery_is_consumed(workspace, run, reference)? {
        Ok(())
    } else {
        Err(RecoveryError::invalid(
            "pending recovery requires `seaf loop rerun --recovery <id>` before ordinary resume",
        ))
    }
}

pub(crate) fn verify_recovery_authorization(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    step: LoopStepName,
    attempt: u32,
) -> Result<(), RecoveryError> {
    let mut reference = run.latest_recovery.clone().ok_or_else(|| {
        RecoveryError::invalid("provider attempt has no active recovery authorization")
    })?;
    loop {
        let (recovery, source, _) = load_verified_recovery_lineage(workspace, &reference)?;
        if recovery.run_id == run.run_id
            && run
                .provider_exchange_records
                .starts_with(&source.provider_exchange_records)
            && recovery.step == step
            && recovery.next_step_attempt == attempt
        {
            return Ok(());
        }
        reference = recovery.previous_recovery.ok_or_else(|| {
            RecoveryError::invalid(
                "provider attempt does not match any recovery in the authenticated chain",
            )
        })?;
    }
}

pub(crate) fn verify_latest_recovery_authorization(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    step: LoopStepName,
    attempt: u32,
) -> Result<(), RecoveryError> {
    let reference = run.latest_recovery.as_ref().ok_or_else(|| {
        RecoveryError::invalid("provider attempt has no latest recovery authorization")
    })?;
    let (recovery, source, _) = load_verified_recovery_lineage(workspace, reference)?;
    let identity_matches = recovery.run_id == run.run_id;
    let head_matches = run.latest_recovery.as_ref() == Some(reference);
    let prefix_matches = run
        .provider_exchange_records
        .starts_with(&source.provider_exchange_records);
    let predecessor_matches =
        recovery.previous_provider_head == run.provider_exchange_records.last().cloned();
    let consumed_matches = run
        .provider_exchange_records
        .get(source.provider_exchange_records.len())
        .is_some_and(|candidate| {
            candidate.step == recovery.step
                && candidate.step_attempt == recovery.next_step_attempt
                && candidate.exchange_index == 1
                && candidate.kind == ProviderExchangeKind::Initial
                && candidate.phase == ProviderExchangePhase::Request
        });
    let coordinates_match = recovery.step == step && recovery.next_step_attempt == attempt;
    if !identity_matches
        || !head_matches
        || !prefix_matches
        || (!predecessor_matches && !consumed_matches)
        || !coordinates_match
    {
        return Err(RecoveryError::invalid(
            "provider attempt does not match exact latest recovery authorization",
        ));
    }
    Ok(())
}

pub fn validate_requested_recovery(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    recovery_id: u32,
) -> Result<RecoveryAttemptV1, RecoveryError> {
    let reference = run
        .latest_recovery
        .as_ref()
        .ok_or_else(|| RecoveryError::invalid("run has no recovery authorization"))?;
    if reference.recovery_id != recovery_id {
        return Err(RecoveryError::invalid(
            "requested recovery is not the latest authority",
        ));
    }
    let recovery = load_verified_recovery(workspace, run, reference)?;
    if recovery_is_consumed(workspace, run, reference)? {
        return Err(RecoveryError::invalid(
            "recovery request is already durable; use ordinary loop resume",
        ));
    }
    validate_operational_recovery_authority(workspace, run, &recovery)?;
    Ok(recovery)
}

fn validate_operational_recovery_authority(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    recovery: &RecoveryAttemptV1,
) -> Result<(), RecoveryError> {
    let lock = acquire_candidate_lock(workspace).map_err(RecoveryError::wrapped)?;
    let result = (|| {
        let current = state::load_run(workspace).map_err(RecoveryError::wrapped)?;
        if &current != run {
            return Err(RecoveryError::invalid(
                "LoopRun changed before operational recovery validation",
            ));
        }
        let candidate = current.candidate_workspace.as_ref().ok_or_else(|| {
            RecoveryError::invalid("operational recovery lost candidate authority")
        })?;
        let source_authority = capture_source_worktree_authority(
            Path::new(&candidate.source_worktree_root),
            Some(workspace.run_directory()),
        )
        .map_err(RecoveryError::wrapped)?;
        if canonical_sha256_digest(&source_authority).map_err(RecoveryError::wrapped)?
            != recovery.source_worktree_state_digest
        {
            return Err(RecoveryError::invalid(
                "source worktree authority changed before exact recovery rerun",
            ));
        }
        validate_physical_candidate_locked(workspace, &current, recovery.step)
    })();
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(RecoveryError::wrapped(error)),
        (Err(error), _) => Err(error),
    }
}

fn recovery_is_consumed(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    reference: &RecoveryReference,
) -> Result<bool, RecoveryError> {
    let (recovery, source, projection) = load_verified_recovery_lineage(workspace, reference)?;
    let expected_request_path = workspace.run_directory().join(format!(
        "artifacts/{}.attempt-{:03}.exchange-001.initial.request.record.json",
        state::step_file_stem(recovery.step),
        recovery.next_step_attempt
    ));
    match fs::symlink_metadata(&expected_request_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(RecoveryError::wrapped(error)),
        Ok(_) => {}
    }
    let prospective = preflight_provider_exchange_reconciliation(workspace, run)
        .map_err(RecoveryError::wrapped)?;
    validate_current_descendant(
        workspace,
        &prospective,
        reference,
        &source,
        &projection,
        &recovery,
    )?;
    for candidate in &prospective.provider_exchange_records {
        if candidate.step == recovery.step
            && candidate.step_attempt == recovery.next_step_attempt
            && candidate.exchange_index == 1
            && candidate.kind == ProviderExchangeKind::Initial
            && candidate.phase == ProviderExchangePhase::Request
        {
            let record = load_provider_exchange_record(workspace.run_directory(), candidate)
                .map_err(RecoveryError::wrapped)?;
            return Ok(record.previous_record_digest
                == recovery
                    .previous_provider_head
                    .as_ref()
                    .map(|head| head.digest.clone()));
        }
    }
    Ok(false)
}

fn load_verified_recovery(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    reference: &RecoveryReference,
) -> Result<RecoveryAttemptV1, RecoveryError> {
    let (recovery, source, projection) = load_verified_recovery_lineage(workspace, reference)?;
    validate_current_descendant(workspace, run, reference, &source, &projection, &recovery)?;
    Ok(recovery)
}

fn load_verified_recovery_lineage(
    workspace: &LoopWorkspace,
    reference: &RecoveryReference,
) -> Result<(RecoveryAttemptV1, LoopRun, LoopRun), RecoveryError> {
    let bytes = read_verified_regular_file(
        workspace.run_directory(),
        &reference.artifact.path,
        "recovery attempt",
    )
    .map_err(RecoveryError::wrapped)?;
    if digest_bytes(&bytes) != reference.artifact.digest {
        return Err(RecoveryError::invalid("recovery attempt digest mismatch"));
    }
    let recovery: RecoveryAttemptV1 =
        serde_json::from_slice(&bytes).map_err(RecoveryError::wrapped)?;
    if canonical_json_bytes(&recovery).map_err(RecoveryError::wrapped)? != bytes {
        return Err(RecoveryError::invalid(
            "recovery attempt is not canonical JSON",
        ));
    }
    validate_recovery_contract(&recovery)?;
    if recovery.recovery_id != reference.recovery_id
        || reference.artifact.path != recovery_path(reference.recovery_id)
        || !is_lower_hex_digest(&reference.artifact.digest)
    {
        return Err(RecoveryError::invalid(
            "recovery bindings do not match LoopRun authority",
        ));
    }
    let snapshot_bytes = read_verified_regular_file(
        workspace.run_directory(),
        &recovery.source_run.path,
        "recovery source run",
    )
    .map_err(RecoveryError::wrapped)?;
    if digest_bytes(&snapshot_bytes) != recovery.source_run.digest {
        return Err(RecoveryError::invalid(
            "recovery source snapshot digest mismatch",
        ));
    }
    let snapshot: RecoverySourceRunV1 =
        serde_json::from_slice(&snapshot_bytes).map_err(RecoveryError::wrapped)?;
    if canonical_json_bytes(&snapshot).map_err(RecoveryError::wrapped)? != snapshot_bytes
        || snapshot.schema_version != RECOVERY_SCHEMA_VERSION
        || snapshot.recovery_id != recovery.recovery_id
        || canonical_sha256_digest(&snapshot.run).map_err(RecoveryError::wrapped)?
            != recovery.source_run_digest
    {
        return Err(RecoveryError::invalid(
            "recovery source snapshot binding mismatch",
        ));
    }
    let source_errors = seaf_core::validate_loop_run(&snapshot.run);
    if !source_errors.is_empty() {
        return Err(RecoveryError::invalid(format!(
            "recovery source snapshot contains an invalid LoopRun: {source_errors:?}"
        )));
    }
    validate_source_bindings(&snapshot.run, &recovery)?;
    validate_prior_recovery_chain(workspace, recovery.previous_recovery.clone())?;
    let projection = reset_run(
        &snapshot.run,
        recovery.step,
        recovery.recovery_id,
        &reference.artifact.path,
        &recovery.created_at,
    )?;
    if canonical_sha256_digest(&projection).map_err(RecoveryError::wrapped)?
        != recovery.expected_reset_projection_digest
    {
        return Err(RecoveryError::invalid("reset projection digest mismatch"));
    }
    Ok((recovery, snapshot.run, projection))
}

fn validate_prior_recovery_chain(
    workspace: &LoopWorkspace,
    mut reference: Option<RecoveryReference>,
) -> Result<(), RecoveryError> {
    while let Some(current) = reference {
        if current.artifact.path != recovery_path(current.recovery_id)
            || !is_lower_hex_digest(&current.artifact.digest)
        {
            return Err(RecoveryError::invalid(
                "prior recovery chain contains a noncanonical reference",
            ));
        }
        let bytes = read_verified_regular_file(
            workspace.run_directory(),
            &current.artifact.path,
            "prior recovery attempt",
        )
        .map_err(RecoveryError::wrapped)?;
        if digest_bytes(&bytes) != current.artifact.digest {
            return Err(RecoveryError::invalid(
                "prior recovery chain digest mismatch",
            ));
        }
        let recovery: RecoveryAttemptV1 =
            serde_json::from_slice(&bytes).map_err(RecoveryError::wrapped)?;
        if canonical_json_bytes(&recovery).map_err(RecoveryError::wrapped)? != bytes
            || recovery.recovery_id != current.recovery_id
        {
            return Err(RecoveryError::invalid(
                "prior recovery chain artifact is not exact canonical authority",
            ));
        }
        validate_recovery_contract(&recovery)?;
        let source_bytes = read_verified_regular_file(
            workspace.run_directory(),
            &recovery.source_run.path,
            "prior recovery source run",
        )
        .map_err(RecoveryError::wrapped)?;
        if digest_bytes(&source_bytes) != recovery.source_run.digest {
            return Err(RecoveryError::invalid(
                "prior recovery source snapshot digest mismatch",
            ));
        }
        let source: RecoverySourceRunV1 =
            serde_json::from_slice(&source_bytes).map_err(RecoveryError::wrapped)?;
        if canonical_json_bytes(&source).map_err(RecoveryError::wrapped)? != source_bytes
            || source.schema_version != RECOVERY_SCHEMA_VERSION
            || source.recovery_id != recovery.recovery_id
            || canonical_sha256_digest(&source.run).map_err(RecoveryError::wrapped)?
                != recovery.source_run_digest
            || !seaf_core::validate_loop_run(&source.run).is_empty()
        {
            return Err(RecoveryError::invalid(
                "prior recovery source snapshot binding mismatch",
            ));
        }
        validate_source_bindings(&source.run, &recovery)?;
        let projection = reset_run(
            &source.run,
            recovery.step,
            recovery.recovery_id,
            &current.artifact.path,
            &recovery.created_at,
        )?;
        if canonical_sha256_digest(&projection).map_err(RecoveryError::wrapped)?
            != recovery.expected_reset_projection_digest
        {
            return Err(RecoveryError::invalid(
                "prior recovery reset projection binding mismatch",
            ));
        }
        reference = recovery.previous_recovery;
    }
    Ok(())
}

fn validate_source_bindings(
    source: &LoopRun,
    recovery: &RecoveryAttemptV1,
) -> Result<(), RecoveryError> {
    let candidate = source.candidate_workspace.as_ref().ok_or_else(|| {
        RecoveryError::invalid("recovery source snapshot lost candidate authority")
    })?;
    if recovery.run_id != source.run_id
        || recovery.input_digests != source.input_digests
        || recovery.previous_recovery != source.latest_recovery
        || recovery.previous_provider_head != source.provider_exchange_records.last().cloned()
        || recovery.candidate_state_digest
            != canonical_sha256_digest(candidate).map_err(RecoveryError::wrapped)?
        || recovery.candidate_head != candidate.candidate_head
        || recovery.candidate_tree != candidate.candidate_tree
        || recovery.candidate_diff_digest != candidate.candidate_diff_digest
    {
        return Err(RecoveryError::invalid(
            "recovery fields do not bind the exact source authority",
        ));
    }
    validate_snapshot_attempt_authority(source, recovery)
}

fn validate_current_descendant(
    workspace: &LoopWorkspace,
    current: &LoopRun,
    reference: &RecoveryReference,
    source: &LoopRun,
    zero_digest_projection: &LoopRun,
    recovery: &RecoveryAttemptV1,
) -> Result<(), RecoveryError> {
    let core_errors = seaf_core::validate_loop_run(current);
    if !core_errors.is_empty() {
        return Err(RecoveryError::invalid(format!(
            "current recovery descendant is not a valid LoopRun: {core_errors:?}"
        )));
    }
    if current.run_id != source.run_id
        || current.ticket_id != source.ticket_id
        || current.goal_id != source.goal_id
        || current.execution_mode != source.execution_mode
        || current.provider != source.provider
        || current.model != source.model
        || current.input_digests != source.input_digests
        || current.latest_recovery.as_ref() != Some(reference)
        || !current
            .provider_exchange_records
            .starts_with(&source.provider_exchange_records)
    {
        return Err(RecoveryError::invalid(
            "current LoopRun is not a descendant of the recovery source",
        ));
    }
    validate_authoritative_provider_exchange_records(workspace, current)
        .map_err(RecoveryError::wrapped)?;
    let mut persisted_reset = zero_digest_projection.clone();
    persisted_reset.latest_recovery = Some(reference.clone());
    if current == &persisted_reset {
        return Ok(());
    }
    let mut pre_request = persisted_reset.clone();
    pre_request.status = LoopStatus::Running;
    pre_request.updated_at = current.updated_at.clone();
    if let Some(step) = pre_request
        .steps
        .iter_mut()
        .find(|record| record.name == recovery.step)
    {
        step.status = seaf_core::LoopStepStatus::Running;
    }
    if current == &pre_request {
        return Ok(());
    }
    let first_new = current
        .provider_exchange_records
        .get(source.provider_exchange_records.len())
        .ok_or_else(|| RecoveryError::invalid("recovery descendant has no consuming request"))?;
    if first_new.step != recovery.step
        || first_new.step_attempt != recovery.next_step_attempt
        || first_new.exchange_index != 1
        || first_new.kind != ProviderExchangeKind::Initial
        || first_new.phase != ProviderExchangePhase::Request
    {
        return Err(RecoveryError::invalid(
            "recovery descendant does not begin with the exact authorized request",
        ));
    }
    let request = load_provider_exchange_record(workspace.run_directory(), first_new)
        .map_err(RecoveryError::wrapped)?;
    if request.previous_record_digest
        != recovery
            .previous_provider_head
            .as_ref()
            .map(|head| head.digest.clone())
    {
        return Err(RecoveryError::invalid(
            "recovery consuming request substituted provider lineage",
        ));
    }
    Ok(())
}

fn validate_creation_eligibility(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    step: LoopStepName,
) -> Result<(), RecoveryError> {
    if run.execution_mode != LoopExecutionMode::IsolatedCandidate {
        return Err(RecoveryError::invalid(
            "legacy runs cannot create recovery authority",
        ));
    }
    let candidate = run
        .candidate_workspace
        .as_ref()
        .ok_or_else(|| RecoveryError::invalid("isolated recovery requires candidate authority"))?;
    if let Some(reference) = &run.latest_recovery {
        if !recovery_is_consumed(workspace, run, reference)? {
            return Err(RecoveryError::invalid("a prior recovery is still pending"));
        }
    }
    let inspection = inspect_loop_run(
        workspace
            .run_directory()
            .parent()
            .ok_or_else(|| RecoveryError::invalid("run has no runs root"))?,
        &run.run_id,
    )
    .map_err(RecoveryError::wrapped)?;
    if inspection.integrity != InspectionIntegrity::Verified {
        return Err(RecoveryError::invalid(
            "recovery requires unambiguous verified run authority",
        ));
    }
    validate_eligibility_shape(
        run,
        candidate,
        step,
        !inspection.evaluation_prefix.is_empty(),
    )?;
    Ok(())
}

fn validate_eligibility_shape(
    run: &LoopRun,
    candidate: &CandidateWorkspaceState,
    step: LoopStepName,
    has_evaluation_prefix: bool,
) -> Result<(), RecoveryError> {
    if run.execution_mode != LoopExecutionMode::IsolatedCandidate
        || candidate.schema_version != CANDIDATE_WORKSPACE_SCHEMA_VERSION
        || candidate.lifecycle != CandidateWorkspaceLifecycle::Active
    {
        return Err(RecoveryError::invalid(
            "recovery requires active candidate schema version 2",
        ));
    }
    if has_evaluation_prefix {
        return Err(RecoveryError::invalid(
            "provider recovery rejects every factual evaluation prefix",
        ));
    }
    let applied = candidate
        .patch_transaction
        .as_ref()
        .map(|transaction| transaction.phase);
    let provider_step = step_index(step).map_err(RecoveryError::wrapped)?
        <= step_index(LoopStepName::OutputReview).map_err(RecoveryError::wrapped)?;
    if !provider_step {
        return Err(RecoveryError::invalid(
            "evaluation steps require M1-09c recovery",
        ));
    }
    match run.status {
        LoopStatus::Blocked | LoopStatus::Failed if run.human_approval.is_none() => match applied {
            None if step_index(step).map_err(RecoveryError::wrapped)?
                <= step_index(LoopStepName::Development).map_err(RecoveryError::wrapped)?
                && step_index(step).map_err(RecoveryError::wrapped)?
                    <= step_index(run.current_step).map_err(RecoveryError::wrapped)? => {}
            Some(CandidatePatchPhase::Applied) if step == LoopStepName::OutputReview => {}
            _ => {
                return Err(RecoveryError::invalid(
                    "candidate phase is not eligible for this recovery step",
                ))
            }
        },
        LoopStatus::AwaitingHumanReview | LoopStatus::Approved
            if applied == Some(CandidatePatchPhase::Applied)
                && step == LoopStepName::OutputReview => {}
        LoopStatus::AwaitingHumanReview | LoopStatus::Approved => {
            return Err(RecoveryError::invalid(
                "human-review recovery requires OutputReview and an empty evaluation prefix",
            ))
        }
        _ => {
            return Err(RecoveryError::invalid(
                "run status is not eligible for provider recovery",
            ))
        }
    }
    Ok(())
}

fn validate_physical_candidate_locked(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    step: LoopStepName,
) -> Result<(), RecoveryError> {
    let candidate = run.candidate_workspace.as_ref().expect("checked");
    if candidate
        .patch_transaction
        .as_ref()
        .map(|transaction| transaction.phase)
        == Some(CandidatePatchPhase::Applied)
    {
        if step != LoopStepName::OutputReview {
            return Err(RecoveryError::invalid(
                "Applied candidate permits only OutputReview recovery",
            ));
        }
        verify_candidate_patch_evidence_locked(
            workspace,
            Path::new(&candidate.source_worktree_root),
        )
        .map_err(RecoveryError::wrapped)?;
    } else {
        validate_candidate_workspace(
            workspace.run_directory(),
            Path::new(&candidate.source_worktree_root),
            candidate,
        )
        .map_err(RecoveryError::wrapped)?;
    }
    Ok(())
}

fn authenticated_source_attempt(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    step: LoopStepName,
) -> Result<u32, RecoveryError> {
    let attempt = run
        .provider_exchange_records
        .iter()
        .filter(|reference| reference.step == step)
        .map(|reference| reference.step_attempt)
        .max()
        .ok_or_else(|| {
            RecoveryError::invalid("selected step has no authenticated source attempt")
        })?;
    let has_request = run.provider_exchange_records.iter().any(|reference| {
        reference.step == step
            && reference.step_attempt == attempt
            && reference.kind == ProviderExchangeKind::Initial
            && reference.phase == ProviderExchangePhase::Request
    });
    let has_response = run.provider_exchange_records.iter().any(|reference| {
        reference.step == step
            && reference.step_attempt == attempt
            && reference.phase == ProviderExchangePhase::Response
    });
    if !has_request
        || !has_response
        || latest_step_attempt(workspace, step).map_err(RecoveryError::wrapped)? != Some(attempt)
    {
        return Err(RecoveryError::invalid(
            "selected step attempt authority is incomplete or ambiguous",
        ));
    }
    Ok(attempt)
}

fn reset_run(
    source: &LoopRun,
    step: LoopStepName,
    recovery_id: u32,
    recovery_path: &str,
    created_at: &str,
) -> Result<LoopRun, RecoveryError> {
    let mut reset = source.clone();
    state::reset_from_step(&mut reset, step).map_err(RecoveryError::wrapped)?;
    if step_index(step).map_err(RecoveryError::wrapped)?
        <= step_index(LoopStepName::Development).map_err(RecoveryError::wrapped)?
    {
        let run_id = reset.run_id.clone();
        reset.policy_decisions.retain(|decision| {
            decision.get("patch_id").and_then(serde_json::Value::as_str) != Some(run_id.as_str())
        });
    }
    if step == LoopStepName::OutputReview {
        reset.human_approval = None;
        reset.eval_report_path = None;
    }
    reset.promotion = None;
    reset.updated_at = created_at.to_string();
    reset.latest_recovery = Some(RecoveryReference {
        recovery_id,
        artifact: ArtifactReference {
            path: recovery_path.to_string(),
            digest: "0".repeat(64),
        },
    });
    Ok(reset)
}

fn validate_reset_relation(
    source: &LoopRun,
    intended: &LoopRun,
    recovery: &RecoveryAttemptV1,
) -> Result<(), RecoveryError> {
    let mut projection = intended.clone();
    projection
        .latest_recovery
        .as_mut()
        .ok_or_else(|| RecoveryError::invalid("reset lost recovery reference"))?
        .artifact
        .digest = "0".repeat(64);
    if canonical_sha256_digest(&projection).map_err(RecoveryError::wrapped)?
        != recovery.expected_reset_projection_digest
        || source.provider_exchange_records != intended.provider_exchange_records
        || source.candidate_workspace != intended.candidate_workspace
        || source.input_digests != intended.input_digests
        || source.provider != intended.provider
        || source.model != intended.model
        || source.run_id != intended.run_id
        || source.ticket_id != intended.ticket_id
        || source.goal_id != intended.goal_id
        || intended.status != LoopStatus::Pending
        || intended.current_step != recovery.step
    {
        return Err(RecoveryError::invalid(
            "intended recovery reset relation is invalid",
        ));
    }
    Ok(())
}

fn validate_recovery_contract(recovery: &RecoveryAttemptV1) -> Result<(), RecoveryError> {
    if recovery.schema_version != RECOVERY_SCHEMA_VERSION || recovery.recovery_id == 0 {
        return Err(RecoveryError::invalid("invalid recovery schema or ID"));
    }
    validate_note("actor", &recovery.actor, 256)?;
    validate_note("reason", &recovery.reason, 1024)?;
    let canonical_timestamp = recovery
        .created_at
        .parse::<u64>()
        .ok()
        .is_some_and(|value| value.to_string() == recovery.created_at);
    let provider_step = step_index(recovery.step)
        .ok()
        .zip(step_index(LoopStepName::OutputReview).ok())
        .is_some_and(|(step, output_review)| step <= output_review);
    let previous_recovery_valid = match (&recovery.previous_recovery, recovery.recovery_id) {
        (None, 1) => true,
        (Some(previous), id) if id > 1 => {
            previous.recovery_id.checked_add(1) == Some(id)
                && previous.artifact.path == recovery_path(previous.recovery_id)
                && is_lower_hex_digest(&previous.artifact.digest)
        }
        _ => false,
    };
    if !canonical_timestamp
        || !provider_step
        || !previous_recovery_valid
        || recovery.source_step_attempt == 0
        || recovery.next_step_attempt != recovery.source_step_attempt.checked_add(1).unwrap_or(0)
        || recovery.source_run.path != recovery_source_path(recovery.recovery_id)
        || !is_lower_hex_digest(&recovery.source_run.digest)
        || !is_lower_hex_digest(&recovery.source_run_digest)
        || !is_lower_hex_digest(&recovery.candidate_state_digest)
        || !is_lower_hex_digest(&recovery.candidate_diff_digest)
        || !is_lower_hex_digest(&recovery.source_worktree_state_digest)
        || !is_git_object_id(&recovery.candidate_head)
        || !is_git_object_id(&recovery.candidate_tree)
        || !is_lower_hex_digest(&recovery.expected_reset_projection_digest)
        || !is_lower_hex_digest(&recovery.input_digests.ticket)
        || !is_lower_hex_digest(&recovery.input_digests.policy)
        || !is_lower_hex_digest(&recovery.input_digests.config)
        || !is_lower_hex_digest(&recovery.input_digests.repository)
        || recovery
            .input_digests
            .eval_config
            .as_ref()
            .is_some_and(|digest| !is_lower_hex_digest(digest))
    {
        return Err(RecoveryError::invalid(
            "recovery contract fields are invalid",
        ));
    }
    Ok(())
}

fn validate_snapshot_attempt_authority(
    source: &LoopRun,
    recovery: &RecoveryAttemptV1,
) -> Result<(), RecoveryError> {
    let max_attempt = source
        .provider_exchange_records
        .iter()
        .filter(|reference| reference.step == recovery.step)
        .map(|reference| reference.step_attempt)
        .max();
    let has_initial_request = source.provider_exchange_records.iter().any(|reference| {
        reference.step == recovery.step
            && reference.step_attempt == recovery.source_step_attempt
            && reference.exchange_index == 1
            && reference.kind == ProviderExchangeKind::Initial
            && reference.phase == ProviderExchangePhase::Request
    });
    let has_response = source.provider_exchange_records.iter().any(|reference| {
        reference.step == recovery.step
            && reference.step_attempt == recovery.source_step_attempt
            && reference.phase == ProviderExchangePhase::Response
    });
    if max_attempt != Some(recovery.source_step_attempt) || !has_initial_request || !has_response {
        return Err(RecoveryError::invalid(
            "recovery attempt numbers do not match source provider history",
        ));
    }
    Ok(())
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_note(field: &str, value: &str, max: usize) -> Result<(), RecoveryError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > max
        || value.chars().any(char::is_control)
    {
        Err(RecoveryError::invalid(format!(
            "{field} must be trimmed, nonempty, control-free, and at most {max} bytes"
        )))
    } else {
        Ok(())
    }
}

fn existing_or_new_timestamp(
    workspace: &LoopWorkspace,
    path: &str,
) -> Result<String, RecoveryError> {
    match fs::symlink_metadata(workspace.run_directory().join(path)) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            RecoveryError::invalid("recovery orphan is not a real regular file"),
        ),
        Ok(_) => {
            let bytes =
                read_verified_regular_file(workspace.run_directory(), path, "recovery orphan")
                    .map_err(RecoveryError::wrapped)?;
            let recovery: RecoveryAttemptV1 =
                serde_json::from_slice(&bytes).map_err(RecoveryError::wrapped)?;
            Ok(recovery.created_at)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(now_timestamp()),
        Err(error) => Err(RecoveryError::wrapped(error)),
    }
}

fn validate_recovery_namespace(
    workspace: &LoopWorkspace,
    latest_persisted_id: u32,
    allowed_orphan_id: u32,
) -> Result<(), RecoveryError> {
    let artifacts = workspace.run_directory().join("artifacts");
    let entries = fs::read_dir(&artifacts).map_err(RecoveryError::wrapped)?;
    let mut seen_recovery = std::collections::BTreeSet::new();
    let mut seen_source = std::collections::BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(RecoveryError::wrapped)?;
        let file_type = entry.file_type().map_err(RecoveryError::wrapped)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| RecoveryError::invalid("recovery artifact filename is not valid UTF-8"))?;
        if !name.starts_with("recovery-") {
            continue;
        }
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(RecoveryError::invalid(
                "recovery artifact namespace entry is not a real regular file",
            ));
        }
        let (id, is_source) = parse_recovery_filename(&name).ok_or_else(|| {
            RecoveryError::invalid(format!("noncanonical recovery artifact filename `{name}`"))
        })?;
        if id == 0 || id > allowed_orphan_id {
            return Err(RecoveryError::invalid(
                "recovery artifact namespace contains a gap or unexpected future ID",
            ));
        }
        if is_source {
            seen_source.insert(id);
        } else {
            seen_recovery.insert(id);
        }
    }
    validate_contiguous_history(&seen_recovery, latest_persisted_id)?;
    validate_contiguous_history(&seen_source, latest_persisted_id)?;
    Ok(())
}

fn validate_contiguous_history(
    ids: &std::collections::BTreeSet<u32>,
    latest: u32,
) -> Result<(), RecoveryError> {
    let historical: Vec<u32> = ids.iter().copied().filter(|id| *id <= latest).collect();
    if historical.len() != latest as usize {
        return Err(RecoveryError::invalid(
            "recovery artifact namespace has a gap in historical IDs",
        ));
    }
    let mut expected = 1_u32;
    for id in historical {
        if id != expected {
            return Err(RecoveryError::invalid(
                "recovery artifact namespace has a gap in historical IDs",
            ));
        }
        if id != latest {
            expected = expected.checked_add(1).ok_or_else(|| {
                RecoveryError::invalid("recovery artifact ID sequence is exhausted")
            })?;
        }
    }
    Ok(())
}

fn parse_recovery_filename(name: &str) -> Option<(u32, bool)> {
    let (digits, is_source) = if let Some(digits) = name
        .strip_prefix("recovery-")
        .and_then(|value| value.strip_suffix(".source-run.json"))
    {
        (digits, true)
    } else {
        (
            name.strip_prefix("recovery-")?.strip_suffix(".json")?,
            false,
        )
    };
    let id: u32 = digits.parse().ok()?;
    let canonical = if is_source {
        format!("recovery-{id:03}.source-run.json")
    } else {
        format!("recovery-{id:03}.json")
    };
    (canonical == name).then_some((id, is_source))
}

fn recovery_path(id: u32) -> String {
    format!("artifacts/recovery-{id:03}.json")
}

fn recovery_source_path(id: u32) -> String {
    format!("artifacts/recovery-{id:03}.source-run.json")
}

fn digest_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn now_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[derive(Debug)]
pub struct RecoveryError(String);

impl RecoveryError {
    fn invalid(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    fn wrapped(error: impl fmt::Display) -> Self {
        Self(error.to_string())
    }
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "audited recovery failed: {}", self.0)
    }
}

impl Error for RecoveryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use seaf_core::{
        CandidatePatchTransaction, HumanApprovalEvidence, LoopStepStatus, ProviderRole,
    };

    fn shape_run(status: LoopStatus, applied: bool) -> LoopRun {
        let mut run = state::create_run(state::NewLoopRun {
            run_id: "eligibility-shape".to_string(),
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
        let artifact = ArtifactReference {
            path: "artifacts/evidence.json".to_string(),
            digest: "e".repeat(64),
        };
        run.execution_mode = LoopExecutionMode::IsolatedCandidate;
        run.status = status;
        run.current_step = if applied {
            LoopStepName::OutputReview
        } else {
            LoopStepName::Development
        };
        run.candidate_workspace = Some(CandidateWorkspaceState {
            schema_version: CANDIDATE_WORKSPACE_SCHEMA_VERSION,
            run_directory_digest: Some("1".repeat(64)),
            path: "/tmp/candidate".to_string(),
            source_worktree_root: "/tmp/source".to_string(),
            git_common_dir: "/tmp/source/.git".to_string(),
            repository_identity_digest: "d".repeat(64),
            starting_head: "2".repeat(40),
            starting_tree: "3".repeat(40),
            candidate_head: "4".repeat(40),
            candidate_tree: "5".repeat(40),
            candidate_diff_digest: "6".repeat(64),
            patch_transaction: applied.then(|| CandidatePatchTransaction {
                schema_version: 1,
                phase: CandidatePatchPhase::Applied,
                intent: artifact.clone(),
                applied_evidence: Some(artifact.clone()),
                started_at: "1".to_string(),
                applied_at: Some("2".to_string()),
            }),
            lifecycle: CandidateWorkspaceLifecycle::Active,
            cleanup_started_at: None,
            cleaned_at: None,
        });
        if status == LoopStatus::Approved {
            let provider_reference = ProviderExchangeRecordReference {
                run_id: run.run_id.clone(),
                step: LoopStepName::OutputReview,
                role: ProviderRole::OutputReviewer,
                step_attempt: 1,
                exchange_index: 1,
                kind: ProviderExchangeKind::Initial,
                context_round: None,
                phase: ProviderExchangePhase::Response,
                path: "artifacts/provider.json".to_string(),
                digest: "f".repeat(64),
            };
            run.human_approval = Some(HumanApprovalEvidence {
                schema_version: 1,
                run_id: run.run_id.clone(),
                reviewer: "reviewer@example.invalid".to_string(),
                approved_at: "3".to_string(),
                candidate_diff: artifact.clone(),
                starting_head: "2".repeat(40),
                policy_decision_digest: "7".repeat(64),
                output_review: artifact,
                output_review_request: ProviderExchangeRecordReference {
                    phase: ProviderExchangePhase::Request,
                    ..provider_reference.clone()
                },
                output_review_response: provider_reference,
            });
        }
        for record in &mut run.steps {
            if record.name == run.current_step {
                record.status = LoopStepStatus::Completed;
            }
        }
        run
    }

    #[test]
    fn recovery_filename_parser_is_canonical_and_unbounded_by_three_digits() {
        assert_eq!(
            parse_recovery_filename("recovery-001.json"),
            Some((1, false))
        );
        assert_eq!(
            parse_recovery_filename("recovery-1000.source-run.json"),
            Some((1000, true))
        );
        assert_eq!(parse_recovery_filename("recovery-1.json"), None);
        assert_eq!(
            parse_recovery_filename("recovery-000.json"),
            Some((0, false))
        );
    }

    #[test]
    fn contiguous_history_rejects_gaps_and_max_id_without_numeric_range_expansion() {
        let contiguous = [1, 2, 3].into_iter().collect();
        validate_contiguous_history(&contiguous, 3).unwrap();

        let gap = [1, 3].into_iter().collect();
        assert!(validate_contiguous_history(&gap, 3).is_err());

        let bounded = [u32::MAX].into_iter().collect();
        assert!(validate_contiguous_history(&bounded, u32::MAX).is_err());
    }

    #[test]
    fn provider_recovery_eligibility_shape_matrix_is_fail_closed() {
        for status in [
            LoopStatus::Pending,
            LoopStatus::Running,
            LoopStatus::Completed,
            LoopStatus::Passed,
            LoopStatus::EvalPassed,
            LoopStatus::Promoted,
        ] {
            let run = shape_run(status, false);
            assert!(
                validate_eligibility_shape(
                    &run,
                    run.candidate_workspace.as_ref().unwrap(),
                    LoopStepName::Development,
                    false,
                )
                .is_err(),
                "{status:?} must reject"
            );
        }

        for status in [LoopStatus::Blocked, LoopStatus::Failed] {
            let run = shape_run(status, false);
            assert!(
                validate_eligibility_shape(
                    &run,
                    run.candidate_workspace.as_ref().unwrap(),
                    LoopStepName::Development,
                    false,
                )
                .is_ok(),
                "{status:?} pristine Development must be eligible"
            );
        }
        for status in [LoopStatus::AwaitingHumanReview, LoopStatus::Approved] {
            let run = shape_run(status, true);
            assert!(
                validate_eligibility_shape(
                    &run,
                    run.candidate_workspace.as_ref().unwrap(),
                    LoopStepName::OutputReview,
                    false,
                )
                .is_ok(),
                "{status:?} Applied OutputReview must be eligible"
            );
        }

        let mut wrong_phase = shape_run(LoopStatus::AwaitingHumanReview, true);
        assert!(validate_eligibility_shape(
            &wrong_phase,
            wrong_phase.candidate_workspace.as_ref().unwrap(),
            LoopStepName::Development,
            false,
        )
        .is_err());
        wrong_phase
            .candidate_workspace
            .as_mut()
            .unwrap()
            .patch_transaction
            .as_mut()
            .unwrap()
            .phase = CandidatePatchPhase::Applying;
        assert!(validate_eligibility_shape(
            &wrong_phase,
            wrong_phase.candidate_workspace.as_ref().unwrap(),
            LoopStepName::OutputReview,
            false,
        )
        .is_err());
        for lifecycle in [
            CandidateWorkspaceLifecycle::Provisioning,
            CandidateWorkspaceLifecycle::Cleaning,
            CandidateWorkspaceLifecycle::Cleaned,
        ] {
            let mut lifecycle_run = shape_run(LoopStatus::AwaitingHumanReview, true);
            lifecycle_run
                .candidate_workspace
                .as_mut()
                .unwrap()
                .lifecycle = lifecycle;
            assert!(
                validate_eligibility_shape(
                    &lifecycle_run,
                    lifecycle_run.candidate_workspace.as_ref().unwrap(),
                    LoopStepName::OutputReview,
                    false,
                )
                .is_err(),
                "{lifecycle:?} must reject"
            );
        }
        let mut legacy = shape_run(LoopStatus::Blocked, false);
        legacy.execution_mode = LoopExecutionMode::LegacyProposalOnly;
        assert!(validate_eligibility_shape(
            &legacy,
            legacy.candidate_workspace.as_ref().unwrap(),
            LoopStepName::Development,
            false,
        )
        .is_err());
        let mut approved_failure = shape_run(LoopStatus::Failed, false);
        approved_failure.human_approval = shape_run(LoopStatus::Approved, true).human_approval;
        assert!(validate_eligibility_shape(
            &approved_failure,
            approved_failure.candidate_workspace.as_ref().unwrap(),
            LoopStepName::Development,
            false,
        )
        .is_err());
        let prefix = shape_run(LoopStatus::Blocked, false);
        assert!(validate_eligibility_shape(
            &prefix,
            prefix.candidate_workspace.as_ref().unwrap(),
            LoopStepName::Development,
            true,
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_namespace_rejects_symlink_directory_and_non_utf8_entries() {
        use std::os::unix::fs::symlink;

        #[cfg(target_os = "linux")]
        let collisions = vec!["symlink", "directory", "non-utf8"];
        #[cfg(not(target_os = "linux"))]
        let collisions = vec!["symlink", "directory"];
        for collision in collisions {
            let temp = tempfile::tempdir().unwrap();
            let workspace =
                LoopWorkspace::create(&temp.path().join("runs"), &format!("namespace-{collision}"))
                    .unwrap();
            let artifacts = workspace.run_directory().join("artifacts");
            match collision {
                "symlink" => {
                    symlink("missing", artifacts.join("recovery-001.json")).unwrap();
                }
                "directory" => {
                    fs::create_dir(artifacts.join("recovery-001.json")).unwrap();
                }
                "non-utf8" => {
                    #[cfg(target_os = "linux")]
                    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
                    let mut name = b"recovery-".to_vec();
                    name.push(0xff);
                    name.extend_from_slice(b".json");
                    #[cfg(target_os = "linux")]
                    fs::write(artifacts.join(OsString::from_vec(name)), b"x").unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                validate_recovery_namespace(&workspace, 0, 1).is_err(),
                "{collision} recovery entry must fail closed"
            );
        }
    }
}
