use std::{
    env,
    error::Error,
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, ArtifactReference, LoopRun, LoopStatus,
    LoopStepName, PromotionEvidence,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    candidate_workspace::{
        acquire_candidate_lock, acquire_repository_operation_lock,
        verify_candidate_patch_evidence_for_evaluation_locked, verify_worktree_matches_index,
        verify_worktree_matches_private_index, VerifiedCandidatePatchEvidence,
    },
    immutable_artifact::{publish_create_only, read_verified_regular_file},
    state, LoopWorkspace, VerifiedFinalEvaluationAuthority,
};

const PROMOTION_INTENT_PATH: &str = "artifacts/09-promotion.intent.json";
const PROMOTION_SCHEMA_VERSION: u32 = 1;
static INDEX_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromotionIntent {
    schema_version: u32,
    run_id: String,
    reviewer: String,
    started_at: String,
    candidate_diff: ArtifactReference,
    testing_evidence: ArtifactReference,
    eval_report: ArtifactReference,
    policy_decision_digest: String,
    target_head: String,
    eval_passed_run_digest: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromotionOutcome {
    pub run: LoopRun,
    pub evidence: PromotionEvidence,
}

pub fn promote_evaluated_candidate(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
    reviewer: &str,
    confirmed_candidate_diff_digest: &str,
    confirmed_eval_report_digest: &str,
    confirmed_target_head: &str,
) -> Result<PromotionOutcome, PromotionError> {
    validate_reviewer(reviewer)?;
    let candidate_lock = acquire_candidate_lock(workspace).map_err(PromotionError::wrapped)?;
    let result = promote_evaluated_candidate_locked(
        workspace,
        source_worktree_root,
        reviewer,
        confirmed_candidate_diff_digest,
        confirmed_eval_report_digest,
        confirmed_target_head,
        || Ok(()),
    );
    let unlock = candidate_lock.unlock();
    match (result, unlock) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Ok(_), Err(error)) => Err(PromotionError::wrapped(error)),
        (Err(error), _) => Err(error),
    }
}

fn promote_evaluated_candidate_locked<F>(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
    reviewer: &str,
    confirmed_candidate_diff_digest: &str,
    confirmed_eval_report_digest: &str,
    confirmed_target_head: &str,
    after_apply: F,
) -> Result<PromotionOutcome, PromotionError>
where
    F: FnOnce() -> Result<(), PromotionError>,
{
    let expected = state::load_run(workspace).map_err(PromotionError::wrapped)?;
    let operator_guard =
        crate::operator_evidence::OperatorEvidenceGuard::load(workspace, &expected)
            .map_err(PromotionError::invalid)?;
    operator_guard
        .validate_current_run_file(workspace)
        .and_then(|()| operator_guard.validate_run(&expected))
        .and_then(|()| operator_guard.validate_structural(reviewer))
        .map_err(PromotionError::invalid)?;
    if expected.status == LoopStatus::Promoted {
        return validate_exact_retry(
            workspace,
            source_worktree_root,
            expected,
            reviewer,
            confirmed_candidate_diff_digest,
            confirmed_eval_report_digest,
            confirmed_target_head,
        );
    }
    if expected.status != LoopStatus::EvalPassed {
        return Err(PromotionError::invalid(
            "promotion requires exact EvalPassed authority",
        ));
    }
    let verified = authenticate_promotion_authority(workspace, source_worktree_root, &expected)?;
    let bindings = promotion_bindings(&expected, &verified)?;
    validate_confirmations(
        &bindings,
        reviewer,
        confirmed_candidate_diff_digest,
        confirmed_eval_report_digest,
        confirmed_target_head,
    )?;

    let existing_intent = load_optional_intent(workspace, &operator_guard)?;
    if existing_intent.is_none() {
        require_clean_target(workspace, source_worktree_root, &bindings.target_head)?;
    }
    let (intent, publish_intent) = match existing_intent {
        Some(intent) => {
            validate_intent(&intent, reviewer, &bindings)?;
            (intent, false)
        }
        None => {
            let intent = PromotionIntent {
                schema_version: PROMOTION_SCHEMA_VERSION,
                run_id: expected.run_id.clone(),
                reviewer: reviewer.to_string(),
                started_at: now_timestamp()?,
                candidate_diff: bindings.candidate_diff.clone(),
                testing_evidence: bindings.testing_evidence.clone(),
                eval_report: bindings.eval_report.clone(),
                policy_decision_digest: bindings.policy_decision_digest.clone(),
                target_head: bindings.target_head.clone(),
                eval_passed_run_digest: bindings.eval_passed_run_digest.clone(),
            };
            (intent, true)
        }
    };
    let intent_bytes = operator_guard
        .validate_canonical_artifact(&intent)
        .map_err(PromotionError::invalid)?;
    let intent_reference = ArtifactReference {
        path: PROMOTION_INTENT_PATH.to_string(),
        digest: digest_bytes(&intent_bytes),
    };
    let evidence = PromotionEvidence {
        schema_version: PROMOTION_SCHEMA_VERSION,
        run_id: expected.run_id.clone(),
        reviewer: reviewer.to_string(),
        promoted_at: intent.started_at.clone(),
        intent: intent_reference.clone(),
        candidate_diff: bindings.candidate_diff.clone(),
        testing_evidence: bindings.testing_evidence.clone(),
        eval_report: bindings.eval_report.clone(),
        policy_decision_digest: bindings.policy_decision_digest.clone(),
        target_head: bindings.target_head.clone(),
        eval_passed_run_digest: bindings.eval_passed_run_digest.clone(),
        eval_passed_updated_at: expected.updated_at.clone(),
    };
    let mut intended = expected.clone();
    intended.status = LoopStatus::Promoted;
    intended.updated_at = evidence.promoted_at.clone();
    intended.promotion = Some(evidence.clone());
    operator_guard
        .validate_future_run(&intended)
        .map_err(PromotionError::invalid)?;
    if publish_intent {
        let current_guard =
            crate::operator_evidence::OperatorEvidenceGuard::load(workspace, &expected)
                .map_err(PromotionError::invalid)?;
        current_guard
            .validate_current_run_file(workspace)
            .and_then(|()| current_guard.validate_canonical_artifact(&intent).map(drop))
            .and_then(|()| current_guard.validate_future_run(&intended).map(drop))
            .map_err(PromotionError::invalid)?;
        publish_create_only(
            workspace.run_directory(),
            PROMOTION_INTENT_PATH,
            &intent_bytes,
        )
        .map_err(PromotionError::wrapped)?;
    }

    let candidate = expected.candidate_workspace.as_ref().ok_or_else(|| {
        PromotionError::invalid("EvalPassed authority has no candidate workspace")
    })?;
    let repository_lock = acquire_repository_operation_lock(Path::new(&candidate.git_common_dir))
        .map_err(PromotionError::wrapped)?;
    let result = (|| {
        let current = state::load_run(workspace).map_err(PromotionError::wrapped)?;
        if current != expected {
            return Err(PromotionError::invalid(
                "LoopRun changed before promotion mutation lock",
            ));
        }
        let reverified =
            authenticate_promotion_authority(workspace, source_worktree_root, &current)?;
        let current_operator_guard =
            crate::operator_evidence::OperatorEvidenceGuard::load(workspace, &current)
                .map_err(PromotionError::invalid)?;
        current_operator_guard
            .validate_current_run_file(workspace)
            .and_then(|()| current_operator_guard.validate_run(&current))
            .and_then(|()| current_operator_guard.validate_structural(reviewer))
            .and_then(|()| {
                current_operator_guard
                    .validate_canonical_artifact(&intent)
                    .map(drop)
            })
            .and_then(|()| {
                current_operator_guard
                    .validate_future_run(&intended)
                    .map(drop)
            })
            .map_err(PromotionError::invalid)?;
        let current_bindings = promotion_bindings(&current, &reverified)?;
        if current_bindings != bindings {
            return Err(PromotionError::invalid(
                "promotion authority changed inside repository operation lock",
            ));
        }
        validate_intent(&intent, reviewer, &current_bindings)?;
        load_exact_intent(
            workspace,
            &intent,
            &intent_reference,
            &current_operator_guard,
        )?;

        match classify_target(
            workspace,
            source_worktree_root,
            &current_bindings.target_head,
            current_bindings.patch_bytes.as_bytes(),
            &reverified,
        )? {
            TargetState::Clean => {
                let candidate_root = current
                    .candidate_workspace
                    .as_ref()
                    .ok_or_else(|| {
                        PromotionError::invalid("EvalPassed authority has no candidate workspace")
                    })?
                    .path
                    .as_str();
                let changed_paths = &reverified.policy_decision.changed_paths;
                let check_overrides = promotion_filter_driver_overrides(
                    source_worktree_root,
                    Path::new(candidate_root),
                    changed_paths,
                )?;
                git_apply(
                    source_worktree_root,
                    current_bindings.patch_bytes.as_bytes(),
                    true,
                    &check_overrides,
                )?;
                let apply_overrides = promotion_filter_driver_overrides(
                    source_worktree_root,
                    Path::new(candidate_root),
                    changed_paths,
                )?;
                git_apply(
                    source_worktree_root,
                    current_bindings.patch_bytes.as_bytes(),
                    false,
                    &apply_overrides,
                )?;
                after_apply()?;
            }
            TargetState::ExactPatch => {}
        }
        verify_exact_applied_target(
            workspace,
            source_worktree_root,
            &current_bindings.target_head,
            current_bindings.patch_bytes.as_bytes(),
            &reverified,
        )?;

        crate::provider_exchange::persist_run_with_full_compare_and_validator(
            workspace,
            &current,
            &intended,
            |locked| {
                if locked != &current {
                    return Err(crate::provider_exchange::ProviderExchangeError::Invalid(
                        "EvalPassed authority changed before Promoted publication".to_string(),
                    ));
                }
                let latest = {
                    let operator_guard =
                        crate::operator_evidence::OperatorEvidenceGuard::load(workspace, locked)
                            .map_err(crate::provider_exchange::ProviderExchangeError::Invalid)?;
                    operator_guard
                        .validate_current_run_file(workspace)
                        .and_then(|()| operator_guard.validate_run(locked))
                        .and_then(|()| operator_guard.validate_structural(reviewer))
                        .and_then(|()| operator_guard.validate_future_run(&intended).map(drop))
                        .map_err(crate::provider_exchange::ProviderExchangeError::Invalid)?;
                    load_exact_intent(workspace, &intent, &intent_reference, &operator_guard)
                        .map_err(|error| {
                            crate::provider_exchange::ProviderExchangeError::Invalid(
                                error.to_string(),
                            )
                        })?
                };
                validate_intent(&latest, reviewer, &current_bindings).map_err(|error| {
                    crate::provider_exchange::ProviderExchangeError::Invalid(error.to_string())
                })?;
                let latest_candidate =
                    authenticate_promotion_authority(workspace, source_worktree_root, locked)
                        .map_err(|error| {
                            crate::provider_exchange::ProviderExchangeError::Invalid(
                                error.to_string(),
                            )
                        })?;
                verify_exact_applied_target(
                    workspace,
                    source_worktree_root,
                    &current_bindings.target_head,
                    current_bindings.patch_bytes.as_bytes(),
                    &latest_candidate,
                )
                .map_err(|error| {
                    crate::provider_exchange::ProviderExchangeError::Invalid(error.to_string())
                })?;
                Ok(())
            },
        )
        .map_err(PromotionError::wrapped)?;
        Ok(PromotionOutcome {
            run: intended.clone(),
            evidence: evidence.clone(),
        })
    })();
    let unlock = repository_lock.unlock();
    match (result, unlock) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Ok(_), Err(error)) => Err(PromotionError::wrapped(error)),
        (Err(error), _) => Err(error),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PromotionBindings {
    run_id: String,
    candidate_diff: ArtifactReference,
    testing_evidence: ArtifactReference,
    eval_report: ArtifactReference,
    policy_decision_digest: String,
    target_head: String,
    eval_passed_run_digest: String,
    eval_passed_updated_at: String,
    patch_bytes: String,
}

fn promotion_bindings(
    run: &LoopRun,
    verified: &VerifiedCandidatePatchEvidence,
) -> Result<PromotionBindings, PromotionError> {
    let approval = run
        .human_approval
        .as_ref()
        .ok_or_else(|| PromotionError::invalid("EvalPassed authority has no human approval"))?;
    let testing_evidence = step_reference(run, LoopStepName::Testing)?;
    let eval_report = step_reference(run, LoopStepName::EvalReport)?;
    Ok(PromotionBindings {
        run_id: run.run_id.clone(),
        candidate_diff: approval.candidate_diff.clone(),
        testing_evidence,
        eval_report,
        policy_decision_digest: approval.policy_decision_digest.clone(),
        target_head: approval.starting_head.clone(),
        eval_passed_run_digest: canonical_sha256_digest(run).map_err(PromotionError::wrapped)?,
        eval_passed_updated_at: run.updated_at.clone(),
        patch_bytes: verified.applied_diff_content.clone(),
    })
}

fn authenticate_promotion_authority(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
    run: &LoopRun,
) -> Result<VerifiedCandidatePatchEvidence, PromotionError> {
    crate::provider_exchange::validate_run_for_atomic_publication(workspace, run)
        .map_err(PromotionError::wrapped)?;
    let final_authority = crate::load_verified_final_evaluation_authority(workspace, run)
        .map_err(PromotionError::wrapped)?;
    verify_final_supporting_artifacts(workspace, run, &final_authority)?;
    let verified =
        verify_candidate_patch_evidence_for_evaluation_locked(workspace, source_worktree_root)
            .map_err(PromotionError::wrapped)?;
    let approval = run
        .human_approval
        .as_ref()
        .ok_or_else(|| PromotionError::invalid("EvalPassed authority has no human approval"))?;
    if verified.applied_diff != approval.candidate_diff
        || verified.policy_decision_digest != approval.policy_decision_digest
        || verified.candidate_authority.starting_head != approval.starting_head
    {
        return Err(PromotionError::invalid(
            "physical candidate differs from final approved authority",
        ));
    }
    Ok(verified)
}

fn verify_final_supporting_artifacts(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    authority: &VerifiedFinalEvaluationAuthority,
) -> Result<(), PromotionError> {
    for (path, digest, label) in [
        (
            "inputs/ticket.json",
            run.input_digests.ticket.as_str(),
            "immutable ticket snapshot",
        ),
        (
            "inputs/eval-config.json",
            run.input_digests.eval_config.as_deref().ok_or_else(|| {
                PromotionError::invalid("EvalPassed authority has no eval config digest")
            })?,
            "immutable eval config snapshot",
        ),
    ] {
        let bytes = read_verified_regular_file(workspace.run_directory(), path, label)
            .map_err(PromotionError::wrapped)?;
        verify_canonical_json_digest(&bytes, digest, label)?;
    }
    for check in &authority.testing_evidence().checks {
        for (path, digest, label) in [
            (
                check.stdout_path.as_deref(),
                check.stdout_digest.as_deref(),
                "Testing stdout log",
            ),
            (
                check.stderr_path.as_deref(),
                check.stderr_digest.as_deref(),
                "Testing stderr log",
            ),
        ] {
            let path =
                path.ok_or_else(|| PromotionError::invalid(format!("{label} has no path")))?;
            let digest =
                digest.ok_or_else(|| PromotionError::invalid(format!("{label} has no digest")))?;
            let bytes = read_verified_regular_file(workspace.run_directory(), path, label)
                .map_err(PromotionError::wrapped)?;
            if digest_bytes(&bytes) != digest {
                return Err(PromotionError::invalid(format!("{label} digest mismatch")));
            }
        }
    }
    let intent_reference = authority.execution_intent_reference();
    let intent_bytes = read_verified_regular_file(
        workspace.run_directory(),
        &intent_reference.path,
        "Testing execution intent",
    )
    .map_err(PromotionError::wrapped)?;
    if digest_bytes(&intent_bytes) != intent_reference.digest
        || authority.execution_intent().planned_check_count() == 0
    {
        return Err(PromotionError::invalid(
            "Testing execution intent does not match final evaluation authority",
        ));
    }
    Ok(())
}

fn verify_canonical_json_digest(
    bytes: &[u8],
    expected: &str,
    label: &str,
) -> Result<(), PromotionError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(PromotionError::wrapped)?;
    if canonical_json_bytes(&value).map_err(PromotionError::wrapped)? != bytes
        || canonical_sha256_digest(&value).map_err(PromotionError::wrapped)? != expected
    {
        return Err(PromotionError::invalid(format!(
            "{label} bytes or digest mismatch"
        )));
    }
    Ok(())
}

fn validate_confirmations(
    bindings: &PromotionBindings,
    _reviewer: &str,
    candidate_diff: &str,
    eval_report: &str,
    target_head: &str,
) -> Result<(), PromotionError> {
    if candidate_diff != bindings.candidate_diff.digest {
        return Err(PromotionError::invalid(
            "confirmed candidate diff digest does not match EvalPassed authority",
        ));
    }
    if eval_report != bindings.eval_report.digest {
        return Err(PromotionError::invalid(
            "confirmed EvalReport digest does not match EvalPassed authority",
        ));
    }
    if target_head != bindings.target_head {
        return Err(PromotionError::invalid(
            "confirmed target HEAD does not match EvalPassed authority",
        ));
    }
    Ok(())
}

fn validate_exact_retry(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
    run: LoopRun,
    reviewer: &str,
    candidate_diff: &str,
    eval_report: &str,
    target_head: &str,
) -> Result<PromotionOutcome, PromotionError> {
    let operator_guard = crate::operator_evidence::OperatorEvidenceGuard::load(workspace, &run)
        .map_err(PromotionError::invalid)?;
    operator_guard
        .validate_current_run_file(workspace)
        .and_then(|()| operator_guard.validate_run(&run))
        .and_then(|()| operator_guard.validate_structural(reviewer))
        .and_then(|()| operator_guard.validate_future_run(&run).map(drop))
        .map_err(PromotionError::invalid)?;
    let evidence = run
        .promotion
        .clone()
        .ok_or_else(|| PromotionError::invalid("Promoted authority has no promotion evidence"))?;
    if evidence.reviewer != reviewer
        || evidence.candidate_diff.digest != candidate_diff
        || evidence.eval_report.digest != eval_report
        || evidence.target_head != target_head
    {
        return Err(PromotionError::invalid(
            "promotion retry must exactly match the original fresh confirmation",
        ));
    }
    let intent = load_optional_intent(workspace, &operator_guard)?
        .ok_or_else(|| PromotionError::invalid("Promoted authority lost promotion intent"))?;
    if evidence.intent.path != PROMOTION_INTENT_PATH
        || canonical_sha256_digest(&intent).map_err(PromotionError::wrapped)?
            != evidence.intent.digest
        || intent.schema_version != PROMOTION_SCHEMA_VERSION
        || intent.run_id != evidence.run_id
        || intent.reviewer != evidence.reviewer
        || intent.started_at != evidence.promoted_at
        || intent.candidate_diff != evidence.candidate_diff
        || intent.testing_evidence != evidence.testing_evidence
        || intent.eval_report != evidence.eval_report
        || intent.policy_decision_digest != evidence.policy_decision_digest
        || intent.target_head != evidence.target_head
        || intent.eval_passed_run_digest != evidence.eval_passed_run_digest
    {
        return Err(PromotionError::invalid(
            "promotion intent does not match immutable Promoted evidence",
        ));
    }
    let verified = authenticate_promotion_authority(workspace, source_worktree_root, &run)?;
    verify_exact_applied_target(
        workspace,
        source_worktree_root,
        &evidence.target_head,
        verified.applied_diff_content.as_bytes(),
        &verified,
    )?;
    state::resync_exact_run(workspace, &run).map_err(PromotionError::wrapped)?;
    Ok(PromotionOutcome { run, evidence })
}

fn validate_intent(
    intent: &PromotionIntent,
    reviewer: &str,
    bindings: &PromotionBindings,
) -> Result<(), PromotionError> {
    if intent.schema_version != PROMOTION_SCHEMA_VERSION
        || intent.run_id != bindings.run_id
        || intent.reviewer != reviewer
        || intent.candidate_diff != bindings.candidate_diff
        || intent.testing_evidence != bindings.testing_evidence
        || intent.eval_report != bindings.eval_report
        || intent.policy_decision_digest != bindings.policy_decision_digest
        || intent.target_head != bindings.target_head
        || intent.eval_passed_run_digest != bindings.eval_passed_run_digest
        || canonical_unix_seconds(&intent.started_at)
            .zip(canonical_unix_seconds(&bindings.eval_passed_updated_at))
            .is_none_or(|(started, evaluated)| started < evaluated)
    {
        return Err(PromotionError::invalid(
            "existing promotion intent does not match the exact fresh confirmation and EvalPassed authority",
        ));
    }
    Ok(())
}

fn load_exact_intent(
    workspace: &LoopWorkspace,
    expected: &PromotionIntent,
    reference: &ArtifactReference,
    operator_guard: &crate::operator_evidence::OperatorEvidenceGuard,
) -> Result<PromotionIntent, PromotionError> {
    if reference.path != PROMOTION_INTENT_PATH {
        return Err(PromotionError::invalid(
            "promotion intent reference path is not canonical",
        ));
    }
    let loaded = load_optional_intent(workspace, operator_guard)?
        .ok_or_else(|| PromotionError::invalid("promotion intent disappeared"))?;
    if &loaded != expected
        || canonical_sha256_digest(&loaded).map_err(PromotionError::wrapped)? != reference.digest
    {
        return Err(PromotionError::invalid(
            "promotion intent changed after durable publication",
        ));
    }
    Ok(loaded)
}

fn canonical_unix_seconds(value: &str) -> Option<u64> {
    let parsed = value.parse::<u64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn load_optional_intent(
    workspace: &LoopWorkspace,
    operator_guard: &crate::operator_evidence::OperatorEvidenceGuard,
) -> Result<Option<PromotionIntent>, PromotionError> {
    let path = workspace.run_directory().join(PROMOTION_INTENT_PATH);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(PromotionError::wrapped(error)),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            PromotionError::invalid("promotion intent path is not a real regular file"),
        ),
        Ok(_) => {
            let bytes = read_verified_regular_file(
                workspace.run_directory(),
                PROMOTION_INTENT_PATH,
                "promotion intent",
            )
            .map_err(PromotionError::wrapped)?;
            operator_guard
                .validate_exact_raw_bytes(&bytes)
                .map_err(PromotionError::invalid)?;
            let intent: PromotionIntent =
                serde_json::from_slice(&bytes).map_err(PromotionError::wrapped)?;
            if canonical_json_bytes(&intent).map_err(PromotionError::wrapped)? != bytes {
                return Err(PromotionError::invalid(
                    "promotion intent is not canonical JSON",
                ));
            }
            operator_guard
                .validate_structural(&intent.reviewer)
                .map_err(PromotionError::invalid)?;
            Ok(Some(intent))
        }
    }
}

fn step_reference(run: &LoopRun, name: LoopStepName) -> Result<ArtifactReference, PromotionError> {
    let step = run
        .steps
        .iter()
        .find(|step| step.name == name)
        .ok_or_else(|| {
            PromotionError::invalid(format!("EvalPassed authority has no {name:?} step"))
        })?;
    Ok(ArtifactReference {
        path: step.artifact_path.clone().ok_or_else(|| {
            PromotionError::invalid(format!("EvalPassed {name:?} step has no artifact path"))
        })?,
        digest: step.artifact_digest.clone().ok_or_else(|| {
            PromotionError::invalid(format!("EvalPassed {name:?} step has no artifact digest"))
        })?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetState {
    Clean,
    ExactPatch,
}

fn classify_target(
    workspace: &LoopWorkspace,
    root: &Path,
    expected_head: &str,
    patch: &[u8],
    verified: &VerifiedCandidatePatchEvidence,
) -> Result<TargetState, PromotionError> {
    validate_target_head(root, expected_head)?;
    if target_is_completely_clean(workspace, root)? {
        return Ok(TargetState::Clean);
    }
    verify_exact_applied_target(workspace, root, expected_head, patch, verified)?;
    Ok(TargetState::ExactPatch)
}

fn require_clean_target(
    workspace: &LoopWorkspace,
    root: &Path,
    expected_head: &str,
) -> Result<(), PromotionError> {
    validate_target_head(root, expected_head)?;
    if !target_is_completely_clean(workspace, root)? {
        return Err(PromotionError::invalid(
            "promotion target must have a completely clean index, worktree, untracked, and ignored state",
        ));
    }
    Ok(())
}

fn validate_target_head(root: &Path, expected: &str) -> Result<(), PromotionError> {
    let canonical = root.canonicalize().map_err(PromotionError::wrapped)?;
    if canonical != root {
        return Err(PromotionError::invalid(
            "promotion target repository path is symlinked or non-canonical",
        ));
    }
    let head = git_output(root, &["rev-parse", "HEAD"], None)?;
    if String::from_utf8(head)
        .map_err(PromotionError::wrapped)?
        .trim()
        != expected
    {
        return Err(PromotionError::invalid(
            "promotion target HEAD no longer matches confirmed target HEAD",
        ));
    }
    Ok(())
}

fn target_is_completely_clean(
    workspace: &LoopWorkspace,
    root: &Path,
) -> Result<bool, PromotionError> {
    if index_tree(root, None)? != head_tree(root)? || verify_worktree_matches_index(root).is_err() {
        return Ok(false);
    }
    let ordinary = git_output(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
        None,
    )?;
    let ignored = git_output(
        root,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
            "--",
        ],
        None,
    )?;
    let excluded = runtime_relative_prefix(workspace, root)?;
    Ok(filter_nul_paths(&ordinary, excluded.as_deref()).is_empty()
        && filter_nul_paths(&ignored, excluded.as_deref()).is_empty())
}

fn verify_exact_applied_target(
    workspace: &LoopWorkspace,
    root: &Path,
    expected_head: &str,
    patch: &[u8],
    verified: &VerifiedCandidatePatchEvidence,
) -> Result<(), PromotionError> {
    validate_target_head(root, expected_head)?;
    if index_tree(root, None)? != head_tree(root)? {
        return Err(PromotionError::invalid(
            "promoted target index must remain unchanged",
        ));
    }
    let index = PrivateIndex::create(root, patch, verified)?;
    verify_worktree_matches_private_index(root, &index.path).map_err(PromotionError::wrapped)?;
    let ordinary_untracked = git_output(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
        Some(&index.path),
    )?;
    let ignored_untracked = git_output(
        root,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
            "--",
        ],
        Some(&index.path),
    )?;
    let excluded = runtime_relative_prefix(workspace, root)?;
    if !filter_nul_paths(&ordinary_untracked, excluded.as_deref()).is_empty()
        || !filter_nul_paths(&ignored_untracked, excluded.as_deref()).is_empty()
    {
        return Err(PromotionError::invalid(
            "target working bytes do not represent exactly the evaluated candidate patch",
        ));
    }
    Ok(())
}

fn runtime_relative_prefix(
    workspace: &LoopWorkspace,
    root: &Path,
) -> Result<Option<Vec<u8>>, PromotionError> {
    let run = workspace
        .run_directory()
        .canonicalize()
        .map_err(PromotionError::wrapped)?;
    let root = root.canonicalize().map_err(PromotionError::wrapped)?;
    let Ok(relative) = run.strip_prefix(&root) else {
        return Ok(None);
    };
    Ok(Some(relative.as_os_str().as_encoded_bytes().to_vec()))
}

fn filter_nul_paths(bytes: &[u8], excluded: Option<&[u8]>) -> Vec<Vec<u8>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty() && !path_is_within(path, excluded))
        .map(ToOwned::to_owned)
        .collect()
}

fn head_tree(root: &Path) -> Result<String, PromotionError> {
    String::from_utf8(git_output(root, &["rev-parse", "HEAD^{tree}"], None)?)
        .map(|tree| tree.trim().to_string())
        .map_err(PromotionError::wrapped)
}

fn index_tree(root: &Path, index: Option<&Path>) -> Result<String, PromotionError> {
    String::from_utf8(git_output(root, &["write-tree"], index)?)
        .map(|tree| tree.trim().to_string())
        .map_err(PromotionError::wrapped)
}

fn path_is_within(path: &[u8], excluded: Option<&[u8]>) -> bool {
    let Some(excluded) = excluded else {
        return false;
    };
    path == excluded || (path.starts_with(excluded) && path.get(excluded.len()) == Some(&b'/'))
}

struct PrivateIndex {
    directory: PathBuf,
    path: PathBuf,
}

impl PrivateIndex {
    fn create(
        root: &Path,
        patch: &[u8],
        verified: &VerifiedCandidatePatchEvidence,
    ) -> Result<Self, PromotionError> {
        let directory = create_private_index_directory()?;
        let path = directory.join("index");
        let index = Self { directory, path };
        git_output(root, &["read-tree", "HEAD"], Some(&index.path))?;
        validate_private_index(&index)?;
        git_apply_with_index(
            root,
            patch,
            &index.path,
            &verified.policy_decision.changed_paths,
        )?;
        validate_private_index(&index)?;
        let tree = git_output(root, &["write-tree"], Some(&index.path))?;
        if String::from_utf8(tree)
            .map_err(PromotionError::wrapped)?
            .trim()
            != verified.candidate_tree
        {
            return Err(PromotionError::invalid(
                "private promotion index tree differs from evaluated candidate tree",
            ));
        }
        validate_private_index(&index)?;
        Ok(index)
    }
}

impl Drop for PrivateIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn create_private_index_directory() -> Result<PathBuf, PromotionError> {
    let root = env::temp_dir()
        .canonicalize()
        .map_err(PromotionError::wrapped)?;
    loop {
        let path = root.join(format!(
            "seaf-promotion-index-{}-{}",
            std::process::id(),
            INDEX_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&path) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
                }
                let metadata = fs::symlink_metadata(&path)?;
                if metadata.file_type().is_symlink()
                    || !metadata.is_dir()
                    || path.canonicalize().map_err(PromotionError::wrapped)? != path
                {
                    let _ = fs::remove_dir_all(&path);
                    return Err(PromotionError::invalid(
                        "private promotion index directory identity is unsafe",
                    ));
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(PromotionError::wrapped(error)),
        }
    }
}

fn validate_private_index(index: &PrivateIndex) -> Result<(), PromotionError> {
    let directory = fs::symlink_metadata(&index.directory)?;
    let file = fs::symlink_metadata(&index.path)?;
    if directory.file_type().is_symlink()
        || !directory.is_dir()
        || file.file_type().is_symlink()
        || !file.is_file()
        || index.path.parent() != Some(index.directory.as_path())
    {
        return Err(PromotionError::invalid(
            "private promotion index identity is unsafe",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&index.directory, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(&index.path, fs::Permissions::from_mode(0o600))?;
        if fs::symlink_metadata(&index.directory)?.permissions().mode() & 0o777 != 0o700
            || fs::symlink_metadata(&index.path)?.permissions().mode() & 0o777 != 0o600
        {
            return Err(PromotionError::invalid(
                "private promotion index permissions are unsafe",
            ));
        }
    }
    Ok(())
}

fn git_apply(
    root: &Path,
    patch: &[u8],
    check: bool,
    overrides: &[(String, String)],
) -> Result<(), PromotionError> {
    let mut args = vec!["apply", "--whitespace=nowarn"];
    if check {
        args.push("--check");
    }
    run_git_with_stdin(root, &args, patch, None, overrides)
}

fn git_apply_with_index(
    root: &Path,
    patch: &[u8],
    index: &Path,
    changed_paths: &[String],
) -> Result<(), PromotionError> {
    let overrides = filter_driver_overrides(root, changed_paths)?;
    run_git_with_stdin(
        root,
        &["apply", "--cached", "--whitespace=nowarn"],
        patch,
        Some(index),
        &overrides,
    )
}

fn run_git_with_stdin(
    root: &Path,
    args: &[&str],
    bytes: &[u8],
    index: Option<&Path>,
    overrides: &[(String, String)],
) -> Result<(), PromotionError> {
    let mut command = sanitized_git_command();
    for (key, value) in overrides {
        command.arg("-c").arg(format!("{key}={value}"));
    }
    command
        .args(args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let mut child = command.spawn().map_err(PromotionError::wrapped)?;
    child
        .stdin
        .take()
        .ok_or_else(|| PromotionError::invalid("git apply stdin unavailable"))?
        .write_all(bytes)
        .map_err(PromotionError::wrapped)?;
    let output = child.wait_with_output().map_err(PromotionError::wrapped)?;
    if !output.status.success() {
        return Err(PromotionError::invalid(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn git_output(root: &Path, args: &[&str], index: Option<&Path>) -> Result<Vec<u8>, PromotionError> {
    let mut command = sanitized_git_command();
    command.args(args).current_dir(root);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = command.output().map_err(PromotionError::wrapped)?;
    if !output.status.success() {
        return Err(PromotionError::invalid(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

fn filter_driver_overrides(
    root: &Path,
    changed_paths: &[String],
) -> Result<Vec<(String, String)>, PromotionError> {
    let drivers = filter_driver_names(root, changed_paths)?;
    Ok(filter_driver_overrides_for_names(drivers))
}

fn promotion_filter_driver_overrides(
    source_root: &Path,
    candidate_root: &Path,
    changed_paths: &[String],
) -> Result<Vec<(String, String)>, PromotionError> {
    let mut drivers = filter_driver_names(source_root, changed_paths)?;
    for driver in filter_driver_names(candidate_root, changed_paths)? {
        if !drivers.contains(&driver) {
            drivers.push(driver);
        }
    }
    Ok(filter_driver_overrides_for_names(drivers))
}

fn filter_driver_names(
    root: &Path,
    changed_paths: &[String],
) -> Result<Vec<String>, PromotionError> {
    if changed_paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut command = sanitized_git_command();
    let output = command
        .args(["check-attr", "-z", "filter", "--"])
        .args(changed_paths)
        .current_dir(root)
        .output()
        .map_err(PromotionError::wrapped)?;
    if !output.status.success() {
        return Err(PromotionError::invalid("git check-attr filter failed"));
    }
    let fields = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    if fields.len() % 3 != 0 {
        return Err(PromotionError::invalid(
            "git check-attr returned malformed metadata",
        ));
    }
    let mut drivers = Vec::new();
    for triple in fields.chunks_exact(3) {
        let value = std::str::from_utf8(triple[2]).map_err(PromotionError::wrapped)?;
        if matches!(value, "unspecified" | "unset" | "set") {
            continue;
        }
        if value.is_empty()
            || !value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
            })
        {
            return Err(PromotionError::invalid("unsafe Git filter driver name"));
        }
        if !drivers.contains(&value.to_string()) {
            drivers.push(value.to_string());
        }
    }
    Ok(drivers)
}

fn filter_driver_overrides_for_names(drivers: Vec<String>) -> Vec<(String, String)> {
    let mut overrides = Vec::new();
    for driver in drivers {
        overrides.push((format!("filter.{driver}.clean"), String::new()));
        overrides.push((format!("filter.{driver}.smudge"), String::new()));
        overrides.push((format!("filter.{driver}.process"), String::new()));
        overrides.push((format!("filter.{driver}.required"), "false".to_string()));
    }
    overrides
}

fn sanitized_git_command() -> Command {
    let mut command = Command::new("git");
    command.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
    ]);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_ATTR_NOSYSTEM",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_EXTERNAL_DIFF",
        "GIT_DIFF_OPTS",
        "GIT_PAGER",
        "GIT_EDITOR",
        "GIT_SEQUENCE_EDITOR",
        "GIT_ASKPASS",
        "SSH_ASKPASS",
    ] {
        command.env_remove(name);
    }
    for (name, _) in env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("GIT_CONFIG_KEY_") || name.starts_with("GIT_CONFIG_VALUE_") {
            command.env_remove(name.as_ref());
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1");
    command
}

fn validate_reviewer(reviewer: &str) -> Result<(), PromotionError> {
    if reviewer.is_empty()
        || reviewer.len() > 256
        || reviewer.trim() != reviewer
        || reviewer.chars().any(char::is_control)
    {
        return Err(PromotionError::invalid(
            "reviewer identity must be 1..=256 bytes with no surrounding whitespace or control characters",
        ));
    }
    Ok(())
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn now_timestamp() -> Result<String, PromotionError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(PromotionError::wrapped)?
        .as_secs()
        .to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionError {
    message: String,
}

impl PromotionError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn wrapped(error: impl Error) -> Self {
        Self::invalid(error.to_string())
    }
}

impl fmt::Display for PromotionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for PromotionError {}

impl From<std::io::Error> for PromotionError {
    fn from(error: std::io::Error) -> Self {
        Self::wrapped(error)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    use crate::{
        context::{CandidateContextAuthority, CandidateContextAuthorityKind},
        PatchDecisionKind, PolicyDecision,
    };

    #[test]
    fn private_index_uses_a_private_unique_directory_and_cleans_it_recursively() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init"]);
        git(&repo, &["config", "user.name", "SEAF Tests"]);
        git(&repo, &["config", "user.email", "tests@seaf.invalid"]);
        fs::write(repo.join("tracked.txt"), b"before\n").unwrap();
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "initial"]);
        fs::write(repo.join("tracked.txt"), b"after\n").unwrap();
        let patch = git_bytes(
            &repo,
            &[
                "diff",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--no-textconv",
                "--",
            ],
        );
        git(&repo, &["add", "tracked.txt"]);
        let candidate_tree = String::from_utf8(git_bytes(&repo, &["write-tree"]))
            .unwrap()
            .trim()
            .to_string();
        git(&repo, &["reset", "--hard", "HEAD"]);
        let reference = ArtifactReference {
            path: "artifacts/test".to_string(),
            digest: "a".repeat(64),
        };
        let verified = VerifiedCandidatePatchEvidence {
            development_evidence: reference.clone(),
            policy_decision: PolicyDecision {
                patch_id: "run".to_string(),
                patch_sha256: format!("sha256:{}", "b".repeat(64)),
                changed_paths: vec!["tracked.txt".to_string()],
                decision: PatchDecisionKind::Allowed,
                reasons: Vec::new(),
                requires_human_review: false,
                apply_requested: true,
                applied: false,
            },
            policy_decision_digest: "c".repeat(64),
            candidate_authority: CandidateContextAuthority {
                kind: CandidateContextAuthorityKind::IsolatedCandidate,
                repository_identity_digest: "d".repeat(64),
                candidate_path_digest: "e".repeat(64),
                starting_head: "f".repeat(40),
                starting_tree: "1".repeat(40),
            },
            intent: reference.clone(),
            applied_evidence: reference.clone(),
            candidate_tree,
            applied_diff: reference,
            applied_diff_digest: "2".repeat(64),
            applied_diff_content: String::from_utf8(patch.clone()).unwrap(),
        };

        let index = PrivateIndex::create(&repo, &patch, &verified).unwrap();
        let private_directory = index.path.parent().unwrap().to_path_buf();
        assert_ne!(private_directory, env::temp_dir());
        assert_eq!(
            fs::metadata(&private_directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&index.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(index);
        assert!(!private_directory.exists());
    }

    #[test]
    fn source_apply_never_executes_filter_from_current_target_attributes() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        initialize_repository(&repo);
        fs::write(repo.join(".gitattributes"), b"tracked.txt filter=evil\n").unwrap();
        fs::write(repo.join("tracked.txt"), b"before\n").unwrap();
        git(&repo, &["add", ".gitattributes", "tracked.txt"]);
        git(&repo, &["commit", "-m", "initial"]);
        fs::write(repo.join("tracked.txt"), b"after\n").unwrap();
        let patch = git_bytes(
            &repo,
            &[
                "diff",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--no-textconv",
                "--",
            ],
        );
        git(&repo, &["reset", "--hard", "HEAD"]);
        let marker = temp.path().join("current-filter-executed");
        configure_marker_filter(&repo, &temp.path().join("evil-current.sh"), &marker);

        let changed_paths = ["tracked.txt".to_string()];
        let check_overrides =
            promotion_filter_driver_overrides(&repo, &repo, &changed_paths).unwrap();
        git_apply(&repo, &patch, true, &check_overrides).unwrap();
        assert!(!marker.exists(), "git apply --check executed clean filter");
        assert_eq!(fs::read(repo.join("tracked.txt")).unwrap(), b"before\n");
        let apply_overrides =
            promotion_filter_driver_overrides(&repo, &repo, &changed_paths).unwrap();
        git_apply(&repo, &patch, false, &apply_overrides).unwrap();
        assert!(
            !marker.exists(),
            "git apply executed clean or smudge filter"
        );
        assert_eq!(fs::read(repo.join("tracked.txt")).unwrap(), b"after\n");
    }

    #[test]
    fn source_apply_never_executes_filter_introduced_by_evaluated_attributes() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        initialize_repository(&repo);
        fs::write(repo.join("base.txt"), b"base\n").unwrap();
        fs::write(repo.join("introduced.txt"), b"before\n").unwrap();
        git(&repo, &["add", "base.txt", "introduced.txt"]);
        git(&repo, &["commit", "-m", "initial"]);
        fs::write(repo.join(".gitattributes"), b"introduced.txt filter=evil\n").unwrap();
        fs::write(repo.join("introduced.txt"), b"raw changed bytes\n").unwrap();
        git(&repo, &["add", ".gitattributes", "introduced.txt"]);
        let patch = git_bytes(
            &repo,
            &[
                "diff",
                "--cached",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--no-textconv",
                "HEAD",
                "--",
            ],
        );
        git(&repo, &["reset", "--hard", "HEAD"]);
        let candidate = temp.path().join("candidate");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                candidate.to_str().unwrap(),
                "HEAD",
            ],
        );
        let candidate_patch = temp.path().join("introduced.patch");
        fs::write(&candidate_patch, &patch).unwrap();
        git(
            &candidate,
            &[
                "apply",
                "--whitespace=nowarn",
                candidate_patch.to_str().unwrap(),
            ],
        );
        let marker = temp.path().join("introduced-filter-executed");
        configure_marker_filter(&repo, &temp.path().join("evil-introduced.sh"), &marker);
        let overrides = promotion_filter_driver_overrides(
            &repo,
            &candidate,
            &[".gitattributes".to_string(), "introduced.txt".to_string()],
        )
        .unwrap();
        assert!(
            overrides.iter().any(|(key, _)| key == "filter.evil.clean"),
            "candidate attribute view did not contribute filter neutralization"
        );

        git_apply(&repo, &patch, true, &overrides).unwrap();
        assert!(
            !marker.exists(),
            "git apply --check executed introduced filter"
        );
        assert!(!repo.join(".gitattributes").exists());
        assert_eq!(fs::read(repo.join("introduced.txt")).unwrap(), b"before\n");
        let apply_overrides = promotion_filter_driver_overrides(
            &repo,
            &candidate,
            &[".gitattributes".to_string(), "introduced.txt".to_string()],
        )
        .unwrap();
        git_apply(&repo, &patch, false, &apply_overrides).unwrap();
        assert!(!marker.exists(), "git apply executed introduced filter");
        assert_eq!(
            fs::read(repo.join("introduced.txt")).unwrap(),
            b"raw changed bytes\n"
        );
    }

    fn initialize_repository(repo: &Path) {
        git(repo, &["init"]);
        git(repo, &["config", "user.name", "SEAF Tests"]);
        git(repo, &["config", "user.email", "tests@seaf.invalid"]);
    }

    fn configure_marker_filter(repo: &Path, script: &Path, marker: &Path) {
        fs::write(
            script,
            format!("#!/bin/sh\nprintf executed > '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        let mut permissions = fs::metadata(script).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(script, permissions).unwrap();
        git(
            repo,
            &["config", "filter.evil.clean", script.to_str().unwrap()],
        );
        git(
            repo,
            &["config", "filter.evil.smudge", script.to_str().unwrap()],
        );
        git(repo, &["config", "filter.evil.required", "true"]);
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_bytes(root: &Path, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success());
        output.stdout
    }
}
