use std::{
    error::Error,
    fmt, fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, ArtifactReference, CandidatePatchPhase,
    CandidateWorkspaceLifecycle, CandidateWorkspaceState, LoopExecutionMode, LoopInputDigests,
    LoopRun, LoopStatus, LoopStepName, LoopStepStatus, ProviderExchangeKind, ProviderExchangePhase,
    ProviderExchangeRecordReference, RecoveryReference,
};
use serde::{Deserialize, Serialize};

use crate::{
    artifacts::latest_step_attempt,
    candidate_workspace::{
        acquire_candidate_lock, capture_source_worktree_authority, validate_candidate_workspace,
        validate_source_worktree_authority, verify_candidate_patch_evidence_for_evaluation_locked,
        verify_candidate_patch_evidence_locked, SourceWorktreeAuthority,
        CANDIDATE_WORKSPACE_SCHEMA_VERSION,
    },
    evaluation_attempt::{
        fixed_spelling, load_intent, reference_for_path, selected_attempt,
        ApprovedEvaluationIntent, EvaluationAttemptInventory, EvaluationInvalidationPrefixPaths,
    },
    immutable_artifact::{
        publish_create_only, publish_create_only_consuming_evaluation_slot,
        publish_create_only_with_guard_after_commitment_projection, read_verified_regular_file,
    },
    inspect::{inspect_loop_run, InspectionIntegrity},
    provider_exchange::{
        load_provider_exchange_record, persist_evaluation_adoption_with_validator,
        persist_evaluation_invalidation_with_validator,
        persist_recovery_reset_with_full_compare_and_validator,
        preflight_provider_exchange_reconciliation,
        validate_authoritative_provider_exchange_records,
    },
    state::{self, step_index},
    LoopWorkspace, TestingEvidence, VerifiedCandidatePatchEvidence,
};

pub const RECOVERY_SCHEMA_VERSION: u32 = 1;
pub const EVALUATION_RECOVERY_SCHEMA_VERSION: u32 = 2;
pub const EVALUATION_INVALIDATION_SCHEMA_VERSION: u32 = 3;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationRecoveryAction {
    AdoptApprovedEvaluation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationInvalidationAction {
    InvalidateApprovedEvaluation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAuthorityKind {
    ProviderV1,
    EvaluationAdoptionV2,
    EvaluationInvalidationV3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationRecoveryReportDisposition {
    VerifyExisting,
    CreateMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationPrefixSpellingV1 {
    FixedV1,
    IndexedV2,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationPrefixAuthorityV1 {
    pub evaluation_attempt: u32,
    pub spelling: EvaluationPrefixSpellingV1,
    pub execution_intent: ArtifactReference,
    pub testing_evidence: ArtifactReference,
    pub eval_report: Option<ArtifactReference>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationRecoverySourceRunV2 {
    pub schema_version: u32,
    pub recovery_id: u32,
    pub actor: String,
    pub reason: String,
    pub created_at: String,
    pub run: LoopRun,
    pub evaluation_prefix: EvaluationPrefixAuthorityV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationRecoveryAttemptV2 {
    pub schema_version: u32,
    pub recovery_id: u32,
    pub run_id: String,
    pub action: EvaluationRecoveryAction,
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
    pub evaluation_attempt: u32,
    pub execution_intent: ArtifactReference,
    pub testing_evidence: ArtifactReference,
    pub eval_report: ArtifactReference,
    pub report_disposition: EvaluationRecoveryReportDisposition,
    pub previous_recovery: Option<RecoveryReference>,
    pub previous_provider_head: Option<ProviderExchangeRecordReference>,
    pub expected_final_projection_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationInvalidationPrefixAuthorityV1 {
    pub evaluation_attempt: u32,
    pub spelling: EvaluationPrefixSpellingV1,
    pub present_artifacts: Vec<ArtifactReference>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationInvalidationFinalAuthorityV1 {
    pub testing_evidence: ArtifactReference,
    pub eval_report: ArtifactReference,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationInvalidationSourceRunV3 {
    pub schema_version: u32,
    pub recovery_id: u32,
    pub actor: String,
    pub reason: String,
    pub created_at: String,
    pub run: LoopRun,
    pub approved_run: LoopRun,
    pub evaluation_prefix: EvaluationInvalidationPrefixAuthorityV1,
    pub prior_final: Option<EvaluationInvalidationFinalAuthorityV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerifiedStagedEvaluationSource {
    Adoption { missing_report: bool },
    Invalidation,
}

pub(crate) fn validate_staged_evaluation_source_for_storage(
    workspace: &LoopWorkspace,
    current: &LoopRun,
    source_path: &str,
    source_bytes: &[u8],
    expected_attempt: Option<u32>,
) -> Result<VerifiedStagedEvaluationSource, RecoveryError> {
    let operator_guard = crate::operator_evidence::OperatorEvidenceGuard::load(workspace, current)
        .map_err(RecoveryError::invalid)?;
    operator_guard
        .validate_current_run_file(workspace)
        .and_then(|()| operator_guard.validate_run(current))
        .and_then(|()| operator_guard.validate_exact_raw_bytes(source_bytes))
        .map_err(RecoveryError::invalid)?;
    let value: serde_json::Value =
        serde_json::from_slice(source_bytes).map_err(RecoveryError::wrapped)?;
    if canonical_json_bytes(&value).map_err(RecoveryError::wrapped)? != source_bytes {
        return Err(RecoveryError::invalid(
            "staged evaluation source is not canonical JSON",
        ));
    }
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(schema) if schema == u64::from(EVALUATION_RECOVERY_SCHEMA_VERSION) => {
            let source: EvaluationRecoverySourceRunV2 =
                serde_json::from_value(value).map_err(RecoveryError::wrapped)?;
            operator_guard
                .validate_recovery_fields(&source.actor, &source.reason)
                .map_err(RecoveryError::invalid)?;
            if expected_attempt
                .is_some_and(|attempt| attempt != source.evaluation_prefix.evaluation_attempt)
            {
                return Err(RecoveryError::invalid(
                    "staged evaluation adoption source is not for the active attempt",
                ));
            }
            validate_staged_adoption_source(
                workspace,
                current,
                source_path,
                &source,
                &operator_guard,
            )?;
            Ok(VerifiedStagedEvaluationSource::Adoption {
                missing_report: source.evaluation_prefix.eval_report.is_none(),
            })
        }
        Some(schema) if schema == u64::from(EVALUATION_INVALIDATION_SCHEMA_VERSION) => {
            let source: EvaluationInvalidationSourceRunV3 =
                serde_json::from_value(value).map_err(RecoveryError::wrapped)?;
            operator_guard
                .validate_recovery_fields(&source.actor, &source.reason)
                .map_err(RecoveryError::invalid)?;
            if expected_attempt
                .is_some_and(|attempt| attempt != source.evaluation_prefix.evaluation_attempt)
            {
                return Err(RecoveryError::invalid(
                    "staged evaluation invalidation source is not for the active attempt",
                ));
            }
            validate_staged_invalidation_source(
                workspace,
                current,
                source_path,
                &source,
                &operator_guard,
            )?;
            Ok(VerifiedStagedEvaluationSource::Invalidation)
        }
        _ => Err(RecoveryError::invalid(
            "unsupported staged evaluation source schema",
        )),
    }
}

fn validate_staged_adoption_source(
    workspace: &LoopWorkspace,
    current: &LoopRun,
    source_path: &str,
    source: &EvaluationRecoverySourceRunV2,
    operator_guard: &crate::operator_evidence::OperatorEvidenceGuard,
) -> Result<(), RecoveryError> {
    if source.schema_version != EVALUATION_RECOVERY_SCHEMA_VERSION
        || source.recovery_id == 0
        || source_path != recovery_source_path(source.recovery_id)
        || &source.run != current
        || current.status != LoopStatus::Approved
        || current.current_step != LoopStepName::Testing
        || parse_canonical_timestamp(&source.created_at).is_none()
    {
        return Err(RecoveryError::invalid(
            "staged evaluation adoption source does not bind current authority",
        ));
    }
    validate_note("actor", &source.actor, 256)?;
    validate_note("reason", &source.reason, 1024)?;
    validate_authoritative_provider_exchange_records(workspace, current)
        .map_err(RecoveryError::wrapped)?;
    let inventory = EvaluationAttemptInventory::load_for_invalidation(workspace)
        .map_err(RecoveryError::invalid)?;
    if inventory.latest_attempt() != Some(source.evaluation_prefix.evaluation_attempt) {
        return Err(RecoveryError::invalid(
            "staged evaluation adoption source does not select the latest factual attempt",
        ));
    }
    let prefix = inventory
        .recovery_prefix_paths()
        .map_err(RecoveryError::invalid)?;
    let expected_spelling = if fixed_spelling(prefix.spelling) {
        EvaluationPrefixSpellingV1::FixedV1
    } else {
        EvaluationPrefixSpellingV1::IndexedV2
    };
    let screened_prefix = preflight_adoption_evaluation_prefix(
        workspace,
        operator_guard,
        &prefix.intent,
        &prefix.testing,
        prefix.report_present.then_some(prefix.report.as_str()),
    )?;
    let intent_reference = screened_reference(&screened_prefix, &prefix.intent)?;
    let testing_reference = screened_reference(&screened_prefix, &prefix.testing)?;
    let current_report_reference = prefix
        .report_present
        .then(|| screened_reference(&screened_prefix, &prefix.report))
        .transpose()?;
    if source.evaluation_prefix.evaluation_attempt != prefix.attempt
        || source.evaluation_prefix.spelling != expected_spelling
        || source.evaluation_prefix.execution_intent != intent_reference
        || source.evaluation_prefix.testing_evidence != testing_reference
        || source
            .evaluation_prefix
            .eval_report
            .as_ref()
            .is_some_and(|reference| Some(reference) != current_report_reference.as_ref())
    {
        return Err(RecoveryError::invalid(
            "staged evaluation adoption source prefix was substituted",
        ));
    }
    let testing = TestingEvidence::load_for_approved_run(workspace, &testing_reference, current)
        .map_err(RecoveryError::wrapped)?;
    let intent = load_intent(workspace, &intent_reference).map_err(RecoveryError::invalid)?;
    intent
        .validate_observed_check_names(&testing.checks)
        .map_err(RecoveryError::invalid)?;
    let eval_config = load_recovery_eval_config(workspace, current)?;
    let expected_recovery = if prefix.attempt == 1 {
        None
    } else {
        current.latest_recovery.as_ref()
    };
    intent
        .validate_against_with_recovery(current, &eval_config.evals.required, expected_recovery)
        .map_err(RecoveryError::invalid)?;
    if intent.attempt() != prefix.attempt
        || testing.evaluation_attempt.unwrap_or(1) != prefix.attempt
        || testing
            .execution_intent
            .as_ref()
            .is_some_and(|value| value != &intent_reference)
    {
        return Err(RecoveryError::invalid(
            "staged evaluation adoption source attempt authority mismatch",
        ));
    }
    inventory
        .validate_selected_logs(prefix.attempt, &testing.checks)
        .map_err(RecoveryError::invalid)?;
    verify_evaluation_prefix_references(
        workspace,
        operator_guard,
        &testing,
        &intent_reference,
        &testing_reference,
    )?;
    if let Some(report_reference) = source.evaluation_prefix.eval_report.as_ref() {
        let bytes = read_verified_regular_file(
            workspace.run_directory(),
            &report_reference.path,
            "staged adoption EvalReport",
        )
        .map_err(RecoveryError::wrapped)?;
        if digest_bytes(&bytes) != report_reference.digest {
            return Err(RecoveryError::invalid(
                "staged adoption EvalReport digest mismatch",
            ));
        }
        let report: seaf_core::EvalReport =
            serde_json::from_slice(&bytes).map_err(RecoveryError::wrapped)?;
        if canonical_json_bytes(&report).map_err(RecoveryError::wrapped)? != bytes {
            return Err(RecoveryError::invalid(
                "staged adoption EvalReport is not canonical",
            ));
        }
        crate::approved_eval::validate_integrated_eval_report_binding(
            current,
            &testing,
            testing_reference,
            &report,
        )
        .map_err(RecoveryError::wrapped)?;
    }
    Ok(())
}

fn validate_staged_invalidation_source(
    workspace: &LoopWorkspace,
    current: &LoopRun,
    source_path: &str,
    source: &EvaluationInvalidationSourceRunV3,
    operator_guard: &crate::operator_evidence::OperatorEvidenceGuard,
) -> Result<(), RecoveryError> {
    if source.schema_version != EVALUATION_INVALIDATION_SCHEMA_VERSION
        || source.recovery_id == 0
        || source_path != recovery_source_path(source.recovery_id)
        || &source.run != current
        || parse_canonical_timestamp(&source.created_at).is_none()
    {
        return Err(RecoveryError::invalid(
            "staged evaluation invalidation source does not bind current authority",
        ));
    }
    validate_note("actor", &source.actor, 256)?;
    validate_note("reason", &source.reason, 1024)?;
    let run_errors = seaf_core::validate_loop_run(&source.run);
    let approved_errors = seaf_core::validate_loop_run(&source.approved_run);
    if !run_errors.is_empty()
        || !approved_errors.is_empty()
        || source.approved_run.status != LoopStatus::Approved
    {
        return Err(RecoveryError::invalid(
            "staged invalidation source contains invalid run authority",
        ));
    }
    validate_authoritative_provider_exchange_records(workspace, &source.run)
        .map_err(RecoveryError::wrapped)?;
    match source.run.status {
        LoopStatus::Approved
            if source.run == source.approved_run && source.prior_final.is_none() => {}
        LoopStatus::Failed if source.run.human_approval.is_some() => {
            let authority = crate::load_verified_final_evaluation_authority(workspace, &source.run)
                .map_err(RecoveryError::wrapped)?;
            let prior = source.prior_final.as_ref().ok_or_else(|| {
                RecoveryError::invalid("staged invalidation source lost prior final authority")
            })?;
            if authority.approved_run() != &source.approved_run
                || prior.testing_evidence
                    != final_step_reference(&source.run, LoopStepName::Testing)?
                || prior.eval_report != final_step_reference(&source.run, LoopStepName::EvalReport)?
            {
                return Err(RecoveryError::invalid(
                    "staged invalidation source prior final authority mismatch",
                ));
            }
        }
        _ => {
            return Err(RecoveryError::invalid(
                "staged invalidation source has unsupported run status",
            ))
        }
    }
    let inventory = EvaluationAttemptInventory::load_for_invalidation(workspace)
        .map_err(RecoveryError::invalid)?;
    if inventory.latest_attempt() != Some(source.evaluation_prefix.evaluation_attempt) {
        return Err(RecoveryError::invalid(
            "staged evaluation invalidation source does not select the latest factual attempt",
        ));
    }
    let prefix = inventory
        .invalidation_prefix_paths_for(source.evaluation_prefix.evaluation_attempt)
        .map_err(RecoveryError::invalid)?;
    let expected_spelling = if fixed_spelling(prefix.spelling) {
        EvaluationPrefixSpellingV1::FixedV1
    } else {
        EvaluationPrefixSpellingV1::IndexedV2
    };
    let screened_prefix =
        preflight_evaluation_prefix_paths(workspace, operator_guard, &prefix.paths)?;
    let references = screened_references(&screened_prefix);
    if source.evaluation_prefix.spelling != expected_spelling
        || source.evaluation_prefix.present_artifacts != references
    {
        return Err(RecoveryError::invalid(
            "staged invalidation source prefix was substituted",
        ));
    }
    let intent_reference = references
        .first()
        .ok_or_else(|| RecoveryError::invalid("staged invalidation source lost intent"))?;
    let intent = load_intent(workspace, intent_reference).map_err(RecoveryError::invalid)?;
    let source_digest = intent
        .source_worktree_state_digest()
        .unwrap_or_default()
        .to_string();
    let eval_config = load_recovery_eval_config(workspace, &source.approved_run)?;
    validate_invalidation_prefix(
        workspace,
        &source.approved_run,
        &prefix,
        &eval_config.evals.required,
        &source_digest,
        (source.run.status == LoopStatus::Failed).then_some(false),
        operator_guard,
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationInvalidationAttemptV3 {
    pub schema_version: u32,
    pub recovery_id: u32,
    pub run_id: String,
    pub action: EvaluationInvalidationAction,
    pub step: LoopStepName,
    pub actor: String,
    pub reason: String,
    pub created_at: String,
    pub source_run: ArtifactReference,
    pub source_run_digest: String,
    pub approved_run_digest: String,
    pub input_digests: LoopInputDigests,
    pub candidate_state_digest: String,
    pub candidate_head: String,
    pub candidate_tree: String,
    pub candidate_diff_digest: String,
    pub source_worktree_state_digest: String,
    pub invalidated_attempt: u32,
    pub next_evaluation_attempt: u32,
    pub present_artifacts: Vec<ArtifactReference>,
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

#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationAdoptionOutcome {
    pub run: LoopRun,
    pub recovery: EvaluationRecoveryAttemptV2,
    pub reference: RecoveryReference,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationInvalidationOutcome {
    pub run: LoopRun,
    pub recovery: EvaluationInvalidationAttemptV3,
    pub reference: RecoveryReference,
}

pub fn invalidate_approved_evaluation(
    workspace: &LoopWorkspace,
    actor: &str,
    reason: &str,
) -> Result<EvaluationInvalidationOutcome, RecoveryError> {
    validate_note("actor", actor, 256)?;
    validate_note("reason", reason, 1024)?;
    let candidate_lock = acquire_candidate_lock(workspace).map_err(RecoveryError::wrapped)?;
    let result = invalidate_approved_evaluation_locked(workspace, actor, reason);
    let unlock = candidate_lock.unlock();
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(RecoveryError::wrapped(error)),
        (Err(error), _) => Err(error),
    }
}

fn invalidate_approved_evaluation_locked(
    workspace: &LoopWorkspace,
    actor: &str,
    reason: &str,
) -> Result<EvaluationInvalidationOutcome, RecoveryError> {
    let source = state::load_run(workspace).map_err(RecoveryError::wrapped)?;
    let operator_guard = crate::operator_evidence::OperatorEvidenceGuard::load(workspace, &source)
        .map_err(RecoveryError::invalid)?;
    operator_guard
        .validate_current_run_file(workspace)
        .and_then(|()| operator_guard.validate_run(&source))
        .and_then(|()| operator_guard.validate_structural(actor))
        .map_err(RecoveryError::invalid)?;
    let reason = operator_guard
        .sanitize_reason(reason, 1024)
        .map_err(RecoveryError::invalid)?;
    validate_note("reason", &reason, 1024)?;
    ensure_no_promotion_intent(workspace)?;
    if let Some(retry) = exact_evaluation_invalidation_retry(workspace, &source, actor, &reason)? {
        operator_guard
            .validate_future_run(&retry.run)
            .map_err(RecoveryError::invalid)?;
        state::resync_exact_run(workspace, &retry.run).map_err(RecoveryError::wrapped)?;
        return Ok(retry);
    }
    let approved = match source.status {
        LoopStatus::Approved if source.current_step == LoopStepName::Testing => source.clone(),
        LoopStatus::Failed if source.human_approval.is_some() => {
            crate::load_verified_final_evaluation_authority(workspace, &source)
                .map_err(RecoveryError::wrapped)?
                .approved_run()
                .clone()
        }
        LoopStatus::EvalPassed | LoopStatus::Promoted => {
            return Err(RecoveryError::invalid(
                "EvalPassed and Promoted evaluation authority is immutable",
            ))
        }
        _ => {
            return Err(RecoveryError::invalid(
                "evaluation invalidation requires exact Approved Testing or approval-bound Failed authority",
            ))
        }
    };
    validate_evaluation_invalidation_source(workspace, &source, &approved)?;
    let candidate = approved.candidate_workspace.as_ref().ok_or_else(|| {
        RecoveryError::invalid("evaluation invalidation lost candidate authority")
    })?;
    let source_root = Path::new(&candidate.source_worktree_root);
    let verified = verify_candidate_patch_evidence_for_evaluation_locked(workspace, source_root)
        .map_err(RecoveryError::wrapped)?;
    let approval = approved.human_approval.as_ref().ok_or_else(|| {
        RecoveryError::invalid("evaluation invalidation lost human approval authority")
    })?;
    if verified.applied_diff != approval.candidate_diff
        || verified.policy_decision_digest != approval.policy_decision_digest
        || verified.candidate_authority.starting_head != approval.starting_head
    {
        return Err(RecoveryError::invalid(
            "physical candidate authority does not match exact human approval",
        ));
    }
    let source_authority =
        capture_source_worktree_authority(source_root, Some(workspace.run_directory()))
            .map_err(RecoveryError::wrapped)?;
    let source_authority_digest =
        canonical_sha256_digest(&source_authority).map_err(RecoveryError::wrapped)?;

    let inventory = EvaluationAttemptInventory::load_for_invalidation(workspace)
        .map_err(RecoveryError::invalid)?;
    let prefix = inventory
        .invalidation_prefix_paths()
        .map_err(RecoveryError::invalid)?;
    let eval_config = load_recovery_eval_config(workspace, &approved)?;
    let present_artifacts = validate_invalidation_prefix(
        workspace,
        &approved,
        &prefix,
        &eval_config.evals.required,
        &source_authority_digest,
        (source.status == LoopStatus::Failed).then_some(false),
        &operator_guard,
    )?;
    if source.status == LoopStatus::Approved && prefix.testing_present {
        return Err(RecoveryError::invalid(
            "complete Testing evidence must use zero-command adoption instead of invalidation",
        ));
    }
    let next_evaluation_attempt = prefix
        .attempt
        .checked_add(1)
        .ok_or_else(|| RecoveryError::invalid("evaluation attempt sequence is exhausted"))?;
    let recovery_id = next_invalidation_recovery_id(workspace, &source, &prefix)?;
    validate_recovery_namespace(
        workspace,
        source
            .latest_recovery
            .as_ref()
            .map_or(0, |reference| reference.recovery_id),
        recovery_id,
    )?;
    let source_path = recovery_source_path(recovery_id);
    let recovery_path = recovery_path(recovery_id);
    let created_at = existing_invalidation_or_new_timestamp(
        workspace,
        recovery_id,
        &source_path,
        &recovery_path,
        actor,
        &reason,
        &operator_guard,
    )?;
    let prior_final = if source.status == LoopStatus::Failed {
        Some(EvaluationInvalidationFinalAuthorityV1 {
            testing_evidence: final_step_reference(&source, LoopStepName::Testing)?,
            eval_report: final_step_reference(&source, LoopStepName::EvalReport)?,
        })
    } else {
        None
    };
    let source_snapshot = EvaluationInvalidationSourceRunV3 {
        schema_version: EVALUATION_INVALIDATION_SCHEMA_VERSION,
        recovery_id,
        actor: actor.to_string(),
        reason: reason.clone(),
        created_at: created_at.clone(),
        run: source.clone(),
        approved_run: approved.clone(),
        evaluation_prefix: EvaluationInvalidationPrefixAuthorityV1 {
            evaluation_attempt: prefix.attempt,
            spelling: if fixed_spelling(prefix.spelling) {
                EvaluationPrefixSpellingV1::FixedV1
            } else {
                EvaluationPrefixSpellingV1::IndexedV2
            },
            present_artifacts: present_artifacts.clone(),
        },
        prior_final,
    };
    let source_bytes = operator_guard
        .validate_canonical_artifact(&source_snapshot)
        .map_err(RecoveryError::invalid)?;
    let source_reference = ArtifactReference {
        path: source_path.clone(),
        digest: digest_bytes(&source_bytes),
    };
    let zero_reference = RecoveryReference {
        recovery_id,
        artifact: ArtifactReference {
            path: recovery_path.clone(),
            digest: "0".repeat(64),
        },
    };
    let mut zero_projection = build_evaluation_invalidation_reset(&approved, &created_at);
    zero_projection.latest_recovery = Some(zero_reference);
    let recovery = EvaluationInvalidationAttemptV3 {
        schema_version: EVALUATION_INVALIDATION_SCHEMA_VERSION,
        recovery_id,
        run_id: source.run_id.clone(),
        action: EvaluationInvalidationAction::InvalidateApprovedEvaluation,
        step: LoopStepName::Testing,
        actor: actor.to_string(),
        reason: reason.clone(),
        created_at: created_at.clone(),
        source_run: source_reference,
        source_run_digest: canonical_sha256_digest(&source).map_err(RecoveryError::wrapped)?,
        approved_run_digest: canonical_sha256_digest(&approved).map_err(RecoveryError::wrapped)?,
        input_digests: source.input_digests.clone(),
        candidate_state_digest: canonical_sha256_digest(candidate)
            .map_err(RecoveryError::wrapped)?,
        candidate_head: candidate.candidate_head.clone(),
        candidate_tree: candidate.candidate_tree.clone(),
        candidate_diff_digest: candidate.candidate_diff_digest.clone(),
        source_worktree_state_digest: source_authority_digest,
        invalidated_attempt: prefix.attempt,
        next_evaluation_attempt,
        present_artifacts,
        previous_recovery: source.latest_recovery.clone(),
        previous_provider_head: source.provider_exchange_records.last().cloned(),
        expected_reset_projection_digest: canonical_sha256_digest(&zero_projection)
            .map_err(RecoveryError::wrapped)?,
    };
    validate_evaluation_invalidation_contract(&recovery)?;
    let recovery_bytes = operator_guard
        .validate_canonical_artifact(&recovery)
        .map_err(RecoveryError::invalid)?;
    let reference = RecoveryReference {
        recovery_id,
        artifact: ArtifactReference {
            path: recovery_path.clone(),
            digest: digest_bytes(&recovery_bytes),
        },
    };
    let mut intended = zero_projection;
    intended.latest_recovery = Some(reference.clone());
    operator_guard
        .validate_future_run(&intended)
        .map_err(RecoveryError::invalid)?;
    preflight_exact_artifact(
        workspace,
        &source_path,
        &source_bytes,
        true,
        Some(&operator_guard),
    )?;
    preflight_exact_artifact(
        workspace,
        &recovery_path,
        &recovery_bytes,
        true,
        Some(&operator_guard),
    )?;
    let publication_guard =
        crate::operator_evidence::OperatorEvidenceGuard::load(workspace, &source)
            .map_err(RecoveryError::invalid)?;
    publication_guard
        .validate_current_run_file(workspace)
        .and_then(|()| publication_guard.validate_exact_raw_bytes(&source_bytes))
        .and_then(|()| publication_guard.validate_exact_raw_bytes(&recovery_bytes))
        .and_then(|()| publication_guard.validate_future_run(&intended).map(drop))
        .map_err(RecoveryError::invalid)?;
    publish_invalidation_source_activating_if_needed(
        workspace,
        &source,
        recovery_id,
        &source_path,
        &source_bytes,
    )
    .map_err(RecoveryError::wrapped)?;
    publish_create_only_consuming_evaluation_slot(
        workspace.run_directory(),
        &recovery_path,
        &recovery_bytes,
    )
    .map_err(RecoveryError::wrapped)?;

    reauthenticate_evaluation_invalidation(
        workspace,
        &source,
        &approved,
        &intended,
        &reference,
        source_root,
        &source_authority,
        &verified,
    )?;
    persist_evaluation_invalidation_with_validator(workspace, &source, &intended, |locked| {
        reauthenticate_evaluation_invalidation(
            workspace,
            locked,
            &approved,
            &intended,
            &reference,
            source_root,
            &source_authority,
            &verified,
        )
        .map_err(|error| {
            crate::provider_exchange::ProviderExchangeError::Invalid(error.to_string())
        })
    })
    .map_err(RecoveryError::wrapped)?;
    Ok(EvaluationInvalidationOutcome {
        run: intended,
        recovery,
        reference,
    })
}

pub fn adopt_approved_evaluation(
    workspace: &LoopWorkspace,
    actor: &str,
    reason: &str,
) -> Result<EvaluationAdoptionOutcome, RecoveryError> {
    validate_note("actor", actor, 256)?;
    validate_note("reason", reason, 1024)?;
    let candidate_lock = acquire_candidate_lock(workspace).map_err(RecoveryError::wrapped)?;
    let result = adopt_approved_evaluation_locked(workspace, actor, reason);
    let unlock = candidate_lock.unlock();
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(RecoveryError::wrapped(error)),
        (Err(error), _) => Err(error),
    }
}

fn adopt_approved_evaluation_locked(
    workspace: &LoopWorkspace,
    actor: &str,
    reason: &str,
) -> Result<EvaluationAdoptionOutcome, RecoveryError> {
    let approved = state::load_run(workspace).map_err(RecoveryError::wrapped)?;
    let operator_guard =
        crate::operator_evidence::OperatorEvidenceGuard::load(workspace, &approved)
            .map_err(RecoveryError::invalid)?;
    operator_guard
        .validate_current_run_file(workspace)
        .and_then(|()| operator_guard.validate_run(&approved))
        .and_then(|()| operator_guard.validate_structural(actor))
        .map_err(RecoveryError::invalid)?;
    let reason = operator_guard
        .sanitize_reason(reason, 1024)
        .map_err(RecoveryError::invalid)?;
    validate_note("reason", &reason, 1024)?;
    if is_final_evaluation_status(&approved) {
        return exact_evaluation_adoption_retry(workspace, approved, actor, &reason);
    }
    validate_evaluation_adoption_source(workspace, &approved)?;

    let candidate = approved
        .candidate_workspace
        .as_ref()
        .expect("adoption source validation checked candidate");
    let source_root = Path::new(&candidate.source_worktree_root);
    let verified = verify_candidate_patch_evidence_for_evaluation_locked(workspace, source_root)
        .map_err(RecoveryError::wrapped)?;
    let approval = approved
        .human_approval
        .as_ref()
        .expect("adoption source validation checked approval");
    if verified.applied_diff != approval.candidate_diff
        || verified.policy_decision_digest != approval.policy_decision_digest
        || verified.candidate_authority.starting_head != approval.starting_head
    {
        return Err(RecoveryError::invalid(
            "physical candidate authority does not match exact human approval",
        ));
    }
    let source_authority =
        capture_source_worktree_authority(source_root, Some(workspace.run_directory()))
            .map_err(RecoveryError::wrapped)?;
    let source_authority_digest =
        canonical_sha256_digest(&source_authority).map_err(RecoveryError::wrapped)?;

    let inventory = EvaluationAttemptInventory::load_for_invalidation(workspace)
        .map_err(RecoveryError::invalid)?;
    let prefix = inventory
        .recovery_prefix_paths()
        .map_err(RecoveryError::invalid)?;
    let screened_prefix = preflight_adoption_evaluation_prefix(
        workspace,
        &operator_guard,
        &prefix.intent,
        &prefix.testing,
        prefix.report_present.then_some(prefix.report.as_str()),
    )?;
    let intent_reference = screened_reference(&screened_prefix, &prefix.intent)?;
    let testing_reference = screened_reference(&screened_prefix, &prefix.testing)?;
    let testing = TestingEvidence::load_for_approved_run(workspace, &testing_reference, &approved)
        .map_err(RecoveryError::wrapped)?;
    let intent = load_intent(workspace, &intent_reference).map_err(RecoveryError::invalid)?;
    intent
        .validate_observed_check_names(&testing.checks)
        .map_err(RecoveryError::invalid)?;
    let eval_config = load_recovery_eval_config(workspace, &approved)?;
    let expected_attempt_recovery = if prefix.attempt == 1 {
        None
    } else {
        approved.latest_recovery.as_ref()
    };
    intent
        .validate_against_with_recovery(
            &approved,
            &eval_config.evals.required,
            expected_attempt_recovery,
        )
        .map_err(RecoveryError::invalid)?;
    if intent.attempt() != prefix.attempt
        || testing.evaluation_attempt.unwrap_or(1) != prefix.attempt
        || (testing.schema_version == 1) != fixed_spelling(prefix.spelling)
        || intent.recovery() != expected_attempt_recovery
        || testing
            .recovery
            .as_ref()
            .and_then(|recovery| recovery.as_ref())
            != expected_attempt_recovery
    {
        return Err(RecoveryError::invalid(
            "adoption prefix schema, attempt, or recovery authority is invalid",
        ));
    }
    match (&intent, testing.schema_version) {
        (ApprovedEvaluationIntent::V1(_), 1)
            if testing.evaluation_attempt.is_none()
                && testing.execution_intent.is_none()
                && testing.recovery.is_none() => {}
        (ApprovedEvaluationIntent::V2(_) | ApprovedEvaluationIntent::V3(_), 2)
            if testing.evaluation_attempt == Some(prefix.attempt)
                && testing.execution_intent.as_ref() == Some(&intent_reference)
                && testing
                    .recovery
                    .as_ref()
                    .is_some_and(|recovery| recovery.as_ref() == expected_attempt_recovery) => {}
        _ => {
            return Err(RecoveryError::invalid(
                "adoption prefix spelling and evidence schema disagree",
            ))
        }
    }
    if let Some(source_digest) = intent.source_worktree_state_digest() {
        if source_digest != source_authority_digest {
            return Err(RecoveryError::invalid(
                "adoption source worktree authority changed after command execution",
            ));
        }
    }
    inventory
        .validate_selected_logs(prefix.attempt, &testing.checks)
        .map_err(RecoveryError::invalid)?;
    verify_evaluation_prefix_references(
        workspace,
        &operator_guard,
        &testing,
        &intent_reference,
        &testing_reference,
    )?;

    let report = crate::approved_eval::build_integrated_eval_report(
        &approved,
        &testing,
        testing_reference.clone(),
    )
    .map_err(RecoveryError::wrapped)?;
    let report_bytes = canonical_json_bytes(&report).map_err(RecoveryError::wrapped)?;
    operator_guard
        .validate_exact_raw_bytes(&report_bytes)
        .map_err(RecoveryError::invalid)?;
    if prefix.report_present {
        preflight_exact_artifact(
            workspace,
            &prefix.report,
            &report_bytes,
            false,
            Some(&operator_guard),
        )?;
    }
    let report_reference = ArtifactReference {
        path: prefix.report.clone(),
        digest: digest_bytes(&report_bytes),
    };

    let recovery_id = next_evaluation_recovery_id(workspace, &approved)?;
    let source_path = recovery_source_path(recovery_id);
    let recovery_path = recovery_path(recovery_id);
    validate_recovery_namespace(
        workspace,
        approved
            .latest_recovery
            .as_ref()
            .map_or(0, |reference| reference.recovery_id),
        recovery_id,
    )?;
    let orphan = load_evaluation_adoption_orphan(
        workspace,
        recovery_id,
        &source_path,
        &recovery_path,
        actor,
        &reason,
        &operator_guard,
    )?;
    let created_at = orphan
        .as_ref()
        .map(|orphan| orphan.created_at.clone())
        .unwrap_or_else(now_timestamp);
    let report_disposition = orphan
        .as_ref()
        .map(|orphan| orphan.report_disposition)
        .unwrap_or(if prefix.report_present {
            EvaluationRecoveryReportDisposition::VerifyExisting
        } else {
            EvaluationRecoveryReportDisposition::CreateMissing
        });
    if report_disposition == EvaluationRecoveryReportDisposition::VerifyExisting
        && !prefix.report_present
    {
        return Err(RecoveryError::invalid(
            "VerifyExisting adoption recovery lost its exact EvalReport",
        ));
    }

    let source_snapshot = EvaluationRecoverySourceRunV2 {
        schema_version: EVALUATION_RECOVERY_SCHEMA_VERSION,
        recovery_id,
        actor: actor.to_string(),
        reason: reason.clone(),
        created_at: created_at.clone(),
        run: approved.clone(),
        evaluation_prefix: EvaluationPrefixAuthorityV1 {
            evaluation_attempt: prefix.attempt,
            spelling: if fixed_spelling(prefix.spelling) {
                EvaluationPrefixSpellingV1::FixedV1
            } else {
                EvaluationPrefixSpellingV1::IndexedV2
            },
            execution_intent: intent_reference.clone(),
            testing_evidence: testing_reference.clone(),
            eval_report: (report_disposition
                == EvaluationRecoveryReportDisposition::VerifyExisting)
                .then(|| report_reference.clone()),
        },
    };
    let source_bytes = operator_guard
        .validate_canonical_artifact(&source_snapshot)
        .map_err(RecoveryError::invalid)?;
    let source_reference = ArtifactReference {
        path: source_path.clone(),
        digest: digest_bytes(&source_bytes),
    };
    let zero_reference = RecoveryReference {
        recovery_id,
        artifact: ArtifactReference {
            path: recovery_path.clone(),
            digest: "0".repeat(64),
        },
    };
    let mut zero_projection = build_adopted_final(
        &approved,
        &testing,
        &testing_reference,
        &report_reference,
        &created_at,
    )?;
    zero_projection.latest_recovery = Some(zero_reference);
    let recovery = EvaluationRecoveryAttemptV2 {
        schema_version: EVALUATION_RECOVERY_SCHEMA_VERSION,
        recovery_id,
        run_id: approved.run_id.clone(),
        action: EvaluationRecoveryAction::AdoptApprovedEvaluation,
        step: LoopStepName::Testing,
        actor: actor.to_string(),
        reason: reason.clone(),
        created_at: created_at.clone(),
        source_run: source_reference,
        source_run_digest: canonical_sha256_digest(&approved).map_err(RecoveryError::wrapped)?,
        input_digests: approved.input_digests.clone(),
        candidate_state_digest: canonical_sha256_digest(candidate)
            .map_err(RecoveryError::wrapped)?,
        candidate_head: candidate.candidate_head.clone(),
        candidate_tree: candidate.candidate_tree.clone(),
        candidate_diff_digest: candidate.candidate_diff_digest.clone(),
        source_worktree_state_digest: source_authority_digest,
        evaluation_attempt: prefix.attempt,
        execution_intent: intent_reference,
        testing_evidence: testing_reference.clone(),
        eval_report: report_reference.clone(),
        report_disposition,
        previous_recovery: approved.latest_recovery.clone(),
        previous_provider_head: approved.provider_exchange_records.last().cloned(),
        expected_final_projection_digest: canonical_sha256_digest(&zero_projection)
            .map_err(RecoveryError::wrapped)?,
    };
    let recovery_bytes = operator_guard
        .validate_canonical_artifact(&recovery)
        .map_err(RecoveryError::invalid)?;
    let reference = RecoveryReference {
        recovery_id,
        artifact: ArtifactReference {
            path: recovery_path.clone(),
            digest: digest_bytes(&recovery_bytes),
        },
    };
    let mut final_run = zero_projection;
    final_run.latest_recovery = Some(reference.clone());
    operator_guard
        .validate_future_run(&final_run)
        .map_err(RecoveryError::invalid)?;

    // Every possible collision is inspected before the first create-only publication.
    preflight_exact_artifact(
        workspace,
        &source_path,
        &source_bytes,
        true,
        Some(&operator_guard),
    )?;
    preflight_exact_artifact(
        workspace,
        &recovery_path,
        &recovery_bytes,
        true,
        Some(&operator_guard),
    )?;
    preflight_exact_artifact(
        workspace,
        &prefix.report,
        &report_bytes,
        report_disposition == EvaluationRecoveryReportDisposition::CreateMissing,
        Some(&operator_guard),
    )?;

    let publication_guard =
        crate::operator_evidence::OperatorEvidenceGuard::load(workspace, &approved)
            .map_err(RecoveryError::invalid)?;
    publication_guard
        .validate_current_run_file(workspace)
        .and_then(|()| publication_guard.validate_exact_raw_bytes(&report_bytes))
        .and_then(|()| publication_guard.validate_exact_raw_bytes(&source_bytes))
        .and_then(|()| publication_guard.validate_exact_raw_bytes(&recovery_bytes))
        .and_then(|()| publication_guard.validate_future_run(&final_run).map(drop))
        .map_err(RecoveryError::invalid)?;
    preflight_exact_artifact(
        workspace,
        &prefix.report,
        &report_bytes,
        report_disposition == EvaluationRecoveryReportDisposition::CreateMissing,
        Some(&publication_guard),
    )?;

    publish_create_only_consuming_evaluation_slot(
        workspace.run_directory(),
        &source_path,
        &source_bytes,
    )
    .map_err(RecoveryError::wrapped)?;
    publish_create_only_consuming_evaluation_slot(
        workspace.run_directory(),
        &recovery_path,
        &recovery_bytes,
    )
    .map_err(RecoveryError::wrapped)?;
    if report_disposition == EvaluationRecoveryReportDisposition::CreateMissing {
        publish_create_only_consuming_evaluation_slot(
            workspace.run_directory(),
            &prefix.report,
            &report_bytes,
        )
        .map_err(RecoveryError::wrapped)?;
    }

    reauthenticate_evaluation_adoption(
        workspace,
        &approved,
        &final_run,
        &reference,
        source_root,
        &source_authority,
        &verified,
        &prefix.report,
        &report_bytes,
    )?;
    persist_evaluation_adoption_with_validator(workspace, &approved, &final_run, |locked| {
        reauthenticate_evaluation_adoption(
            workspace,
            locked,
            &final_run,
            &reference,
            source_root,
            &source_authority,
            &verified,
            &prefix.report,
            &report_bytes,
        )
        .map_err(|error| {
            crate::provider_exchange::ProviderExchangeError::Invalid(error.to_string())
        })
    })
    .map_err(RecoveryError::wrapped)?;
    Ok(EvaluationAdoptionOutcome {
        run: final_run,
        recovery,
        reference,
    })
}

#[derive(Debug)]
struct EvaluationAdoptionOrphan {
    created_at: String,
    report_disposition: EvaluationRecoveryReportDisposition,
}

fn is_final_evaluation_status(run: &LoopRun) -> bool {
    run.status == LoopStatus::EvalPassed
        || (run.status == LoopStatus::Failed && run.human_approval.is_some())
}

fn exact_evaluation_adoption_retry(
    workspace: &LoopWorkspace,
    run: LoopRun,
    actor: &str,
    reason: &str,
) -> Result<EvaluationAdoptionOutcome, RecoveryError> {
    validate_operator_run_envelope(workspace, &run)?;
    ensure_no_promotion_intent(workspace)?;
    let reference = run
        .latest_recovery
        .clone()
        .ok_or_else(|| RecoveryError::invalid("fresh terminal evaluation adoption is forbidden"))?;
    let (recovery, source) = load_verified_evaluation_recovery(workspace, &reference)?
        .ok_or_else(|| RecoveryError::invalid("terminal evaluation was not adopted"))?;
    if recovery.actor != actor
        || recovery.reason != reason
        || recovery.action != EvaluationRecoveryAction::AdoptApprovedEvaluation
    {
        return Err(RecoveryError::invalid(
            "evaluation adoption retry does not match its exact action, actor, and reason",
        ));
    }
    let authority = crate::load_verified_final_evaluation_authority(workspace, &run)
        .map_err(RecoveryError::wrapped)?;
    if authority.approved_run() != &source.run {
        return Err(RecoveryError::invalid(
            "evaluation adoption retry changed adopted final authority",
        ));
    }
    let mut expected = build_adopted_final(
        &source.run,
        authority.testing_evidence(),
        &recovery.testing_evidence,
        &recovery.eval_report,
        &recovery.created_at,
    )?;
    expected.latest_recovery = Some(reference.clone());
    if run != expected {
        return Err(RecoveryError::invalid(
            "evaluation adoption retry is not the exact adopted final authority",
        ));
    }
    validate_authoritative_provider_exchange_records(workspace, &run)
        .map_err(RecoveryError::wrapped)?;
    crate::runner::load_verified_authoritative_run_inputs(workspace, &source.run)
        .map_err(RecoveryError::wrapped)?;
    let reconciled = preflight_provider_exchange_reconciliation(workspace, &run)
        .map_err(RecoveryError::wrapped)?;
    if reconciled != run {
        return Err(RecoveryError::invalid(
            "evaluation adoption retry has an unpublished provider exchange winner",
        ));
    }
    let candidate = run.candidate_workspace.as_ref().ok_or_else(|| {
        RecoveryError::invalid("adopted final authority lost its active candidate")
    })?;
    if candidate.lifecycle != CandidateWorkspaceLifecycle::Active {
        return Err(RecoveryError::invalid(
            "exact evaluation adoption retry requires the original active final authority",
        ));
    }
    verify_candidate_patch_evidence_for_evaluation_locked(
        workspace,
        Path::new(&candidate.source_worktree_root),
    )
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
            "source worktree authority changed after evaluation adoption",
        ));
    }
    state::resync_exact_run(workspace, &run).map_err(RecoveryError::wrapped)?;
    Ok(EvaluationAdoptionOutcome {
        run,
        recovery,
        reference,
    })
}

fn validate_evaluation_adoption_source(
    workspace: &LoopWorkspace,
    source: &LoopRun,
) -> Result<(), RecoveryError> {
    if source.status != LoopStatus::Approved
        || source.current_step != LoopStepName::Testing
        || source.execution_mode != LoopExecutionMode::IsolatedCandidate
        || source.human_approval.is_none()
        || source.promotion.is_some()
    {
        return Err(RecoveryError::invalid(
            "evaluation adoption requires exact Approved Testing authority",
        ));
    }
    ensure_no_promotion_intent(workspace)?;
    let candidate = source.candidate_workspace.as_ref().ok_or_else(|| {
        RecoveryError::invalid("evaluation adoption source lost candidate authority")
    })?;
    if candidate.schema_version != CANDIDATE_WORKSPACE_SCHEMA_VERSION
        || candidate.lifecycle != CandidateWorkspaceLifecycle::Active
        || candidate.cleanup_started_at.is_some()
        || candidate.cleaned_at.is_some()
    {
        return Err(RecoveryError::invalid(
            "evaluation adoption requires active candidate authority",
        ));
    }
    crate::provider_exchange::validate_run_for_atomic_publication(workspace, source)
        .map_err(RecoveryError::wrapped)?;
    crate::runner::load_verified_authoritative_run_inputs(workspace, source)
        .map_err(RecoveryError::wrapped)?;
    let reconciled = preflight_provider_exchange_reconciliation(workspace, source)
        .map_err(RecoveryError::wrapped)?;
    if &reconciled != source {
        return Err(RecoveryError::invalid(
            "evaluation adoption source has an unpublished provider exchange winner",
        ));
    }
    validate_evaluation_source_prior_consumption(workspace, source)
}

fn validate_evaluation_invalidation_source(
    workspace: &LoopWorkspace,
    source: &LoopRun,
    approved: &LoopRun,
) -> Result<(), RecoveryError> {
    if approved.status != LoopStatus::Approved
        || approved.current_step != LoopStepName::Testing
        || approved.execution_mode != LoopExecutionMode::IsolatedCandidate
        || approved.human_approval.is_none()
        || approved.promotion.is_some()
    {
        return Err(RecoveryError::invalid(
            "evaluation invalidation lost exact Approved Testing authority",
        ));
    }
    let candidate = approved.candidate_workspace.as_ref().ok_or_else(|| {
        RecoveryError::invalid("evaluation invalidation source lost candidate authority")
    })?;
    if source.candidate_workspace.as_ref() != Some(candidate) {
        return Err(RecoveryError::invalid(
            "observed invalidation source changed candidate authority",
        ));
    }
    if candidate.schema_version != CANDIDATE_WORKSPACE_SCHEMA_VERSION
        || candidate.lifecycle != CandidateWorkspaceLifecycle::Active
        || candidate.cleanup_started_at.is_some()
        || candidate.cleaned_at.is_some()
    {
        return Err(RecoveryError::invalid(
            "evaluation invalidation requires active candidate authority",
        ));
    }
    crate::provider_exchange::validate_run_for_atomic_publication(workspace, source)
        .map_err(RecoveryError::wrapped)?;
    crate::runner::load_verified_authoritative_run_inputs(workspace, approved)
        .map_err(RecoveryError::wrapped)?;
    let reconciled = preflight_provider_exchange_reconciliation(workspace, source)
        .map_err(RecoveryError::wrapped)?;
    if &reconciled != source {
        return Err(RecoveryError::invalid(
            "evaluation invalidation source has an unpublished provider exchange winner",
        ));
    }
    let verified = verify_candidate_patch_evidence_for_evaluation_locked(
        workspace,
        Path::new(&candidate.source_worktree_root),
    )
    .map_err(RecoveryError::wrapped)?;
    let approval = approved
        .human_approval
        .as_ref()
        .ok_or_else(|| RecoveryError::invalid("evaluation rerun lost human approval authority"))?;
    if verified.applied_diff != approval.candidate_diff
        || verified.policy_decision_digest != approval.policy_decision_digest
        || verified.candidate_authority.starting_head != approval.starting_head
    {
        return Err(RecoveryError::invalid(
            "physical candidate authority changed after evaluation invalidation",
        ));
    }
    Ok(())
}

pub(crate) fn validate_final_evaluation_invalidation_retry(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    recovery_id: u32,
) -> Result<RecoveryReference, RecoveryError> {
    validate_operator_run_envelope(workspace, run)?;
    ensure_no_promotion_intent(workspace)?;
    let reference = run
        .latest_recovery
        .as_ref()
        .ok_or_else(|| RecoveryError::invalid("final evaluation lost recovery authority"))?;
    if reference.recovery_id != recovery_id {
        return Err(RecoveryError::invalid(
            "final evaluation consumed another recovery ID",
        ));
    }
    let (recovery, _) =
        load_verified_evaluation_invalidation(workspace, reference)?.ok_or_else(|| {
            RecoveryError::invalid("final evaluation recovery is not invalidation V3")
        })?;
    validate_invalidation_operational(workspace, run, &recovery)?;
    Ok(reference.clone())
}

struct ScreenedEvaluationPrefixArtifact {
    path: String,
    bytes: Vec<u8>,
}

fn preflight_evaluation_prefix_paths(
    workspace: &LoopWorkspace,
    operator_guard: &crate::operator_evidence::OperatorEvidenceGuard,
    paths: &[String],
) -> Result<Vec<ScreenedEvaluationPrefixArtifact>, RecoveryError> {
    paths
        .iter()
        .map(|path| {
            let bytes = read_verified_regular_file(
                workspace.run_directory(),
                path,
                "evaluation recovery prefix artifact",
            )
            .map_err(RecoveryError::wrapped)?;
            operator_guard
                .validate_exact_raw_bytes(&bytes)
                .map_err(RecoveryError::invalid)?;
            Ok(ScreenedEvaluationPrefixArtifact {
                path: path.clone(),
                bytes,
            })
        })
        .collect()
}

fn preflight_adoption_evaluation_prefix(
    workspace: &LoopWorkspace,
    operator_guard: &crate::operator_evidence::OperatorEvidenceGuard,
    intent_path: &str,
    testing_path: &str,
    report_path: Option<&str>,
) -> Result<Vec<ScreenedEvaluationPrefixArtifact>, RecoveryError> {
    let mut structural_paths = vec![intent_path.to_string(), testing_path.to_string()];
    if let Some(report_path) = report_path {
        structural_paths.push(report_path.to_string());
    }
    let mut screened =
        preflight_evaluation_prefix_paths(workspace, operator_guard, &structural_paths)?;
    let testing_bytes = screened
        .iter()
        .find(|artifact| artifact.path == testing_path)
        .map(|artifact| artifact.bytes.as_slice())
        .ok_or_else(|| RecoveryError::invalid("evaluation adoption prefix lost Testing bytes"))?;
    let testing: TestingEvidence =
        serde_json::from_slice(testing_bytes).map_err(RecoveryError::wrapped)?;
    let mut log_paths = Vec::with_capacity(testing.checks.len().saturating_mul(2));
    for check in &testing.checks {
        log_paths.push(
            check
                .stdout_path
                .clone()
                .ok_or_else(|| RecoveryError::invalid("adoption prefix lost check log path"))?,
        );
        log_paths.push(
            check
                .stderr_path
                .clone()
                .ok_or_else(|| RecoveryError::invalid("adoption prefix lost check log path"))?,
        );
    }
    screened.extend(preflight_evaluation_prefix_paths(
        workspace,
        operator_guard,
        &log_paths,
    )?);
    Ok(screened)
}

fn screened_reference(
    screened: &[ScreenedEvaluationPrefixArtifact],
    path: &str,
) -> Result<ArtifactReference, RecoveryError> {
    let artifact = screened
        .iter()
        .find(|artifact| artifact.path == path)
        .ok_or_else(|| RecoveryError::invalid("evaluation recovery prefix lost artifact bytes"))?;
    Ok(ArtifactReference {
        path: artifact.path.clone(),
        digest: digest_bytes(&artifact.bytes),
    })
}

fn screened_references(screened: &[ScreenedEvaluationPrefixArtifact]) -> Vec<ArtifactReference> {
    screened
        .iter()
        .map(|artifact| ArtifactReference {
            path: artifact.path.clone(),
            digest: digest_bytes(&artifact.bytes),
        })
        .collect()
}

fn validate_invalidation_prefix(
    workspace: &LoopWorkspace,
    approved: &LoopRun,
    prefix: &EvaluationInvalidationPrefixPaths,
    planned_checks: &[seaf_core::EvalCommandConfig],
    source_worktree_state_digest: &str,
    expected_passed: Option<bool>,
    operator_guard: &crate::operator_evidence::OperatorEvidenceGuard,
) -> Result<Vec<ArtifactReference>, RecoveryError> {
    if prefix.complete_log_pairs as usize > planned_checks.len()
        || (prefix.trailing_stdout && prefix.complete_log_pairs as usize >= planned_checks.len())
        || (prefix.testing_present && prefix.complete_log_pairs as usize != planned_checks.len())
        || (prefix.report_present && !prefix.testing_present)
    {
        return Err(RecoveryError::invalid(
            "evaluation invalidation prefix does not match the planned command sequence",
        ));
    }
    let screened_prefix =
        preflight_evaluation_prefix_paths(workspace, operator_guard, &prefix.paths)?;
    let references = screened_references(&screened_prefix);
    let intent_reference = references
        .first()
        .ok_or_else(|| RecoveryError::invalid("evaluation invalidation prefix lost intent"))?;
    let intent = load_intent(workspace, intent_reference).map_err(RecoveryError::invalid)?;
    let expected_recovery = if prefix.attempt == 1 {
        None
    } else {
        approved.latest_recovery.as_ref()
    };
    intent
        .validate_against_with_recovery(approved, planned_checks, expected_recovery)
        .map_err(RecoveryError::invalid)?;
    if intent.attempt() != prefix.attempt {
        return Err(RecoveryError::invalid(
            "evaluation invalidation intent selects another attempt",
        ));
    }
    if let Some(source_digest) = intent.source_worktree_state_digest() {
        if source_digest != source_worktree_state_digest {
            return Err(RecoveryError::invalid(
                "evaluation invalidation source authority changed after command execution",
            ));
        }
    }
    if prefix.attempt > 1 && expected_recovery.is_none() {
        return Err(RecoveryError::invalid(
            "recovered evaluation attempt lost invalidation authority",
        ));
    }
    if prefix.testing_present {
        let paths = if fixed_spelling(prefix.spelling) {
            None
        } else {
            Some(
                crate::evaluation_attempt::EvaluationAttemptPaths::indexed(prefix.attempt)
                    .map_err(RecoveryError::invalid)?,
            )
        };
        let testing_path = paths
            .as_ref()
            .map_or("artifacts/07-testing.json", |paths| paths.testing.as_str());
        let testing_reference = references
            .iter()
            .find(|reference| reference.path == testing_path)
            .ok_or_else(|| RecoveryError::invalid("evaluation prefix lost Testing reference"))?;
        let testing =
            TestingEvidence::load_for_approved_run(workspace, testing_reference, approved)
                .map_err(RecoveryError::wrapped)?;
        intent
            .validate_observed_check_names(&testing.checks)
            .map_err(RecoveryError::invalid)?;
        if testing.evaluation_attempt.unwrap_or(1) != prefix.attempt
            || testing
                .recovery
                .as_ref()
                .and_then(|recovery| recovery.as_ref())
                != expected_recovery
            || testing
                .execution_intent
                .as_ref()
                .is_some_and(|reference| reference != intent_reference)
            || testing.checks.len() != planned_checks.len()
            || expected_passed.is_some_and(|passed| testing.passed != passed)
        {
            return Err(RecoveryError::invalid(
                "evaluation invalidation Testing authority mismatch",
            ));
        }
        let inventory = EvaluationAttemptInventory::load_for_invalidation(workspace)
            .map_err(RecoveryError::invalid)?;
        inventory
            .validate_selected_logs(prefix.attempt, &testing.checks)
            .map_err(RecoveryError::invalid)?;
        if prefix.report_present {
            let report_path = paths
                .as_ref()
                .map_or("artifacts/08-eval-report.json", |paths| {
                    paths.report.as_str()
                });
            let report_reference = references
                .iter()
                .find(|reference| reference.path == report_path)
                .ok_or_else(|| {
                    RecoveryError::invalid("evaluation prefix lost EvalReport reference")
                })?;
            let expected_report = crate::approved_eval::build_integrated_eval_report(
                approved,
                &testing,
                testing_reference.clone(),
            )
            .map_err(RecoveryError::wrapped)?;
            if canonical_sha256_digest(&expected_report).map_err(RecoveryError::wrapped)?
                != report_reference.digest
            {
                return Err(RecoveryError::invalid(
                    "evaluation invalidation EvalReport authority mismatch",
                ));
            }
        }
    }
    Ok(references)
}

fn build_evaluation_invalidation_reset(approved: &LoopRun, created_at: &str) -> LoopRun {
    let mut reset = approved.clone();
    reset.status = LoopStatus::Approved;
    reset.current_step = LoopStepName::Testing;
    reset.updated_at = created_at.to_string();
    reset.eval_report_path = None;
    reset.promotion = None;
    for name in [LoopStepName::Testing, LoopStepName::EvalReport] {
        if let Some(step) = reset.steps.iter_mut().find(|step| step.name == name) {
            step.status = LoopStepStatus::Pending;
            step.artifact_path = None;
            step.artifact_digest = None;
        }
    }
    reset
}

fn ensure_no_promotion_intent(workspace: &LoopWorkspace) -> Result<(), RecoveryError> {
    match fs::symlink_metadata(
        workspace
            .run_directory()
            .join("artifacts/09-promotion.intent.json"),
    ) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(RecoveryError::invalid(
            "evaluation adoption is forbidden after promotion intent publication",
        )),
        Err(error) => Err(RecoveryError::wrapped(error)),
    }
}

fn next_evaluation_recovery_id(
    workspace: &LoopWorkspace,
    source: &LoopRun,
) -> Result<u32, RecoveryError> {
    source.latest_recovery.as_ref().map_or(Ok(1), |reference| {
        let consumed = match load_verified_any_recovery_entry(workspace, reference)? {
            VerifiedRecoveryLineage::Provider { .. } => {
                recovery_is_consumed(workspace, source, reference)?
            }
            VerifiedRecoveryLineage::Invalidation { .. } => {
                invalidation_is_consumed(workspace, source, reference)?
            }
            VerifiedRecoveryLineage::Evaluation { .. } => false,
        };
        if !consumed {
            return Err(RecoveryError::invalid(
                "evaluation adoption source carries an unconsumed prior recovery",
            ));
        }
        reference
            .recovery_id
            .checked_add(1)
            .ok_or_else(|| RecoveryError::invalid("recovery ID sequence is exhausted"))
    })
}

fn next_invalidation_recovery_id(
    workspace: &LoopWorkspace,
    source: &LoopRun,
    prefix: &EvaluationInvalidationPrefixPaths,
) -> Result<u32, RecoveryError> {
    let Some(previous) = source.latest_recovery.as_ref() else {
        return Ok(1);
    };
    let consumed = match load_verified_any_recovery_entry(workspace, previous)? {
        VerifiedRecoveryLineage::Provider { .. } => {
            recovery_is_consumed(workspace, source, previous)?
        }
        VerifiedRecoveryLineage::Evaluation { recovery, .. } => {
            source.status == LoopStatus::Failed
                && recovery.action == EvaluationRecoveryAction::AdoptApprovedEvaluation
        }
        VerifiedRecoveryLineage::Invalidation { recovery, .. } => {
            prefix.attempt == recovery.next_evaluation_attempt
                && invalidation_is_consumed(workspace, source, previous)?
        }
    };
    if !consumed {
        return Err(RecoveryError::invalid(
            "a prior recovery is still pending its exact evaluation rerun",
        ));
    }
    previous
        .recovery_id
        .checked_add(1)
        .ok_or_else(|| RecoveryError::invalid("recovery ID sequence is exhausted"))
}

fn existing_invalidation_or_new_timestamp(
    workspace: &LoopWorkspace,
    recovery_id: u32,
    source_path: &str,
    recovery_path: &str,
    actor: &str,
    reason: &str,
    operator_guard: &crate::operator_evidence::OperatorEvidenceGuard,
) -> Result<String, RecoveryError> {
    let source_bytes =
        read_optional_verified(workspace, source_path, "invalidation source orphan")?;
    let recovery_bytes =
        read_optional_verified(workspace, recovery_path, "invalidation recovery orphan")?;
    for bytes in [source_bytes.as_deref(), recovery_bytes.as_deref()]
        .into_iter()
        .flatten()
    {
        operator_guard
            .validate_exact_raw_bytes(bytes)
            .map_err(RecoveryError::invalid)?;
    }
    let source = source_bytes
        .map(|bytes| serde_json::from_slice::<EvaluationInvalidationSourceRunV3>(&bytes))
        .transpose()
        .map_err(RecoveryError::wrapped)?;
    let recovery = recovery_bytes
        .map(|bytes| serde_json::from_slice::<EvaluationInvalidationAttemptV3>(&bytes))
        .transpose()
        .map_err(RecoveryError::wrapped)?;
    if let Some(source) = source.as_ref() {
        operator_guard
            .validate_recovery_fields(&source.actor, &source.reason)
            .map_err(RecoveryError::invalid)?;
        operator_guard
            .validate_run(&source.run)
            .and_then(|()| operator_guard.validate_run(&source.approved_run))
            .map_err(RecoveryError::invalid)?;
    }
    if let Some(recovery) = recovery.as_ref() {
        operator_guard
            .validate_recovery_fields(&recovery.actor, &recovery.reason)
            .map_err(RecoveryError::invalid)?;
    }
    match (source, recovery) {
        (None, None) => Ok(now_timestamp()),
        (Some(source), None)
            if source.schema_version == EVALUATION_INVALIDATION_SCHEMA_VERSION
                && source.recovery_id == recovery_id
                && source.actor == actor
                && source.reason == reason
                && parse_canonical_timestamp(&source.created_at).is_some() =>
        {
            Ok(source.created_at)
        }
        (Some(source), Some(recovery))
            if source.recovery_id == recovery_id
                && recovery.recovery_id == recovery_id
                && source.actor == actor
                && source.reason == reason
                && recovery.actor == actor
                && recovery.reason == reason
                && source.created_at == recovery.created_at
                && parse_canonical_timestamp(&source.created_at).is_some() =>
        {
            Ok(source.created_at)
        }
        (None, Some(_)) => Err(RecoveryError::invalid(
            "evaluation invalidation decision orphan exists without source snapshot",
        )),
        _ => Err(RecoveryError::invalid(
            "evaluation invalidation orphan audit fields disagree",
        )),
    }
}

fn exact_evaluation_invalidation_retry(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    actor: &str,
    reason: &str,
) -> Result<Option<EvaluationInvalidationOutcome>, RecoveryError> {
    let Some(reference) = run.latest_recovery.as_ref() else {
        return Ok(None);
    };
    let Some((recovery, source)) = load_verified_evaluation_invalidation(workspace, reference)?
    else {
        return Ok(None);
    };
    let mut expected =
        build_evaluation_invalidation_reset(&source.approved_run, &recovery.created_at);
    expected.latest_recovery = Some(reference.clone());
    if run != &expected {
        return Ok(None);
    }
    validate_invalidation_operational(workspace, run, &recovery)?;
    if invalidation_is_consumed(workspace, run, reference)? {
        return Ok(None);
    }
    if recovery.actor != actor || recovery.reason != reason {
        return Err(RecoveryError::invalid(
            "evaluation invalidation retry does not match exact actor and reason",
        ));
    }
    Ok(Some(EvaluationInvalidationOutcome {
        run: run.clone(),
        recovery,
        reference: reference.clone(),
    }))
}

fn invalidation_is_consumed(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    reference: &RecoveryReference,
) -> Result<bool, RecoveryError> {
    let Some((recovery, source)) = load_verified_evaluation_invalidation(workspace, reference)?
    else {
        return Ok(false);
    };
    invalidation_entry_is_consumed(workspace, run, reference, &recovery, &source)
}

fn invalidation_entry_is_consumed(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    reference: &RecoveryReference,
    recovery: &EvaluationInvalidationAttemptV3,
    source: &EvaluationInvalidationSourceRunV3,
) -> Result<bool, RecoveryError> {
    let paths = crate::evaluation_attempt::EvaluationAttemptPaths::indexed(
        recovery.next_evaluation_attempt,
    )
    .map_err(RecoveryError::invalid)?;
    let Some(bytes) =
        read_optional_verified(workspace, &paths.intent, "recovered evaluation intent")?
    else {
        return Ok(false);
    };
    let intent_reference = ArtifactReference {
        path: paths.intent,
        digest: digest_bytes(&bytes),
    };
    let intent = load_intent(workspace, &intent_reference).map_err(RecoveryError::invalid)?;
    let eval_config = load_recovery_eval_config(workspace, &source.approved_run)?;
    let mut authorized_approved =
        build_evaluation_invalidation_reset(&source.approved_run, &recovery.created_at);
    authorized_approved.latest_recovery = Some(reference.clone());
    intent
        .validate_against_with_recovery(
            &authorized_approved,
            &eval_config.evals.required,
            Some(reference),
        )
        .map_err(RecoveryError::invalid)?;
    if intent.attempt() != recovery.next_evaluation_attempt
        || run.run_id != recovery.run_id
        || run.latest_recovery.as_ref() != Some(reference)
    {
        return Err(RecoveryError::invalid(
            "recovered evaluation intent does not consume exact invalidation authority",
        ));
    }
    Ok(true)
}

fn validate_invalidation_operational(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    recovery: &EvaluationInvalidationAttemptV3,
) -> Result<(), RecoveryError> {
    crate::provider_exchange::validate_run_for_atomic_publication(workspace, run)
        .map_err(RecoveryError::wrapped)?;
    crate::runner::load_verified_authoritative_run_inputs(workspace, run)
        .map_err(RecoveryError::wrapped)?;
    let reconciled = preflight_provider_exchange_reconciliation(workspace, run)
        .map_err(RecoveryError::wrapped)?;
    if &reconciled != run {
        return Err(RecoveryError::invalid(
            "evaluation rerun has an unpublished provider exchange winner",
        ));
    }
    let candidate = run
        .candidate_workspace
        .as_ref()
        .ok_or_else(|| RecoveryError::invalid("evaluation rerun lost candidate authority"))?;
    if canonical_sha256_digest(candidate).map_err(RecoveryError::wrapped)?
        != recovery.candidate_state_digest
        || candidate.candidate_head != recovery.candidate_head
        || candidate.candidate_tree != recovery.candidate_tree
        || candidate.candidate_diff_digest != recovery.candidate_diff_digest
    {
        return Err(RecoveryError::invalid(
            "evaluation rerun candidate authority changed after invalidation",
        ));
    }
    let source_authority = capture_source_worktree_authority(
        Path::new(&candidate.source_worktree_root),
        Some(workspace.run_directory()),
    )
    .map_err(RecoveryError::wrapped)?;
    if canonical_sha256_digest(&source_authority).map_err(RecoveryError::wrapped)?
        != recovery.source_worktree_state_digest
    {
        return Err(RecoveryError::invalid(
            "source worktree authority changed after evaluation invalidation",
        ));
    }
    let verified = verify_candidate_patch_evidence_for_evaluation_locked(
        workspace,
        Path::new(&candidate.source_worktree_root),
    )
    .map_err(RecoveryError::wrapped)?;
    let approval = run
        .human_approval
        .as_ref()
        .ok_or_else(|| RecoveryError::invalid("evaluation rerun lost human approval authority"))?;
    if verified.applied_diff != approval.candidate_diff
        || verified.policy_decision_digest != approval.policy_decision_digest
        || verified.candidate_authority.starting_head != approval.starting_head
    {
        return Err(RecoveryError::invalid(
            "physical candidate authority changed after evaluation invalidation",
        ));
    }
    Ok(())
}

pub(crate) fn validate_requested_evaluation_invalidation(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    recovery_id: u32,
) -> Result<(RecoveryReference, u32), RecoveryError> {
    validate_operator_run_envelope(workspace, run)?;
    let reference = run
        .latest_recovery
        .as_ref()
        .ok_or_else(|| RecoveryError::invalid("run has no evaluation invalidation authority"))?;
    if reference.recovery_id != recovery_id {
        return Err(RecoveryError::invalid(
            "requested recovery is not the latest evaluation authority",
        ));
    }
    let (recovery, source) = load_verified_evaluation_invalidation(workspace, reference)?
        .ok_or_else(|| {
            RecoveryError::invalid("requested recovery is not evaluation invalidation")
        })?;
    let mut expected =
        build_evaluation_invalidation_reset(&source.approved_run, &recovery.created_at);
    expected.latest_recovery = Some(reference.clone());
    if run != &expected {
        return Err(RecoveryError::invalid(
            "current run is not exact invalidation reset authority",
        ));
    }
    validate_invalidation_operational(workspace, run, &recovery)?;
    let inventory = EvaluationAttemptInventory::load_for_invalidation(workspace)
        .map_err(RecoveryError::invalid)?;
    let latest = inventory
        .invalidation_prefix_paths()
        .map_err(RecoveryError::invalid)?;
    if latest.attempt != recovery.invalidated_attempt {
        return Err(RecoveryError::invalid(
            "evaluation rerun already has durable attempt artifacts",
        ));
    }
    if invalidation_is_consumed(workspace, run, reference)? {
        return Err(RecoveryError::invalid(
            "evaluation rerun recovery is already consumed",
        ));
    }
    Ok((reference.clone(), recovery.next_evaluation_attempt))
}

pub(crate) fn reauthenticate_evaluation_invalidation_execution(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    reference: &RecoveryReference,
    source_worktree_state_digest: &str,
) -> Result<(), RecoveryError> {
    validate_operator_run_envelope(workspace, run)?;
    let (recovery, source) = load_verified_evaluation_invalidation(workspace, reference)?
        .ok_or_else(|| RecoveryError::invalid("evaluation execution lost V3 invalidation"))?;
    let mut expected =
        build_evaluation_invalidation_reset(&source.approved_run, &recovery.created_at);
    expected.latest_recovery = Some(reference.clone());
    if run != &expected
        || run.latest_recovery.as_ref() != Some(reference)
        || recovery.source_worktree_state_digest != source_worktree_state_digest
    {
        return Err(RecoveryError::invalid(
            "evaluation execution changed exact invalidation reset or source authority",
        ));
    }
    validate_invalidation_operational(workspace, run, &recovery)?;
    let paths = crate::evaluation_attempt::EvaluationAttemptPaths::indexed(
        recovery.next_evaluation_attempt,
    )
    .map_err(RecoveryError::invalid)?;
    if read_optional_verified(workspace, &paths.intent, "recovered evaluation intent")?.is_some()
        && !invalidation_entry_is_consumed(workspace, run, reference, &recovery, &source)?
    {
        return Err(RecoveryError::invalid(
            "evaluation execution intent does not consume exact invalidation",
        ));
    }
    Ok(())
}

fn verify_evaluation_prefix_references(
    workspace: &LoopWorkspace,
    operator_guard: &crate::operator_evidence::OperatorEvidenceGuard,
    testing: &TestingEvidence,
    intent: &ArtifactReference,
    testing_reference: &ArtifactReference,
) -> Result<(), RecoveryError> {
    let mut references = vec![intent.clone(), testing_reference.clone()];
    for check in &testing.checks {
        for (path, digest) in [
            (check.stdout_path.as_ref(), check.stdout_digest.as_ref()),
            (check.stderr_path.as_ref(), check.stderr_digest.as_ref()),
        ] {
            references.push(ArtifactReference {
                path: path
                    .cloned()
                    .ok_or_else(|| RecoveryError::invalid("adoption prefix lost check log path"))?,
                digest: digest.cloned().ok_or_else(|| {
                    RecoveryError::invalid("adoption prefix lost check log digest")
                })?,
            });
        }
    }
    for reference in references {
        let bytes = read_verified_regular_file(
            workspace.run_directory(),
            &reference.path,
            "evaluation adoption prefix artifact",
        )
        .map_err(RecoveryError::wrapped)?;
        operator_guard
            .validate_exact_raw_bytes(&bytes)
            .map_err(RecoveryError::invalid)?;
        if digest_bytes(&bytes) != reference.digest {
            return Err(RecoveryError::invalid(
                "evaluation adoption prefix artifact digest mismatch",
            ));
        }
    }
    Ok(())
}

fn build_adopted_final(
    approved: &LoopRun,
    testing: &TestingEvidence,
    testing_reference: &ArtifactReference,
    report_reference: &ArtifactReference,
    created_at: &str,
) -> Result<LoopRun, RecoveryError> {
    let passed = testing.passed;
    let mut run = approved.clone();
    run.status = if passed {
        LoopStatus::EvalPassed
    } else {
        LoopStatus::Failed
    };
    run.current_step = LoopStepName::EvalReport;
    run.updated_at = created_at.to_string();
    for (name, reference) in [
        (LoopStepName::Testing, testing_reference),
        (LoopStepName::EvalReport, report_reference),
    ] {
        let step = run
            .steps
            .iter_mut()
            .find(|step| step.name == name)
            .ok_or_else(|| RecoveryError::invalid("evaluation step chain is incomplete"))?;
        step.status = if passed {
            LoopStepStatus::Passed
        } else {
            LoopStepStatus::Failed
        };
        step.artifact_path = Some(reference.path.clone());
        step.artifact_digest = Some(reference.digest.clone());
    }
    run.eval_report_path = Some(report_reference.path.clone());
    Ok(run)
}

fn load_evaluation_adoption_orphan(
    workspace: &LoopWorkspace,
    recovery_id: u32,
    source_path: &str,
    recovery_path: &str,
    actor: &str,
    reason: &str,
    operator_guard: &crate::operator_evidence::OperatorEvidenceGuard,
) -> Result<Option<EvaluationAdoptionOrphan>, RecoveryError> {
    let existing_source = read_optional_verified(workspace, source_path, "adoption source orphan")?
        .map(|bytes| {
            operator_guard
                .validate_exact_raw_bytes(&bytes)
                .map_err(RecoveryError::invalid)?;
            let source: EvaluationRecoverySourceRunV2 =
                serde_json::from_slice(&bytes).map_err(RecoveryError::wrapped)?;
            if canonical_json_bytes(&source).map_err(RecoveryError::wrapped)? != bytes
                || source.schema_version != EVALUATION_RECOVERY_SCHEMA_VERSION
                || source.recovery_id != recovery_id
                || parse_canonical_timestamp(&source.created_at).is_none()
            {
                return Err(RecoveryError::invalid(
                    "evaluation adoption source orphan is invalid",
                ));
            }
            Ok(source)
        })
        .transpose()?;
    let existing_recovery =
        read_optional_verified(workspace, recovery_path, "adoption recovery orphan")?
            .map(|bytes| {
                operator_guard
                    .validate_exact_raw_bytes(&bytes)
                    .map_err(RecoveryError::invalid)?;
                let recovery: EvaluationRecoveryAttemptV2 =
                    serde_json::from_slice(&bytes).map_err(RecoveryError::wrapped)?;
                if canonical_json_bytes(&recovery).map_err(RecoveryError::wrapped)? != bytes
                    || recovery.schema_version != EVALUATION_RECOVERY_SCHEMA_VERSION
                    || recovery.recovery_id != recovery_id
                    || parse_canonical_timestamp(&recovery.created_at).is_none()
                {
                    return Err(RecoveryError::invalid(
                        "evaluation adoption recovery orphan is invalid",
                    ));
                }
                Ok(recovery)
            })
            .transpose()?;
    if let Some(source) = existing_source.as_ref() {
        operator_guard
            .validate_recovery_fields(&source.actor, &source.reason)
            .and_then(|()| operator_guard.validate_run(&source.run))
            .map_err(RecoveryError::invalid)?;
    }
    if let Some(recovery) = existing_recovery.as_ref() {
        operator_guard
            .validate_recovery_fields(&recovery.actor, &recovery.reason)
            .map_err(RecoveryError::invalid)?;
    }
    match (existing_source, existing_recovery) {
        (None, None) => Ok(None),
        (Some(source), None) => {
            if source.actor != actor || source.reason != reason {
                return Err(RecoveryError::invalid(
                    "evaluation adoption source orphan actor or reason disagrees",
                ));
            }
            Ok(Some(EvaluationAdoptionOrphan {
                created_at: source.created_at,
                report_disposition: if source.evaluation_prefix.eval_report.is_some() {
                    EvaluationRecoveryReportDisposition::VerifyExisting
                } else {
                    EvaluationRecoveryReportDisposition::CreateMissing
                },
            }))
        }
        (None, Some(_)) => Err(RecoveryError::invalid(
            "evaluation adoption recovery orphan exists without its source snapshot",
        )),
        (Some(source), Some(recovery)) => {
            if source.created_at != recovery.created_at
                || source.actor != actor
                || source.reason != reason
                || recovery.actor != actor
                || recovery.reason != reason
            {
                return Err(RecoveryError::invalid(
                    "evaluation adoption orphan audit fields disagree",
                ));
            }
            Ok(Some(EvaluationAdoptionOrphan {
                created_at: recovery.created_at,
                report_disposition: recovery.report_disposition,
            }))
        }
    }
}

fn read_optional_verified(
    workspace: &LoopWorkspace,
    path: &str,
    label: &str,
) -> Result<Option<Vec<u8>>, RecoveryError> {
    match fs::symlink_metadata(workspace.run_directory().join(path)) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            RecoveryError::invalid(format!("{label} is not a real regular file")),
        ),
        Ok(_) => read_verified_regular_file(workspace.run_directory(), path, label)
            .map(Some)
            .map_err(RecoveryError::wrapped),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(RecoveryError::wrapped(error)),
    }
}

fn preflight_exact_artifact(
    workspace: &LoopWorkspace,
    path: &str,
    expected: &[u8],
    allow_absent: bool,
    operator_guard: Option<&crate::operator_evidence::OperatorEvidenceGuard>,
) -> Result<(), RecoveryError> {
    match read_optional_verified(workspace, path, "evaluation adoption artifact")? {
        Some(bytes) => {
            if let Some(operator_guard) = operator_guard {
                operator_guard
                    .validate_exact_raw_bytes(&bytes)
                    .map_err(RecoveryError::invalid)?;
            }
            if bytes == expected {
                Ok(())
            } else {
                Err(RecoveryError::invalid(format!(
                    "evaluation adoption artifact collision at {path}"
                )))
            }
        }
        None if allow_absent => Ok(()),
        None => Err(RecoveryError::invalid(format!(
            "evaluation adoption requires existing exact artifact at {path}"
        ))),
    }
}

fn validate_operator_run_envelope(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<(), RecoveryError> {
    let operator_guard = crate::operator_evidence::OperatorEvidenceGuard::load(workspace, run)
        .map_err(RecoveryError::invalid)?;
    operator_guard
        .validate_current_run_file(workspace)
        .and_then(|()| operator_guard.validate_run(run))
        .and_then(|()| operator_guard.validate_future_run(run).map(drop))
        .map_err(RecoveryError::invalid)
}

#[allow(clippy::too_many_arguments)]
fn reauthenticate_evaluation_adoption(
    workspace: &LoopWorkspace,
    approved: &LoopRun,
    final_run: &LoopRun,
    reference: &RecoveryReference,
    source_root: &Path,
    source_authority: &SourceWorktreeAuthority,
    verified: &VerifiedCandidatePatchEvidence,
    report_path: &str,
    report_bytes: &[u8],
) -> Result<(), RecoveryError> {
    if state::load_run(workspace).map_err(RecoveryError::wrapped)? != *approved {
        return Err(RecoveryError::invalid(
            "Approved authority changed before evaluation adoption CAS",
        ));
    }
    let operator_guard = crate::operator_evidence::OperatorEvidenceGuard::load(workspace, approved)
        .map_err(RecoveryError::invalid)?;
    operator_guard
        .validate_current_run_file(workspace)
        .and_then(|()| operator_guard.validate_run(approved))
        .and_then(|()| operator_guard.validate_future_run(final_run).map(|_| ()))
        .map_err(RecoveryError::invalid)?;
    preflight_exact_artifact(
        workspace,
        report_path,
        report_bytes,
        false,
        Some(&operator_guard),
    )?;
    validate_evaluation_adoption_source(workspace, approved)?;
    let current_verified =
        verify_candidate_patch_evidence_for_evaluation_locked(workspace, source_root)
            .map_err(RecoveryError::wrapped)?;
    if &current_verified != verified {
        return Err(RecoveryError::invalid(
            "candidate authority changed during evaluation adoption",
        ));
    }
    validate_source_worktree_authority(
        source_root,
        Some(workspace.run_directory()),
        source_authority,
    )
    .map_err(RecoveryError::wrapped)?;
    load_verified_evaluation_recovery(workspace, reference)?
        .ok_or_else(|| RecoveryError::invalid("evaluation adoption recovery lost v2 authority"))?;
    crate::load_verified_final_evaluation_authority(workspace, final_run)
        .map_err(RecoveryError::wrapped)?;
    if crate::evaluation_storage::derive_active_evaluation_storage_commitment(
        workspace.run_directory(),
        final_run,
    )
    .map_err(RecoveryError::invalid)?
    .is_some()
    {
        return Err(RecoveryError::invalid(
            "adopted final authority retained an active evaluation storage commitment",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn reauthenticate_evaluation_invalidation(
    workspace: &LoopWorkspace,
    source: &LoopRun,
    approved: &LoopRun,
    intended: &LoopRun,
    reference: &RecoveryReference,
    source_root: &Path,
    source_authority: &SourceWorktreeAuthority,
    verified: &VerifiedCandidatePatchEvidence,
) -> Result<(), RecoveryError> {
    if state::load_run(workspace).map_err(RecoveryError::wrapped)? != *source {
        return Err(RecoveryError::invalid(
            "evaluation authority changed before invalidation CAS",
        ));
    }
    let operator_guard = crate::operator_evidence::OperatorEvidenceGuard::load(workspace, approved)
        .map_err(RecoveryError::invalid)?;
    operator_guard
        .validate_current_run_file(workspace)
        .and_then(|()| operator_guard.validate_run(source))
        .and_then(|()| operator_guard.validate_run(approved))
        .and_then(|()| operator_guard.validate_future_run(intended).map(|_| ()))
        .map_err(RecoveryError::invalid)?;
    validate_evaluation_invalidation_source(workspace, source, approved)?;
    let current_verified =
        verify_candidate_patch_evidence_for_evaluation_locked(workspace, source_root)
            .map_err(RecoveryError::wrapped)?;
    if &current_verified != verified {
        return Err(RecoveryError::invalid(
            "candidate authority changed during evaluation invalidation",
        ));
    }
    validate_source_worktree_authority(
        source_root,
        Some(workspace.run_directory()),
        source_authority,
    )
    .map_err(RecoveryError::wrapped)?;
    let (recovery, snapshot) = load_verified_evaluation_invalidation(workspace, reference)?
        .ok_or_else(|| RecoveryError::invalid("evaluation invalidation lost V3 authority"))?;
    if snapshot.run != *source || snapshot.approved_run != *approved {
        return Err(RecoveryError::invalid(
            "evaluation invalidation source changed before CAS",
        ));
    }
    let mut expected = build_evaluation_invalidation_reset(approved, &recovery.created_at);
    expected.latest_recovery = Some(reference.clone());
    if &expected != intended {
        return Err(RecoveryError::invalid(
            "evaluation invalidation intended reset changed",
        ));
    }
    if crate::evaluation_storage::derive_active_evaluation_storage_commitment(
        workspace.run_directory(),
        intended,
    )
    .map_err(RecoveryError::invalid)?
    .is_some()
    {
        return Err(RecoveryError::invalid(
            "evaluation invalidation reset retained the superseded storage commitment",
        ));
    }
    Ok(())
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
    let operator_guard = crate::operator_evidence::OperatorEvidenceGuard::load(workspace, &source)
        .map_err(RecoveryError::invalid)?;
    operator_guard
        .validate_current_run_file(workspace)
        .and_then(|()| operator_guard.validate_run(&source))
        .and_then(|()| operator_guard.validate_structural(actor))
        .map_err(RecoveryError::invalid)?;
    let reason = operator_guard
        .sanitize_reason(reason, 1024)
        .map_err(RecoveryError::invalid)?;
    validate_note("reason", &reason, 1024)?;
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
                    operator_guard
                        .validate_future_run(&source)
                        .map_err(RecoveryError::invalid)?;
                    state::resync_exact_run(workspace, &source).map_err(RecoveryError::wrapped)?;
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

    let recovery_path = recovery_path(recovery_id);
    let created_at = existing_or_new_timestamp(workspace, &recovery_path, &operator_guard)?;

    let source_snapshot = RecoverySourceRunV1 {
        schema_version: RECOVERY_SCHEMA_VERSION,
        recovery_id,
        run: source.clone(),
    };
    let source_bytes = operator_guard
        .validate_canonical_artifact(&source_snapshot)
        .map_err(RecoveryError::invalid)?;
    let source_path = recovery_source_path(recovery_id);
    let source_reference = ArtifactReference {
        path: source_path.clone(),
        digest: digest_bytes(&source_bytes),
    };

    let mut projection = reset_run(&source, step, recovery_id, &recovery_path, &created_at)?;
    let projection_digest = canonical_sha256_digest(&projection).map_err(RecoveryError::wrapped)?;
    let recovery = RecoveryAttemptV1 {
        schema_version: RECOVERY_SCHEMA_VERSION,
        recovery_id,
        run_id: source.run_id.clone(),
        action: RecoveryAction::ReviseProviderStep,
        step,
        actor: actor.to_string(),
        reason,
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
    let recovery_bytes = operator_guard
        .validate_canonical_artifact(&recovery)
        .map_err(RecoveryError::invalid)?;
    let recovery_digest = digest_bytes(&recovery_bytes);
    let reference = RecoveryReference {
        recovery_id,
        artifact: ArtifactReference {
            path: recovery_path.clone(),
            digest: recovery_digest.clone(),
        },
    };
    projection.latest_recovery = Some(reference.clone());
    let intended = projection;
    operator_guard
        .validate_future_run(&intended)
        .map_err(RecoveryError::invalid)?;
    validate_reset_relation(&source, &intended, &recovery)?;
    preflight_exact_artifact(
        workspace,
        &source_path,
        &source_bytes,
        true,
        Some(&operator_guard),
    )?;
    preflight_exact_artifact(
        workspace,
        &recovery_path,
        &recovery_bytes,
        true,
        Some(&operator_guard),
    )?;
    let publication_guard =
        crate::operator_evidence::OperatorEvidenceGuard::load(workspace, &source)
            .map_err(RecoveryError::invalid)?;
    publication_guard
        .validate_current_run_file(workspace)
        .and_then(|()| publication_guard.validate_exact_raw_bytes(&source_bytes))
        .and_then(|()| publication_guard.validate_exact_raw_bytes(&recovery_bytes))
        .and_then(|()| publication_guard.validate_future_run(&intended).map(drop))
        .map_err(RecoveryError::invalid)?;
    publish_create_only(workspace.run_directory(), &source_path, &source_bytes)
        .map_err(RecoveryError::wrapped)?;
    publish_create_only(workspace.run_directory(), &recovery_path, &recovery_bytes)
        .map_err(RecoveryError::wrapped)?;
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
                let operator_guard =
                    crate::operator_evidence::OperatorEvidenceGuard::load(workspace, current)
                        .map_err(RecoveryError::invalid)?;
                operator_guard
                    .validate_current_run_file(workspace)
                    .and_then(|()| operator_guard.validate_run(current))
                    .and_then(|()| {
                        operator_guard.validate_recovery_fields(&recovery.actor, &recovery.reason)
                    })
                    .and_then(|()| operator_guard.validate_exact_raw_bytes(&source_bytes))
                    .and_then(|()| operator_guard.validate_exact_raw_bytes(&recovery_bytes))
                    .and_then(|()| operator_guard.validate_future_run(&intended).map(drop))
                    .map_err(RecoveryError::invalid)?;
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

pub(crate) fn load_verified_latest_provider_recovery_source(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<(RecoveryAttemptV1, LoopRun), RecoveryError> {
    let reference = run.latest_recovery.as_ref().ok_or_else(|| {
        RecoveryError::invalid("provider attempt has no latest recovery authority")
    })?;
    load_verified_provider_recovery_source(workspace, run, reference)
}

pub fn ensure_no_pending_recovery(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<(), RecoveryError> {
    let Some(reference) = &run.latest_recovery else {
        return Ok(());
    };
    validate_operator_run_envelope(workspace, run)?;
    let consumed = match load_verified_any_recovery_entry(workspace, reference)? {
        VerifiedRecoveryLineage::Provider { .. } => {
            recovery_is_consumed(workspace, run, reference)?
        }
        VerifiedRecoveryLineage::Evaluation { .. } => {
            is_final_evaluation_status(run)
                && crate::load_verified_final_evaluation_authority(workspace, run).is_ok()
        }
        VerifiedRecoveryLineage::Invalidation { .. } => {
            invalidation_is_consumed(workspace, run, reference)?
        }
    };
    if consumed {
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
    validate_operator_run_envelope(workspace, run)?;
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
    validate_operator_run_envelope(workspace, run)?;
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
    provider_recovery_entry_is_consumed(workspace, run, reference, &recovery, &source, &projection)
}

fn provider_recovery_entry_is_consumed(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    reference: &RecoveryReference,
    recovery: &RecoveryAttemptV1,
    source: &LoopRun,
    projection: &LoopRun,
) -> Result<bool, RecoveryError> {
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
        source,
        projection,
        recovery,
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
    load_verified_provider_recovery_source(workspace, run, reference).map(|(recovery, _)| recovery)
}

fn load_verified_provider_recovery_source(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    reference: &RecoveryReference,
) -> Result<(RecoveryAttemptV1, LoopRun), RecoveryError> {
    validate_operator_run_envelope(workspace, run)?;
    let (recovery, source, projection) = load_verified_recovery_lineage(workspace, reference)?;
    validate_current_descendant(workspace, run, reference, &source, &projection, &recovery)?;
    Ok((recovery, source))
}

fn load_verified_recovery_lineage(
    workspace: &LoopWorkspace,
    reference: &RecoveryReference,
) -> Result<(RecoveryAttemptV1, LoopRun, LoopRun), RecoveryError> {
    let result = load_verified_provider_recovery_entry(workspace, reference)?;
    let lineage = VerifiedRecoveryLineage::Provider {
        recovery: result.0.clone(),
        source: Box::new(result.1.clone()),
        projection: Box::new(result.2.clone()),
    };
    validate_mixed_recovery_chain_iteratively(workspace, &lineage)?;
    Ok(result)
}

fn load_verified_provider_recovery_entry(
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
    let operator_guard =
        crate::operator_evidence::OperatorEvidenceGuard::load(workspace, &snapshot.run)
            .map_err(RecoveryError::invalid)?;
    operator_guard
        .validate_exact_raw_bytes(&bytes)
        .and_then(|()| operator_guard.validate_exact_raw_bytes(&snapshot_bytes))
        .and_then(|()| operator_guard.validate_run(&snapshot.run))
        .and_then(|()| operator_guard.validate_recovery_fields(&recovery.actor, &recovery.reason))
        .map_err(RecoveryError::invalid)?;
    let source_errors = seaf_core::validate_loop_run(&snapshot.run);
    if !source_errors.is_empty() {
        return Err(RecoveryError::invalid(format!(
            "recovery source snapshot contains an invalid LoopRun: {source_errors:?}"
        )));
    }
    validate_source_bindings(&snapshot.run, &recovery)?;
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

#[derive(Debug)]
enum VerifiedRecoveryLineage {
    Provider {
        recovery: RecoveryAttemptV1,
        source: Box<LoopRun>,
        projection: Box<LoopRun>,
    },
    Evaluation {
        recovery: EvaluationRecoveryAttemptV2,
        source: Box<EvaluationRecoverySourceRunV2>,
    },
    Invalidation {
        recovery: EvaluationInvalidationAttemptV3,
        source: Box<EvaluationInvalidationSourceRunV3>,
    },
}

pub(crate) fn load_evaluation_recovery_source_for_final(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<Option<LoopRun>, RecoveryError> {
    let Some(reference) = run.latest_recovery.as_ref() else {
        return Ok(None);
    };
    validate_operator_run_envelope(workspace, run)?;
    if let Some((recovery, source)) = load_verified_evaluation_invalidation(workspace, reference)? {
        let testing_reference = final_step_reference(run, LoopStepName::Testing)?;
        let report_reference = final_step_reference(run, LoopStepName::EvalReport)?;
        let (attempt, spelling) = selected_attempt(&testing_reference.path, &report_reference.path)
            .map_err(RecoveryError::invalid)?;
        if attempt != recovery.next_evaluation_attempt
            || fixed_spelling(spelling)
            || run.eval_report_path.as_deref() != Some(report_reference.path.as_str())
        {
            return Err(RecoveryError::invalid(
                "evaluation final does not select invalidation-authorized attempt",
            ));
        }
        let intent_path = crate::evaluation_attempt::EvaluationAttemptPaths::indexed(attempt)
            .map_err(RecoveryError::invalid)?
            .intent;
        let intent_reference =
            reference_for_path(workspace, &intent_path).map_err(RecoveryError::invalid)?;
        let intent = load_intent(workspace, &intent_reference).map_err(RecoveryError::invalid)?;
        let mut reset =
            build_evaluation_invalidation_reset(&source.approved_run, &recovery.created_at);
        reset.latest_recovery = Some(reference.clone());
        let eval_config = load_recovery_eval_config(workspace, &reset)?;
        intent
            .validate_against_with_recovery(&reset, &eval_config.evals.required, Some(reference))
            .map_err(RecoveryError::invalid)?;
        return Ok(Some(reset));
    }
    let Some((recovery, source)) = load_verified_evaluation_recovery(workspace, reference)? else {
        return Ok(None);
    };
    let testing_reference = final_step_reference(run, LoopStepName::Testing)?;
    let report_reference = final_step_reference(run, LoopStepName::EvalReport)?;
    if testing_reference != recovery.testing_evidence
        || report_reference != recovery.eval_report
        || run.eval_report_path.as_deref() != Some(recovery.eval_report.path.as_str())
    {
        return Err(RecoveryError::invalid(
            "evaluation final descendant substituted recovery evidence or timestamp",
        ));
    }
    let mut zero_digest_projection = run.clone();
    normalize_evaluation_final_cleanup(&mut zero_digest_projection, &recovery.created_at)?;
    let latest = zero_digest_projection
        .latest_recovery
        .as_mut()
        .ok_or_else(|| RecoveryError::invalid("evaluation final projection lost recovery"))?;
    latest.artifact.digest = "0".repeat(64);
    if canonical_sha256_digest(&zero_digest_projection).map_err(RecoveryError::wrapped)?
        != recovery.expected_final_projection_digest
    {
        return Err(RecoveryError::invalid(
            "evaluation final projection digest mismatch",
        ));
    }
    Ok(Some(source.run))
}

pub(crate) fn load_verified_evaluation_recovery(
    workspace: &LoopWorkspace,
    reference: &RecoveryReference,
) -> Result<Option<(EvaluationRecoveryAttemptV2, EvaluationRecoverySourceRunV2)>, RecoveryError> {
    match load_verified_any_recovery_lineage(workspace, reference)? {
        VerifiedRecoveryLineage::Evaluation { recovery, source } => Ok(Some((recovery, *source))),
        VerifiedRecoveryLineage::Provider { .. } | VerifiedRecoveryLineage::Invalidation { .. } => {
            Ok(None)
        }
    }
}

fn normalize_evaluation_final_cleanup(
    run: &mut LoopRun,
    recovery_created_at: &str,
) -> Result<(), RecoveryError> {
    let recovery_time = parse_canonical_timestamp(recovery_created_at)
        .ok_or_else(|| RecoveryError::invalid("evaluation recovery created_at is not canonical"))?;
    let cleanup_allowed = run.status == LoopStatus::Failed && run.human_approval.is_some();
    let candidate = run.candidate_workspace.as_mut().ok_or_else(|| {
        RecoveryError::invalid("evaluation final descendant lost candidate authority")
    })?;
    match candidate.lifecycle {
        CandidateWorkspaceLifecycle::Active
            if candidate.cleanup_started_at.is_none()
                && candidate.cleaned_at.is_none()
                && run.updated_at == recovery_created_at => {}
        CandidateWorkspaceLifecycle::Cleaning => {
            if !cleanup_allowed {
                return Err(RecoveryError::invalid(
                    "only approval-bound Failed evaluation may enter Cleaning",
                ));
            }
            let started = candidate
                .cleanup_started_at
                .as_deref()
                .and_then(parse_canonical_timestamp)
                .ok_or_else(|| {
                    RecoveryError::invalid("evaluation cleanup start timestamp is invalid")
                })?;
            if candidate.cleaned_at.is_some()
                || started < recovery_time
                || run.updated_at.as_str() != candidate.cleanup_started_at.as_deref().unwrap()
            {
                return Err(RecoveryError::invalid(
                    "evaluation Cleaning descendant timestamp relation is invalid",
                ));
            }
        }
        CandidateWorkspaceLifecycle::Cleaned => {
            if !cleanup_allowed {
                return Err(RecoveryError::invalid(
                    "only approval-bound Failed evaluation may become Cleaned",
                ));
            }
            let started = candidate
                .cleanup_started_at
                .as_deref()
                .and_then(parse_canonical_timestamp)
                .ok_or_else(|| {
                    RecoveryError::invalid("evaluation cleanup start timestamp is invalid")
                })?;
            let cleaned = candidate
                .cleaned_at
                .as_deref()
                .and_then(parse_canonical_timestamp)
                .ok_or_else(|| RecoveryError::invalid("evaluation cleaned timestamp is invalid"))?;
            if started < recovery_time
                || cleaned < started
                || run.updated_at.as_str() != candidate.cleaned_at.as_deref().unwrap()
            {
                return Err(RecoveryError::invalid(
                    "evaluation Cleaned descendant timestamp relation is invalid",
                ));
            }
        }
        _ => {
            return Err(RecoveryError::invalid(
                "evaluation final candidate cleanup relation is invalid",
            ))
        }
    }
    candidate.lifecycle = CandidateWorkspaceLifecycle::Active;
    candidate.cleanup_started_at = None;
    candidate.cleaned_at = None;
    run.updated_at = recovery_created_at.to_string();
    Ok(())
}

fn parse_canonical_timestamp(value: &str) -> Option<u64> {
    let parsed = value.parse::<u64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn final_step_reference(
    run: &LoopRun,
    name: LoopStepName,
) -> Result<ArtifactReference, RecoveryError> {
    let step = run
        .steps
        .iter()
        .find(|step| step.name == name)
        .ok_or_else(|| RecoveryError::invalid("evaluation final lost its exact step chain"))?;
    Ok(ArtifactReference {
        path: step
            .artifact_path
            .clone()
            .ok_or_else(|| RecoveryError::invalid("evaluation final step lost artifact path"))?,
        digest: step
            .artifact_digest
            .clone()
            .ok_or_else(|| RecoveryError::invalid("evaluation final step lost artifact digest"))?,
    })
}

fn load_verified_any_recovery_lineage(
    workspace: &LoopWorkspace,
    reference: &RecoveryReference,
) -> Result<VerifiedRecoveryLineage, RecoveryError> {
    let lineage = load_verified_any_recovery_entry(workspace, reference)?;
    validate_mixed_recovery_chain_iteratively(workspace, &lineage)?;
    Ok(lineage)
}

fn load_verified_any_recovery_entry(
    workspace: &LoopWorkspace,
    reference: &RecoveryReference,
) -> Result<VerifiedRecoveryLineage, RecoveryError> {
    let bytes = read_verified_regular_file(
        workspace.run_directory(),
        &reference.artifact.path,
        "recovery attempt",
    )
    .map_err(RecoveryError::wrapped)?;
    if digest_bytes(&bytes) != reference.artifact.digest {
        return Err(RecoveryError::invalid("recovery attempt digest mismatch"));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(RecoveryError::wrapped)?;
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(1) => {
            let (recovery, source, projection) =
                load_verified_provider_recovery_entry(workspace, reference)?;
            Ok(VerifiedRecoveryLineage::Provider {
                recovery,
                source: Box::new(source),
                projection: Box::new(projection),
            })
        }
        Some(2) => {
            let (recovery, source) =
                load_verified_evaluation_recovery_lineage(workspace, reference, &bytes)?;
            Ok(VerifiedRecoveryLineage::Evaluation {
                recovery,
                source: Box::new(source),
            })
        }
        Some(3) => {
            let (recovery, source) =
                load_verified_evaluation_invalidation_lineage(workspace, reference, &bytes)?;
            Ok(VerifiedRecoveryLineage::Invalidation {
                recovery,
                source: Box::new(source),
            })
        }
        _ => Err(RecoveryError::invalid(
            "unsupported recovery schema version",
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryChainChildKind {
    Provider,
    Evaluation,
    Invalidation,
}

#[derive(Debug, Clone)]
struct RecoveryChainChild {
    kind: RecoveryChainChildKind,
    source_run: LoopRun,
    previous: Option<RecoveryReference>,
}

fn recovery_chain_child(lineage: &VerifiedRecoveryLineage) -> RecoveryChainChild {
    match lineage {
        VerifiedRecoveryLineage::Provider {
            recovery, source, ..
        } => RecoveryChainChild {
            kind: RecoveryChainChildKind::Provider,
            source_run: (**source).clone(),
            previous: recovery.previous_recovery.clone(),
        },
        VerifiedRecoveryLineage::Evaluation { recovery, source } => RecoveryChainChild {
            kind: RecoveryChainChildKind::Evaluation,
            source_run: source.run.clone(),
            previous: recovery.previous_recovery.clone(),
        },
        VerifiedRecoveryLineage::Invalidation { recovery, source } => RecoveryChainChild {
            kind: RecoveryChainChildKind::Invalidation,
            source_run: source.run.clone(),
            previous: recovery.previous_recovery.clone(),
        },
    }
}

fn validate_mixed_recovery_chain_iteratively(
    workspace: &LoopWorkspace,
    lineage: &VerifiedRecoveryLineage,
) -> Result<(), RecoveryError> {
    let mut child = recovery_chain_child(lineage);
    while let Some(previous_reference) = child.previous.clone() {
        let previous = load_verified_any_recovery_entry(workspace, &previous_reference)?;
        let consumed = match &previous {
            VerifiedRecoveryLineage::Provider {
                recovery,
                source,
                projection,
            } => provider_recovery_entry_is_consumed(
                workspace,
                &child.source_run,
                &previous_reference,
                recovery,
                source,
                projection,
            )?,
            VerifiedRecoveryLineage::Evaluation { recovery, source } => {
                child.kind == RecoveryChainChildKind::Invalidation
                    && adopted_evaluation_entry_is_exact_final(
                        workspace,
                        &child.source_run,
                        &previous_reference,
                        recovery,
                        source,
                    )?
            }
            VerifiedRecoveryLineage::Invalidation { recovery, source } => {
                matches!(
                    child.kind,
                    RecoveryChainChildKind::Evaluation | RecoveryChainChildKind::Invalidation
                ) && invalidation_entry_is_consumed(
                    workspace,
                    &child.source_run,
                    &previous_reference,
                    recovery,
                    source,
                )?
            }
        };
        if !consumed {
            return Err(RecoveryError::invalid(
                "recovery chain predecessor is not demonstrably consumed",
            ));
        }
        child = recovery_chain_child(&previous);
    }
    Ok(())
}

fn adopted_evaluation_entry_is_exact_final(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    reference: &RecoveryReference,
    recovery: &EvaluationRecoveryAttemptV2,
    source: &EvaluationRecoverySourceRunV2,
) -> Result<bool, RecoveryError> {
    if run.latest_recovery.as_ref() != Some(reference) || run.status != LoopStatus::Failed {
        return Ok(false);
    }
    let testing =
        TestingEvidence::load_for_approved_run(workspace, &recovery.testing_evidence, &source.run)
            .map_err(RecoveryError::wrapped)?;
    let mut expected = build_adopted_final(
        &source.run,
        &testing,
        &recovery.testing_evidence,
        &recovery.eval_report,
        &recovery.created_at,
    )?;
    expected.latest_recovery = Some(reference.clone());
    Ok(&expected == run)
}

fn load_verified_evaluation_recovery_lineage(
    workspace: &LoopWorkspace,
    reference: &RecoveryReference,
    bytes: &[u8],
) -> Result<(EvaluationRecoveryAttemptV2, EvaluationRecoverySourceRunV2), RecoveryError> {
    let recovery: EvaluationRecoveryAttemptV2 =
        serde_json::from_slice(bytes).map_err(RecoveryError::wrapped)?;
    if canonical_json_bytes(&recovery).map_err(RecoveryError::wrapped)? != bytes {
        return Err(RecoveryError::invalid(
            "evaluation recovery attempt is not canonical JSON",
        ));
    }
    validate_evaluation_recovery_contract(&recovery, reference)?;
    let source_bytes = read_verified_regular_file(
        workspace.run_directory(),
        &recovery.source_run.path,
        "evaluation recovery source run",
    )
    .map_err(RecoveryError::wrapped)?;
    if digest_bytes(&source_bytes) != recovery.source_run.digest {
        return Err(RecoveryError::invalid(
            "evaluation recovery source snapshot digest mismatch",
        ));
    }
    let source: EvaluationRecoverySourceRunV2 =
        serde_json::from_slice(&source_bytes).map_err(RecoveryError::wrapped)?;
    if canonical_json_bytes(&source).map_err(RecoveryError::wrapped)? != source_bytes
        || source.schema_version != EVALUATION_RECOVERY_SCHEMA_VERSION
        || source.recovery_id != recovery.recovery_id
        || source.actor != recovery.actor
        || source.reason != recovery.reason
        || source.created_at != recovery.created_at
        || canonical_sha256_digest(&source.run).map_err(RecoveryError::wrapped)?
            != recovery.source_run_digest
    {
        return Err(RecoveryError::invalid(
            "evaluation recovery source snapshot binding mismatch",
        ));
    }
    let operator_guard =
        crate::operator_evidence::OperatorEvidenceGuard::load(workspace, &source.run)
            .map_err(RecoveryError::invalid)?;
    operator_guard
        .validate_exact_raw_bytes(bytes)
        .and_then(|()| operator_guard.validate_exact_raw_bytes(&source_bytes))
        .and_then(|()| operator_guard.validate_run(&source.run))
        .and_then(|()| operator_guard.validate_recovery_fields(&source.actor, &source.reason))
        .and_then(|()| operator_guard.validate_recovery_fields(&recovery.actor, &recovery.reason))
        .map_err(RecoveryError::invalid)?;
    let errors = seaf_core::validate_loop_run(&source.run);
    if !errors.is_empty()
        || source.run.status != LoopStatus::Approved
        || source.run.current_step != LoopStepName::Testing
    {
        return Err(RecoveryError::invalid(
            "evaluation recovery source is not exact Approved Testing authority",
        ));
    }
    validate_authoritative_provider_exchange_records(workspace, &source.run)
        .map_err(RecoveryError::wrapped)?;
    validate_evaluation_source_bindings(&source, &recovery)?;
    validate_evaluation_prefix(workspace, &source, &recovery)?;
    Ok((recovery, source))
}

pub(crate) fn load_verified_evaluation_invalidation(
    workspace: &LoopWorkspace,
    reference: &RecoveryReference,
) -> Result<
    Option<(
        EvaluationInvalidationAttemptV3,
        EvaluationInvalidationSourceRunV3,
    )>,
    RecoveryError,
> {
    match load_verified_any_recovery_lineage(workspace, reference)? {
        VerifiedRecoveryLineage::Invalidation { recovery, source } => Ok(Some((recovery, *source))),
        VerifiedRecoveryLineage::Provider { .. } | VerifiedRecoveryLineage::Evaluation { .. } => {
            Ok(None)
        }
    }
}

pub fn load_verified_recovery_authority_kind(
    workspace: &LoopWorkspace,
    reference: &RecoveryReference,
) -> Result<RecoveryAuthorityKind, RecoveryError> {
    match load_verified_any_recovery_lineage(workspace, reference)? {
        VerifiedRecoveryLineage::Provider { .. } => Ok(RecoveryAuthorityKind::ProviderV1),
        VerifiedRecoveryLineage::Evaluation { .. } => {
            Ok(RecoveryAuthorityKind::EvaluationAdoptionV2)
        }
        VerifiedRecoveryLineage::Invalidation { .. } => {
            Ok(RecoveryAuthorityKind::EvaluationInvalidationV3)
        }
    }
}

fn load_verified_evaluation_invalidation_lineage(
    workspace: &LoopWorkspace,
    reference: &RecoveryReference,
    bytes: &[u8],
) -> Result<
    (
        EvaluationInvalidationAttemptV3,
        EvaluationInvalidationSourceRunV3,
    ),
    RecoveryError,
> {
    let recovery: EvaluationInvalidationAttemptV3 =
        serde_json::from_slice(bytes).map_err(RecoveryError::wrapped)?;
    if canonical_json_bytes(&recovery).map_err(RecoveryError::wrapped)? != bytes {
        return Err(RecoveryError::invalid(
            "evaluation invalidation is not canonical JSON",
        ));
    }
    validate_evaluation_invalidation_contract(&recovery)?;
    if recovery.recovery_id != reference.recovery_id
        || reference.artifact.path != recovery_path(reference.recovery_id)
        || digest_bytes(bytes) != reference.artifact.digest
    {
        return Err(RecoveryError::invalid(
            "evaluation invalidation reference binding mismatch",
        ));
    }
    let source_bytes = read_verified_regular_file(
        workspace.run_directory(),
        &recovery.source_run.path,
        "evaluation invalidation source",
    )
    .map_err(RecoveryError::wrapped)?;
    if digest_bytes(&source_bytes) != recovery.source_run.digest {
        return Err(RecoveryError::invalid(
            "evaluation invalidation source digest mismatch",
        ));
    }
    let source: EvaluationInvalidationSourceRunV3 =
        serde_json::from_slice(&source_bytes).map_err(RecoveryError::wrapped)?;
    if canonical_json_bytes(&source).map_err(RecoveryError::wrapped)? != source_bytes
        || source.schema_version != EVALUATION_INVALIDATION_SCHEMA_VERSION
        || source.recovery_id != recovery.recovery_id
        || source.actor != recovery.actor
        || source.reason != recovery.reason
        || source.created_at != recovery.created_at
        || canonical_sha256_digest(&source.run).map_err(RecoveryError::wrapped)?
            != recovery.source_run_digest
        || canonical_sha256_digest(&source.approved_run).map_err(RecoveryError::wrapped)?
            != recovery.approved_run_digest
        || source.evaluation_prefix.evaluation_attempt != recovery.invalidated_attempt
        || source.evaluation_prefix.present_artifacts != recovery.present_artifacts
    {
        return Err(RecoveryError::invalid(
            "evaluation invalidation source binding mismatch",
        ));
    }
    let operator_guard =
        crate::operator_evidence::OperatorEvidenceGuard::load(workspace, &source.approved_run)
            .map_err(RecoveryError::invalid)?;
    operator_guard
        .validate_exact_raw_bytes(bytes)
        .and_then(|()| operator_guard.validate_exact_raw_bytes(&source_bytes))
        .and_then(|()| operator_guard.validate_run(&source.run))
        .and_then(|()| operator_guard.validate_run(&source.approved_run))
        .and_then(|()| operator_guard.validate_recovery_fields(&source.actor, &source.reason))
        .and_then(|()| operator_guard.validate_recovery_fields(&recovery.actor, &recovery.reason))
        .map_err(RecoveryError::invalid)?;
    let source_errors = seaf_core::validate_loop_run(&source.run);
    let approved_errors = seaf_core::validate_loop_run(&source.approved_run);
    if !source_errors.is_empty()
        || !approved_errors.is_empty()
        || source.approved_run.status != LoopStatus::Approved
        || source.approved_run.current_step != LoopStepName::Testing
    {
        return Err(RecoveryError::invalid(
            "evaluation invalidation source contains invalid run authority",
        ));
    }
    validate_invalidation_source_bindings(&source, &recovery)?;
    let inventory = EvaluationAttemptInventory::load_for_invalidation(workspace)
        .map_err(RecoveryError::invalid)?;
    let prefix = inventory
        .invalidation_prefix_paths_for(recovery.invalidated_attempt)
        .map_err(RecoveryError::invalid)?;
    let spelling = if fixed_spelling(prefix.spelling) {
        EvaluationPrefixSpellingV1::FixedV1
    } else {
        EvaluationPrefixSpellingV1::IndexedV2
    };
    if spelling != source.evaluation_prefix.spelling
        || prefix.paths
            != recovery
                .present_artifacts
                .iter()
                .map(|artifact| artifact.path.clone())
                .collect::<Vec<_>>()
    {
        return Err(RecoveryError::invalid(
            "evaluation invalidation manifest is not the exact ordered prefix",
        ));
    }
    let eval_config = load_recovery_eval_config(workspace, &source.approved_run)?;
    let verified_manifest = validate_invalidation_prefix(
        workspace,
        &source.approved_run,
        &prefix,
        &eval_config.evals.required,
        &recovery.source_worktree_state_digest,
        (source.run.status == LoopStatus::Failed).then_some(false),
        &operator_guard,
    )?;
    if verified_manifest != recovery.present_artifacts {
        return Err(RecoveryError::invalid(
            "evaluation invalidation manifest bytes changed",
        ));
    }
    for artifact in &recovery.present_artifacts {
        let actual = read_verified_regular_file(
            workspace.run_directory(),
            &artifact.path,
            "evaluation invalidation prefix artifact",
        )
        .map_err(RecoveryError::wrapped)?;
        if digest_bytes(&actual) != artifact.digest {
            return Err(RecoveryError::invalid(
                "evaluation invalidation prefix artifact digest mismatch",
            ));
        }
    }
    let mut projection =
        build_evaluation_invalidation_reset(&source.approved_run, &recovery.created_at);
    projection.latest_recovery = Some(RecoveryReference {
        recovery_id: recovery.recovery_id,
        artifact: ArtifactReference {
            path: recovery_path(recovery.recovery_id),
            digest: "0".repeat(64),
        },
    });
    if canonical_sha256_digest(&projection).map_err(RecoveryError::wrapped)?
        != recovery.expected_reset_projection_digest
    {
        return Err(RecoveryError::invalid(
            "evaluation invalidation reset projection digest mismatch",
        ));
    }
    Ok((recovery, source))
}

fn validate_evaluation_source_prior_consumption(
    workspace: &LoopWorkspace,
    source: &LoopRun,
) -> Result<(), RecoveryError> {
    let Some(reference) = source.latest_recovery.as_ref() else {
        return Ok(());
    };
    match load_verified_any_recovery_entry(workspace, reference)? {
        VerifiedRecoveryLineage::Provider { .. } => {
            if !recovery_is_consumed(workspace, source, reference)? {
                return Err(RecoveryError::invalid(
                    "evaluation recovery source carries an unconsumed provider-v1 recovery",
                ));
            }
        }
        VerifiedRecoveryLineage::Evaluation { .. } => {
            return Err(RecoveryError::invalid(
                "evaluation adoption cannot descend from evaluation-v2 recovery authority",
            ));
        }
        VerifiedRecoveryLineage::Invalidation { .. } => {
            if !invalidation_is_consumed(workspace, source, reference)? {
                return Err(RecoveryError::invalid(
                    "evaluation adoption source carries an unconsumed invalidation",
                ));
            }
        }
    }
    Ok(())
}

fn validate_evaluation_recovery_contract(
    recovery: &EvaluationRecoveryAttemptV2,
    reference: &RecoveryReference,
) -> Result<(), RecoveryError> {
    validate_note("actor", &recovery.actor, 256)?;
    validate_note("reason", &recovery.reason, 1024)?;
    let canonical_timestamp = recovery
        .created_at
        .parse::<u64>()
        .ok()
        .is_some_and(|value| value.to_string() == recovery.created_at);
    let previous_recovery_valid = match (&recovery.previous_recovery, recovery.recovery_id) {
        (None, 1) => true,
        (Some(previous), id) if id > 1 => {
            previous.recovery_id.checked_add(1) == Some(id)
                && previous.artifact.path == recovery_path(previous.recovery_id)
                && is_lower_hex_digest(&previous.artifact.digest)
        }
        _ => false,
    };
    if recovery.schema_version != EVALUATION_RECOVERY_SCHEMA_VERSION
        || recovery.recovery_id == 0
        || recovery.action != EvaluationRecoveryAction::AdoptApprovedEvaluation
        || recovery.step != LoopStepName::Testing
        || !canonical_timestamp
        || !previous_recovery_valid
        || recovery.evaluation_attempt == 0
        || recovery.source_run.path != recovery_source_path(recovery.recovery_id)
        || recovery.recovery_id != reference.recovery_id
        || reference.artifact.path != recovery_path(recovery.recovery_id)
        || !is_lower_hex_digest(&reference.artifact.digest)
        || !is_lower_hex_digest(&recovery.source_run.digest)
        || !is_lower_hex_digest(&recovery.source_run_digest)
        || !is_lower_hex_digest(&recovery.execution_intent.digest)
        || !is_lower_hex_digest(&recovery.testing_evidence.digest)
        || !is_lower_hex_digest(&recovery.eval_report.digest)
        || !is_lower_hex_digest(&recovery.candidate_state_digest)
        || !is_lower_hex_digest(&recovery.candidate_diff_digest)
        || !is_lower_hex_digest(&recovery.source_worktree_state_digest)
        || !is_git_object_id(&recovery.candidate_head)
        || !is_git_object_id(&recovery.candidate_tree)
        || !is_lower_hex_digest(&recovery.expected_final_projection_digest)
        || !is_lower_hex_digest(&recovery.input_digests.ticket)
        || !is_lower_hex_digest(&recovery.input_digests.policy)
        || !is_lower_hex_digest(&recovery.input_digests.config)
        || !is_lower_hex_digest(&recovery.input_digests.repository)
        || recovery
            .input_digests
            .eval_config
            .as_ref()
            .is_none_or(|digest| !is_lower_hex_digest(digest))
    {
        return Err(RecoveryError::invalid(
            "evaluation recovery contract fields are invalid",
        ));
    }
    Ok(())
}

fn validate_evaluation_invalidation_contract(
    recovery: &EvaluationInvalidationAttemptV3,
) -> Result<(), RecoveryError> {
    validate_note("actor", &recovery.actor, 256)?;
    validate_note("reason", &recovery.reason, 1024)?;
    let previous_valid = match (&recovery.previous_recovery, recovery.recovery_id) {
        (None, 1) => true,
        (Some(previous), id) if id > 1 => {
            previous.recovery_id.checked_add(1) == Some(id)
                && previous.artifact.path == recovery_path(previous.recovery_id)
                && is_lower_hex_digest(&previous.artifact.digest)
        }
        _ => false,
    };
    let artifacts_valid = !recovery.present_artifacts.is_empty()
        && recovery.present_artifacts.iter().all(|artifact| {
            is_lower_hex_digest(&artifact.digest)
                && (artifact.path.starts_with("artifacts/07-testing")
                    || artifact.path.starts_with("artifacts/08-eval-report"))
        });
    if recovery.schema_version != EVALUATION_INVALIDATION_SCHEMA_VERSION
        || recovery.recovery_id == 0
        || recovery.action != EvaluationInvalidationAction::InvalidateApprovedEvaluation
        || recovery.step != LoopStepName::Testing
        || parse_canonical_timestamp(&recovery.created_at).is_none()
        || !previous_valid
        || recovery.invalidated_attempt == 0
        || recovery.next_evaluation_attempt
            != recovery.invalidated_attempt.checked_add(1).unwrap_or(0)
        || recovery.source_run.path != recovery_source_path(recovery.recovery_id)
        || !is_lower_hex_digest(&recovery.source_run.digest)
        || !is_lower_hex_digest(&recovery.source_run_digest)
        || !is_lower_hex_digest(&recovery.approved_run_digest)
        || !is_lower_hex_digest(&recovery.candidate_state_digest)
        || !is_git_object_id(&recovery.candidate_head)
        || !is_git_object_id(&recovery.candidate_tree)
        || !is_lower_hex_digest(&recovery.candidate_diff_digest)
        || !is_lower_hex_digest(&recovery.source_worktree_state_digest)
        || !is_lower_hex_digest(&recovery.expected_reset_projection_digest)
        || !is_lower_hex_digest(&recovery.input_digests.ticket)
        || !is_lower_hex_digest(&recovery.input_digests.policy)
        || !is_lower_hex_digest(&recovery.input_digests.config)
        || !is_lower_hex_digest(&recovery.input_digests.repository)
        || recovery
            .input_digests
            .eval_config
            .as_ref()
            .is_none_or(|digest| !is_lower_hex_digest(digest))
        || !artifacts_valid
    {
        return Err(RecoveryError::invalid(
            "evaluation invalidation contract fields are invalid",
        ));
    }
    Ok(())
}

fn validate_invalidation_source_bindings(
    source: &EvaluationInvalidationSourceRunV3,
    recovery: &EvaluationInvalidationAttemptV3,
) -> Result<(), RecoveryError> {
    let candidate = source
        .approved_run
        .candidate_workspace
        .as_ref()
        .ok_or_else(|| {
            RecoveryError::invalid("evaluation invalidation source lost candidate authority")
        })?;
    if source.run.run_id != source.approved_run.run_id
        || recovery.run_id != source.run.run_id
        || source.run.input_digests != source.approved_run.input_digests
        || recovery.input_digests != source.run.input_digests
        || recovery.previous_recovery != source.run.latest_recovery
        || recovery.previous_provider_head != source.run.provider_exchange_records.last().cloned()
        || source.run.provider_exchange_records != source.approved_run.provider_exchange_records
        || source.run.candidate_workspace.as_ref() != Some(candidate)
        || recovery.candidate_state_digest
            != canonical_sha256_digest(candidate).map_err(RecoveryError::wrapped)?
        || recovery.candidate_head != candidate.candidate_head
        || recovery.candidate_tree != candidate.candidate_tree
        || recovery.candidate_diff_digest != candidate.candidate_diff_digest
    {
        return Err(RecoveryError::invalid(
            "evaluation invalidation fields do not bind exact source authority",
        ));
    }
    match source.run.status {
        LoopStatus::Approved => {
            if source.run != source.approved_run || source.prior_final.is_some() {
                return Err(RecoveryError::invalid(
                    "Approved invalidation source changed reconstructed authority",
                ));
            }
        }
        LoopStatus::Failed if source.run.human_approval.is_some() => {
            let prior = source.prior_final.as_ref().ok_or_else(|| {
                RecoveryError::invalid("Failed invalidation source lost final references")
            })?;
            let (expected_testing_path, expected_report_path) =
                match source.evaluation_prefix.spelling {
                    EvaluationPrefixSpellingV1::FixedV1 => (
                        "artifacts/07-testing.json".to_string(),
                        "artifacts/08-eval-report.json".to_string(),
                    ),
                    EvaluationPrefixSpellingV1::IndexedV2 => {
                        let paths = crate::evaluation_attempt::EvaluationAttemptPaths::indexed(
                            source.evaluation_prefix.evaluation_attempt,
                        )
                        .map_err(RecoveryError::invalid)?;
                        (paths.testing, paths.report)
                    }
                };
            let expected_testing = source
                .evaluation_prefix
                .present_artifacts
                .iter()
                .find(|reference| reference.path == expected_testing_path)
                .ok_or_else(|| {
                    RecoveryError::invalid("Failed prefix lost canonical Testing reference")
                })?;
            let expected_report = source
                .evaluation_prefix
                .present_artifacts
                .iter()
                .find(|reference| reference.path == expected_report_path)
                .ok_or_else(|| {
                    RecoveryError::invalid("Failed prefix lost canonical EvalReport reference")
                })?;
            if prior.testing_evidence != final_step_reference(&source.run, LoopStepName::Testing)?
                || prior.eval_report != final_step_reference(&source.run, LoopStepName::EvalReport)?
                || &prior.testing_evidence != expected_testing
                || &prior.eval_report != expected_report
                || source.run.eval_report_path.as_deref() != Some(expected_report_path.as_str())
            {
                return Err(RecoveryError::invalid(
                    "Failed invalidation source final references disagree",
                ));
            }
            let mut reconstructed = source.run.clone();
            reconstructed.status = LoopStatus::Approved;
            reconstructed.current_step = LoopStepName::Testing;
            reconstructed.updated_at = source.approved_run.updated_at.clone();
            reconstructed.eval_report_path = None;
            reconstructed.promotion = None;
            reconstructed.latest_recovery = source.approved_run.latest_recovery.clone();
            for name in [LoopStepName::Testing, LoopStepName::EvalReport] {
                let step = reconstructed
                    .steps
                    .iter_mut()
                    .find(|step| step.name == name)
                    .ok_or_else(|| RecoveryError::invalid("Failed source lost evaluation steps"))?;
                step.status = LoopStepStatus::Pending;
                step.artifact_path = None;
                step.artifact_digest = None;
            }
            if reconstructed != source.approved_run {
                return Err(RecoveryError::invalid(
                    "Failed invalidation source does not reconstruct exact Approved authority",
                ));
            }
        }
        _ => {
            return Err(RecoveryError::invalid(
                "evaluation invalidation source status is ineligible",
            ))
        }
    }
    Ok(())
}

fn validate_evaluation_source_bindings(
    source: &EvaluationRecoverySourceRunV2,
    recovery: &EvaluationRecoveryAttemptV2,
) -> Result<(), RecoveryError> {
    let run = &source.run;
    let candidate = run.candidate_workspace.as_ref().ok_or_else(|| {
        RecoveryError::invalid("evaluation recovery source lost candidate authority")
    })?;
    if recovery.run_id != run.run_id
        || recovery.input_digests != run.input_digests
        || recovery.previous_recovery != run.latest_recovery
        || recovery.previous_provider_head != run.provider_exchange_records.last().cloned()
        || recovery.candidate_state_digest
            != canonical_sha256_digest(candidate).map_err(RecoveryError::wrapped)?
        || recovery.candidate_head != candidate.candidate_head
        || recovery.candidate_tree != candidate.candidate_tree
        || recovery.candidate_diff_digest != candidate.candidate_diff_digest
        || recovery.execution_intent != source.evaluation_prefix.execution_intent
        || recovery.testing_evidence != source.evaluation_prefix.testing_evidence
        || recovery.evaluation_attempt != source.evaluation_prefix.evaluation_attempt
    {
        return Err(RecoveryError::invalid(
            "evaluation recovery fields do not bind the exact source authority",
        ));
    }
    match recovery.report_disposition {
        EvaluationRecoveryReportDisposition::VerifyExisting
            if source.evaluation_prefix.eval_report.as_ref() == Some(&recovery.eval_report) => {}
        EvaluationRecoveryReportDisposition::CreateMissing
            if source.evaluation_prefix.eval_report.is_none() => {}
        _ => {
            return Err(RecoveryError::invalid(
                "evaluation recovery report disposition does not match its source prefix",
            ))
        }
    }
    Ok(())
}

fn validate_evaluation_prefix(
    workspace: &LoopWorkspace,
    source: &EvaluationRecoverySourceRunV2,
    recovery: &EvaluationRecoveryAttemptV2,
) -> Result<(), RecoveryError> {
    let (attempt, spelling) =
        selected_attempt(&recovery.testing_evidence.path, &recovery.eval_report.path)
            .map_err(RecoveryError::invalid)?;
    let expected_spelling = if crate::evaluation_attempt::fixed_spelling(spelling) {
        EvaluationPrefixSpellingV1::FixedV1
    } else {
        EvaluationPrefixSpellingV1::IndexedV2
    };
    if attempt != recovery.evaluation_attempt
        || source.evaluation_prefix.spelling != expected_spelling
    {
        return Err(RecoveryError::invalid(
            "evaluation recovery prefix attempt or spelling mismatch",
        ));
    }
    let testing =
        TestingEvidence::load_for_approved_run(workspace, &recovery.testing_evidence, &source.run)
            .map_err(RecoveryError::wrapped)?;
    if testing.evaluation_attempt.unwrap_or(1) != attempt
        || testing
            .execution_intent
            .as_ref()
            .is_some_and(|reference| reference != &recovery.execution_intent)
    {
        return Err(RecoveryError::invalid(
            "evaluation recovery Testing reference substituted attempt authority",
        ));
    }
    let intent =
        load_intent(workspace, &recovery.execution_intent).map_err(RecoveryError::invalid)?;
    intent
        .validate_observed_check_names(&testing.checks)
        .map_err(RecoveryError::invalid)?;
    let eval_config = load_recovery_eval_config(workspace, &source.run)?;
    let expected_attempt_recovery = if attempt == 1 {
        None
    } else {
        source.run.latest_recovery.as_ref()
    };
    intent
        .validate_against_with_recovery(
            &source.run,
            &eval_config.evals.required,
            expected_attempt_recovery,
        )
        .map_err(RecoveryError::invalid)?;
    if intent.attempt() != attempt {
        return Err(RecoveryError::invalid(
            "evaluation recovery intent selects another attempt",
        ));
    }
    match (expected_spelling, &intent) {
        (EvaluationPrefixSpellingV1::FixedV1, ApprovedEvaluationIntent::V1(_))
            if testing.schema_version == 1
                && testing.evaluation_attempt.is_none()
                && testing.recovery.is_none()
                && testing.execution_intent.is_none() => {}
        (EvaluationPrefixSpellingV1::IndexedV2, intent)
            if intent.is_indexed()
                && testing.schema_version == 2
                && testing.evaluation_attempt == Some(attempt)
                && testing
                    .recovery
                    .as_ref()
                    .is_some_and(|recovery| recovery.as_ref() == expected_attempt_recovery)
                && testing.execution_intent.as_ref() == Some(&recovery.execution_intent)
                && intent.recovery() == expected_attempt_recovery => {}
        _ => {
            return Err(RecoveryError::invalid(
                "evaluation recovery prefix spelling and schema authority disagree",
            ))
        }
    }
    if let Some(source_digest) = intent.source_worktree_state_digest() {
        if source_digest != recovery.source_worktree_state_digest {
            return Err(RecoveryError::invalid(
                "evaluation recovery source worktree authority mismatch",
            ));
        }
    }
    let inventory = EvaluationAttemptInventory::load_for_invalidation(workspace)
        .map_err(RecoveryError::invalid)?;
    let report_present = inventory
        .require_recovery_prefix(
            attempt,
            &recovery.execution_intent.path,
            &recovery.testing_evidence.path,
            &recovery.eval_report.path,
            recovery.report_disposition == EvaluationRecoveryReportDisposition::CreateMissing,
            &testing.checks,
        )
        .map_err(RecoveryError::invalid)?;
    let mut references = vec![
        recovery.execution_intent.clone(),
        recovery.testing_evidence.clone(),
    ];
    if report_present {
        references.push(recovery.eval_report.clone());
    }
    for check in &testing.checks {
        for (path, digest) in [
            (check.stdout_path.as_ref(), check.stdout_digest.as_ref()),
            (check.stderr_path.as_ref(), check.stderr_digest.as_ref()),
        ] {
            references.push(ArtifactReference {
                path: path
                    .cloned()
                    .ok_or_else(|| RecoveryError::invalid("evaluation recovery lost log path"))?,
                digest: digest
                    .cloned()
                    .ok_or_else(|| RecoveryError::invalid("evaluation recovery lost log digest"))?,
            });
        }
    }
    for reference in &references {
        let bytes = read_verified_regular_file(
            workspace.run_directory(),
            &reference.path,
            "evaluation recovery prefix artifact",
        )
        .map_err(RecoveryError::wrapped)?;
        if digest_bytes(&bytes) != reference.digest {
            return Err(RecoveryError::invalid(
                "evaluation recovery prefix artifact digest mismatch",
            ));
        }
    }
    Ok(())
}

fn load_recovery_eval_config(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<seaf_core::EvalConfig, RecoveryError> {
    let expected = run.input_digests.eval_config.as_ref().ok_or_else(|| {
        RecoveryError::invalid("evaluation recovery source lost eval config digest")
    })?;
    let bytes = read_verified_regular_file(
        workspace.run_directory(),
        "inputs/eval-config.json",
        "evaluation recovery eval config",
    )
    .map_err(RecoveryError::wrapped)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(RecoveryError::wrapped)?;
    if canonical_json_bytes(&value).map_err(RecoveryError::wrapped)? != bytes
        || canonical_sha256_digest(&value).map_err(RecoveryError::wrapped)? != *expected
    {
        return Err(RecoveryError::invalid(
            "evaluation recovery eval config bytes or digest mismatch",
        ));
    }
    let config: seaf_core::EvalConfig =
        serde_json::from_value(value).map_err(RecoveryError::wrapped)?;
    seaf_core::validate_eval_config(&config).map_err(RecoveryError::wrapped)?;
    Ok(config)
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
        reset
            .policy_decisions
            .retain(|decision| decision.patch_id != run_id);
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
    operator_guard: &crate::operator_evidence::OperatorEvidenceGuard,
) -> Result<String, RecoveryError> {
    match fs::symlink_metadata(workspace.run_directory().join(path)) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            RecoveryError::invalid("recovery orphan is not a real regular file"),
        ),
        Ok(_) => {
            let bytes =
                read_verified_regular_file(workspace.run_directory(), path, "recovery orphan")
                    .map_err(RecoveryError::wrapped)?;
            operator_guard
                .validate_exact_raw_bytes(&bytes)
                .map_err(RecoveryError::invalid)?;
            let recovery: RecoveryAttemptV1 =
                serde_json::from_slice(&bytes).map_err(RecoveryError::wrapped)?;
            if canonical_json_bytes(&recovery).map_err(RecoveryError::wrapped)? != bytes {
                return Err(RecoveryError::invalid(
                    "recovery orphan is not canonical JSON",
                ));
            }
            operator_guard
                .validate_recovery_fields(&recovery.actor, &recovery.reason)
                .map_err(RecoveryError::invalid)?;
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

fn publish_invalidation_source_activating_if_needed(
    workspace: &LoopWorkspace,
    expected: &LoopRun,
    recovery_id: u32,
    source_path: &str,
    source_bytes: &[u8],
) -> Result<(), std::io::Error> {
    let guard = crate::run_persistence::RunMutationGuard::acquire(workspace.run_directory())
        .map_err(std::io::Error::other)?;
    let current = state::load_run(workspace).map_err(std::io::Error::other)?;
    if &current != expected {
        return Err(std::io::Error::other(
            "evaluation invalidation authority changed before source publication",
        ));
    }
    match crate::storage_authority::derive_active_storage_commitment(workspace.run_directory())
        .map_err(std::io::Error::other)?
    {
        Some(_) => {
            crate::immutable_artifact::publish_create_only_with_guard_consuming_evaluation_slot(
                &guard,
                source_path,
                source_bytes,
            )
            .map_err(std::io::Error::other)?;
        }
        None => {
            let commitment =
                crate::evaluation_storage::derive_invalidation_source_activation_commitment(
                    &current,
                    recovery_id,
                    source_path,
                    source_bytes,
                )
                .map_err(std::io::Error::other)?;
            guard
                .validate_create_activating_commitment(source_path, source_bytes.len(), &commitment)
                .map_err(std::io::Error::other)?;
            publish_create_only_with_guard_after_commitment_projection(
                &guard,
                source_path,
                source_bytes,
            )
            .map_err(std::io::Error::other)?;
        }
    }
    guard
        .validate_active_storage_commitment()
        .map_err(std::io::Error::other)?;
    guard.unlock().map_err(std::io::Error::other)
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
    fn create_missing_recovery_rejects_malformed_expected_report_digest_before_publication() {
        let recovery_path = recovery_path(1);
        let reference = RecoveryReference {
            recovery_id: 1,
            artifact: ArtifactReference {
                path: recovery_path,
                digest: "a".repeat(64),
            },
        };
        let mut recovery = EvaluationRecoveryAttemptV2 {
            schema_version: EVALUATION_RECOVERY_SCHEMA_VERSION,
            recovery_id: 1,
            run_id: "missing-report".into(),
            action: EvaluationRecoveryAction::AdoptApprovedEvaluation,
            step: LoopStepName::Testing,
            actor: "reviewer@example.invalid".into(),
            reason: "adopt complete prefix".into(),
            created_at: "1".into(),
            source_run: ArtifactReference {
                path: recovery_source_path(1),
                digest: "b".repeat(64),
            },
            source_run_digest: "c".repeat(64),
            input_digests: LoopInputDigests {
                ticket: "d".repeat(64),
                policy: "e".repeat(64),
                config: "f".repeat(64),
                repository: "1".repeat(64),
                eval_config: Some("2".repeat(64)),
            },
            candidate_state_digest: "3".repeat(64),
            candidate_head: "4".repeat(40),
            candidate_tree: "5".repeat(40),
            candidate_diff_digest: "6".repeat(64),
            source_worktree_state_digest: "7".repeat(64),
            evaluation_attempt: 1,
            execution_intent: ArtifactReference {
                path: "artifacts/07-testing.attempt-001.execution-intent.json".into(),
                digest: "8".repeat(64),
            },
            testing_evidence: ArtifactReference {
                path: "artifacts/07-testing.attempt-001.json".into(),
                digest: "9".repeat(64),
            },
            eval_report: ArtifactReference {
                path: "artifacts/08-eval-report.attempt-001.json".into(),
                digest: "a".repeat(64),
            },
            report_disposition: EvaluationRecoveryReportDisposition::CreateMissing,
            previous_recovery: None,
            previous_provider_head: None,
            expected_final_projection_digest: "b".repeat(64),
        };
        validate_evaluation_recovery_contract(&recovery, &reference).unwrap();

        recovery.eval_report.digest = "not-a-digest".into();
        assert!(validate_evaluation_recovery_contract(&recovery, &reference).is_err());
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
