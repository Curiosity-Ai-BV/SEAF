use std::{ffi::OsStr, io, path::Path};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, validate_eval_config, EvalConfig, LoopRun,
    LoopStatus,
};

use crate::{
    artifact_storage::StorageCommitment,
    eval_engine::normalize_eval_output_limit,
    evaluation_attempt::{
        ApprovedEvaluationIntent, EvaluationAttemptInventory, EvaluationCommitmentPrefix,
    },
    immutable_artifact::read_verified_regular_file,
    recovery::{EvaluationInvalidationAttemptV3, EvaluationInvalidationSourceRunV3},
};

const EVALUATION_ARTIFACT_CAP: u64 = 2 * 1024 * 1024;
const FINAL_RUN_REPLACEMENT_CAP: u64 = 2 * 1024 * 1024;

pub(crate) fn derive_active_evaluation_storage_commitment(
    run_directory: &Path,
    run: &LoopRun,
) -> Result<Option<StorageCommitment>, String> {
    if run.status != LoopStatus::Approved {
        return derive_staged_invalidation_commitment(run_directory, run);
    }
    let inventory = EvaluationAttemptInventory::load_from_run_directory(run_directory, true)?;
    let Some(prefix) = inventory.latest_commitment_prefix()? else {
        return derive_staged_invalidation_commitment(run_directory, run);
    };
    if prefix_is_superseded_by_invalidation(run_directory, run, prefix.attempt)? {
        return Ok(None);
    }
    if let Some(commitment) = derive_recovery_transition_commitment(run_directory, run, &prefix)? {
        return Ok(Some(commitment));
    }
    let intent_reference = crate::evaluation_attempt::reference_for_path(
        &workspace_from_run_directory(run_directory)?,
        &prefix.intent,
    )?;
    let intent = crate::evaluation_attempt::load_intent(
        &workspace_from_run_directory(run_directory)?,
        &intent_reference,
    )
    .map_err(|error| format!("invalid evaluation intent {}: {error}", prefix.intent))?;
    let checks = load_eval_config(run_directory, run)?;
    validate_intent(run, &intent, &checks.evals.required)?;
    if intent.attempt() != prefix.attempt {
        return Err("evaluation commitment intent selects a different attempt".to_string());
    }
    validate_present_evaluation_evidence(
        run_directory,
        run,
        &inventory,
        &prefix,
        &intent_reference,
        &intent,
    )?;
    normal_prefix_commitment(run, &prefix, &checks, |path| {
        existing_size(run_directory, path)?
            .ok_or_else(|| format!("evaluation commitment prefix artifact disappeared: {path}"))
    })
    .map(Some)
}

fn validate_present_evaluation_evidence(
    run_directory: &Path,
    run: &LoopRun,
    inventory: &EvaluationAttemptInventory,
    prefix: &EvaluationCommitmentPrefix,
    intent_reference: &seaf_core::ArtifactReference,
    intent: &ApprovedEvaluationIntent,
) -> Result<(), String> {
    let Some(testing_path) = prefix.testing.as_deref() else {
        if prefix.report.is_some() {
            return Err("EvalReport exists without authenticated Testing evidence".into());
        }
        return Ok(());
    };
    let workspace = workspace_from_run_directory(run_directory)?;
    let testing_reference =
        crate::evaluation_attempt::reference_for_path(&workspace, testing_path)?;
    let testing =
        crate::TestingEvidence::load_for_approved_run(&workspace, &testing_reference, run)
            .map_err(|error| {
                format!("existing Testing artifact has different bytes or authority: {error}")
            })?;
    let testing_binds_intent = match intent {
        ApprovedEvaluationIntent::V1(_) => {
            testing.schema_version == 1
                && testing.evaluation_attempt.is_none()
                && testing.execution_intent.is_none()
                && testing.recovery.is_none()
        }
        ApprovedEvaluationIntent::V2(_) => {
            testing.schema_version == 2
                && testing.evaluation_attempt == Some(prefix.attempt)
                && testing.execution_intent.as_ref() == Some(intent_reference)
                && testing.recovery.as_ref().and_then(Option::as_ref) == intent.recovery()
        }
    };
    if !testing_binds_intent {
        return Err("Testing evidence does not bind the active evaluation intent".into());
    }
    inventory.validate_selected_logs(prefix.attempt, &testing.checks)?;
    for check in &testing.checks {
        for (path, digest) in [
            (check.stdout_path.as_deref(), check.stdout_digest.as_deref()),
            (check.stderr_path.as_deref(), check.stderr_digest.as_deref()),
        ] {
            let (Some(path), Some(digest)) = (path, digest) else {
                return Err("Testing evidence lost an authenticated log".into());
            };
            let bytes = read_verified_regular_file(run_directory, path, "Testing log")
                .map_err(|error| error.to_string())?;
            if format!("{:x}", sha2::Sha256::digest(&bytes)) != digest {
                return Err("Testing evidence log digest mismatch".into());
            }
        }
    }
    if let Some(report_path) = prefix.report.as_deref() {
        let report_bytes = read_verified_regular_file(run_directory, report_path, "EvalReport")
            .map_err(|error| error.to_string())?;
        let report: seaf_core::EvalReport =
            serde_json::from_slice(&report_bytes).map_err(|error| error.to_string())?;
        if canonical_json_bytes(&report).map_err(|error| error.to_string())? != report_bytes {
            return Err("EvalReport is not canonical".into());
        }
        crate::approved_eval::validate_integrated_eval_report_binding(
            run,
            &testing,
            testing_reference,
            &report,
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn prefix_is_superseded_by_invalidation(
    run_directory: &Path,
    run: &LoopRun,
    attempt: u32,
) -> Result<bool, String> {
    let Some(reference) = run.latest_recovery.as_ref() else {
        return Ok(false);
    };
    let workspace = workspace_from_run_directory(run_directory)?;
    let Some((invalidation, _)) =
        crate::recovery::load_verified_evaluation_invalidation(&workspace, reference)
            .map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    Ok(invalidation.next_evaluation_attempt > attempt)
}

pub(crate) fn derive_fresh_evaluation_storage_commitment(
    run_directory: &Path,
    run: &LoopRun,
    intent: &ApprovedEvaluationIntent,
    intent_path: &str,
) -> Result<StorageCommitment, String> {
    if run.status != LoopStatus::Approved {
        return Err("fresh evaluation commitment requires exact Approved authority".to_string());
    }
    let inventory = EvaluationAttemptInventory::load_from_run_directory(run_directory, true)?;
    let attempt = intent.attempt();
    if inventory.contains_attempt(attempt) {
        return Err("fresh evaluation commitment target attempt is already occupied".to_string());
    }
    let expected = inventory.latest_attempt().map_or(Ok(1), |latest| {
        latest
            .checked_add(1)
            .ok_or_else(|| "evaluation attempt sequence is exhausted".to_string())
    })?;
    if attempt != expected {
        return Err("fresh evaluation commitment target is not the exact next attempt".to_string());
    }
    let checks = load_eval_config(run_directory, run)?;
    validate_intent(run, intent, &checks.evals.required)?;
    let prefix = EvaluationCommitmentPrefix {
        attempt,
        intent: intent_path.to_string(),
        testing: None,
        report: None,
        logs: Default::default(),
    };
    normal_prefix_commitment(run, &prefix, &checks, |_| {
        Err("fresh evaluation prefix cannot contain an existing artifact".to_string())
    })
}

pub(crate) fn derive_invalidation_source_activation_commitment(
    run: &LoopRun,
    recovery_id: u32,
    source_path: &str,
    source_bytes: &[u8],
) -> Result<StorageCommitment, String> {
    ensure_canonical(source_bytes, "evaluation invalidation source")?;
    let source: EvaluationInvalidationSourceRunV3 =
        serde_json::from_slice(source_bytes).map_err(|error| error.to_string())?;
    if source.schema_version != crate::recovery::EVALUATION_INVALIDATION_SCHEMA_VERSION
        || source.recovery_id != recovery_id
        || source_path != recovery_source_path(recovery_id)
        || &source.run != run
    {
        return Err("evaluation invalidation source does not bind exact current authority".into());
    }
    let mut commitment = empty_commitment();
    add_slot(
        &mut commitment,
        recovery_path(recovery_id),
        EVALUATION_ARTIFACT_CAP,
    )?;
    commitment.transient_bytes = FINAL_RUN_REPLACEMENT_CAP;
    commitment.transient_entries = 1;
    commitment.consumable_transient_path = Some("run.json".into());
    Ok(commitment)
}

pub(crate) fn validate_pre_spawn_evaluation_prefix(
    run_directory: &Path,
    attempt: u32,
    completed: &[seaf_core::EvalCheck],
) -> Result<(), String> {
    let inventory = EvaluationAttemptInventory::load_from_run_directory(run_directory, true)?;
    let prefix = inventory
        .latest_commitment_prefix()?
        .ok_or_else(|| "pre-spawn evaluation prefix lost execution intent".to_string())?;
    if prefix.attempt != attempt
        || prefix.testing.is_some()
        || prefix.report.is_some()
        || prefix.logs.len() != completed.len()
    {
        return Err("pre-spawn evaluation prefix has unauthenticated or surplus artifacts".into());
    }
    for (index, check) in completed.iter().enumerate() {
        let number = u32::try_from(index + 1)
            .map_err(|_| "evaluation check sequence is exhausted".to_string())?;
        let (stdout, stderr) = prefix
            .logs
            .get(&number)
            .ok_or_else(|| "pre-spawn evaluation logs are not contiguous".to_string())?;
        for (path, expected_path, expected_digest, label) in [
            (
                stdout.as_deref(),
                check.stdout_path.as_deref(),
                check.stdout_digest.as_deref(),
                "stdout",
            ),
            (
                stderr.as_deref(),
                check.stderr_path.as_deref(),
                check.stderr_digest.as_deref(),
                "stderr",
            ),
        ] {
            let (Some(path), Some(expected_path), Some(expected_digest)) =
                (path, expected_path, expected_digest)
            else {
                return Err(format!(
                    "completed evaluation {label} lost durable authority"
                ));
            };
            if path != expected_path {
                return Err(format!("completed evaluation {label} path was substituted"));
            }
            let bytes = read_verified_regular_file(run_directory, path, "completed evaluation log")
                .map_err(|error| error.to_string())?;
            if format!("{:x}", sha2::Sha256::digest(&bytes)) != expected_digest {
                return Err(format!(
                    "completed evaluation {label} bytes were substituted"
                ));
            }
        }
    }
    Ok(())
}

fn validate_intent(
    run: &LoopRun,
    intent: &ApprovedEvaluationIntent,
    checks: &[seaf_core::EvalCommandConfig],
) -> Result<(), String> {
    let expected_recovery = intent.recovery();
    if expected_recovery.is_some() && expected_recovery != run.latest_recovery.as_ref() {
        return Err("evaluation intent recovery is not the current durable recovery head".into());
    }
    intent
        .validate_against_with_recovery(run, checks, expected_recovery)
        .map_err(|error| {
            if error == "Approved evaluation intent bindings do not match exact authority" {
                format!("Approved authority changed: {error}")
            } else {
                error
            }
        })
}

fn normal_prefix_commitment<F>(
    run: &LoopRun,
    prefix: &EvaluationCommitmentPrefix,
    config: &EvalConfig,
    mut physical_size: F,
) -> Result<StorageCommitment, String>
where
    F: FnMut(&str) -> Result<u64, String>,
{
    let paths = AttemptPathSet::from_intent(&prefix.intent, prefix.attempt)?;
    if prefix.logs.len() > config.evals.required.len() {
        return Err("evaluation commitment prefix has surplus check logs".to_string());
    }
    let mut commitment = empty_commitment();
    for (index, check) in config.evals.required.iter().enumerate() {
        let number = u32::try_from(index + 1)
            .map_err(|_| "evaluation check sequence is exhausted".to_string())?;
        let limit = u64::try_from(
            normalize_eval_output_limit(&check.name, check.max_output_bytes)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|_| "evaluation output limit is not representable".to_string())?;
        let present = prefix.logs.get(&number);
        let stdout = paths.stdout(index + 1);
        let stderr = paths.stderr(index + 1);
        match present {
            Some((Some(actual), _)) if actual == &stdout => {
                if prefix.testing.is_none() {
                    add_residual(
                        &mut commitment,
                        limit.checked_sub(physical_size(actual)?).ok_or_else(|| {
                            "evaluation stdout exceeds its normalized limit".to_string()
                        })?,
                    )?;
                }
            }
            Some((Some(_), _)) => {
                return Err("evaluation stdout path is not canonical for its attempt".into())
            }
            _ => add_slot(&mut commitment, stdout, limit)?,
        }
        match present {
            Some((_, Some(actual))) if actual == &stderr => {
                if prefix.testing.is_none() {
                    add_residual(
                        &mut commitment,
                        limit.checked_sub(physical_size(actual)?).ok_or_else(|| {
                            "evaluation stderr exceeds its normalized limit".to_string()
                        })?,
                    )?;
                }
            }
            Some((_, Some(_))) => {
                return Err("evaluation stderr path is not canonical for its attempt".into())
            }
            _ => add_slot(&mut commitment, stderr, limit)?,
        }
    }
    if prefix.testing.is_some()
        && commitment
            .consumable_permanent_paths
            .iter()
            .any(|(path, _)| path.ends_with(".stdout.log") || path.ends_with(".stderr.log"))
    {
        return Err("Testing evidence cannot precede complete evaluation logs".into());
    }
    match prefix.testing.as_deref() {
        Some(actual) if actual == paths.testing => {}
        Some(_) => return Err("Testing evidence path is not canonical for its attempt".into()),
        None => add_slot(
            &mut commitment,
            paths.testing.clone(),
            EVALUATION_ARTIFACT_CAP,
        )?,
    }
    match prefix.report.as_deref() {
        Some(_) if prefix.testing.is_none() => {
            return Err("EvalReport cannot precede Testing evidence".into())
        }
        Some(actual) if actual == paths.report => {}
        Some(_) => return Err("EvalReport path is not canonical for its attempt".into()),
        None => add_slot(&mut commitment, paths.report, EVALUATION_ARTIFACT_CAP)?,
    }
    let recovery_id = next_recovery_id(run)?;
    add_slot(
        &mut commitment,
        recovery_source_path(recovery_id),
        EVALUATION_ARTIFACT_CAP,
    )?;
    add_slot(
        &mut commitment,
        recovery_path(recovery_id),
        EVALUATION_ARTIFACT_CAP,
    )?;
    commitment.transient_bytes = FINAL_RUN_REPLACEMENT_CAP;
    commitment.transient_entries = 1;
    commitment.consumable_transient_path = Some("run.json".to_string());
    Ok(commitment)
}

fn derive_staged_invalidation_commitment(
    run_directory: &Path,
    run: &LoopRun,
) -> Result<Option<StorageCommitment>, String> {
    let recovery_id = next_recovery_id(run)?;
    let source_path = recovery_source_path(recovery_id);
    if existing_size(run_directory, &source_path)?.is_none() {
        if existing_size(run_directory, &recovery_path(recovery_id))?.is_some() {
            return Err("evaluation recovery decision exists without its source".into());
        }
        return Ok(None);
    }
    if recovery_source_schema(run_directory, &source_path)? == Some(1) {
        return Ok(None);
    }
    derive_transition_after_source(run_directory, run, recovery_id, None).map(Some)
}

fn derive_recovery_transition_commitment(
    run_directory: &Path,
    run: &LoopRun,
    prefix: &EvaluationCommitmentPrefix,
) -> Result<Option<StorageCommitment>, String> {
    let recovery_id = next_recovery_id(run)?;
    let source_path = recovery_source_path(recovery_id);
    if existing_size(run_directory, &source_path)?.is_none() {
        if existing_size(run_directory, &recovery_path(recovery_id))?.is_some() {
            return Err("evaluation recovery decision exists without its source".into());
        }
        return Ok(None);
    }
    if recovery_source_schema(run_directory, &source_path)? == Some(1) {
        return Ok(None);
    }
    derive_transition_after_source(run_directory, run, recovery_id, Some(prefix.attempt)).map(Some)
}

fn recovery_source_schema(run_directory: &Path, source_path: &str) -> Result<Option<u64>, String> {
    let bytes = read_verified_regular_file(run_directory, source_path, "recovery source schema")
        .map_err(|error| error.to_string())?;
    ensure_canonical(&bytes, "recovery source")?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    Ok(value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64))
}

fn derive_transition_after_source(
    run_directory: &Path,
    run: &LoopRun,
    recovery_id: u32,
    expected_attempt: Option<u32>,
) -> Result<StorageCommitment, String> {
    let source_path = recovery_source_path(recovery_id);
    let source_bytes =
        read_verified_regular_file(run_directory, &source_path, "evaluation recovery source")
            .map_err(|error| error.to_string())?;
    let workspace = workspace_from_run_directory(run_directory)?;
    let transition_kind = match crate::recovery::validate_staged_evaluation_source_for_storage(
        &workspace,
        run,
        &source_path,
        &source_bytes,
        expected_attempt,
    )
    .map_err(|error| error.to_string())?
    {
        crate::recovery::VerifiedStagedEvaluationSource::Adoption {
            missing_report: true,
        } => {
            let source: crate::recovery::EvaluationRecoverySourceRunV2 =
                serde_json::from_slice(&source_bytes).map_err(|error| error.to_string())?;
            EvaluationRecoveryTransitionKind::AdoptionCreateMissing {
                report_path: AttemptPathSet::for_spelling(
                    source.evaluation_prefix.evaluation_attempt,
                    source.evaluation_prefix.spelling,
                )?
                .report,
            }
        }
        crate::recovery::VerifiedStagedEvaluationSource::Adoption {
            missing_report: false,
        } => EvaluationRecoveryTransitionKind::AdoptionVerifyExisting,
        crate::recovery::VerifiedStagedEvaluationSource::Invalidation => {
            EvaluationRecoveryTransitionKind::Invalidation
        }
    };
    let decision_path = recovery_path(recovery_id);
    let (decision, decision_report) = if existing_size(run_directory, &decision_path)?.is_none() {
        (DurableDecisionState::Missing, None)
    } else {
        (
            DurableDecisionState::Verified,
            validate_recovery_decision(run_directory, recovery_id, &source_path, &source_bytes)?,
        )
    };
    let report = match &transition_kind {
        EvaluationRecoveryTransitionKind::AdoptionCreateMissing { report_path } => {
            match existing_size(run_directory, report_path)? {
                None => AdoptionReportState::Missing,
                Some(size) => {
                    let authenticated = decision_report.as_ref().is_some_and(|reference| {
                        reference.path == *report_path
                            && read_verified_regular_file(
                                run_directory,
                                report_path,
                                "adoption EvalReport",
                            )
                            .ok()
                            .is_some_and(|bytes| {
                                format!("{:x}", sha2::Sha256::digest(&bytes)) == reference.digest
                            })
                    });
                    if authenticated {
                        AdoptionReportState::Verified
                    } else {
                        AdoptionReportState::PresentUnauthenticated { bytes: size }
                    }
                }
            }
        }
        EvaluationRecoveryTransitionKind::AdoptionVerifyExisting
        | EvaluationRecoveryTransitionKind::Invalidation => AdoptionReportState::NotApplicable,
    };
    recovery_transition_commitment(EvaluationRecoveryTransition::Active {
        recovery_id,
        kind: transition_kind,
        decision,
        report,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvaluationRecoveryTransitionKind {
    AdoptionCreateMissing { report_path: String },
    AdoptionVerifyExisting,
    Invalidation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurableDecisionState {
    Missing,
    Verified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdoptionReportState {
    NotApplicable,
    Missing,
    PresentUnauthenticated { bytes: u64 },
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvaluationRecoveryTransition {
    Active {
        recovery_id: u32,
        kind: EvaluationRecoveryTransitionKind,
        decision: DurableDecisionState,
        report: AdoptionReportState,
    },
}

fn recovery_transition_commitment(
    transition: EvaluationRecoveryTransition,
) -> Result<StorageCommitment, String> {
    let EvaluationRecoveryTransition::Active {
        recovery_id,
        kind,
        decision,
        report,
    } = transition;
    let mut commitment = empty_commitment();
    if decision == DurableDecisionState::Missing {
        add_slot(
            &mut commitment,
            recovery_path(recovery_id),
            EVALUATION_ARTIFACT_CAP,
        )?;
    }
    match (kind, report) {
        (
            EvaluationRecoveryTransitionKind::AdoptionCreateMissing { report_path },
            AdoptionReportState::Missing,
        ) => add_slot(&mut commitment, report_path, EVALUATION_ARTIFACT_CAP)?,
        (
            EvaluationRecoveryTransitionKind::AdoptionCreateMissing { .. },
            AdoptionReportState::PresentUnauthenticated { bytes },
        ) => add_residual(
            &mut commitment,
            EVALUATION_ARTIFACT_CAP
                .checked_sub(bytes)
                .ok_or_else(|| "adoption EvalReport exceeds its artifact limit".to_string())?,
        )?,
        (
            EvaluationRecoveryTransitionKind::AdoptionCreateMissing { .. },
            AdoptionReportState::Verified,
        )
        | (
            EvaluationRecoveryTransitionKind::AdoptionVerifyExisting
            | EvaluationRecoveryTransitionKind::Invalidation,
            AdoptionReportState::NotApplicable,
        ) => {}
        _ => return Err("evaluation recovery transition has an impossible report state".into()),
    }
    commitment.transient_bytes = FINAL_RUN_REPLACEMENT_CAP;
    commitment.transient_entries = 1;
    commitment.consumable_transient_path = Some("run.json".into());
    Ok(commitment)
}

fn validate_recovery_decision(
    run_directory: &Path,
    recovery_id: u32,
    source_path: &str,
    source_bytes: &[u8],
) -> Result<Option<seaf_core::ArtifactReference>, String> {
    let path = recovery_path(recovery_id);
    let bytes = read_verified_regular_file(run_directory, &path, "evaluation recovery decision")
        .map_err(|error| error.to_string())?;
    ensure_canonical(&bytes, "evaluation recovery decision")?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let source_digest = format!("{:x}", sha2::Sha256::digest(source_bytes));
    let reference = seaf_core::RecoveryReference {
        recovery_id,
        artifact: seaf_core::ArtifactReference {
            path: path.clone(),
            digest: format!("{:x}", sha2::Sha256::digest(&bytes)),
        },
    };
    let workspace = workspace_from_run_directory(run_directory)?;
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(2) => {
            let decision: crate::recovery::EvaluationRecoveryAttemptV2 =
                serde_json::from_value(value).map_err(|error| error.to_string())?;
            if decision.recovery_id != recovery_id
                || decision.source_run.path != source_path
                || decision.source_run.digest != source_digest
            {
                return Err("evaluation adoption decision does not bind its source".into());
            }
            let verified =
                crate::recovery::load_verified_evaluation_recovery(&workspace, &reference)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| {
                        "evaluation adoption decision has the wrong authority kind".to_string()
                    })?;
            if verified.0 != decision {
                return Err("evaluation adoption decision failed exact lineage validation".into());
            }
            return Ok(Some(decision.eval_report));
        }
        Some(3) => {
            let decision: EvaluationInvalidationAttemptV3 =
                serde_json::from_value(value).map_err(|error| error.to_string())?;
            if decision.recovery_id != recovery_id
                || decision.source_run.path != source_path
                || decision.source_run.digest != source_digest
            {
                return Err("evaluation invalidation decision does not bind its source".into());
            }
            let verified =
                crate::recovery::load_verified_evaluation_invalidation(&workspace, &reference)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| {
                        "evaluation invalidation decision has the wrong authority kind".to_string()
                    })?;
            if verified.0 != decision {
                return Err(
                    "evaluation invalidation decision failed exact lineage validation".into(),
                );
            }
        }
        _ => return Err("unsupported evaluation recovery decision schema".into()),
    }
    Ok(None)
}

fn load_eval_config(run_directory: &Path, run: &LoopRun) -> Result<EvalConfig, String> {
    let expected = run
        .input_digests
        .eval_config
        .as_deref()
        .ok_or_else(|| "Approved authority has no eval config digest".to_string())?;
    let bytes = read_verified_regular_file(run_directory, "inputs/eval-config.json", "eval config")
        .map_err(|error| error.to_string())?;
    let config: EvalConfig = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid inputs/eval-config.json: {error}"))?;
    if canonical_json_bytes(&config).map_err(|error| error.to_string())? != bytes
        || canonical_sha256_digest(&config).map_err(|error| error.to_string())? != expected
    {
        return Err("evaluation config bytes or digest mismatch".into());
    }
    validate_eval_config(&config).map_err(|error| error.to_string())?;
    Ok(config)
}

fn existing_size(run_directory: &Path, relative_path: &str) -> Result<Option<u64>, String> {
    let root = crate::artifact_safety::PinnedPrivateDirectory::open(run_directory)
        .map_err(|error| error.to_string())?;
    let artifacts = match root.open_child_directory(OsStr::new("artifacts")) {
        Ok(artifacts) => artifacts,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let name = Path::new(relative_path)
        .file_name()
        .ok_or_else(|| "evaluation artifact path has no file name".to_string())?;
    match artifacts.open_existing_file(name, true, false) {
        Ok(file) => {
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            artifacts
                .validate_single_link_file(name, &metadata)
                .map_err(|error| error.to_string())?;
            crate::artifact_storage::validate_artifact_size_u64(relative_path, metadata.len())
                .map_err(|error| error.to_string())?;
            Ok(Some(metadata.len()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn ensure_canonical(bytes: &[u8], label: &str) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if canonical_json_bytes(&value).map_err(|error| error.to_string())? != bytes {
        return Err(format!("{label} is not canonical JSON"));
    }
    Ok(())
}

fn add_slot(commitment: &mut StorageCommitment, path: String, bytes: u64) -> Result<(), String> {
    commitment.permanent_bytes = commitment
        .permanent_bytes
        .checked_add(bytes)
        .ok_or_else(|| "evaluation commitment byte accounting overflowed".to_string())?;
    commitment.permanent_entries = commitment
        .permanent_entries
        .checked_add(1)
        .ok_or_else(|| "evaluation commitment entry accounting overflowed".to_string())?;
    commitment.consumable_permanent_paths.push((path, bytes));
    Ok(())
}

fn add_residual(commitment: &mut StorageCommitment, bytes: u64) -> Result<(), String> {
    commitment.permanent_bytes = commitment
        .permanent_bytes
        .checked_add(bytes)
        .ok_or_else(|| "evaluation commitment byte accounting overflowed".to_string())?;
    Ok(())
}

fn empty_commitment() -> StorageCommitment {
    StorageCommitment {
        permanent_bytes: 0,
        transient_bytes: 0,
        permanent_entries: 0,
        transient_entries: 0,
        consumable_permanent_paths: Vec::new(),
        consumable_transient_path: None,
    }
}

fn next_recovery_id(run: &LoopRun) -> Result<u32, String> {
    run.latest_recovery.as_ref().map_or(Ok(1), |reference| {
        reference
            .recovery_id
            .checked_add(1)
            .ok_or_else(|| "recovery sequence is exhausted".to_string())
    })
}

fn recovery_path(id: u32) -> String {
    format!("artifacts/recovery-{id:03}.json")
}

fn recovery_source_path(id: u32) -> String {
    format!("artifacts/recovery-{id:03}.source-run.json")
}

struct AttemptPathSet {
    testing: String,
    report: String,
    fixed: bool,
}

impl AttemptPathSet {
    fn from_intent(path: &str, attempt: u32) -> Result<Self, String> {
        if path == crate::evaluation_attempt::FIXED_INTENT_PATH && attempt == 1 {
            return Ok(Self {
                testing: "artifacts/07-testing.json".into(),
                report: "artifacts/08-eval-report.json".into(),
                fixed: true,
            });
        }
        let indexed = crate::evaluation_attempt::EvaluationAttemptPaths::indexed(attempt)?;
        if indexed.intent != path {
            return Err("evaluation intent path is not canonical for its attempt".into());
        }
        Ok(Self {
            testing: indexed.testing,
            report: indexed.report,
            fixed: false,
        })
    }

    fn for_spelling(
        attempt: u32,
        spelling: crate::recovery::EvaluationPrefixSpellingV1,
    ) -> Result<Self, String> {
        match spelling {
            crate::recovery::EvaluationPrefixSpellingV1::FixedV1 if attempt == 1 => Ok(Self {
                testing: "artifacts/07-testing.json".into(),
                report: "artifacts/08-eval-report.json".into(),
                fixed: true,
            }),
            crate::recovery::EvaluationPrefixSpellingV1::IndexedV2 => {
                let paths = crate::evaluation_attempt::EvaluationAttemptPaths::indexed(attempt)?;
                Ok(Self {
                    testing: paths.testing,
                    report: paths.report,
                    fixed: false,
                })
            }
            _ => Err("evaluation recovery prefix spelling is invalid".into()),
        }
    }

    fn stdout(&self, check: usize) -> String {
        if self.fixed {
            format!("artifacts/07-testing.check-{check:03}.stdout.log")
        } else {
            self.testing
                .replace(".json", &format!(".check-{check:03}.stdout.log"))
        }
    }

    fn stderr(&self, check: usize) -> String {
        if self.fixed {
            format!("artifacts/07-testing.check-{check:03}.stderr.log")
        } else {
            self.testing
                .replace(".json", &format!(".check-{check:03}.stderr.log"))
        }
    }
}

fn workspace_from_run_directory(run_directory: &Path) -> Result<crate::LoopWorkspace, String> {
    let runs_root = run_directory
        .parent()
        .ok_or_else(|| "run directory has no runs root".to_string())?;
    let run_id = run_directory
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| "run directory has no UTF-8 run id".to_string())?;
    crate::LoopWorkspace::open(runs_root, run_id).map_err(|error| error.to_string())
}

use sha2::Digest;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use seaf_core::{EvalCommandConfig, EvalGroup, LoopInputDigests};

    use super::*;

    #[test]
    fn durable_recovery_transition_table_has_literal_remaining_capacity() {
        let report = "artifacts/08-eval-report.attempt-003.json";
        let cases = vec![
            (
                "v2 CreateMissing source",
                EvaluationRecoveryTransition::Active {
                    recovery_id: 7,
                    kind: EvaluationRecoveryTransitionKind::AdoptionCreateMissing {
                        report_path: report.into(),
                    },
                    decision: DurableDecisionState::Missing,
                    report: AdoptionReportState::Missing,
                },
                StorageCommitment {
                    permanent_bytes: 4_194_304,
                    transient_bytes: 2_097_152,
                    permanent_entries: 2,
                    transient_entries: 1,
                    consumable_permanent_paths: vec![
                        ("artifacts/recovery-007.json".into(), 2_097_152),
                        (report.into(), 2_097_152),
                    ],
                    consumable_transient_path: Some("run.json".into()),
                },
            ),
            (
                "v2 CreateMissing decision",
                EvaluationRecoveryTransition::Active {
                    recovery_id: 7,
                    kind: EvaluationRecoveryTransitionKind::AdoptionCreateMissing {
                        report_path: report.into(),
                    },
                    decision: DurableDecisionState::Verified,
                    report: AdoptionReportState::Missing,
                },
                StorageCommitment {
                    permanent_bytes: 2_097_152,
                    transient_bytes: 2_097_152,
                    permanent_entries: 1,
                    transient_entries: 1,
                    consumable_permanent_paths: vec![(report.into(), 2_097_152)],
                    consumable_transient_path: Some("run.json".into()),
                },
            ),
            (
                "v2 CreateMissing unauthenticated report",
                EvaluationRecoveryTransition::Active {
                    recovery_id: 7,
                    kind: EvaluationRecoveryTransitionKind::AdoptionCreateMissing {
                        report_path: report.into(),
                    },
                    decision: DurableDecisionState::Missing,
                    report: AdoptionReportState::PresentUnauthenticated { bytes: 17 },
                },
                StorageCommitment {
                    permanent_bytes: 4_194_287,
                    transient_bytes: 2_097_152,
                    permanent_entries: 1,
                    transient_entries: 1,
                    consumable_permanent_paths: vec![(
                        "artifacts/recovery-007.json".into(),
                        2_097_152,
                    )],
                    consumable_transient_path: Some("run.json".into()),
                },
            ),
            (
                "v2 CreateMissing report",
                EvaluationRecoveryTransition::Active {
                    recovery_id: 7,
                    kind: EvaluationRecoveryTransitionKind::AdoptionCreateMissing {
                        report_path: report.into(),
                    },
                    decision: DurableDecisionState::Verified,
                    report: AdoptionReportState::Verified,
                },
                StorageCommitment {
                    permanent_bytes: 0,
                    transient_bytes: 2_097_152,
                    permanent_entries: 0,
                    transient_entries: 1,
                    consumable_permanent_paths: vec![],
                    consumable_transient_path: Some("run.json".into()),
                },
            ),
            (
                "v2 VerifyExisting source",
                EvaluationRecoveryTransition::Active {
                    recovery_id: 7,
                    kind: EvaluationRecoveryTransitionKind::AdoptionVerifyExisting,
                    decision: DurableDecisionState::Missing,
                    report: AdoptionReportState::NotApplicable,
                },
                StorageCommitment {
                    permanent_bytes: 2_097_152,
                    transient_bytes: 2_097_152,
                    permanent_entries: 1,
                    transient_entries: 1,
                    consumable_permanent_paths: vec![(
                        "artifacts/recovery-007.json".into(),
                        2_097_152,
                    )],
                    consumable_transient_path: Some("run.json".into()),
                },
            ),
            (
                "v2 VerifyExisting decision",
                EvaluationRecoveryTransition::Active {
                    recovery_id: 7,
                    kind: EvaluationRecoveryTransitionKind::AdoptionVerifyExisting,
                    decision: DurableDecisionState::Verified,
                    report: AdoptionReportState::NotApplicable,
                },
                StorageCommitment {
                    permanent_bytes: 0,
                    transient_bytes: 2_097_152,
                    permanent_entries: 0,
                    transient_entries: 1,
                    consumable_permanent_paths: vec![],
                    consumable_transient_path: Some("run.json".into()),
                },
            ),
            (
                "v3 source",
                EvaluationRecoveryTransition::Active {
                    recovery_id: 7,
                    kind: EvaluationRecoveryTransitionKind::Invalidation,
                    decision: DurableDecisionState::Missing,
                    report: AdoptionReportState::NotApplicable,
                },
                StorageCommitment {
                    permanent_bytes: 2_097_152,
                    transient_bytes: 2_097_152,
                    permanent_entries: 1,
                    transient_entries: 1,
                    consumable_permanent_paths: vec![(
                        "artifacts/recovery-007.json".into(),
                        2_097_152,
                    )],
                    consumable_transient_path: Some("run.json".into()),
                },
            ),
            (
                "v3 decision",
                EvaluationRecoveryTransition::Active {
                    recovery_id: 7,
                    kind: EvaluationRecoveryTransitionKind::Invalidation,
                    decision: DurableDecisionState::Verified,
                    report: AdoptionReportState::NotApplicable,
                },
                StorageCommitment {
                    permanent_bytes: 0,
                    transient_bytes: 2_097_152,
                    permanent_entries: 0,
                    transient_entries: 1,
                    consumable_permanent_paths: vec![],
                    consumable_transient_path: Some("run.json".into()),
                },
            ),
        ];

        for (label, transition, expected) in cases {
            assert_eq!(
                recovery_transition_commitment(transition).unwrap(),
                expected,
                "{label}"
            );
        }
    }

    #[test]
    fn terminal_run_without_a_later_staged_recovery_has_no_active_commitment() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let run_directory = temp.path().join("terminal-run");
        std::fs::create_dir(&run_directory).unwrap();
        std::fs::set_permissions(&run_directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let artifacts = run_directory.join("artifacts");
        std::fs::create_dir(&artifacts).unwrap();
        std::fs::set_permissions(&artifacts, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut terminal = run();
        terminal.status = LoopStatus::EvalPassed;

        assert_eq!(
            derive_active_evaluation_storage_commitment(&run_directory, &terminal).unwrap(),
            None
        );
    }

    fn run() -> LoopRun {
        crate::state::create_run(crate::state::NewLoopRun {
            run_id: "eval-commitment".into(),
            ticket_id: "ticket".into(),
            goal_id: "goal".into(),
            provider: "local".into(),
            model: "local".into(),
            input_digests: LoopInputDigests {
                ticket: "1".repeat(64),
                policy: "2".repeat(64),
                config: "3".repeat(64),
                repository: "4".repeat(64),
                eval_config: Some("2".repeat(64)),
            },
        })
    }

    fn config() -> EvalConfig {
        EvalConfig {
            evals: EvalGroup {
                allow_commands: vec![],
                required: [None, Some(1), Some(1024 * 1024)]
                    .into_iter()
                    .enumerate()
                    .map(|(index, max_output_bytes)| EvalCommandConfig {
                        name: format!("check-{index}"),
                        command: "true".into(),
                        cwd: None,
                        env: BTreeMap::new(),
                        timeout_ms: None,
                        max_output_bytes,
                    })
                    .collect(),
            },
            thresholds: None,
        }
    }

    fn prefix() -> EvaluationCommitmentPrefix {
        EvaluationCommitmentPrefix {
            attempt: 1,
            intent: "artifacts/07-testing.attempt-001.execution-intent.json".into(),
            testing: None,
            report: None,
            logs: BTreeMap::new(),
        }
    }

    #[test]
    fn normal_prefix_reserves_and_consumes_each_exact_output_and_recovery_slot() {
        let run = run();
        let config = config();
        let mut prefix = prefix();
        let initial = normal_prefix_commitment(&run, &prefix, &config, |_| {
            Err("intent-only prefix has no physical logs".into())
        })
        .unwrap();
        let log_bytes = 2 * (64 * 1024 + 1 + 1024 * 1024);
        assert_eq!(
            initial.permanent_bytes,
            u64::try_from(log_bytes).unwrap() + 4 * EVALUATION_ARTIFACT_CAP
        );
        assert_eq!(initial.permanent_entries, 10);
        assert_eq!(initial.transient_bytes, FINAL_RUN_REPLACEMENT_CAP);
        assert_eq!(initial.transient_entries, 1);
        assert_eq!(
            initial.consumable_permanent_paths,
            vec![
                (
                    "artifacts/07-testing.attempt-001.check-001.stdout.log".into(),
                    64 * 1024
                ),
                (
                    "artifacts/07-testing.attempt-001.check-001.stderr.log".into(),
                    64 * 1024
                ),
                (
                    "artifacts/07-testing.attempt-001.check-002.stdout.log".into(),
                    1
                ),
                (
                    "artifacts/07-testing.attempt-001.check-002.stderr.log".into(),
                    1
                ),
                (
                    "artifacts/07-testing.attempt-001.check-003.stdout.log".into(),
                    1024 * 1024
                ),
                (
                    "artifacts/07-testing.attempt-001.check-003.stderr.log".into(),
                    1024 * 1024
                ),
                (
                    "artifacts/07-testing.attempt-001.json".into(),
                    EVALUATION_ARTIFACT_CAP
                ),
                (
                    "artifacts/08-eval-report.attempt-001.json".into(),
                    EVALUATION_ARTIFACT_CAP
                ),
                (
                    "artifacts/recovery-001.source-run.json".into(),
                    EVALUATION_ARTIFACT_CAP
                ),
                (
                    "artifacts/recovery-001.json".into(),
                    EVALUATION_ARTIFACT_CAP
                ),
            ]
        );
        assert_eq!(
            initial.consumable_transient_path.as_deref(),
            Some("run.json")
        );

        prefix.logs.insert(
            1,
            (
                Some("artifacts/07-testing.attempt-001.check-001.stdout.log".into()),
                None,
            ),
        );
        let trailing_stdout = normal_prefix_commitment(&run, &prefix, &config, |_| Ok(1)).unwrap();
        assert_eq!(trailing_stdout.permanent_bytes, initial.permanent_bytes - 1);
        assert_eq!(trailing_stdout.permanent_entries, 9);
        assert_eq!(
            1 + trailing_stdout.permanent_bytes,
            initial.permanent_bytes,
            "an unauthenticated one-byte same-name log must retain the other 65,535 bytes"
        );
        assert!(!trailing_stdout
            .consumable_permanent_paths
            .iter()
            .any(|(path, _)| path.ends_with("check-001.stdout.log")));

        for check in 1..=3_u32 {
            prefix.logs.insert(
                check,
                (
                    Some(format!(
                        "artifacts/07-testing.attempt-001.check-{check:03}.stdout.log"
                    )),
                    Some(format!(
                        "artifacts/07-testing.attempt-001.check-{check:03}.stderr.log"
                    )),
                ),
            );
        }
        let logs_complete = normal_prefix_commitment(&run, &prefix, &config, |_| Ok(1)).unwrap();
        assert_eq!(
            logs_complete.permanent_bytes,
            u64::try_from(log_bytes).unwrap() - 6 + 4 * EVALUATION_ARTIFACT_CAP
        );
        assert_eq!(logs_complete.permanent_entries, 4);

        prefix.testing = Some("artifacts/07-testing.attempt-001.json".into());
        let testing = normal_prefix_commitment(&run, &prefix, &config, |_| {
            Err("Testing authenticates all log slots".into())
        })
        .unwrap();
        assert_eq!(testing.permanent_bytes, 3 * EVALUATION_ARTIFACT_CAP);
        assert_eq!(testing.permanent_entries, 3);
        assert!(testing
            .consumable_permanent_paths
            .iter()
            .any(|(path, _)| path == "artifacts/recovery-001.source-run.json"));

        prefix.report = Some("artifacts/08-eval-report.attempt-001.json".into());
        let report = normal_prefix_commitment(&run, &prefix, &config, |_| {
            Err("Testing authenticates all log slots".into())
        })
        .unwrap();
        assert_eq!(report.permanent_bytes, 2 * EVALUATION_ARTIFACT_CAP);
        assert_eq!(report.permanent_entries, 2);
        assert_eq!(
            report.consumable_permanent_paths,
            vec![
                ("artifacts/recovery-001.source-run.json".into(), 2_097_152,),
                ("artifacts/recovery-001.json".into(), 2_097_152),
            ]
        );
        assert_eq!(report.transient_bytes, FINAL_RUN_REPLACEMENT_CAP);
    }

    #[test]
    fn testing_cannot_release_missing_log_slots() {
        let run = run();
        let mut prefix = prefix();
        prefix.testing = Some("artifacts/07-testing.attempt-001.json".into());
        let error = normal_prefix_commitment(&run, &prefix, &config(), |_| {
            Err("Testing prefix has no logs".into())
        })
        .unwrap_err();
        assert!(error.contains("complete evaluation logs"), "{error}");
    }
}
