use std::{error::Error, fmt};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, is_portable_artifact_path, validate_eval_report,
    validate_loop_run, ArtifactReference, EvalConfig, EvalDecision, EvalReport, LoopRun,
    LoopStatus, LoopStepName, LoopStepStatus,
};
use serde_json::Value;

use crate::{
    evaluation_attempt::{
        fixed_spelling, load_intent, reference_for_path, selected_attempt,
        ApprovedEvaluationIntent, EvaluationAttemptInventory,
    },
    LoopWorkspace, TestingEvidence,
};

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedFinalEvaluationAuthority {
    approved_run: LoopRun,
    testing_evidence: TestingEvidence,
    eval_report: EvalReport,
    execution_intent: ApprovedEvaluationIntent,
    execution_intent_reference: ArtifactReference,
}

impl VerifiedFinalEvaluationAuthority {
    pub fn approved_run(&self) -> &LoopRun {
        &self.approved_run
    }

    pub fn testing_evidence(&self) -> &TestingEvidence {
        &self.testing_evidence
    }

    pub fn eval_report(&self) -> &EvalReport {
        &self.eval_report
    }

    pub(crate) fn execution_intent(&self) -> &ApprovedEvaluationIntent {
        &self.execution_intent
    }

    pub(crate) fn execution_intent_reference(&self) -> &ArtifactReference {
        &self.execution_intent_reference
    }
}

pub fn load_verified_final_evaluation_authority(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<VerifiedFinalEvaluationAuthority, FinalEvaluationAuthorityError> {
    if workspace
        .run_directory()
        .file_name()
        .and_then(|name| name.to_str())
        != Some(run.run_id.as_str())
    {
        return Err(FinalEvaluationAuthorityError::invalid(
            "final LoopRun run_id does not match the authoritative run directory",
        ));
    }
    let passed = match run.status {
        LoopStatus::EvalPassed | LoopStatus::Promoted => true,
        LoopStatus::Failed if run.human_approval.is_some() => false,
        _ => {
            return Err(FinalEvaluationAuthorityError::invalid(
                "LoopRun is not final integrated evaluation authority",
            ));
        }
    };
    let run_errors = validate_loop_run(run);
    if !run_errors.is_empty() {
        return Err(FinalEvaluationAuthorityError::invalid(format!(
            "invalid final LoopRun: {}",
            format_field_errors(run_errors)
        )));
    }
    let evaluation_run;
    let run = if run.status == LoopStatus::Promoted {
        let promotion = run.promotion.as_ref().ok_or_else(|| {
            FinalEvaluationAuthorityError::invalid("Promoted authority lost promotion evidence")
        })?;
        evaluation_run = {
            let mut predecessor = run.clone();
            predecessor.status = LoopStatus::EvalPassed;
            predecessor.updated_at = promotion.eval_passed_updated_at.clone();
            predecessor.promotion = None;
            predecessor
        };
        &evaluation_run
    } else {
        run
    };

    let inventory = EvaluationAttemptInventory::load_for_invalidation(workspace)
        .map_err(FinalEvaluationAuthorityError::invalid)?;
    let testing_reference = step_artifact_reference(run, LoopStepName::Testing)?;
    let report_reference = step_artifact_reference(run, LoopStepName::EvalReport)?;
    let (evaluation_attempt, spelling) =
        selected_attempt(&testing_reference.path, &report_reference.path)
            .map_err(FinalEvaluationAuthorityError::invalid)?;
    inventory
        .require_selected(
            evaluation_attempt,
            &testing_reference.path,
            &report_reference.path,
        )
        .map_err(FinalEvaluationAuthorityError::invalid)?;
    if run.eval_report_path.as_deref() != Some(report_reference.path.as_str()) {
        return Err(FinalEvaluationAuthorityError::invalid(
            "LoopRun eval_report_path does not select the EvalReport artifact",
        ));
    }

    let approved_run = reconstruct_approved_authority(workspace, run)?;
    let testing_evidence =
        TestingEvidence::load_for_approved_run(workspace, &testing_reference, &approved_run)
            .map_err(|error| {
                FinalEvaluationAuthorityError::invalid(format!(
                    "invalid Testing evidence authority: {error}"
                ))
            })?;
    match (
        testing_evidence.schema_version,
        testing_evidence.evaluation_attempt,
        fixed_spelling(spelling),
    ) {
        (1, None, true) => {}
        (2, Some(attempt), false) if attempt == evaluation_attempt => {}
        _ => {
            return Err(FinalEvaluationAuthorityError::invalid(
                "Testing evidence schema does not match selected evaluation attempt path",
            ))
        }
    }
    inventory
        .validate_selected_logs(evaluation_attempt, &testing_evidence.checks)
        .map_err(FinalEvaluationAuthorityError::invalid)?;
    let execution_intent_reference = match testing_evidence.execution_intent.clone() {
        Some(reference) => reference,
        None => reference_for_path(
            workspace,
            inventory.intent_path(evaluation_attempt).ok_or_else(|| {
                FinalEvaluationAuthorityError::invalid("evaluation attempt lost execution intent")
            })?,
        )
        .map_err(FinalEvaluationAuthorityError::invalid)?,
    };
    let execution_intent = load_intent(workspace, &execution_intent_reference)
        .map_err(FinalEvaluationAuthorityError::invalid)?;
    if execution_intent.attempt() != evaluation_attempt {
        return Err(FinalEvaluationAuthorityError::invalid(
            "Testing evidence selects a cross-attempt execution intent",
        ));
    }
    if testing_evidence.schema_version == 2
        && testing_evidence
            .recovery
            .as_ref()
            .and_then(|recovery| recovery.as_ref())
            != execution_intent.recovery()
    {
        return Err(FinalEvaluationAuthorityError::invalid(
            "Testing evidence recovery does not match its execution intent",
        ));
    }
    let eval_config = load_eval_config(workspace, run)?;
    execution_intent
        .validate_against_with_recovery(
            &approved_run,
            &eval_config.evals.required,
            testing_evidence
                .recovery
                .as_ref()
                .and_then(|recovery| recovery.as_ref()),
        )
        .map_err(FinalEvaluationAuthorityError::invalid)?;
    let eval_report = load_verified_eval_report(workspace, &report_reference)?;
    let loop_evidence = eval_report.loop_evidence.as_ref().ok_or_else(|| {
        FinalEvaluationAuthorityError::invalid("final EvalReport requires integrated loop evidence")
    })?;
    let approval = run.human_approval.as_ref().ok_or_else(|| {
        FinalEvaluationAuthorityError::invalid(
            "final evaluation authority lost human approval evidence",
        )
    })?;
    let eval_config_digest = run.input_digests.eval_config.as_ref().ok_or_else(|| {
        FinalEvaluationAuthorityError::invalid("final evaluation authority lost eval config digest")
    })?;
    let human_approval_digest = canonical_sha256_digest(approval)
        .map_err(|error| FinalEvaluationAuthorityError::invalid(error.to_string()))?;

    if eval_report.patch_id != run.run_id
        || eval_report.goal_id != run.goal_id
        || loop_evidence.run_id != run.run_id
        || loop_evidence.ticket_id != run.ticket_id
        || loop_evidence.ticket_digest != run.input_digests.ticket
        || loop_evidence.eval_config.path != "inputs/eval-config.json"
        || loop_evidence.eval_config.digest != *eval_config_digest
        || loop_evidence.candidate_diff != approval.candidate_diff
        || loop_evidence.starting_head != approval.starting_head
        || loop_evidence.human_approval_digest != human_approval_digest
        || loop_evidence.policy_decision_digest != approval.policy_decision_digest
        || loop_evidence.testing_evidence != testing_reference
    {
        return Err(FinalEvaluationAuthorityError::invalid(
            "EvalReport integrated bindings do not match the final LoopRun authority",
        ));
    }
    if eval_report.checks != testing_evidence.checks {
        return Err(FinalEvaluationAuthorityError::invalid(
            "EvalReport checks do not exactly match ordered Testing evidence checks",
        ));
    }
    if testing_evidence.passed != passed || eval_report.passed != passed {
        return Err(FinalEvaluationAuthorityError::invalid(
            "Testing evidence and EvalReport aggregate do not match final LoopRun status",
        ));
    }
    match (passed, eval_report.decision) {
        (true, EvalDecision::Reject) => {
            return Err(FinalEvaluationAuthorityError::invalid(
                "passing final authority cannot use a rejecting EvalReport",
            ));
        }
        (false, decision) if decision != EvalDecision::Reject => {
            return Err(FinalEvaluationAuthorityError::invalid(
                "reported evaluation failure requires a rejecting EvalReport",
            ));
        }
        _ => {}
    }

    Ok(VerifiedFinalEvaluationAuthority {
        approved_run,
        testing_evidence,
        eval_report,
        execution_intent,
        execution_intent_reference,
    })
}

fn load_eval_config(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<EvalConfig, FinalEvaluationAuthorityError> {
    let digest = run.input_digests.eval_config.as_ref().ok_or_else(|| {
        FinalEvaluationAuthorityError::invalid("final authority lost eval config digest")
    })?;
    let bytes = crate::immutable_artifact::read_verified_regular_file(
        workspace.run_directory(),
        "inputs/eval-config.json",
        "final evaluation config",
    )
    .map_err(|error| FinalEvaluationAuthorityError::invalid(error.to_string()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| FinalEvaluationAuthorityError::invalid(error.to_string()))?;
    if canonical_json_bytes(&value)
        .map_err(|error| FinalEvaluationAuthorityError::invalid(error.to_string()))?
        != bytes
        || canonical_sha256_digest(&value)
            .map_err(|error| FinalEvaluationAuthorityError::invalid(error.to_string()))?
            != *digest
    {
        return Err(FinalEvaluationAuthorityError::invalid(
            "final evaluation config bytes or digest mismatch",
        ));
    }
    serde_json::from_value(value)
        .map_err(|error| FinalEvaluationAuthorityError::invalid(error.to_string()))
}

fn reconstruct_approved_authority(
    workspace: &LoopWorkspace,
    final_run: &LoopRun,
) -> Result<LoopRun, FinalEvaluationAuthorityError> {
    let approval = final_run.human_approval.as_ref().ok_or_else(|| {
        FinalEvaluationAuthorityError::invalid(
            "final evaluation authority lost human approval evidence",
        )
    })?;
    let mut approved = final_run.clone();
    approved.status = LoopStatus::Approved;
    approved.current_step = LoopStepName::Testing;
    approved.updated_at = approval.approved_at.clone();
    approved.eval_report_path = None;
    for step in [LoopStepName::Testing, LoopStepName::EvalReport] {
        let record = approved
            .steps
            .iter_mut()
            .find(|record| record.name == step)
            .ok_or_else(|| {
                FinalEvaluationAuthorityError::invalid(
                    "final evaluation authority lost its exact step chain",
                )
            })?;
        record.status = LoopStepStatus::Pending;
        record.artifact_path = None;
        record.artifact_digest = None;
    }
    if let Some(candidate) = approved.candidate_workspace.as_mut() {
        candidate.lifecycle = seaf_core::CandidateWorkspaceLifecycle::Active;
        candidate.cleanup_started_at = None;
        candidate.cleaned_at = None;
    }
    let recovery_source =
        crate::recovery::load_evaluation_recovery_source_for_final(workspace, final_run)
            .map_err(|error| FinalEvaluationAuthorityError::invalid(error.to_string()))?;
    if let Some(source) = recovery_source {
        approved.latest_recovery = source.latest_recovery.clone();
        approved.updated_at = source.updated_at.clone();
        if approved != source {
            return Err(FinalEvaluationAuthorityError::invalid(
                "final LoopRun is not an allowed descendant of its evaluation recovery source",
            ));
        }
        approved = source;
    }
    let errors = validate_loop_run(&approved);
    if !errors.is_empty() {
        return Err(FinalEvaluationAuthorityError::invalid(format!(
            "could not reconstruct exact Approved authority: {}",
            format_field_errors(errors)
        )));
    }
    Ok(approved)
}

fn step_artifact_reference(
    run: &LoopRun,
    name: LoopStepName,
) -> Result<ArtifactReference, FinalEvaluationAuthorityError> {
    let record = run
        .steps
        .iter()
        .find(|record| record.name == name)
        .ok_or_else(|| {
            FinalEvaluationAuthorityError::invalid(format!(
                "final evaluation authority has no {name:?} step"
            ))
        })?;
    let path = record.artifact_path.clone().ok_or_else(|| {
        FinalEvaluationAuthorityError::invalid(format!("final {name:?} step has no artifact path"))
    })?;
    let digest = record.artifact_digest.clone().ok_or_else(|| {
        FinalEvaluationAuthorityError::invalid(format!(
            "final {name:?} step has no artifact digest"
        ))
    })?;
    Ok(ArtifactReference { path, digest })
}

fn load_verified_eval_report(
    workspace: &LoopWorkspace,
    reference: &ArtifactReference,
) -> Result<EvalReport, FinalEvaluationAuthorityError> {
    if !is_portable_artifact_path(&reference.path) {
        return Err(FinalEvaluationAuthorityError::invalid(
            "EvalReport reference path is not strict portable relative spelling",
        ));
    }
    let bytes = crate::immutable_artifact::read_verified_regular_file(
        workspace.run_directory(),
        &reference.path,
        "final EvalReport",
    )
    .map_err(|error| FinalEvaluationAuthorityError::invalid(error.to_string()))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        FinalEvaluationAuthorityError::invalid(format!("invalid EvalReport JSON: {error}"))
    })?;
    if canonical_json_bytes(&value)
        .map_err(|error| FinalEvaluationAuthorityError::invalid(error.to_string()))?
        != bytes
    {
        return Err(FinalEvaluationAuthorityError::invalid(
            "final EvalReport is not canonical JSON",
        ));
    }
    if canonical_sha256_digest(&value)
        .map_err(|error| FinalEvaluationAuthorityError::invalid(error.to_string()))?
        != reference.digest
    {
        return Err(FinalEvaluationAuthorityError::invalid(
            "final EvalReport artifact digest mismatch",
        ));
    }
    let report: EvalReport = serde_json::from_value(value).map_err(|error| {
        FinalEvaluationAuthorityError::invalid(format!("invalid EvalReport schema: {error}"))
    })?;
    let errors = validate_eval_report(&report);
    if !errors.is_empty() {
        return Err(FinalEvaluationAuthorityError::invalid(format!(
            "invalid EvalReport: {}",
            format_field_errors(errors)
        )));
    }
    Ok(report)
}

fn format_field_errors(errors: Vec<seaf_core::FieldError>) -> String {
    errors
        .into_iter()
        .map(|error| format!("{}: {}", error.field, error.message))
        .collect::<Vec<_>>()
        .join("; ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalEvaluationAuthorityError {
    message: String,
}

impl FinalEvaluationAuthorityError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FinalEvaluationAuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for FinalEvaluationAuthorityError {}
