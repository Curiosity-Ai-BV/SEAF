use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, validate_eval_config, validate_ticket_spec,
    ArtifactReference, CheckStatus, EvalCheck, EvalConfig, EvalDecision, EvalLoopEvidence,
    EvalReport, LoopRun, LoopStatus, LoopStepName, LoopStepStatus, RiskLevel, TicketSpec,
};
use sha2::{Digest, Sha256};

use crate::{
    candidate_workspace::{
        acquire_candidate_lock, capture_source_worktree_authority,
        validate_source_worktree_authority, verify_candidate_patch_evidence_for_evaluation_locked,
        verify_candidate_patch_evidence_locked,
    },
    eval_engine::execute_eval_checks_with_pre_spawn,
    evaluation_attempt::{
        ApprovedEvaluationIntentV2, EvaluationAttemptInventory, EvaluationAttemptPaths,
    },
    immutable_artifact::{publish_create_only, read_verified_regular_file},
    plan_eval_checks, state, LoopWorkspace, TestingEvidence,
};

pub fn execute_approved_evaluation(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
) -> Result<LoopRun, ApprovedEvaluationError> {
    let candidate_lock =
        acquire_candidate_lock(workspace).map_err(ApprovedEvaluationError::wrapped)?;
    let result = execute_approved_evaluation_locked(workspace, source_worktree_root);
    let unlock = candidate_lock.unlock();
    match (result, unlock) {
        (Ok(run), Ok(())) => Ok(run),
        (Ok(_), Err(error)) => Err(ApprovedEvaluationError::wrapped(error)),
        (Err(error), _) => Err(error),
    }
}

fn execute_approved_evaluation_locked(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
) -> Result<LoopRun, ApprovedEvaluationError> {
    let approved = state::load_run(workspace).map_err(ApprovedEvaluationError::wrapped)?;
    if matches!(approved.status, LoopStatus::EvalPassed)
        || (approved.status == LoopStatus::Failed && approved.human_approval.is_some())
    {
        crate::load_verified_final_evaluation_authority(workspace, &approved)
            .map_err(ApprovedEvaluationError::wrapped)?;
        return Ok(approved);
    }
    if approved.status != LoopStatus::Approved {
        return Err(ApprovedEvaluationError::invalid(
            "local evaluation requires exact Approved authority",
        ));
    }

    // Authenticate all durable and physical authority before creating intent or executing.
    crate::provider_exchange::validate_run_for_atomic_publication(workspace, &approved)
        .map_err(ApprovedEvaluationError::wrapped)?;
    let verified = verify_candidate_patch_evidence_locked(workspace, source_worktree_root)
        .map_err(ApprovedEvaluationError::wrapped)?;
    let approval = approved.human_approval.as_ref().ok_or_else(|| {
        ApprovedEvaluationError::invalid("Approved authority has no human approval")
    })?;
    if verified.applied_diff != approval.candidate_diff
        || verified.policy_decision_digest != approval.policy_decision_digest
        || verified.candidate_authority.starting_head != approval.starting_head
    {
        return Err(ApprovedEvaluationError::invalid(
            "physical candidate authority does not match exact human approval",
        ));
    }
    let (ticket, ticket_bytes) = load_ticket_snapshot(workspace, &approved)?;
    let (eval_config, eval_config_bytes) = load_eval_snapshot(workspace, &approved)?;
    refuse_incomplete_attempt(workspace)?;

    let candidate = approved.candidate_workspace.as_ref().ok_or_else(|| {
        ApprovedEvaluationError::invalid("Approved authority has no candidate workspace")
    })?;
    let candidate_root = Path::new(&candidate.path);
    let plan = plan_eval_checks(
        &eval_config,
        Some(&ticket.autonomy.allow_shell_commands),
        candidate_root,
    )
    .map_err(ApprovedEvaluationError::wrapped)?;
    let mut planned_names = BTreeSet::new();
    if eval_config
        .evals
        .required
        .iter()
        .any(|check| !planned_names.insert(check.name.as_str()))
    {
        return Err(ApprovedEvaluationError::invalid(
            "canonical eval snapshot contains duplicated check names",
        ));
    }

    let approved_run_digest =
        canonical_sha256_digest(&approved).map_err(ApprovedEvaluationError::wrapped)?;
    let source_authority =
        capture_source_worktree_authority(source_worktree_root, Some(workspace.run_directory()))
            .map_err(ApprovedEvaluationError::wrapped)?;
    let attempt = 1;
    let paths =
        EvaluationAttemptPaths::indexed(attempt).map_err(ApprovedEvaluationError::invalid)?;
    let intent = ApprovedEvaluationIntentV2 {
        schema_version: 2,
        evaluation_attempt: attempt,
        run_id: approved.run_id.clone(),
        approved_run_digest,
        input_digests: approved.input_digests.clone(),
        ticket: ArtifactReference {
            path: "inputs/ticket.json".to_string(),
            digest: approved.input_digests.ticket.clone(),
        },
        eval_config: ArtifactReference {
            path: "inputs/eval-config.json".to_string(),
            digest: approved.input_digests.eval_config.clone().ok_or_else(|| {
                ApprovedEvaluationError::invalid("Approved authority has no eval config digest")
            })?,
        },
        candidate_state_digest: canonical_sha256_digest(candidate)
            .map_err(ApprovedEvaluationError::wrapped)?,
        candidate_diff: approval.candidate_diff.clone(),
        source_worktree_state_digest: canonical_sha256_digest(&source_authority)
            .map_err(ApprovedEvaluationError::wrapped)?,
        recovery: None,
        planned_checks: eval_config.evals.required.clone(),
    };
    let intent_bytes = canonical_json_bytes(&intent).map_err(ApprovedEvaluationError::wrapped)?;
    publish_create_only(workspace.run_directory(), &paths.intent, &intent_bytes)
        .map_err(ApprovedEvaluationError::wrapped)?;

    let started_at = now_timestamp()?;
    let mut checks = Vec::with_capacity(eval_config.evals.required.len());
    let executions = execute_eval_checks_with_pre_spawn(&plan, |_| {
        let result = (|| {
            let current = state::load_run(workspace).map_err(ApprovedEvaluationError::wrapped)?;
            if current != approved {
                return Err(ApprovedEvaluationError::invalid(
                    "Approved authority changed before command spawn",
                ));
            }
            crate::provider_exchange::validate_run_for_atomic_publication(workspace, &current)
                .map_err(ApprovedEvaluationError::wrapped)?;
            let current_candidate = verify_candidate_patch_evidence_for_evaluation_locked(
                workspace,
                source_worktree_root,
            )
            .map_err(ApprovedEvaluationError::wrapped)?;
            if current_candidate != verified {
                return Err(ApprovedEvaluationError::invalid(
                    "candidate authority changed before command spawn",
                ));
            }
            validate_source_worktree_authority(
                source_worktree_root,
                Some(workspace.run_directory()),
                &source_authority,
            )
            .map_err(ApprovedEvaluationError::wrapped)?;
            verify_snapshot_bytes(
                workspace,
                "inputs/ticket.json",
                &ticket_bytes,
                &approved.input_digests.ticket,
            )?;
            verify_snapshot_bytes(
                workspace,
                "inputs/eval-config.json",
                &eval_config_bytes,
                approved
                    .input_digests
                    .eval_config
                    .as_deref()
                    .expect("checked above"),
            )?;
            verify_snapshot_bytes(
                workspace,
                &paths.intent,
                &intent_bytes,
                &sha256_bytes(&intent_bytes),
            )?;
            Ok(())
        })();
        result.map_err(|error| error.to_string())
    });
    for (index, result) in executions.enumerate() {
        let configured = &eval_config.evals.required[index];
        let execution = match result {
            Ok(execution) => execution,
            Err(error) => crate::EvalCheckExecution {
                name: configured.name.clone(),
                status: CheckStatus::Failed,
                duration_ms: 0,
                stdout: String::new(),
                stderr: String::new(),
                summary: format!("controlled command execution failed: {error}"),
            },
        };
        let number = index + 1;
        let stdout_path = paths.stdout(number);
        let stderr_path = paths.stderr(number);
        let stdout = execution.stdout.as_bytes();
        let stderr = execution.stderr.as_bytes();
        publish_create_only(workspace.run_directory(), &stdout_path, stdout)
            .map_err(ApprovedEvaluationError::wrapped)?;
        publish_create_only(workspace.run_directory(), &stderr_path, stderr)
            .map_err(ApprovedEvaluationError::wrapped)?;
        checks.push(EvalCheck {
            name: execution.name,
            status: execution.status,
            duration_ms: Some(execution.duration_ms),
            stdout_path: Some(stdout_path),
            stdout_digest: Some(sha256_bytes(stdout)),
            stderr_path: Some(stderr_path),
            stderr_digest: Some(sha256_bytes(stderr)),
            summary: Some(execution.summary),
        });
    }
    let completed_at = now_timestamp()?;

    // Candidate authority remains locked. Reauthenticate it and every immutable input/output byte.
    let current = state::load_run(workspace).map_err(ApprovedEvaluationError::wrapped)?;
    if current != approved {
        return Err(ApprovedEvaluationError::invalid(
            "Approved authority changed during local evaluation",
        ));
    }
    crate::provider_exchange::validate_run_for_atomic_publication(workspace, &current)
        .map_err(ApprovedEvaluationError::wrapped)?;
    let reverified =
        verify_candidate_patch_evidence_for_evaluation_locked(workspace, source_worktree_root)
            .map_err(ApprovedEvaluationError::wrapped)?;
    if reverified != verified {
        return Err(ApprovedEvaluationError::invalid(
            "candidate authority changed during local evaluation",
        ));
    }
    validate_source_worktree_authority(
        source_worktree_root,
        Some(workspace.run_directory()),
        &source_authority,
    )
    .map_err(ApprovedEvaluationError::wrapped)?;
    verify_snapshot_bytes(
        workspace,
        "inputs/ticket.json",
        &ticket_bytes,
        &approved.input_digests.ticket,
    )?;
    verify_snapshot_bytes(
        workspace,
        "inputs/eval-config.json",
        &eval_config_bytes,
        approved
            .input_digests
            .eval_config
            .as_deref()
            .expect("checked above"),
    )?;
    verify_snapshot_bytes(
        workspace,
        &paths.intent,
        &intent_bytes,
        &sha256_bytes(&intent_bytes),
    )?;
    verify_check_logs(workspace, &checks)?;

    let intent_reference = ArtifactReference {
        path: paths.intent.clone(),
        digest: sha256_bytes(&intent_bytes),
    };
    let testing = TestingEvidence::create_v2(
        &approved,
        attempt,
        None,
        intent_reference,
        started_at,
        completed_at.clone(),
        checks.clone(),
    )
    .map_err(ApprovedEvaluationError::wrapped)?;
    let testing_bytes = testing
        .canonical_bytes()
        .map_err(ApprovedEvaluationError::wrapped)?;
    let testing_reference = ArtifactReference {
        path: paths.testing.clone(),
        digest: testing
            .artifact_digest()
            .map_err(ApprovedEvaluationError::wrapped)?,
    };
    publish_create_only(workspace.run_directory(), &paths.testing, &testing_bytes)
        .map_err(ApprovedEvaluationError::wrapped)?;

    let passed = testing.passed;
    let report = EvalReport {
        eval_report_id: format!("eval_{}", approved.run_id),
        patch_id: approved.run_id.clone(),
        goal_id: approved.goal_id.clone(),
        passed,
        summary: if passed {
            "Approved candidate passed all required local checks.".to_string()
        } else {
            "Approved candidate failed one or more required local checks.".to_string()
        },
        checks,
        score_delta_estimate: None,
        risk_level: if passed {
            RiskLevel::Low
        } else {
            RiskLevel::High
        },
        decision: if passed {
            EvalDecision::ApproveForHumanReview
        } else {
            EvalDecision::Reject
        },
        loop_evidence: Some(EvalLoopEvidence {
            schema_version: 1,
            run_id: approved.run_id.clone(),
            ticket_id: approved.ticket_id.clone(),
            ticket_digest: approved.input_digests.ticket.clone(),
            eval_config: intent.eval_config,
            candidate_diff: approval.candidate_diff.clone(),
            starting_head: approval.starting_head.clone(),
            human_approval_digest: canonical_sha256_digest(approval)
                .map_err(ApprovedEvaluationError::wrapped)?,
            policy_decision_digest: approval.policy_decision_digest.clone(),
            testing_evidence: testing_reference.clone(),
        }),
    };
    let report_bytes = canonical_json_bytes(&report).map_err(ApprovedEvaluationError::wrapped)?;
    let report_reference = ArtifactReference {
        path: paths.report.clone(),
        digest: canonical_sha256_digest(&report).map_err(ApprovedEvaluationError::wrapped)?,
    };
    publish_create_only(workspace.run_directory(), &paths.report, &report_bytes)
        .map_err(ApprovedEvaluationError::wrapped)?;

    verify_snapshot_bytes(
        workspace,
        &paths.testing,
        &testing_bytes,
        &testing_reference.digest,
    )?;
    verify_snapshot_bytes(
        workspace,
        &paths.report,
        &report_bytes,
        &report_reference.digest,
    )?;
    verify_check_logs(workspace, &report.checks)?;
    let final_verified =
        verify_candidate_patch_evidence_for_evaluation_locked(workspace, source_worktree_root)
            .map_err(ApprovedEvaluationError::wrapped)?;
    if final_verified != verified
        || state::load_run(workspace).map_err(ApprovedEvaluationError::wrapped)? != approved
    {
        return Err(ApprovedEvaluationError::invalid(
            "evaluation authority changed before final publication",
        ));
    }
    validate_source_worktree_authority(
        source_worktree_root,
        Some(workspace.run_directory()),
        &source_authority,
    )
    .map_err(ApprovedEvaluationError::wrapped)?;

    let mut final_run = approved.clone();
    final_run.status = if passed {
        LoopStatus::EvalPassed
    } else {
        LoopStatus::Failed
    };
    final_run.current_step = LoopStepName::EvalReport;
    final_run.updated_at = completed_at;
    for (name, reference) in [
        (LoopStepName::Testing, testing_reference),
        (LoopStepName::EvalReport, report_reference.clone()),
    ] {
        let record = final_run
            .steps
            .iter_mut()
            .find(|record| record.name == name)
            .ok_or_else(|| {
                ApprovedEvaluationError::invalid("evaluation step chain is incomplete")
            })?;
        record.status = if passed {
            LoopStepStatus::Passed
        } else {
            LoopStepStatus::Failed
        };
        record.artifact_path = Some(reference.path);
        record.artifact_digest = Some(reference.digest);
    }
    final_run.eval_report_path = Some(report_reference.path);

    // Lock order is candidate first, provider second. The c1 relation validates the exact
    // Approved predecessor and complete final artifact authority under the provider lock.
    crate::provider_exchange::persist_run_with_full_compare_and_validator(
        workspace,
        &approved,
        &final_run,
        |locked| {
            if locked != &approved {
                return Err(crate::provider_exchange::ProviderExchangeError::Invalid(
                    "locked Approved authority changed before final publication".to_string(),
                ));
            }
            let reverified = verify_candidate_patch_evidence_for_evaluation_locked(
                workspace,
                source_worktree_root,
            )
            .map_err(|error| {
                crate::provider_exchange::ProviderExchangeError::Invalid(error.to_string())
            })?;
            if reverified != verified {
                return Err(crate::provider_exchange::ProviderExchangeError::Invalid(
                    "candidate authority changed before final publication".to_string(),
                ));
            }
            validate_source_worktree_authority(
                source_worktree_root,
                Some(workspace.run_directory()),
                &source_authority,
            )
            .map_err(|error| {
                crate::provider_exchange::ProviderExchangeError::Invalid(error.to_string())
            })?;
            verify_snapshot_bytes(
                workspace,
                "inputs/ticket.json",
                &ticket_bytes,
                &approved.input_digests.ticket,
            )
            .map_err(|error| {
                crate::provider_exchange::ProviderExchangeError::Invalid(error.to_string())
            })?;
            verify_snapshot_bytes(
                workspace,
                "inputs/eval-config.json",
                &eval_config_bytes,
                approved
                    .input_digests
                    .eval_config
                    .as_deref()
                    .expect("checked"),
            )
            .map_err(|error| {
                crate::provider_exchange::ProviderExchangeError::Invalid(error.to_string())
            })?;
            verify_snapshot_bytes(
                workspace,
                &paths.intent,
                &intent_bytes,
                &sha256_bytes(&intent_bytes),
            )
            .map_err(|error| {
                crate::provider_exchange::ProviderExchangeError::Invalid(error.to_string())
            })?;
            verify_check_logs(workspace, &report.checks).map_err(|error| {
                crate::provider_exchange::ProviderExchangeError::Invalid(error.to_string())
            })?;
            Ok(())
        },
    )
    .map_err(ApprovedEvaluationError::wrapped)?;
    Ok(final_run)
}

fn load_ticket_snapshot(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<(TicketSpec, Vec<u8>), ApprovedEvaluationError> {
    let bytes =
        load_canonical_snapshot(workspace, "inputs/ticket.json", &run.input_digests.ticket)?;
    let ticket: TicketSpec =
        serde_json::from_slice(&bytes).map_err(ApprovedEvaluationError::wrapped)?;
    let errors = validate_ticket_spec(&ticket);
    if !errors.is_empty() || ticket.ticket_id != run.ticket_id || ticket.goal_id != run.goal_id {
        return Err(ApprovedEvaluationError::invalid(
            "canonical ticket snapshot does not match Approved authority",
        ));
    }
    if canonical_json_bytes(&ticket).map_err(ApprovedEvaluationError::wrapped)? != bytes {
        return Err(ApprovedEvaluationError::invalid(
            "canonical ticket snapshot is not canonical typed input",
        ));
    }
    Ok((ticket, bytes))
}

fn load_eval_snapshot(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<(EvalConfig, Vec<u8>), ApprovedEvaluationError> {
    let digest = run.input_digests.eval_config.as_deref().ok_or_else(|| {
        ApprovedEvaluationError::invalid("Approved authority has no eval config digest")
    })?;
    let bytes = load_canonical_snapshot(workspace, "inputs/eval-config.json", digest)?;
    let config: EvalConfig =
        serde_json::from_slice(&bytes).map_err(ApprovedEvaluationError::wrapped)?;
    validate_eval_config(&config).map_err(ApprovedEvaluationError::wrapped)?;
    if canonical_json_bytes(&config).map_err(ApprovedEvaluationError::wrapped)? != bytes {
        return Err(ApprovedEvaluationError::invalid(
            "canonical eval snapshot is not canonical typed input",
        ));
    }
    Ok((config, bytes))
}

fn load_canonical_snapshot(
    workspace: &LoopWorkspace,
    path: &str,
    digest: &str,
) -> Result<Vec<u8>, ApprovedEvaluationError> {
    let bytes = read_verified_regular_file(workspace.run_directory(), path, path)
        .map_err(ApprovedEvaluationError::wrapped)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(ApprovedEvaluationError::wrapped)?;
    if canonical_json_bytes(&value).map_err(ApprovedEvaluationError::wrapped)? != bytes
        || canonical_sha256_digest(&value).map_err(ApprovedEvaluationError::wrapped)? != digest
    {
        return Err(ApprovedEvaluationError::invalid(format!(
            "{path} bytes or digest do not match Approved authority"
        )));
    }
    Ok(bytes)
}

fn refuse_incomplete_attempt(workspace: &LoopWorkspace) -> Result<(), ApprovedEvaluationError> {
    let inventory =
        EvaluationAttemptInventory::load(workspace).map_err(ApprovedEvaluationError::invalid)?;
    if !inventory.is_empty() {
        return Err(ApprovedEvaluationError::invalid(
            "an incomplete Approved evaluation attempt exists; audited recovery is required",
        ));
    }
    Ok(())
}

fn verify_snapshot_bytes(
    workspace: &LoopWorkspace,
    path: &str,
    expected: &[u8],
    digest: &str,
) -> Result<(), ApprovedEvaluationError> {
    let actual = read_verified_regular_file(workspace.run_directory(), path, path)
        .map_err(ApprovedEvaluationError::wrapped)?;
    if actual != expected || sha256_bytes(&actual) != digest {
        return Err(ApprovedEvaluationError::invalid(format!(
            "immutable artifact {path} changed before final publication"
        )));
    }
    Ok(())
}

fn verify_check_logs(
    workspace: &LoopWorkspace,
    checks: &[EvalCheck],
) -> Result<(), ApprovedEvaluationError> {
    for check in checks {
        for (path, digest) in [
            (check.stdout_path.as_deref(), check.stdout_digest.as_deref()),
            (check.stderr_path.as_deref(), check.stderr_digest.as_deref()),
        ] {
            let (Some(path), Some(digest)) = (path, digest) else {
                return Err(ApprovedEvaluationError::invalid(
                    "integrated check lost log authority",
                ));
            };
            let bytes =
                read_verified_regular_file(workspace.run_directory(), path, "evaluation log")
                    .map_err(ApprovedEvaluationError::wrapped)?;
            if sha256_bytes(&bytes) != digest {
                return Err(ApprovedEvaluationError::invalid(
                    "evaluation log digest mismatch",
                ));
            }
        }
    }
    Ok(())
}

fn now_timestamp() -> Result<String, ApprovedEvaluationError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(ApprovedEvaluationError::wrapped)?
        .as_secs()
        .to_string())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedEvaluationError {
    message: String,
}

impl ApprovedEvaluationError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn wrapped(error: impl fmt::Display) -> Self {
        Self::invalid(error.to_string())
    }
}

impl fmt::Display for ApprovedEvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ApprovedEvaluationError {}
