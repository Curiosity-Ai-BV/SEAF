use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, fs,
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, validate_provider_exchange_record,
    ArtifactReference, LoopRun, LoopStepName, ProviderExchangeKind, ProviderExchangeOutcome,
    ProviderExchangePhase, ProviderExchangeRecord, ProviderExchangeRecordReference, ProviderRole,
};
use seaf_models::{ModelError, ModelResponse};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    immutable_artifact::{publish_create_only, read_verified_regular_file, ImmutableArtifactError},
    role_response::{
        parse_role_response, AgentStatus, DeveloperStatus, ReviewDecision, Role, RoleResponse,
        RoleResponseError,
    },
    state::{self, step_file_stem},
    LoopWorkspace,
};

pub const PROVIDER_EXCHANGE_SCHEMA_VERSION: u32 = 1;
const PROVIDER_RERUN_AUTHORIZATION_SCHEMA_VERSION: u32 = 1;
const PROVIDER_EXCHANGE_LOCK_FILE: &str = "provider-exchange.lock";
static RUN_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExchangeCoordinates {
    pub run_id: String,
    pub step: LoopStepName,
    pub role: ProviderRole,
    pub step_attempt: u32,
    pub exchange_index: u32,
    pub kind: ProviderExchangeKind,
    pub context_round: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderExchangeRecordState {
    Staged,
    Referenced { position: usize },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderExchangeResponseAudit {
    ModelResponse { response: ModelResponse },
    ProviderFailure { error: ModelError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderRerunAuthorization {
    schema_version: u32,
    run_id: String,
    step: LoopStepName,
    step_attempt: u32,
    previous_record_digest: Option<String>,
}

#[cfg(test)]
pub(crate) fn authorize_provider_exchange_rerun(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    step: LoopStepName,
    step_attempt: u32,
) -> Result<(), ProviderExchangeError> {
    validate_authoritative_provider_exchange_records(workspace, run)?;
    let authorization = ProviderRerunAuthorization {
        schema_version: PROVIDER_RERUN_AUTHORIZATION_SCHEMA_VERSION,
        run_id: run.run_id.clone(),
        step,
        step_attempt,
        previous_record_digest: run
            .provider_exchange_records
            .last()
            .map(|reference| reference.digest.clone()),
    };
    let bytes = canonical_json_bytes(&authorization)?;
    publish_create_only(
        workspace.run_directory(),
        &rerun_authorization_path(step, step_attempt),
        &bytes,
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn persist_provider_rerun_reset(
    workspace: &LoopWorkspace,
    previous: &LoopRun,
    reset: &LoopRun,
    step: LoopStepName,
    step_attempt: u32,
) -> Result<(), ProviderExchangeError> {
    persist_provider_rerun_reset_with_hook(
        workspace,
        previous,
        reset,
        step,
        step_attempt,
        || Ok(()),
    )
}

#[cfg(test)]
fn persist_provider_rerun_reset_with_hook<F>(
    workspace: &LoopWorkspace,
    previous: &LoopRun,
    reset: &LoopRun,
    step: LoopStepName,
    step_attempt: u32,
    before_run_publish: F,
) -> Result<(), ProviderExchangeError>
where
    F: FnOnce() -> Result<(), ProviderExchangeError>,
{
    let lock = acquire_provider_exchange_lock(workspace)?;
    let result = (|| {
        let current = state::load_run(workspace)?;
        validate_authoritative_provider_exchange_records(workspace, &current)?;
        if current != *previous {
            return Err(ProviderExchangeError::Invalid(
                "loop state changed before provider rerun reset publication".to_string(),
            ));
        }
        if state::is_frozen_review_or_evaluation_authority(&current) {
            return Err(ProviderExchangeError::Invalid(
                "provider rerun reset cannot replace human review authority or final evaluation authority"
                    .to_string(),
            ));
        }
        if reset.provider_exchange_records != current.provider_exchange_records {
            return Err(ProviderExchangeError::Invalid(
                "provider rerun reset changed the authoritative exchange head".to_string(),
            ));
        }
        let expected_attempt = crate::artifacts::next_step_attempt(workspace, step)
            .map_err(|error| ProviderExchangeError::Invalid(error.to_string()))?;
        if step_attempt != expected_attempt {
            return Err(ProviderExchangeError::Invalid(format!(
                "provider rerun reset must use exact step attempt {expected_attempt}"
            )));
        }
        let reset_status = reset
            .steps
            .iter()
            .find(|record| record.name == step)
            .map(|record| record.status);
        if reset.current_step != step
            || reset.status != seaf_core::LoopStatus::Pending
            || reset_status != Some(seaf_core::LoopStepStatus::Pending)
        {
            return Err(ProviderExchangeError::Invalid(
                "provider rerun authorization requires the matching pending reset state"
                    .to_string(),
            ));
        }
        validate_run_for_atomic_publication(workspace, reset)?;

        let authorization = ProviderRerunAuthorization {
            schema_version: PROVIDER_RERUN_AUTHORIZATION_SCHEMA_VERSION,
            run_id: current.run_id.clone(),
            step,
            step_attempt,
            previous_record_digest: current
                .provider_exchange_records
                .last()
                .map(|reference| reference.digest.clone()),
        };
        let authorization_bytes = canonical_json_bytes(&authorization)?;
        publish_create_only(
            workspace.run_directory(),
            &rerun_authorization_path(step, step_attempt),
            &authorization_bytes,
        )?;

        let mut run_bytes = serde_json::to_vec_pretty(reset)?;
        run_bytes.push(b'\n');
        replace_run_file_atomically_with_hook(&workspace.run_file(), &run_bytes, || {
            before_run_publish()?;
            validate_opened_lock_file(
                &lock,
                &workspace.run_directory().join(PROVIDER_EXCHANGE_LOCK_FILE),
            )?;
            Ok(())
        })
    })();
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(error), _) => Err(error),
    }
}

pub fn write_provider_exchange_request(
    run_directory: &Path,
    coordinates: &ProviderExchangeCoordinates,
    bytes: &[u8],
) -> Result<ArtifactReference, ProviderExchangeError> {
    validate_coordinates(coordinates)?;
    write_exchange_bytes(run_directory, request_path(coordinates), bytes)
}

pub fn write_provider_exchange_response(
    run_directory: &Path,
    coordinates: &ProviderExchangeCoordinates,
    audit: &ProviderExchangeResponseAudit,
) -> Result<ArtifactReference, ProviderExchangeError> {
    validate_coordinates(coordinates)?;
    let bytes = canonical_json_bytes(audit)?;
    write_exchange_bytes(run_directory, response_path(coordinates), &bytes)
}

pub fn load_provider_exchange_request(
    run_directory: &Path,
    reference: &ArtifactReference,
) -> Result<Vec<u8>, ProviderExchangeError> {
    let bytes = read_verified_regular_file(run_directory, &reference.path, "provider request")?;
    if digest_bytes(&bytes) != reference.digest {
        return Err(ProviderExchangeError::Invalid(
            "provider request audit digest does not match its reference".to_string(),
        ));
    }
    Ok(bytes)
}

pub(crate) fn load_provider_exchange_response_audit(
    run_directory: &Path,
    reference: &ArtifactReference,
) -> Result<ProviderExchangeResponseAudit, ProviderExchangeError> {
    let bytes = read_verified_regular_file(run_directory, &reference.path, "provider response")?;
    if digest_bytes(&bytes) != reference.digest {
        return Err(ProviderExchangeError::Invalid(
            "provider response audit digest does not match its reference".to_string(),
        ));
    }
    let audit: ProviderExchangeResponseAudit = serde_json::from_slice(&bytes)?;
    if canonical_json_bytes(&audit)? != bytes {
        return Err(ProviderExchangeError::Invalid(
            "provider response audit is not canonical JSON".to_string(),
        ));
    }
    Ok(audit)
}

pub fn stage_provider_exchange_record(
    run_directory: &Path,
    record: &ProviderExchangeRecord,
) -> Result<ProviderExchangeRecordReference, ProviderExchangeError> {
    validate_record(record)?;
    verify_bound_artifact(run_directory, &record.request, "provider request")?;
    if let Some(response) = &record.response {
        verify_bound_artifact(run_directory, response, "provider response")?;
    }
    if let Some(expansion) = &record.expansion {
        verify_bound_artifact(run_directory, expansion, "context expansion")?;
    }
    validate_derived_response_outcome(run_directory, record)?;
    let reference = record_reference(record, canonical_sha256_digest(record)?);
    let bytes = canonical_json_bytes(record)?;
    publish_create_only(run_directory, &reference.path, &bytes)?;
    Ok(reference)
}

pub(crate) fn stage_provider_exchange_response_record(
    run_directory: &Path,
    mut record: ProviderExchangeRecord,
) -> Result<
    (
        ProviderExchangeRecordReference,
        ProviderExchangeResponseClassification,
    ),
    ProviderExchangeError,
> {
    if record.phase != ProviderExchangePhase::Response || record.outcome.is_some() {
        return Err(ProviderExchangeError::Invalid(
            "derived response staging requires a response record without a caller outcome"
                .to_string(),
        ));
    }
    verify_bound_artifact(run_directory, &record.request, "provider request")?;
    let response = record.response.as_ref().ok_or_else(|| {
        ProviderExchangeError::Invalid("response record has no response audit".to_string())
    })?;
    verify_bound_artifact(run_directory, response, "provider response")?;
    if let Some(expansion) = &record.expansion {
        verify_bound_artifact(run_directory, expansion, "context expansion")?;
    }
    let classification =
        classify_bound_provider_exchange_response(run_directory, record.role, response)?;
    record.outcome = Some(classification.outcome);
    validate_record(&record)?;
    let reference = record_reference(&record, canonical_sha256_digest(&record)?);
    let bytes = canonical_json_bytes(&record)?;
    publish_create_only(run_directory, &reference.path, &bytes)?;
    Ok((reference, classification))
}

pub fn load_provider_exchange_record(
    run_directory: &Path,
    reference: &ProviderExchangeRecordReference,
) -> Result<ProviderExchangeRecord, ProviderExchangeError> {
    validate_reference_identity(reference)?;
    let bytes =
        read_verified_regular_file(run_directory, &reference.path, "provider exchange record")?;
    if digest_bytes(&bytes) != reference.digest {
        return Err(ProviderExchangeError::Invalid(
            "provider exchange record digest does not match its reference".to_string(),
        ));
    }
    let record: ProviderExchangeRecord = serde_json::from_slice(&bytes)?;
    if canonical_json_bytes(&record)? != bytes {
        return Err(ProviderExchangeError::Invalid(
            "provider exchange record is not canonical JSON".to_string(),
        ));
    }
    validate_record(&record)?;
    if record_reference(&record, reference.digest.clone()) != *reference {
        return Err(ProviderExchangeError::Invalid(
            "provider exchange record identity does not match its reference".to_string(),
        ));
    }
    verify_bound_artifact(run_directory, &record.request, "provider request")?;
    if let Some(response) = &record.response {
        verify_bound_artifact(run_directory, response, "provider response")?;
    }
    if let Some(expansion) = &record.expansion {
        verify_bound_artifact(run_directory, expansion, "context expansion")?;
    }
    validate_derived_response_outcome(run_directory, &record)?;
    Ok(record)
}

pub fn classify_provider_exchange_record(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    reference: &ProviderExchangeRecordReference,
) -> Result<ProviderExchangeRecordState, ProviderExchangeError> {
    if run.run_id != reference.run_id {
        return Err(ProviderExchangeError::Invalid(
            "provider exchange reference belongs to another run".to_string(),
        ));
    }
    if let Some(authoritative) = run.provider_exchange_records.iter().find(|candidate| {
        candidate.path == reference.path
            || (candidate.step == reference.step
                && candidate.step_attempt == reference.step_attempt
                && candidate.exchange_index == reference.exchange_index
                && candidate.phase == reference.phase)
    }) {
        if authoritative != reference {
            return Err(ProviderExchangeError::Invalid(
                "staged reference conflicts with an authoritative exchange identity".to_string(),
            ));
        }
    }
    load_provider_exchange_record(workspace.run_directory(), reference)?;
    Ok(run
        .provider_exchange_records
        .iter()
        .position(|candidate| candidate == reference)
        .map_or(ProviderExchangeRecordState::Staged, |position| {
            ProviderExchangeRecordState::Referenced { position }
        }))
}

pub fn persist_provider_exchange_record_reference(
    workspace: &LoopWorkspace,
    reference: ProviderExchangeRecordReference,
) -> Result<LoopRun, ProviderExchangeError> {
    if reference.step == LoopStepName::OutputReview
        && reference.role == ProviderRole::OutputReviewer
        && reference.kind == ProviderExchangeKind::Initial
        && reference.phase == ProviderExchangePhase::Request
    {
        return Err(ProviderExchangeError::Invalid(
            "OutputReview initial requests require the authenticated ProviderStepRunner path"
                .to_string(),
        ));
    }
    persist_provider_exchange_record_reference_with_validator(workspace, reference, |_| Ok(()))
}

pub(crate) fn persist_provider_exchange_record_reference_with_validator<F>(
    workspace: &LoopWorkspace,
    reference: ProviderExchangeRecordReference,
    validate_prospective: F,
) -> Result<LoopRun, ProviderExchangeError>
where
    F: Fn(&LoopRun) -> Result<(), ProviderExchangeError>,
{
    let lock = acquire_provider_exchange_lock(workspace)?;
    let result = (|| {
        let mut run = state::load_run(workspace)?;
        if state::is_frozen_review_or_evaluation_authority(&run) {
            return Err(ProviderExchangeError::Invalid(
                "provider exchange history is frozen by human review or final evaluation authority"
                    .to_string(),
            ));
        }
        if run.provider_exchange_records.contains(&reference) {
            return Err(ProviderExchangeError::Invalid(
                "provider exchange record is already referenced".to_string(),
            ));
        }
        validate_provider_exchange_record_append(workspace, &run, &reference)?;
        run.provider_exchange_records.push(reference);
        validate_prospective(&run)?;
        validate_run_for_atomic_publication(workspace, &run)?;
        let mut bytes = serde_json::to_vec_pretty(&run)?;
        bytes.push(b'\n');
        replace_run_file_atomically_with_hook(&workspace.run_file(), &bytes, || {
            validate_opened_lock_file(
                &lock,
                &workspace.run_directory().join(PROVIDER_EXCHANGE_LOCK_FILE),
            )?;
            Ok(())
        })?;
        Ok(run)
    })();
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(run), Ok(())) => Ok(run),
        (Ok(_), Err(error)) => Err(error.into()),
        (Err(error), _) => Err(error),
    }
}

pub(crate) fn persist_run_with_provider_exchange_compare(
    workspace: &LoopWorkspace,
    intended: &LoopRun,
) -> Result<(), ProviderExchangeError> {
    let lock = acquire_provider_exchange_lock(workspace)?;
    let result = (|| {
        let current = state::load_run(workspace)?;
        if state::is_frozen_review_or_evaluation_authority(&current) && &current != intended {
            return Err(ProviderExchangeError::Invalid(
                "ordinary state publication cannot replace awaiting human review, approved authority, or final evaluation authority"
                    .to_string(),
            ));
        }
        if current.provider_exchange_records != intended.provider_exchange_records {
            return Err(ProviderExchangeError::Invalid(
                "provider exchange head changed before state publication".to_string(),
            ));
        }
        if current.candidate_workspace != intended.candidate_workspace {
            return Err(ProviderExchangeError::Invalid(
                "candidate workspace changed before ordinary state publication".to_string(),
            ));
        }
        if current.latest_recovery != intended.latest_recovery {
            return Err(ProviderExchangeError::Invalid(
                "ordinary state publication cannot mint, replace, or clear recovery authority"
                    .to_string(),
            ));
        }
        validate_run_for_atomic_publication(workspace, intended)?;
        let mut bytes = serde_json::to_vec_pretty(intended)?;
        bytes.push(b'\n');
        replace_run_file_atomically_with_hook(&workspace.run_file(), &bytes, || {
            validate_opened_lock_file(
                &lock,
                &workspace.run_directory().join(PROVIDER_EXCHANGE_LOCK_FILE),
            )?;
            Ok(())
        })
    })();
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(error), _) => Err(error),
    }
}

pub(crate) fn persist_run_with_full_compare(
    workspace: &LoopWorkspace,
    expected: &LoopRun,
    intended: &LoopRun,
) -> Result<(), ProviderExchangeError> {
    persist_run_with_full_compare_and_validator(workspace, expected, intended, |_| Ok(()))
}

pub(crate) fn persist_run_with_full_compare_and_validator<F>(
    workspace: &LoopWorkspace,
    expected: &LoopRun,
    intended: &LoopRun,
    validate_current: F,
) -> Result<(), ProviderExchangeError>
where
    F: FnOnce(&LoopRun) -> Result<(), ProviderExchangeError>,
{
    persist_run_with_full_compare_and_validator_mode(
        workspace,
        expected,
        intended,
        false,
        validate_current,
    )
}

pub(crate) fn persist_recovery_reset_with_full_compare_and_validator<F>(
    workspace: &LoopWorkspace,
    expected: &LoopRun,
    intended: &LoopRun,
    validate_current: F,
) -> Result<(), ProviderExchangeError>
where
    F: FnOnce(&LoopRun) -> Result<(), ProviderExchangeError>,
{
    persist_run_with_full_compare_and_validator_mode(
        workspace,
        expected,
        intended,
        true,
        validate_current,
    )
}

pub(crate) fn persist_evaluation_adoption_with_validator<F>(
    workspace: &LoopWorkspace,
    expected: &LoopRun,
    intended: &LoopRun,
    validate_current: F,
) -> Result<(), ProviderExchangeError>
where
    F: FnOnce(&LoopRun) -> Result<(), ProviderExchangeError>,
{
    persist_run_with_full_compare_and_validator_mode(
        workspace,
        expected,
        intended,
        true,
        |current| {
            if current.status != seaf_core::LoopStatus::Approved
                || current.current_step != seaf_core::LoopStepName::Testing
            {
                return Err(ProviderExchangeError::Invalid(
                    "evaluation adoption CAS requires exact Approved Testing authority".to_string(),
                ));
            }
            let reference = intended.latest_recovery.as_ref().ok_or_else(|| {
                ProviderExchangeError::Invalid(
                    "evaluation adoption final lost recovery authority".to_string(),
                )
            })?;
            let expected_id = current
                .latest_recovery
                .as_ref()
                .map_or(Some(1), |previous| previous.recovery_id.checked_add(1));
            if expected_id != Some(reference.recovery_id) {
                return Err(ProviderExchangeError::Invalid(
                    "evaluation adoption did not advance exactly one recovery ID".to_string(),
                ));
            }
            let (_, source) =
                crate::recovery::load_verified_evaluation_recovery(workspace, reference)
                    .map_err(|error| ProviderExchangeError::Invalid(error.to_string()))?
                    .ok_or_else(|| {
                        ProviderExchangeError::Invalid(
                            "evaluation adoption CAS requires evaluation-v2 recovery".to_string(),
                        )
                    })?;
            if source.run != *current {
                return Err(ProviderExchangeError::Invalid(
                    "evaluation adoption recovery does not bind the exact locked Approved source"
                        .to_string(),
                ));
            }
            validate_current(current)
        },
    )
}

fn persist_run_with_full_compare_and_validator_mode<F>(
    workspace: &LoopWorkspace,
    expected: &LoopRun,
    intended: &LoopRun,
    allow_recovery_advance: bool,
    validate_current: F,
) -> Result<(), ProviderExchangeError>
where
    F: FnOnce(&LoopRun) -> Result<(), ProviderExchangeError>,
{
    let lock = acquire_provider_exchange_lock(workspace)?;
    let result = (|| {
        let current = state::load_run(workspace)?;
        if &current != expected {
            return Err(ProviderExchangeError::Invalid(
                "LoopRun changed before compare-and-swap publication".to_string(),
            ));
        }
        if !allow_recovery_advance && current.latest_recovery != intended.latest_recovery {
            return Err(ProviderExchangeError::Invalid(
                "latest recovery authority is immutable outside audited recovery creation"
                    .to_string(),
            ));
        }
        validate_current(&current)?;
        validate_final_authority_cas_relation(workspace, &current, intended)?;
        validate_run_for_atomic_publication(workspace, intended)?;
        let mut bytes = serde_json::to_vec_pretty(intended)?;
        bytes.push(b'\n');
        replace_run_file_atomically_with_hook(&workspace.run_file(), &bytes, || {
            validate_opened_lock_file(
                &lock,
                &workspace.run_directory().join(PROVIDER_EXCHANGE_LOCK_FILE),
            )?;
            Ok(())
        })
    })();
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(error), _) => Err(error),
    }
}

pub(crate) fn reconcile_provider_exchange_state_with_validator<F>(
    workspace: &LoopWorkspace,
    authoritative: &LoopRun,
    validate_prospective: F,
) -> Result<LoopRun, ProviderExchangeError>
where
    F: Fn(&LoopRun) -> Result<(), ProviderExchangeError>,
{
    let lock = acquire_provider_exchange_lock(workspace)?;
    let result = (|| {
        let persisted = state::load_run(workspace)?;
        if state::is_frozen_review_or_evaluation_authority(&persisted) {
            return Err(ProviderExchangeError::Invalid(
                "provider exchange reconciliation is frozen by human review or final evaluation authority"
                    .to_string(),
            ));
        }
        if persisted != *authoritative {
            return Err(ProviderExchangeError::Invalid(
                "persisted LoopRun differs from the verified reconciliation authority".to_string(),
            ));
        }
        let prospective = preflight_provider_exchange_reconciliation(workspace, authoritative)?;
        validate_prospective(&prospective)?;
        if prospective.provider_exchange_records == authoritative.provider_exchange_records {
            return Ok(authoritative.clone());
        }
        validate_run_for_atomic_publication(workspace, &prospective)?;
        let mut bytes = serde_json::to_vec_pretty(&prospective)?;
        bytes.push(b'\n');
        replace_run_file_atomically_with_hook(&workspace.run_file(), &bytes, || {
            validate_opened_lock_file(
                &lock,
                &workspace.run_directory().join(PROVIDER_EXCHANGE_LOCK_FILE),
            )?;
            Ok(())
        })?;
        Ok(prospective)
    })();
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(run), Ok(())) => Ok(run),
        (Ok(_), Err(error)) => Err(error.into()),
        (Err(error), _) => Err(error),
    }
}

pub(crate) fn preflight_provider_exchange_reconciliation(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<LoopRun, ProviderExchangeError> {
    let mut prospective = run.clone();
    let mut staged = BTreeMap::new();
    let authoritative_paths = run
        .provider_exchange_records
        .iter()
        .map(|reference| reference.path.as_str())
        .collect::<BTreeSet<_>>();
    for relative in exchange_family_files(workspace)? {
        if !is_exchange_record_path(&relative) || authoritative_paths.contains(relative.as_str()) {
            continue;
        }
        let bytes = read_verified_regular_file(
            workspace.run_directory(),
            &relative,
            "staged provider exchange record",
        )?;
        let record: ProviderExchangeRecord = serde_json::from_slice(&bytes)?;
        if canonical_json_bytes(&record)? != bytes {
            return Err(ProviderExchangeError::Invalid(format!(
                "staged provider exchange record is not canonical JSON: {relative}"
            )));
        }
        let reference = record_reference(&record, digest_bytes(&bytes));
        if reference.path != relative {
            return Err(ProviderExchangeError::Invalid(format!(
                "staged provider exchange record path does not match its identity: {relative}"
            )));
        }
        load_provider_exchange_record(workspace.run_directory(), &reference)?;
        staged.insert(relative, (record, reference));
    }

    while !staged.is_empty() {
        let mut eligible = Vec::new();
        for (path, (record, reference)) in &staged {
            if validate_append_link(workspace, &prospective, record, true).is_ok() {
                eligible.push((path.clone(), reference.clone()));
            }
        }
        if eligible.len() != 1 {
            return Err(ProviderExchangeError::Invalid(
                "orphaned, reordered, or ambiguous staged provider exchange record".to_string(),
            ));
        }
        let (path, reference) = eligible.pop().expect("one eligible staged record");
        prospective.provider_exchange_records.push(reference);
        staged.remove(&path);
    }

    validate_authoritative_provider_exchange_records(workspace, &prospective)?;
    for reference in &prospective.provider_exchange_records {
        let record = load_provider_exchange_record(workspace.run_directory(), reference)?;
        if record.phase == ProviderExchangePhase::Request
            && record.kind == ProviderExchangeKind::Initial
        {
            verify_conventional_initial_prompt(workspace, &record)?;
        }
    }
    let mut bound = BTreeSet::new();
    for reference in &prospective.provider_exchange_records {
        let record = load_provider_exchange_record(workspace.run_directory(), reference)?;
        bound.insert(reference.path.clone());
        bound.insert(record.request.path);
        if let Some(response) = record.response {
            bound.insert(response.path);
        }
        if let Some(expansion) = record.expansion {
            bound.insert(expansion.path);
        }
    }
    for relative in exchange_family_files(workspace)? {
        if bound.contains(&relative) {
            continue;
        }
        return Err(ProviderExchangeError::Invalid(format!(
            "orphaned provider exchange artifact: {relative}"
        )));
    }
    Ok(prospective)
}

fn verify_conventional_initial_prompt(
    workspace: &LoopWorkspace,
    record: &ProviderExchangeRecord,
) -> Result<(), ProviderExchangeError> {
    let stem = step_file_stem(record.step);
    let file_name = if record.step_attempt == 1 {
        format!("{stem}.prompt.md")
    } else {
        format!("{stem}.attempt-{:03}.prompt.md", record.step_attempt)
    };
    let path = format!("prompts/{file_name}");
    let conventional = read_verified_regular_file(
        workspace.run_directory(),
        &path,
        "conventional provider prompt",
    )?;
    let audited = load_provider_exchange_request(workspace.run_directory(), &record.request)?;
    if conventional != audited {
        return Err(ProviderExchangeError::Invalid(
            "conventional provider prompt does not match the staged initial request bytes"
                .to_string(),
        ));
    }
    Ok(())
}

fn exchange_family_files(workspace: &LoopWorkspace) -> Result<Vec<String>, ProviderExchangeError> {
    let mut files = Vec::new();
    for directory in ["prompts", "responses", "artifacts"] {
        for entry in fs::read_dir(workspace.run_directory().join(directory))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let exchange = name.contains(".attempt-")
                && (name.contains(".exchange-") || name.contains(".context-round-"));
            if exchange {
                let metadata = fs::symlink_metadata(entry.path())?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(ProviderExchangeError::Invalid(format!(
                        "provider exchange artifact is not a real regular file: {directory}/{name}"
                    )));
                }
                files.push(format!("{directory}/{name}"));
            }
        }
    }
    files.sort();
    Ok(files)
}

fn is_exchange_record_path(path: &str) -> bool {
    path.starts_with("artifacts/") && path.ends_with(".record.json")
}

pub fn validate_provider_exchange_record_append(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    reference: &ProviderExchangeRecordReference,
) -> Result<(), ProviderExchangeError> {
    validate_authoritative_provider_exchange_records(workspace, run)?;
    let record = load_provider_exchange_record(workspace.run_directory(), reference)?;
    validate_append_link(workspace, run, &record, false)?;
    let mut prospective = run.clone();
    prospective
        .provider_exchange_records
        .push(reference.clone());
    let errors = seaf_core::validate_loop_run(&prospective);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ProviderExchangeError::Invalid(
            errors
                .into_iter()
                .map(|error| format!("{}: {}", error.field, error.message))
                .collect::<Vec<_>>()
                .join("; "),
        ))
    }
}

fn acquire_provider_exchange_lock(
    workspace: &LoopWorkspace,
) -> Result<fs::File, ProviderExchangeError> {
    let path = workspace.run_directory().join(PROVIDER_EXCHANGE_LOCK_FILE);
    let mut created = false;
    let file = match inspect_lock_path(&path) {
        Ok(()) => open_existing_lock_file(&path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match create_lock_file(&path) {
                Ok(file) => {
                    created = true;
                    file
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    inspect_lock_path(&path)?;
                    open_existing_lock_file(&path)?
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    validate_opened_lock_file(&file, &path)?;
    if created {
        file.sync_all()?;
        fs::File::open(workspace.run_directory())?.sync_all()?;
    }
    file.lock()?;
    if let Err(error) = validate_opened_lock_file(&file, &path) {
        let _ = file.unlock();
        return Err(error.into());
    }
    Ok(file)
}

fn create_lock_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    set_no_follow(&mut options);
    options.open(path)
}

fn open_existing_lock_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    set_no_follow(&mut options);
    options.open(path)
}

#[cfg(target_os = "macos")]
fn set_no_follow(options: &mut fs::OpenOptions) {
    options.custom_flags(0x100);
}

#[cfg(target_os = "linux")]
fn set_no_follow(options: &mut fs::OpenOptions) {
    options.custom_flags(0x20_000);
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn set_no_follow(_options: &mut fs::OpenOptions) {}

fn inspect_lock_path(path: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "provider exchange lock must be a real regular file",
        ));
    }
    Ok(())
}

fn validate_opened_lock_file(file: &fs::File, path: &Path) -> std::io::Result<()> {
    inspect_lock_path(path)?;
    let opened = file.metadata()?;
    let current = fs::metadata(path)?;
    if !metadata_identity_matches(&opened, &current) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "provider exchange lock path changed while it was opened",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn metadata_identity_matches(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn metadata_identity_matches(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    false
}

pub(crate) fn validate_run_for_atomic_publication(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<(), ProviderExchangeError> {
    let errors = seaf_core::validate_loop_run(run);
    if !errors.is_empty() {
        return Err(ProviderExchangeError::Invalid(
            errors
                .into_iter()
                .map(|error| format!("{}: {}", error.field, error.message))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    validate_authoritative_provider_exchange_records(workspace, run)?;
    if run.status == seaf_core::LoopStatus::AwaitingHumanReview || run.human_approval.is_some() {
        let terminal = run.provider_exchange_records.last().ok_or_else(|| {
            ProviderExchangeError::Invalid(
                "human review authority lost terminal OutputReview provider evidence".to_string(),
            )
        })?;
        let record = load_provider_exchange_record(workspace.run_directory(), terminal)?;
        if record.outcome != Some(ProviderExchangeOutcome::ApproveForTests) {
            return Err(ProviderExchangeError::Invalid(
                "human review authority requires an authenticated OutputReview ApproveForTests outcome"
                    .to_string(),
            ));
        }
        let latest_attempt =
            crate::artifacts::latest_step_attempt(workspace, LoopStepName::OutputReview)
                .map_err(|error| ProviderExchangeError::Invalid(error.to_string()))?;
        if latest_attempt != Some(terminal.step_attempt) {
            return Err(ProviderExchangeError::Invalid(
                "human review authority requires the current OutputReview attempt's authenticated response"
                    .to_string(),
            ));
        }
        if run.human_approval.is_some() {
            validate_approved_publication_evidence(workspace, run)?;
        }
    }
    if matches!(
        run.status,
        seaf_core::LoopStatus::EvalPassed | seaf_core::LoopStatus::Promoted
    ) || (run.status == seaf_core::LoopStatus::Failed && run.human_approval.is_some())
    {
        crate::load_verified_final_evaluation_authority(workspace, run).map_err(|error| {
            ProviderExchangeError::Invalid(format!(
                "final evaluation authority validation failed: {error}"
            ))
        })?;
    }
    Ok(())
}

fn validate_final_authority_cas_relation(
    workspace: &LoopWorkspace,
    current: &LoopRun,
    intended: &LoopRun,
) -> Result<(), ProviderExchangeError> {
    let intended_is_final = matches!(
        intended.status,
        seaf_core::LoopStatus::EvalPassed | seaf_core::LoopStatus::Promoted
    ) || (intended.status == seaf_core::LoopStatus::Failed
        && intended.human_approval.is_some());
    let current_is_final = matches!(
        current.status,
        seaf_core::LoopStatus::EvalPassed | seaf_core::LoopStatus::Promoted
    ) || (current.status == seaf_core::LoopStatus::Failed
        && current.human_approval.is_some());
    if current_is_final && !intended_is_final {
        return Err(ProviderExchangeError::Invalid(
            "final evaluation authority permits only its audited candidate cleanup transition"
                .to_string(),
        ));
    }
    if !intended_is_final {
        return Ok(());
    }

    validate_run_for_atomic_publication(workspace, current)?;
    let intended_authority = crate::load_verified_final_evaluation_authority(workspace, intended)
        .map_err(|error| {
        ProviderExchangeError::Invalid(format!(
            "intended final evaluation authority validation failed: {error}"
        ))
    })?;
    match current.status {
        seaf_core::LoopStatus::Approved => {
            let current_digest = canonical_sha256_digest(current)?;
            if intended_authority.approved_run() != current
                || intended_authority.testing_evidence().approved_run_digest != current_digest
            {
                return Err(ProviderExchangeError::Invalid(
                    "final evaluation authority is not bound to the exact locked Approved predecessor"
                        .to_string(),
                ));
            }
        }
        seaf_core::LoopStatus::EvalPassed => {
            if intended.status == seaf_core::LoopStatus::Promoted {
                let promotion = intended.promotion.as_ref().ok_or_else(|| {
                    ProviderExchangeError::Invalid(
                        "Promoted authority lost promotion evidence".to_string(),
                    )
                })?;
                if canonical_sha256_digest(current)? != promotion.eval_passed_run_digest
                    || promotion.eval_passed_updated_at != current.updated_at
                {
                    return Err(ProviderExchangeError::Invalid(
                        "Promoted authority is not bound to the exact EvalPassed predecessor"
                            .to_string(),
                    ));
                }
            } else if current != intended {
                return Err(ProviderExchangeError::Invalid(
                    "EvalPassed authority is immutable until audited promotion".to_string(),
                ));
            }
            let current_authority = crate::load_verified_final_evaluation_authority(
                workspace, current,
            )
            .map_err(|error| {
                ProviderExchangeError::Invalid(format!(
                    "locked final evaluation authority validation failed: {error}"
                ))
            })?;
            validate_preserved_final_lineage(&current_authority, &intended_authority)?;
        }
        seaf_core::LoopStatus::Promoted => {
            if current != intended {
                return Err(ProviderExchangeError::Invalid(
                    "Promoted authority is immutable".to_string(),
                ));
            }
        }
        seaf_core::LoopStatus::Failed if current.human_approval.is_some() => {
            let current_authority = crate::load_verified_final_evaluation_authority(
                workspace, current,
            )
            .map_err(|error| {
                ProviderExchangeError::Invalid(format!(
                    "locked final evaluation authority validation failed: {error}"
                ))
            })?;
            validate_failed_final_cleanup_transition(
                current,
                intended,
                &current_authority,
                &intended_authority,
            )?;
        }
        _ => {
            return Err(ProviderExchangeError::Invalid(
                "initial final evaluation publication requires exact locked Approved authority"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_failed_final_cleanup_transition(
    current: &LoopRun,
    intended: &LoopRun,
    current_authority: &crate::VerifiedFinalEvaluationAuthority,
    intended_authority: &crate::VerifiedFinalEvaluationAuthority,
) -> Result<(), ProviderExchangeError> {
    if intended.status != seaf_core::LoopStatus::Failed {
        return Err(ProviderExchangeError::Invalid(
            "reported final failure permits only candidate cleanup progression".to_string(),
        ));
    }
    validate_preserved_final_lineage(current_authority, intended_authority)?;
    if current_authority.testing_evidence() != intended_authority.testing_evidence()
        || current_authority.eval_report() != intended_authority.eval_report()
    {
        return Err(ProviderExchangeError::Invalid(
            "reported final failure cleanup cannot replace TestingEvidence or EvalReport authority"
                .to_string(),
        ));
    }
    let current_candidate = current.candidate_workspace.as_ref().ok_or_else(|| {
        ProviderExchangeError::Invalid(
            "reported final failure cleanup lost candidate authority".to_string(),
        )
    })?;
    let intended_candidate = intended.candidate_workspace.as_ref().ok_or_else(|| {
        ProviderExchangeError::Invalid(
            "reported final failure cleanup lost candidate authority".to_string(),
        )
    })?;
    validate_failed_cleanup_state(current_candidate, current_authority)?;
    if current == intended {
        return Ok(());
    }

    let mut allowed = current.clone();
    let allowed_candidate = allowed.candidate_workspace.as_mut().ok_or_else(|| {
        ProviderExchangeError::Invalid(
            "reported final failure cleanup lost candidate authority".to_string(),
        )
    })?;
    let transition_timestamp = match (current_candidate.lifecycle, intended_candidate.lifecycle) {
        (
            seaf_core::CandidateWorkspaceLifecycle::Active,
            seaf_core::CandidateWorkspaceLifecycle::Cleaning,
        ) => {
            let cleanup_started_at = intended_candidate
                .cleanup_started_at
                .as_deref()
                .ok_or_else(|| {
                    ProviderExchangeError::Invalid(
                        "Active to Cleaning cleanup requires cleanup_started_at".to_string(),
                    )
                })?;
            let cleanup_started =
                parse_canonical_unix_seconds(cleanup_started_at).ok_or_else(|| {
                    ProviderExchangeError::Invalid(
                        "cleanup_started_at must be canonical decimal Unix seconds within u64"
                            .to_string(),
                    )
                })?;
            let testing_completed =
                parse_canonical_unix_seconds(&current_authority.testing_evidence().completed_at)
                    .ok_or_else(|| {
                        ProviderExchangeError::Invalid(
                            "TestingEvidence completed_at is not canonical Unix seconds"
                                .to_string(),
                        )
                    })?;
            if cleanup_started < testing_completed {
                return Err(ProviderExchangeError::Invalid(
                    "cleanup_started_at cannot precede TestingEvidence completion".to_string(),
                ));
            }
            allowed_candidate.lifecycle = seaf_core::CandidateWorkspaceLifecycle::Cleaning;
            allowed_candidate.cleanup_started_at = Some(cleanup_started_at.to_string());
            allowed_candidate.cleaned_at = None;
            cleanup_started_at
        }
        (
            seaf_core::CandidateWorkspaceLifecycle::Cleaning,
            seaf_core::CandidateWorkspaceLifecycle::Cleaned,
        ) => {
            let cleanup_started_at =
                current_candidate
                    .cleanup_started_at
                    .as_deref()
                    .ok_or_else(|| {
                        ProviderExchangeError::Invalid(
                            "Cleaning authority lost cleanup_started_at".to_string(),
                        )
                    })?;
            let cleanup_started =
                parse_canonical_unix_seconds(cleanup_started_at).ok_or_else(|| {
                    ProviderExchangeError::Invalid(
                        "cleanup_started_at must be canonical decimal Unix seconds within u64"
                            .to_string(),
                    )
                })?;
            let cleaned_at = intended_candidate.cleaned_at.as_deref().ok_or_else(|| {
                ProviderExchangeError::Invalid(
                    "Cleaning to Cleaned cleanup requires cleaned_at".to_string(),
                )
            })?;
            let cleaned = parse_canonical_unix_seconds(cleaned_at).ok_or_else(|| {
                ProviderExchangeError::Invalid(
                    "cleaned_at must be canonical decimal Unix seconds within u64".to_string(),
                )
            })?;
            if intended_candidate.cleanup_started_at.as_deref() != Some(cleanup_started_at)
                || cleaned < cleanup_started
            {
                return Err(ProviderExchangeError::Invalid(
                    "Cleaned authority must preserve cleanup_started_at and complete monotonically"
                        .to_string(),
                ));
            }
            allowed_candidate.lifecycle = seaf_core::CandidateWorkspaceLifecycle::Cleaned;
            allowed_candidate.cleaned_at = Some(cleaned_at.to_string());
            cleaned_at
        }
        _ => {
            return Err(ProviderExchangeError::Invalid(
                "reported final failure allows only Active to Cleaning to Cleaned cleanup progression"
                    .to_string(),
            ));
        }
    };
    if intended.updated_at != current.updated_at && intended.updated_at != transition_timestamp {
        return Err(ProviderExchangeError::Invalid(
            "reported final failure updated_at may change only to the corresponding cleanup timestamp"
                .to_string(),
        ));
    }
    allowed.updated_at = intended.updated_at.clone();
    if intended != &allowed {
        return Err(ProviderExchangeError::Invalid(
            "reported final failure cleanup changed immutable run, candidate, or evaluation authority"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_failed_cleanup_state(
    candidate: &seaf_core::CandidateWorkspaceState,
    authority: &crate::VerifiedFinalEvaluationAuthority,
) -> Result<(), ProviderExchangeError> {
    let testing_completed = parse_canonical_unix_seconds(
        &authority.testing_evidence().completed_at,
    )
    .ok_or_else(|| {
        ProviderExchangeError::Invalid(
            "TestingEvidence completed_at is not canonical Unix seconds".to_string(),
        )
    })?;
    let cleanup_started = match candidate.lifecycle {
        seaf_core::CandidateWorkspaceLifecycle::Active => return Ok(()),
        seaf_core::CandidateWorkspaceLifecycle::Cleaning
        | seaf_core::CandidateWorkspaceLifecycle::Cleaned => {
            let value = candidate.cleanup_started_at.as_deref().ok_or_else(|| {
                ProviderExchangeError::Invalid(
                    "cleanup_started_at is required after cleanup begins".to_string(),
                )
            })?;
            parse_canonical_unix_seconds(value).ok_or_else(|| {
                ProviderExchangeError::Invalid(
                    "cleanup_started_at must be canonical decimal Unix seconds within u64"
                        .to_string(),
                )
            })?
        }
        seaf_core::CandidateWorkspaceLifecycle::Provisioning => {
            return Err(ProviderExchangeError::Invalid(
                "reported final failure cannot have a Provisioning candidate".to_string(),
            ));
        }
    };
    if cleanup_started < testing_completed {
        return Err(ProviderExchangeError::Invalid(
            "cleanup_started_at cannot precede TestingEvidence completion".to_string(),
        ));
    }
    if candidate.lifecycle == seaf_core::CandidateWorkspaceLifecycle::Cleaned {
        let cleaned_at = candidate.cleaned_at.as_deref().ok_or_else(|| {
            ProviderExchangeError::Invalid(
                "cleaned_at is required for Cleaned candidate authority".to_string(),
            )
        })?;
        let cleaned = parse_canonical_unix_seconds(cleaned_at).ok_or_else(|| {
            ProviderExchangeError::Invalid(
                "cleaned_at must be canonical decimal Unix seconds within u64".to_string(),
            )
        })?;
        if cleaned < cleanup_started {
            return Err(ProviderExchangeError::Invalid(
                "cleaned_at cannot precede cleanup_started_at".to_string(),
            ));
        }
    }
    Ok(())
}

fn parse_canonical_unix_seconds(value: &str) -> Option<u64> {
    let parsed = value.parse::<u64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn validate_preserved_final_lineage(
    current: &crate::VerifiedFinalEvaluationAuthority,
    intended: &crate::VerifiedFinalEvaluationAuthority,
) -> Result<(), ProviderExchangeError> {
    if current.approved_run() != intended.approved_run()
        || current.testing_evidence().approved_run_digest
            != intended.testing_evidence().approved_run_digest
    {
        return Err(ProviderExchangeError::Invalid(
            "final evaluation mutation changed its exact Approved predecessor lineage".to_string(),
        ));
    }
    Ok(())
}

fn validate_approved_publication_evidence(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<(), ProviderExchangeError> {
    let evidence = run.human_approval.as_ref().ok_or_else(|| {
        ProviderExchangeError::Invalid("approved run lost human approval evidence".to_string())
    })?;
    let diff = read_verified_regular_file(
        workspace.run_directory(),
        &evidence.candidate_diff.path,
        "approved candidate diff",
    )?;
    if digest_bytes(&diff) != evidence.candidate_diff.digest {
        return Err(ProviderExchangeError::Invalid(
            "approved candidate diff artifact digest mismatch".to_string(),
        ));
    }
    let artifact = crate::ValidatedRoleArtifact::load(
        workspace,
        &evidence.output_review.path,
        &evidence.output_review.digest,
        &run.run_id,
        LoopStepName::OutputReview,
        Role::OutputReviewer,
    )
    .map_err(|error| ProviderExchangeError::Invalid(error.to_string()))?;
    if !matches!(
        artifact.response,
        RoleResponse::Reviewer(ref response)
            if response.decision == ReviewDecision::ApproveForTests
    ) {
        return Err(ProviderExchangeError::Invalid(
            "approved OutputReview artifact is not ApproveForTests".to_string(),
        ));
    }
    load_provider_exchange_record(workspace.run_directory(), &evidence.output_review_request)?;
    let response =
        load_provider_exchange_record(workspace.run_directory(), &evidence.output_review_response)?;
    if response.outcome != Some(ProviderExchangeOutcome::ApproveForTests) {
        return Err(ProviderExchangeError::Invalid(
            "approved terminal OutputReview exchange is not ApproveForTests".to_string(),
        ));
    }
    Ok(())
}

fn replace_run_file_atomically_with_hook<F>(
    run_file: &Path,
    bytes: &[u8],
    before_publish: F,
) -> Result<(), ProviderExchangeError>
where
    F: FnOnce() -> Result<(), ProviderExchangeError>,
{
    let parent = run_file.parent().ok_or_else(|| {
        ProviderExchangeError::Invalid("run file has no parent directory".to_string())
    })?;
    let file_name = run_file
        .file_name()
        .ok_or_else(|| ProviderExchangeError::Invalid("run file has no file name".to_string()))?;
    let (temp_path, mut temp) = create_run_state_temp(parent, file_name, &RUN_TEMP_SEQUENCE)?;
    let result = (|| {
        temp.write_all(bytes)?;
        temp.sync_all()?;
        drop(temp);
        before_publish()?;
        atomic_replace(&temp_path, run_file)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn create_run_state_temp(
    parent: &Path,
    file_name: &std::ffi::OsStr,
    sequence: &AtomicU64,
) -> std::io::Result<(std::path::PathBuf, fs::File)> {
    loop {
        let sequence = sequence.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{}.exchange-state.tmp-{}-{sequence}",
            file_name.to_string_lossy(),
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn atomic_replace(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn atomic_replace(_source: &Path, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic provider exchange state replacement requires macOS or Linux",
    ))
}

pub(crate) fn validate_authoritative_provider_exchange_records(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<(), ProviderExchangeError> {
    let mut verified_prefix = run.clone();
    verified_prefix.provider_exchange_records.clear();
    for reference in &run.provider_exchange_records {
        let record = load_provider_exchange_record(workspace.run_directory(), reference)?;
        validate_append_link(workspace, &verified_prefix, &record, false)?;
        verified_prefix
            .provider_exchange_records
            .push(reference.clone());
    }
    Ok(())
}

fn validate_append_link(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    record: &ProviderExchangeRecord,
    enforce_empty_current: bool,
) -> Result<(), ProviderExchangeError> {
    if record.run_id != run.run_id {
        return Err(ProviderExchangeError::Invalid(
            "provider exchange record belongs to another run".to_string(),
        ));
    }
    let expected_previous = run
        .provider_exchange_records
        .last()
        .map(|previous| previous.digest.clone());
    if record.previous_record_digest != expected_previous {
        return Err(ProviderExchangeError::Invalid(
            "provider exchange record does not link the authoritative previous record".to_string(),
        ));
    }

    if record.phase == ProviderExchangePhase::Response {
        let previous = run.provider_exchange_records.last().ok_or_else(|| {
            ProviderExchangeError::Invalid("response has no authoritative request".to_string())
        })?;
        let request_record = load_provider_exchange_record(workspace.run_directory(), previous)?;
        if request_record.phase != ProviderExchangePhase::Request
            || request_record.request != record.request
            || request_record.expansion != record.expansion
        {
            return Err(ProviderExchangeError::Invalid(
                "response substitutes its request or expansion authority".to_string(),
            ));
        }
    } else if let Some(previous) = run.provider_exchange_records.last() {
        let previous_record = load_provider_exchange_record(workspace.run_directory(), previous)?;
        let outcome = previous_record.outcome.ok_or_else(|| {
            ProviderExchangeError::Invalid(
                "the previous response has no parsed outcome".to_string(),
            )
        })?;
        let same_group =
            previous.step == record.step && previous.step_attempt == record.step_attempt;
        if !same_group {
            let previous_attempt_for_step = run
                .provider_exchange_records
                .iter()
                .filter(|reference| reference.step == record.step)
                .map(|reference| reference.step_attempt)
                .max()
                .unwrap_or(0);
            let expected_attempt = previous_attempt_for_step.checked_add(1).ok_or_else(|| {
                ProviderExchangeError::Invalid(
                    "provider step attempt sequence is exhausted".to_string(),
                )
            })?;
            if record.step_attempt != expected_attempt {
                return Err(ProviderExchangeError::Invalid(format!(
                    "new provider exchange group must use exact step attempt {expected_attempt}"
                )));
            }
            let rerun_authorization =
                verify_attempt_authorization(workspace, run, record.step, record.step_attempt);
            let authorized_rerun = record.kind == ProviderExchangeKind::Initial
                && record.exchange_index == 1
                && record.step_attempt > previous_attempt_for_step
                && rerun_authorization.is_ok();
            if authorized_rerun {
                if enforce_empty_current {
                    let status = run
                        .steps
                        .iter()
                        .find(|step| step.name == record.step)
                        .map(|step| step.status);
                    if record.step != run.current_step
                        || !matches!(
                            status,
                            Some(
                                seaf_core::LoopStepStatus::Pending
                                    | seaf_core::LoopStepStatus::Running
                            )
                        )
                    {
                        return Err(ProviderExchangeError::Invalid(
                            "provider rerun request is not aligned with the reset current step"
                                .to_string(),
                        ));
                    }
                }
                return Ok(());
            }
            if record.step == previous.step
                && record.kind == ProviderExchangeKind::Initial
                && record.exchange_index == 1
                && record.step_attempt > previous_attempt_for_step
            {
                return Err(rerun_authorization.expect_err(
                    "failed authorization is required when same-step rerun is not authorized",
                ));
            }
            if !is_advancing_outcome(previous_record.role, outcome)
                || next_provider_step(previous_record.step) != Some(record.step)
            {
                return Err(ProviderExchangeError::Invalid(
                    "only an advancing successful outcome may start the next provider step"
                        .to_string(),
                ));
            }
            return Ok(());
        }
        match outcome {
            seaf_core::ProviderExchangeOutcome::NeedsContext => {
                let expected_round = run
                    .provider_exchange_records
                    .iter()
                    .filter(|reference| {
                        reference.step == record.step
                            && reference.step_attempt == record.step_attempt
                            && reference.phase == ProviderExchangePhase::Request
                            && reference.kind == ProviderExchangeKind::ContextRetry
                    })
                    .count() as u32
                    + 1;
                if record.kind != ProviderExchangeKind::ContextRetry
                    || record.context_round != Some(expected_round)
                {
                    return Err(ProviderExchangeError::Invalid(format!(
                        "needs_context must transition to context retry round {expected_round}"
                    )));
                }
            }
            seaf_core::ProviderExchangeOutcome::InvalidResponse => {
                if record.kind != ProviderExchangeKind::JsonRepair
                    || previous_record.kind == ProviderExchangeKind::JsonRepair
                {
                    return Err(ProviderExchangeError::Invalid(
                        "invalid_response permits exactly one JSON repair".to_string(),
                    ));
                }
                if record.context_round != previous_record.context_round
                    || record.expansion != previous_record.expansion
                {
                    return Err(ProviderExchangeError::Invalid(
                        "JSON repair must inherit the previous exchange context authority"
                            .to_string(),
                    ));
                }
                if !derive_bound_response_classification(
                    workspace.run_directory(),
                    &previous_record,
                )?
                .json_repair_eligible
                {
                    return Err(ProviderExchangeError::Invalid(
                        "only malformed JSON is eligible for JSON repair".to_string(),
                    ));
                }
            }
            _ => {
                return Err(ProviderExchangeError::Invalid(
                    "a terminal provider outcome cannot be followed by another request".to_string(),
                ));
            }
        }
    } else if enforce_empty_current {
        let current_status = run
            .steps
            .iter()
            .find(|step| step.name == record.step)
            .map(|step| step.status);
        if record.kind != ProviderExchangeKind::Initial
            || record.exchange_index != 1
            || record.step != run.current_step
            || !matches!(
                current_status,
                Some(seaf_core::LoopStepStatus::Pending | seaf_core::LoopStepStatus::Running)
            )
        {
            return Err(ProviderExchangeError::Invalid(
                "the first provider exchange must be the current runnable step initial request"
                    .to_string(),
            ));
        }
        if record.step_attempt > 1 {
            verify_attempt_authorization(workspace, run, record.step, record.step_attempt)?;
        }
    } else {
        if record.kind != ProviderExchangeKind::Initial || record.exchange_index != 1 {
            return Err(ProviderExchangeError::Invalid(
                "the authoritative provider exchange history must begin with an initial request"
                    .to_string(),
            ));
        }
        if record.step_attempt > 1 {
            verify_attempt_authorization(workspace, run, record.step, record.step_attempt)?;
        }
    }
    Ok(())
}

pub(crate) fn verify_rerun_authorization(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    step: LoopStepName,
    step_attempt: u32,
) -> Result<(), ProviderExchangeError> {
    let path = rerun_authorization_path(step, step_attempt);
    let bytes = read_verified_regular_file(
        workspace.run_directory(),
        &path,
        "provider rerun authorization",
    )?;
    let authorization: ProviderRerunAuthorization = serde_json::from_slice(&bytes)?;
    if canonical_json_bytes(&authorization)? != bytes
        || authorization.schema_version != PROVIDER_RERUN_AUTHORIZATION_SCHEMA_VERSION
        || authorization.run_id != run.run_id
        || authorization.step != step
        || authorization.step_attempt != step_attempt
        || authorization.previous_record_digest
            != run
                .provider_exchange_records
                .last()
                .map(|reference| reference.digest.clone())
    {
        return Err(ProviderExchangeError::Invalid(
            "provider rerun authorization does not match the authoritative exchange head"
                .to_string(),
        ));
    }
    Ok(())
}

fn verify_attempt_authorization(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    step: LoopStepName,
    step_attempt: u32,
) -> Result<(), ProviderExchangeError> {
    match verify_rerun_authorization(workspace, run, step, step_attempt) {
        Ok(()) => Ok(()),
        Err(historical_error) => crate::recovery::verify_recovery_authorization(
            workspace,
            run,
            step,
            step_attempt,
        )
        .map_err(|recovery_error| {
            ProviderExchangeError::Invalid(format!(
                "provider attempt has neither historical nor recovery authorization: {historical_error}; {recovery_error}"
            ))
        }),
    }
}

pub(crate) fn validate_recovered_conventional_attempt(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    step: LoopStepName,
    step_attempt: u32,
) -> Result<(), ProviderExchangeError> {
    if verify_attempt_authorization(workspace, run, step, step_attempt).is_ok() {
        return Ok(());
    }
    let Some(head) = run.provider_exchange_records.last() else {
        return Err(ProviderExchangeError::Invalid(
            "recovered provider attempt has no explicit rerun authorization".to_string(),
        ));
    };
    let record = load_provider_exchange_record(workspace.run_directory(), head)?;
    let outcome = record.outcome.ok_or_else(|| {
        ProviderExchangeError::Invalid(
            "recovered provider attempt follows an incomplete exchange".to_string(),
        )
    })?;
    if head.phase == ProviderExchangePhase::Response
        && is_advancing_outcome(record.role, outcome)
        && next_provider_step(record.step) == Some(step)
    {
        return Ok(());
    }
    Err(ProviderExchangeError::Invalid(
        "recovered provider attempt is not authorized by the exchange head or an explicit rerun"
            .to_string(),
    ))
}

fn rerun_authorization_path(step: LoopStepName, step_attempt: u32) -> String {
    format!(
        "artifacts/{}.attempt-{step_attempt:03}.rerun-authorization.json",
        step_file_stem(step)
    )
}

fn is_advancing_outcome(role: ProviderRole, outcome: ProviderExchangeOutcome) -> bool {
    matches!(
        (role, outcome),
        (
            ProviderRole::Researcher | ProviderRole::Analyzer | ProviderRole::SpecWriter,
            ProviderExchangeOutcome::Passed
        ) | (
            ProviderRole::SpecReviewer,
            ProviderExchangeOutcome::ApproveSpec
        ) | (
            ProviderRole::Developer,
            ProviderExchangeOutcome::PatchProposed
        ) | (
            ProviderRole::OutputReviewer,
            ProviderExchangeOutcome::ApproveForTests
        )
    )
}

fn next_provider_step(step: LoopStepName) -> Option<LoopStepName> {
    match step {
        LoopStepName::Research => Some(LoopStepName::Analysis),
        LoopStepName::Analysis => Some(LoopStepName::SpecCreation),
        LoopStepName::SpecCreation => Some(LoopStepName::SpecReview),
        LoopStepName::SpecReview => Some(LoopStepName::Development),
        LoopStepName::Development => Some(LoopStepName::OutputReview),
        LoopStepName::OutputReview | LoopStepName::Testing | LoopStepName::EvalReport => None,
    }
}

fn verify_bound_artifact(
    run_directory: &Path,
    reference: &ArtifactReference,
    label: &str,
) -> Result<(), ProviderExchangeError> {
    let bytes = read_verified_regular_file(run_directory, &reference.path, label)?;
    if digest_bytes(&bytes) != reference.digest {
        return Err(ProviderExchangeError::Invalid(format!(
            "{label} digest does not match its reference"
        )));
    }
    Ok(())
}

fn validate_derived_response_outcome(
    run_directory: &Path,
    record: &ProviderExchangeRecord,
) -> Result<(), ProviderExchangeError> {
    if record.phase != ProviderExchangePhase::Response {
        return Ok(());
    }
    let derived = derive_bound_response_classification(run_directory, record)?;
    if record.outcome != Some(derived.outcome) {
        return Err(ProviderExchangeError::Invalid(format!(
            "provider exchange outcome does not match canonical response audit: expected {:?}",
            derived.outcome
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderExchangeResponseClassification {
    pub outcome: ProviderExchangeOutcome,
    pub json_repair_eligible: bool,
}

fn derive_bound_response_classification(
    run_directory: &Path,
    record: &ProviderExchangeRecord,
) -> Result<ProviderExchangeResponseClassification, ProviderExchangeError> {
    let response = record.response.as_ref().ok_or_else(|| {
        ProviderExchangeError::Invalid("response record has no response audit".to_string())
    })?;
    classify_bound_provider_exchange_response(run_directory, record.role, response)
}

fn classify_bound_provider_exchange_response(
    run_directory: &Path,
    role: ProviderRole,
    response: &ArtifactReference,
) -> Result<ProviderExchangeResponseClassification, ProviderExchangeError> {
    let bytes = read_verified_regular_file(run_directory, &response.path, "provider response")?;
    if digest_bytes(&bytes) != response.digest {
        return Err(ProviderExchangeError::Invalid(
            "provider response audit digest does not match its reference".to_string(),
        ));
    }
    let audit: ProviderExchangeResponseAudit = serde_json::from_slice(&bytes)?;
    if canonical_json_bytes(&audit)? != bytes {
        return Err(ProviderExchangeError::Invalid(
            "provider response audit is not canonical JSON".to_string(),
        ));
    }
    Ok(classify_provider_exchange_response(role, &audit))
}

pub(crate) fn classify_provider_exchange_response(
    role: ProviderRole,
    audit: &ProviderExchangeResponseAudit,
) -> ProviderExchangeResponseClassification {
    let ProviderExchangeResponseAudit::ModelResponse { response } = audit else {
        return ProviderExchangeResponseClassification {
            outcome: ProviderExchangeOutcome::ProviderFailure,
            json_repair_eligible: false,
        };
    };
    let role = provider_role(role);
    let outcome = match parse_role_response(role, &response.content) {
        Ok(RoleResponse::Agent(response)) => match response.status {
            AgentStatus::Passed => ProviderExchangeOutcome::Passed,
            AgentStatus::Blocked => ProviderExchangeOutcome::Blocked,
            AgentStatus::NeedsContext => ProviderExchangeOutcome::NeedsContext,
        },
        Ok(RoleResponse::Developer(response)) => match response.status {
            DeveloperStatus::PatchProposed => ProviderExchangeOutcome::PatchProposed,
            DeveloperStatus::Blocked => ProviderExchangeOutcome::Blocked,
            DeveloperStatus::NeedsContext => ProviderExchangeOutcome::NeedsContext,
        },
        Ok(RoleResponse::Reviewer(response)) => match response.decision {
            ReviewDecision::ApproveSpec if role == Role::SpecReviewer => {
                ProviderExchangeOutcome::ApproveSpec
            }
            ReviewDecision::ApproveForTests if role == Role::OutputReviewer => {
                ProviderExchangeOutcome::ApproveForTests
            }
            ReviewDecision::ApproveSpec | ReviewDecision::ApproveForTests => {
                ProviderExchangeOutcome::InvalidResponse
            }
            ReviewDecision::RequestChanges => ProviderExchangeOutcome::RequestChanges,
            ReviewDecision::Reject => ProviderExchangeOutcome::Reject,
        },
        Err(RoleResponseError::InvalidJson { .. }) => {
            return ProviderExchangeResponseClassification {
                outcome: ProviderExchangeOutcome::InvalidResponse,
                json_repair_eligible: true,
            };
        }
        Err(_) => ProviderExchangeOutcome::InvalidResponse,
    };
    ProviderExchangeResponseClassification {
        outcome,
        json_repair_eligible: false,
    }
}

fn provider_role(role: ProviderRole) -> Role {
    match role {
        ProviderRole::Researcher => Role::Researcher,
        ProviderRole::Analyzer => Role::Analyzer,
        ProviderRole::SpecWriter => Role::SpecWriter,
        ProviderRole::SpecReviewer => Role::SpecReviewer,
        ProviderRole::Developer => Role::Developer,
        ProviderRole::OutputReviewer => Role::OutputReviewer,
    }
}

fn validate_record(record: &ProviderExchangeRecord) -> Result<(), ProviderExchangeError> {
    let errors = validate_provider_exchange_record(record);
    if !errors.is_empty() {
        return Err(ProviderExchangeError::Invalid(
            errors
                .into_iter()
                .map(|error| format!("{}: {}", error.field, error.message))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    let coordinates = coordinates_from_record(record);
    validate_coordinates(&coordinates)?;
    if record.request.path != request_path(&coordinates) {
        return Err(ProviderExchangeError::Invalid(
            "provider request path does not match exchange identity".to_string(),
        ));
    }
    if let Some(response) = &record.response {
        if response.path != response_path(&coordinates) {
            return Err(ProviderExchangeError::Invalid(
                "provider response path does not match exchange identity".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_coordinates(
    coordinates: &ProviderExchangeCoordinates,
) -> Result<(), ProviderExchangeError> {
    let expected_role = match coordinates.step {
        LoopStepName::Research => Some(ProviderRole::Researcher),
        LoopStepName::Analysis => Some(ProviderRole::Analyzer),
        LoopStepName::SpecCreation => Some(ProviderRole::SpecWriter),
        LoopStepName::SpecReview => Some(ProviderRole::SpecReviewer),
        LoopStepName::Development => Some(ProviderRole::Developer),
        LoopStepName::OutputReview => Some(ProviderRole::OutputReviewer),
        LoopStepName::Testing | LoopStepName::EvalReport => None,
    };
    if coordinates.run_id.is_empty()
        || coordinates.step_attempt == 0
        || coordinates.exchange_index == 0
        || coordinates.context_round == Some(0)
        || expected_role != Some(coordinates.role)
    {
        return Err(ProviderExchangeError::Invalid(
            "invalid provider exchange coordinates".to_string(),
        ));
    }
    match coordinates.kind {
        ProviderExchangeKind::ContextRetry
            if coordinates.context_round.is_none() || coordinates.context_round == Some(0) =>
        {
            Err(ProviderExchangeError::Invalid(
                "context retry requires a nonzero context round".to_string(),
            ))
        }
        ProviderExchangeKind::Initial if coordinates.context_round.is_some() => {
            Err(ProviderExchangeError::Invalid(
                "initial exchange cannot carry a context round".to_string(),
            ))
        }
        _ => Ok(()),
    }
}

fn validate_reference_identity(
    reference: &ProviderExchangeRecordReference,
) -> Result<(), ProviderExchangeError> {
    let coordinates = ProviderExchangeCoordinates {
        run_id: reference.run_id.clone(),
        step: reference.step,
        role: reference.role,
        step_attempt: reference.step_attempt,
        exchange_index: reference.exchange_index,
        kind: reference.kind,
        context_round: reference.context_round,
    };
    validate_coordinates(&coordinates)?;
    if reference.path != record_path(&coordinates, reference.phase)
        || reference.digest.len() != 64
        || !reference
            .digest
            .chars()
            .all(|character| character.is_ascii_digit() || matches!(character, 'a'..='f'))
    {
        return Err(ProviderExchangeError::Invalid(
            "invalid provider exchange record reference".to_string(),
        ));
    }
    Ok(())
}

fn coordinates_from_record(record: &ProviderExchangeRecord) -> ProviderExchangeCoordinates {
    ProviderExchangeCoordinates {
        run_id: record.run_id.clone(),
        step: record.step,
        role: record.role,
        step_attempt: record.step_attempt,
        exchange_index: record.exchange_index,
        kind: record.kind,
        context_round: record.context_round,
    }
}

fn record_reference(
    record: &ProviderExchangeRecord,
    digest: String,
) -> ProviderExchangeRecordReference {
    let coordinates = coordinates_from_record(record);
    ProviderExchangeRecordReference {
        run_id: record.run_id.clone(),
        step: record.step,
        role: record.role,
        step_attempt: record.step_attempt,
        exchange_index: record.exchange_index,
        kind: record.kind,
        context_round: record.context_round,
        phase: record.phase,
        path: record_path(&coordinates, record.phase),
        digest,
    }
}

fn write_exchange_bytes(
    run_directory: &Path,
    path: String,
    bytes: &[u8],
) -> Result<ArtifactReference, ProviderExchangeError> {
    publish_create_only(run_directory, &path, bytes)?;
    Ok(ArtifactReference {
        path,
        digest: digest_bytes(bytes),
    })
}

fn request_path(coordinates: &ProviderExchangeCoordinates) -> String {
    format!(
        "prompts/{}.attempt-{:03}.exchange-{:03}.{}.request.md",
        step_file_stem(coordinates.step),
        coordinates.step_attempt,
        coordinates.exchange_index,
        kind_slug(coordinates.kind)
    )
}

fn response_path(coordinates: &ProviderExchangeCoordinates) -> String {
    format!(
        "responses/{}.attempt-{:03}.exchange-{:03}.{}.response.json",
        step_file_stem(coordinates.step),
        coordinates.step_attempt,
        coordinates.exchange_index,
        kind_slug(coordinates.kind)
    )
}

fn record_path(coordinates: &ProviderExchangeCoordinates, phase: ProviderExchangePhase) -> String {
    let phase = match phase {
        ProviderExchangePhase::Request => "request",
        ProviderExchangePhase::Response => "response",
    };
    format!(
        "artifacts/{}.attempt-{:03}.exchange-{:03}.{}.{phase}.record.json",
        step_file_stem(coordinates.step),
        coordinates.step_attempt,
        coordinates.exchange_index,
        kind_slug(coordinates.kind)
    )
}

fn kind_slug(kind: ProviderExchangeKind) -> &'static str {
    match kind {
        ProviderExchangeKind::Initial => "initial",
        ProviderExchangeKind::JsonRepair => "json-repair",
        ProviderExchangeKind::ContextRetry => "context-retry",
    }
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Debug)]
pub enum ProviderExchangeError {
    Invalid(String),
    Artifact(String),
    State(state::StateError),
    Json(serde_json::Error),
    Io(std::io::Error),
}

impl fmt::Display for ProviderExchangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid provider exchange: {message}"),
            Self::Artifact(error) => write!(formatter, "{error}"),
            Self::State(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "provider exchange JSON error: {error}"),
            Self::Io(error) => write!(formatter, "provider exchange I/O error: {error}"),
        }
    }
}

impl Error for ProviderExchangeError {}

impl From<ImmutableArtifactError> for ProviderExchangeError {
    fn from(error: ImmutableArtifactError) -> Self {
        Self::Artifact(error.to_string())
    }
}

impl From<state::StateError> for ProviderExchangeError {
    fn from(error: state::StateError) -> Self {
        Self::State(error)
    }
}

impl From<serde_json::Error> for ProviderExchangeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<std::io::Error> for ProviderExchangeError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
pub(crate) fn persist_test_output_review_ledger(
    workspace: &LoopWorkspace,
    run_id: &str,
) -> LoopRun {
    let coordinates = ProviderExchangeCoordinates {
        run_id: run_id.to_string(),
        step: LoopStepName::OutputReview,
        role: ProviderRole::OutputReviewer,
        step_attempt: 1,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
    };
    crate::artifacts::write_step_request(workspace, LoopStepName::OutputReview, 1, "review")
        .unwrap();
    let request =
        write_provider_exchange_request(workspace.run_directory(), &coordinates, b"review")
            .unwrap();
    let request_record = ProviderExchangeRecord {
        schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
        run_id: run_id.to_string(),
        step: coordinates.step,
        role: coordinates.role,
        step_attempt: coordinates.step_attempt,
        exchange_index: coordinates.exchange_index,
        kind: coordinates.kind,
        context_round: None,
        phase: ProviderExchangePhase::Request,
        previous_record_digest: None,
        request: request.clone(),
        response: None,
        expansion: None,
        outcome: None,
    };
    let request_reference =
        stage_provider_exchange_record(workspace.run_directory(), &request_record).unwrap();
    persist_provider_exchange_record_reference_with_validator(
        workspace,
        request_reference.clone(),
        |_| Ok(()),
    )
    .unwrap();
    let response = write_provider_exchange_response(
        workspace.run_directory(),
        &coordinates,
        &ProviderExchangeResponseAudit::ModelResponse {
            response: ModelResponse {
                content: serde_json::json!({
                    "role": "output_reviewer",
                    "decision": "approve_for_tests",
                    "summary": "Approved for the barrier.",
                    "blocking_issues": [],
                    "non_blocking_issues": []
                })
                .to_string(),
                latency_ms: 1,
                raw_provider_metadata: serde_json::Value::Null,
            },
        },
    )
    .unwrap();
    let response_record = ProviderExchangeRecord {
        phase: ProviderExchangePhase::Response,
        previous_record_digest: Some(request_reference.digest),
        response: Some(response),
        ..request_record
    };
    let (response_reference, _) =
        stage_provider_exchange_response_record(workspace.run_directory(), response_record)
            .unwrap();
    persist_provider_exchange_record_reference_with_validator(workspace, response_reference, |_| {
        Ok(())
    })
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use seaf_core::{
        ArtifactReference, CandidatePatchPhase, CandidatePatchTransaction,
        CandidateWorkspaceLifecycle, CandidateWorkspaceState, CheckStatus, EvalCheck, EvalDecision,
        EvalLoopEvidence, EvalReport, HumanApprovalEvidence, LoopInputDigests, LoopStatus,
        LoopStepStatus, PromotionEvidence, RiskLevel,
    };

    fn awaiting_human_review_run(workspace: &LoopWorkspace) -> LoopRun {
        let mut run = state::create_run(state::NewLoopRun {
            run_id: workspace
                .run_directory()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
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
        run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
        run.status = LoopStatus::AwaitingHumanReview;
        run.current_step = LoopStepName::Testing;
        let review_response = parse_role_response(
            Role::OutputReviewer,
            r#"{"role":"output_reviewer","decision":"approve_for_tests","summary":"Approved.","blocking_issues":[],"non_blocking_issues":[]}"#,
        )
        .unwrap();
        let review_artifact = crate::ValidatedRoleArtifact::new(
            run.run_id.clone(),
            LoopStepName::OutputReview,
            Role::OutputReviewer,
            review_response,
        )
        .unwrap();
        let review_path = "artifacts/06-output-review.md";
        fs::write(
            workspace.run_directory().join(review_path),
            review_artifact.canonical_bytes().unwrap(),
        )
        .unwrap();
        let candidate_diff_path = "artifacts/candidate-patch.applied.diff";
        let candidate_diff = b"candidate diff";
        fs::write(
            workspace.run_directory().join(candidate_diff_path),
            candidate_diff,
        )
        .unwrap();
        let candidate_diff_digest = digest_bytes(candidate_diff);
        for record in &mut run.steps {
            match record.name {
                LoopStepName::Research
                | LoopStepName::Analysis
                | LoopStepName::SpecCreation
                | LoopStepName::SpecReview
                | LoopStepName::Development => record.status = LoopStepStatus::Completed,
                LoopStepName::OutputReview => {
                    record.status = LoopStepStatus::Passed;
                    record.artifact_path = Some(review_path.to_string());
                    record.artifact_digest = Some(review_artifact.artifact_digest().unwrap());
                }
                LoopStepName::Testing | LoopStepName::EvalReport => {}
            }
        }
        run.candidate_workspace = Some(CandidateWorkspaceState {
            schema_version: 2,
            run_directory_digest: Some("9".repeat(64)),
            path: workspace
                .run_directory()
                .join("candidate")
                .display()
                .to_string(),
            source_worktree_root: workspace
                .run_directory()
                .join("source")
                .display()
                .to_string(),
            git_common_dir: workspace
                .run_directory()
                .join("source/.git")
                .display()
                .to_string(),
            repository_identity_digest: "d".repeat(64),
            starting_head: "e".repeat(40),
            starting_tree: "f".repeat(40),
            candidate_head: "e".repeat(40),
            candidate_tree: "1".repeat(40),
            candidate_diff_digest,
            patch_transaction: Some(CandidatePatchTransaction {
                schema_version: 1,
                phase: CandidatePatchPhase::Applied,
                intent: ArtifactReference {
                    path: "artifacts/candidate-patch-intent.json".to_string(),
                    digest: "3".repeat(64),
                },
                applied_evidence: Some(ArtifactReference {
                    path: "artifacts/candidate-patch-applied.json".to_string(),
                    digest: "4".repeat(64),
                }),
                started_at: "1".to_string(),
                applied_at: Some("2".to_string()),
            }),
            lifecycle: CandidateWorkspaceLifecycle::Active,
            cleanup_started_at: None,
            cleaned_at: None,
        });
        run.policy_decisions.push(
            serde_json::from_value(serde_json::json!({
                "patch_id": run.run_id.clone(),
                "decision": "allowed"
            }))
            .unwrap(),
        );
        run
    }

    fn publish_test_final_artifacts(
        workspace: &LoopWorkspace,
        approved: &LoopRun,
        final_run: &mut LoopRun,
        passed: bool,
    ) {
        publish_test_final_artifacts_variant(workspace, approved, final_run, passed, "", "test");
    }

    fn publish_test_eval_config(workspace: &LoopWorkspace) -> seaf_core::EvalConfig {
        let eval_config = seaf_core::parse_eval_config(
            "evals:\n  allow_commands: [true]\n  required:\n    - name: unit\n      command: true\n",
        )
        .unwrap();
        fs::create_dir_all(workspace.run_directory().join("inputs")).unwrap();
        fs::write(
            workspace.run_directory().join("inputs/eval-config.json"),
            canonical_json_bytes(&eval_config).unwrap(),
        )
        .unwrap();
        eval_config
    }

    fn publish_test_final_artifacts_variant(
        workspace: &LoopWorkspace,
        approved: &LoopRun,
        final_run: &mut LoopRun,
        passed: bool,
        label: &str,
        summary: &str,
    ) {
        let suffix = if label.is_empty() {
            String::new()
        } else {
            format!("-{label}")
        };
        let stdout = b"unit stdout\n";
        let stderr = b"unit stderr\n";
        let stdout_path = "artifacts/07-testing.check-001.stdout.log";
        let stderr_path = "artifacts/07-testing.check-001.stderr.log";
        fs::write(workspace.run_directory().join(stdout_path), stdout).unwrap();
        fs::write(workspace.run_directory().join(stderr_path), stderr).unwrap();
        let check = EvalCheck {
            name: "unit".to_string(),
            status: if passed {
                CheckStatus::Passed
            } else {
                CheckStatus::Failed
            },
            duration_ms: Some(1),
            stdout_path: Some(stdout_path.to_string()),
            stdout_digest: Some(digest_bytes(stdout)),
            stderr_path: Some(stderr_path.to_string()),
            stderr_digest: Some(digest_bytes(stderr)),
            summary: Some(summary.to_string()),
        };
        let approved_at = approved
            .human_approval
            .as_ref()
            .unwrap()
            .approved_at
            .clone();
        let testing = crate::TestingEvidence::create(
            approved,
            approved_at.clone(),
            approved_at,
            vec![check.clone()],
        )
        .unwrap();
        let eval_config_bytes =
            fs::read(workspace.run_directory().join("inputs/eval-config.json")).unwrap();
        let eval_config: seaf_core::EvalConfig =
            serde_json::from_slice(&eval_config_bytes).unwrap();
        let approval = approved.human_approval.as_ref().unwrap();
        let intent = crate::evaluation_attempt::ApprovedEvaluationIntentV1 {
            schema_version: 1,
            run_id: approved.run_id.clone(),
            approved_run_digest: canonical_sha256_digest(approved).unwrap(),
            ticket: ArtifactReference {
                path: "inputs/ticket.json".to_string(),
                digest: approved.input_digests.ticket.clone(),
            },
            eval_config: ArtifactReference {
                path: "inputs/eval-config.json".to_string(),
                digest: approved.input_digests.eval_config.clone().unwrap(),
            },
            candidate_diff: approval.candidate_diff.clone(),
            planned_checks: eval_config.evals.required,
        };
        fs::write(
            workspace
                .run_directory()
                .join("artifacts/07-testing.execution-intent.json"),
            canonical_json_bytes(&intent).unwrap(),
        )
        .unwrap();
        let testing_reference = ArtifactReference {
            path: format!("artifacts/07-testing{suffix}.json"),
            digest: testing.artifact_digest().unwrap(),
        };
        fs::write(
            workspace.run_directory().join(&testing_reference.path),
            testing.canonical_bytes().unwrap(),
        )
        .unwrap();
        let approval = approved.human_approval.as_ref().unwrap();
        let report = EvalReport {
            eval_report_id: format!("eval_{}", approved.run_id),
            patch_id: approved.run_id.clone(),
            goal_id: approved.goal_id.clone(),
            passed,
            summary: "integrated".to_string(),
            checks: vec![check],
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
                eval_config: ArtifactReference {
                    path: "inputs/eval-config.json".to_string(),
                    digest: approved.input_digests.eval_config.clone().unwrap(),
                },
                candidate_diff: approval.candidate_diff.clone(),
                starting_head: approval.starting_head.clone(),
                human_approval_digest: canonical_sha256_digest(approval).unwrap(),
                policy_decision_digest: approval.policy_decision_digest.clone(),
                testing_evidence: testing_reference.clone(),
            }),
        };
        let report_reference = ArtifactReference {
            path: format!("artifacts/08-eval-report{suffix}.json"),
            digest: canonical_sha256_digest(&report).unwrap(),
        };
        fs::write(
            workspace.run_directory().join(&report_reference.path),
            canonical_json_bytes(&report).unwrap(),
        )
        .unwrap();
        for (name, reference) in [
            (LoopStepName::Testing, testing_reference),
            (LoopStepName::EvalReport, report_reference.clone()),
        ] {
            let record = final_run
                .steps
                .iter_mut()
                .find(|record| record.name == name)
                .unwrap();
            record.status = if passed {
                LoopStepStatus::Passed
            } else {
                LoopStepStatus::Failed
            };
            record.artifact_path = Some(reference.path);
            record.artifact_digest = Some(reference.digest);
        }
        final_run.eval_report_path = Some(report_reference.path);
    }

    fn persist_test_approved_authority(workspace: &LoopWorkspace) -> LoopRun {
        let mut awaiting = awaiting_human_review_run(workspace);
        let eval_config = publish_test_eval_config(workspace);
        awaiting.input_digests.eval_config = Some(canonical_sha256_digest(&eval_config).unwrap());
        let mut pre_review = awaiting.clone();
        pre_review.status = LoopStatus::Running;
        pre_review.current_step = LoopStepName::OutputReview;
        let output_review = pre_review
            .steps
            .iter_mut()
            .find(|record| record.name == LoopStepName::OutputReview)
            .unwrap();
        output_review.status = LoopStepStatus::Running;
        output_review.artifact_path = None;
        output_review.artifact_digest = None;
        state::save_run(workspace, &pre_review).unwrap();
        let with_ledger = persist_test_output_review_ledger(workspace, &awaiting.run_id);
        awaiting.provider_exchange_records = with_ledger.provider_exchange_records;
        persist_run_with_provider_exchange_compare(workspace, &awaiting).unwrap();

        let policy_decision_digest =
            canonical_sha256_digest(&awaiting.policy_decisions[0]).unwrap();
        let mut approved = awaiting.clone();
        approved.status = LoopStatus::Approved;
        approved.human_approval = Some(HumanApprovalEvidence {
            schema_version: 1,
            run_id: awaiting.run_id.clone(),
            reviewer: "reviewer@example.invalid".to_string(),
            approved_at: "100".to_string(),
            candidate_diff: ArtifactReference {
                path: "artifacts/candidate-patch.applied.diff".to_string(),
                digest: awaiting
                    .candidate_workspace
                    .as_ref()
                    .unwrap()
                    .candidate_diff_digest
                    .clone(),
            },
            starting_head: "e".repeat(40),
            policy_decision_digest,
            output_review: ArtifactReference {
                path: "artifacts/06-output-review.md".to_string(),
                digest: awaiting
                    .steps
                    .iter()
                    .find(|step| step.name == LoopStepName::OutputReview)
                    .unwrap()
                    .artifact_digest
                    .clone()
                    .unwrap(),
            },
            output_review_request: awaiting
                .provider_exchange_records
                .iter()
                .find(|record| {
                    record.step == LoopStepName::OutputReview
                        && record.kind == ProviderExchangeKind::Initial
                        && record.phase == ProviderExchangePhase::Request
                })
                .unwrap()
                .clone(),
            output_review_response: awaiting.provider_exchange_records.last().unwrap().clone(),
        });
        approved.updated_at = "100".to_string();
        persist_run_with_full_compare(workspace, &awaiting, &approved).unwrap();
        approved
    }

    fn persist_test_failed_final_authority(
        run_id: &str,
    ) -> (tempfile::TempDir, LoopWorkspace, LoopRun, LoopRun) {
        let temp = tempfile::tempdir().unwrap();
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), run_id).unwrap();
        let approved = persist_test_approved_authority(&workspace);
        let mut failed = approved.clone();
        failed.status = LoopStatus::Failed;
        failed.current_step = LoopStepName::EvalReport;
        publish_test_final_artifacts(&workspace, &approved, &mut failed, false);
        persist_run_with_full_compare(&workspace, &approved, &failed).unwrap();
        (temp, workspace, approved, failed)
    }

    #[test]
    fn failed_final_authority_cannot_be_rewritten_as_eval_passed() {
        let (_temp, workspace, approved, failed) =
            persist_test_failed_final_authority("failed-to-passed");
        let before = fs::read(workspace.run_file()).unwrap();
        let mut passing = approved.clone();
        passing.status = LoopStatus::EvalPassed;
        passing.current_step = LoopStepName::EvalReport;
        publish_test_final_artifacts_variant(
            &workspace,
            &approved,
            &mut passing,
            true,
            "passing-rewrite",
            "passing rewrite",
        );
        let error = persist_run_with_full_compare(&workspace, &failed, &passing)
            .expect_err("reported failure cannot become EvalPassed");

        assert!(
            error.to_string().contains("cleanup")
                || error.to_string().contains("evaluation artifact"),
            "{error}"
        );
        assert_eq!(fs::read(workspace.run_file()).unwrap(), before);

        let mut non_final = failed.clone();
        non_final.status = LoopStatus::Completed;
        non_final.human_approval = None;
        let error = persist_run_with_full_compare(&workspace, &failed, &non_final)
            .expect_err("reported failure cannot be rewritten as non-final authority");
        assert!(error.to_string().contains("cleanup"), "{error}");
        assert_eq!(fs::read(workspace.run_file()).unwrap(), before);
    }

    #[test]
    fn failed_final_authority_cannot_replace_its_evaluation_bundle() {
        let (_temp, workspace, approved, failed) =
            persist_test_failed_final_authority("failed-bundle-rewrite");
        let before = fs::read(workspace.run_file()).unwrap();
        let mut replacement = approved.clone();
        replacement.status = LoopStatus::Failed;
        replacement.current_step = LoopStepName::EvalReport;
        publish_test_final_artifacts_variant(
            &workspace,
            &approved,
            &mut replacement,
            false,
            "failing-rewrite",
            "different failing result",
        );
        let error = persist_run_with_full_compare(&workspace, &failed, &replacement)
            .expect_err("reported failure cannot replace its evidence bundle");

        assert!(
            error.to_string().contains("cleanup")
                || error.to_string().contains("evaluation artifact"),
            "{error}"
        );
        assert_eq!(fs::read(workspace.run_file()).unwrap(), before);
    }

    #[test]
    fn failed_final_authority_allows_only_monotonic_candidate_cleanup() {
        let (_temp, workspace, _approved, failed) =
            persist_test_failed_final_authority("failed-cleanup-cas");
        let original_authority =
            crate::load_verified_final_evaluation_authority(&workspace, &failed).unwrap();
        let before = fs::read(workspace.run_file()).unwrap();

        let mut arbitrary_touch = failed.clone();
        arbitrary_touch.updated_at = "101".to_string();
        let error = persist_run_with_full_compare(&workspace, &failed, &arbitrary_touch)
            .expect_err("updated_at cannot change without cleanup progression");
        assert!(error.to_string().contains("cleanup"), "{error}");
        assert_eq!(fs::read(workspace.run_file()).unwrap(), before);

        let mut direct_cleaned = failed.clone();
        let candidate = direct_cleaned.candidate_workspace.as_mut().unwrap();
        candidate.lifecycle = CandidateWorkspaceLifecycle::Cleaned;
        candidate.cleanup_started_at = Some("101".to_string());
        candidate.cleaned_at = Some("102".to_string());
        let error = persist_run_with_full_compare(&workspace, &failed, &direct_cleaned)
            .expect_err("Active cannot skip directly to Cleaned");
        assert!(error.to_string().contains("cleanup"), "{error}");
        assert_eq!(fs::read(workspace.run_file()).unwrap(), before);

        let mut cleaning = failed.clone();
        let candidate = cleaning.candidate_workspace.as_mut().unwrap();
        candidate.lifecycle = CandidateWorkspaceLifecycle::Cleaning;
        candidate.cleanup_started_at = Some("101".to_string());
        persist_run_with_full_compare(&workspace, &failed, &cleaning)
            .expect("Active may progress to Cleaning");

        let mut substituted_candidate = cleaning.clone();
        substituted_candidate
            .candidate_workspace
            .as_mut()
            .unwrap()
            .candidate_diff_digest = "8".repeat(64);
        let error = persist_run_with_full_compare(&workspace, &cleaning, &substituted_candidate)
            .expect_err("cleanup cannot replace candidate identity or patch authority");
        assert!(error.to_string().contains("candidate_diff"), "{error}");

        let mut backwards = cleaning.clone();
        let candidate = backwards.candidate_workspace.as_mut().unwrap();
        candidate.lifecycle = CandidateWorkspaceLifecycle::Cleaned;
        candidate.cleaned_at = Some("100".to_string());
        let error = persist_run_with_full_compare(&workspace, &cleaning, &backwards)
            .expect_err("cleanup completion cannot precede cleanup start");
        assert!(error.to_string().contains("cleanup"), "{error}");

        let mut cleaned = cleaning.clone();
        let candidate = cleaned.candidate_workspace.as_mut().unwrap();
        candidate.lifecycle = CandidateWorkspaceLifecycle::Cleaned;
        candidate.cleaned_at = Some("102".to_string());
        cleaned.updated_at = "102".to_string();
        persist_run_with_full_compare(&workspace, &cleaning, &cleaned)
            .expect("Cleaning may progress monotonically to Cleaned");
        persist_run_with_full_compare(&workspace, &cleaned, &cleaned)
            .expect("exact Cleaned retry remains idempotent");

        let cleaned_authority =
            crate::load_verified_final_evaluation_authority(&workspace, &cleaned)
                .expect("cleaned final authority remains verifiable");
        assert_eq!(
            cleaned_authority.testing_evidence(),
            original_authority.testing_evidence()
        );
        assert_eq!(
            cleaned_authority.eval_report(),
            original_authority.eval_report()
        );

        let mut malformed_retry = failed;
        let candidate = malformed_retry.candidate_workspace.as_mut().unwrap();
        candidate.lifecycle = CandidateWorkspaceLifecycle::Cleaning;
        candidate.cleanup_started_at = Some("01".to_string());
        let mut bytes = serde_json::to_vec_pretty(&malformed_retry).unwrap();
        bytes.push(b'\n');
        fs::write(workspace.run_file(), bytes).unwrap();
        let error = persist_run_with_full_compare(&workspace, &malformed_retry, &malformed_retry)
            .expect_err("cleanup retries still require canonical monotonic timestamps");
        assert!(error.to_string().contains("cleanup_started_at"), "{error}");
    }

    #[test]
    fn awaiting_human_review_freezes_ordinary_publication_and_provider_suffixes() {
        let temp = tempfile::tempdir().expect("temp");
        let workspace =
            LoopWorkspace::create(&temp.path().join("runs"), "awaiting-provider-freeze").unwrap();
        let mut run = awaiting_human_review_run(&workspace);
        let eval_config = publish_test_eval_config(&workspace);
        run.input_digests.eval_config = Some(canonical_sha256_digest(&eval_config).unwrap());
        let mut pre_review = run.clone();
        pre_review.status = LoopStatus::Running;
        pre_review.current_step = LoopStepName::OutputReview;
        let output_review = pre_review
            .steps
            .iter_mut()
            .find(|record| record.name == LoopStepName::OutputReview)
            .unwrap();
        output_review.status = LoopStepStatus::Running;
        output_review.artifact_path = None;
        output_review.artifact_digest = None;
        state::save_run(&workspace, &pre_review).expect("pre-review run");
        let with_ledger = persist_test_output_review_ledger(&workspace, &run.run_id);
        run.provider_exchange_records = with_ledger.provider_exchange_records;
        persist_run_with_provider_exchange_compare(&workspace, &run)
            .expect("locked waiting publication");
        let run_bytes = fs::read(workspace.run_file()).unwrap();

        let mut stale = run.clone();
        stale.status = LoopStatus::Running;
        let error = persist_run_with_provider_exchange_compare(&workspace, &stale)
            .expect_err("ordinary stale state cannot replace waiting authority");
        assert!(
            error.to_string().contains("awaiting human review"),
            "{error}"
        );
        assert_eq!(fs::read(workspace.run_file()).unwrap(), run_bytes);

        let coordinates = ProviderExchangeCoordinates {
            run_id: run.run_id.clone(),
            step: LoopStepName::Research,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
        };
        let request =
            write_provider_exchange_request(workspace.run_directory(), &coordinates, b"late")
                .unwrap();
        let record = ProviderExchangeRecord {
            schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: run.run_id.clone(),
            step: coordinates.step,
            role: coordinates.role,
            step_attempt: coordinates.step_attempt,
            exchange_index: coordinates.exchange_index,
            kind: coordinates.kind,
            context_round: None,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: None,
            request,
            response: None,
            expansion: None,
            outcome: None,
        };
        let reference = stage_provider_exchange_record(workspace.run_directory(), &record).unwrap();
        let error = persist_provider_exchange_record_reference(&workspace, reference.clone())
            .expect_err("late provider suffix must remain unreferenced");
        assert!(error.to_string().contains("frozen"), "{error}");
        assert_eq!(fs::read(workspace.run_file()).unwrap(), run_bytes);
        let error = reconcile_provider_exchange_state_with_validator(&workspace, &run, |_| Ok(()))
            .expect_err("Awaiting reconciliation must not adopt a late suffix");
        assert!(error.to_string().contains("frozen"), "{error}");
        assert_eq!(fs::read(workspace.run_file()).unwrap(), run_bytes);

        let policy_decision_digest = canonical_sha256_digest(&run.policy_decisions[0]).unwrap();
        let mut approved = run.clone();
        approved.status = LoopStatus::Approved;
        approved.human_approval = Some(HumanApprovalEvidence {
            schema_version: 1,
            run_id: run.run_id.clone(),
            reviewer: "reviewer@example.invalid".to_string(),
            approved_at: "100".to_string(),
            candidate_diff: ArtifactReference {
                path: "artifacts/candidate-patch.applied.diff".to_string(),
                digest: run
                    .candidate_workspace
                    .as_ref()
                    .unwrap()
                    .candidate_diff_digest
                    .clone(),
            },
            starting_head: "e".repeat(40),
            policy_decision_digest,
            output_review: ArtifactReference {
                path: "artifacts/06-output-review.md".to_string(),
                digest: run
                    .steps
                    .iter()
                    .find(|step| step.name == LoopStepName::OutputReview)
                    .unwrap()
                    .artifact_digest
                    .clone()
                    .unwrap(),
            },
            output_review_request: run
                .provider_exchange_records
                .iter()
                .find(|record| {
                    record.step == LoopStepName::OutputReview
                        && record.kind == ProviderExchangeKind::Initial
                        && record.phase == ProviderExchangePhase::Request
                })
                .unwrap()
                .clone(),
            output_review_response: run.provider_exchange_records.last().unwrap().clone(),
        });
        approved.updated_at = "100".to_string();
        let mut premature_final = approved.clone();
        premature_final.status = LoopStatus::EvalPassed;
        premature_final.current_step = LoopStepName::EvalReport;
        publish_test_final_artifacts(&workspace, &approved, &mut premature_final, true);
        let awaiting_bytes = fs::read(workspace.run_file()).unwrap();
        let error = persist_run_with_full_compare(&workspace, &run, &premature_final)
            .expect_err("Awaiting authority cannot publish a final evaluation bundle");
        assert!(error.to_string().contains("locked Approved"), "{error}");
        assert_eq!(fs::read(workspace.run_file()).unwrap(), awaiting_bytes);

        persist_run_with_full_compare(&workspace, &run, &approved)
            .expect("approval transaction may publish Approved");
        let approved_bytes = fs::read(workspace.run_file()).unwrap();
        let error = persist_provider_exchange_record_reference(&workspace, reference)
            .expect_err("Approved must freeze provider append");
        assert!(error.to_string().contains("frozen"), "{error}");
        let error =
            reconcile_provider_exchange_state_with_validator(&workspace, &approved, |_| Ok(()))
                .expect_err("Approved must freeze reconciliation");
        assert!(error.to_string().contains("frozen"), "{error}");
        let mut stale = approved.clone();
        stale.status = LoopStatus::Running;
        stale.human_approval = None;
        let error = persist_run_with_provider_exchange_compare(&workspace, &stale)
            .expect_err("ordinary publication cannot replace Approved");
        assert!(error.to_string().contains("approved authority"), "{error}");
        let mut reset = approved.clone();
        state::reset_from_step(&mut reset, LoopStepName::OutputReview).unwrap();
        let error = persist_provider_rerun_reset(
            &workspace,
            &approved,
            &reset,
            LoopStepName::OutputReview,
            2,
        )
        .expect_err("provider rerun reset cannot replace Approved");
        assert!(
            error.to_string().contains("human review authority"),
            "{error}"
        );
        assert_eq!(fs::read(workspace.run_file()).unwrap(), approved_bytes);

        let mut substituted_approved = approved.clone();
        substituted_approved
            .human_approval
            .as_mut()
            .unwrap()
            .reviewer = "substituted@example.invalid".to_string();
        substituted_approved
            .human_approval
            .as_mut()
            .unwrap()
            .approved_at = "101".to_string();
        substituted_approved.updated_at = "101".to_string();
        let mut substituted_final = substituted_approved.clone();
        substituted_final.status = LoopStatus::EvalPassed;
        substituted_final.current_step = LoopStepName::EvalReport;
        publish_test_final_artifacts(
            &workspace,
            &substituted_approved,
            &mut substituted_final,
            true,
        );
        let error = persist_run_with_full_compare(&workspace, &approved, &substituted_final)
            .expect_err("self-consistent substituted Approved lineage must fail locked CAS");
        assert!(
            error.to_string().contains("exact locked Approved"),
            "{error}"
        );
        assert_eq!(fs::read(workspace.run_file()).unwrap(), approved_bytes);

        let mut eval_passed = approved.clone();
        eval_passed.status = LoopStatus::EvalPassed;
        eval_passed.current_step = LoopStepName::EvalReport;
        publish_test_final_artifacts(&workspace, &approved, &mut eval_passed, true);
        persist_run_with_full_compare(&workspace, &approved, &eval_passed)
            .expect("private fixture may publish exact EvalPassed validation authority");
        let final_bytes = fs::read(workspace.run_file()).unwrap();
        let error = persist_provider_exchange_record_reference(
            &workspace,
            eval_passed
                .provider_exchange_records
                .last()
                .unwrap()
                .clone(),
        )
        .expect_err("EvalPassed must freeze provider append");
        assert!(error.to_string().contains("frozen"), "{error}");
        let error =
            reconcile_provider_exchange_state_with_validator(&workspace, &eval_passed, |_| Ok(()))
                .expect_err("EvalPassed must freeze reconciliation");
        assert!(error.to_string().contains("frozen"), "{error}");
        let mut stale = eval_passed.clone();
        stale.status = LoopStatus::Completed;
        stale.human_approval = None;
        let error = persist_run_with_provider_exchange_compare(&workspace, &stale)
            .expect_err("ordinary publication cannot replace EvalPassed");
        assert!(error.to_string().contains("final evaluation"), "{error}");
        let mut reset = eval_passed.clone();
        state::reset_from_step(&mut reset, LoopStepName::OutputReview).unwrap();
        let error = persist_provider_rerun_reset(
            &workspace,
            &eval_passed,
            &reset,
            LoopStepName::OutputReview,
            2,
        )
        .expect_err("provider rerun reset cannot replace EvalPassed");
        assert!(
            error.to_string().contains("human review authority"),
            "{error}"
        );
        assert_eq!(fs::read(workspace.run_file()).unwrap(), final_bytes);

        let eval_passed_digest = canonical_sha256_digest(&eval_passed).unwrap();
        let testing = eval_passed
            .steps
            .iter()
            .find(|step| step.name == LoopStepName::Testing)
            .unwrap();
        let report = eval_passed
            .steps
            .iter()
            .find(|step| step.name == LoopStepName::EvalReport)
            .unwrap();
        let intent = serde_json::json!({
            "schema_version": 1,
            "run_id": eval_passed.run_id,
            "reviewer": "promotion-reviewer@example.invalid",
            "started_at": "101",
            "candidate_diff": eval_passed.human_approval.as_ref().unwrap().candidate_diff,
            "testing_evidence": {
                "path": testing.artifact_path,
                "digest": testing.artifact_digest,
            },
            "eval_report": {
                "path": report.artifact_path,
                "digest": report.artifact_digest,
            },
            "policy_decision_digest": eval_passed.human_approval.as_ref().unwrap().policy_decision_digest,
            "target_head": eval_passed.human_approval.as_ref().unwrap().starting_head,
            "eval_passed_run_digest": eval_passed_digest,
        });
        let intent_bytes = canonical_json_bytes(&intent).unwrap();
        let intent_reference = ArtifactReference {
            path: "artifacts/09-promotion.intent.json".to_string(),
            digest: canonical_sha256_digest(&intent).unwrap(),
        };
        fs::write(
            workspace.run_directory().join(&intent_reference.path),
            intent_bytes,
        )
        .unwrap();
        let mut promoted = eval_passed.clone();
        promoted.status = LoopStatus::Promoted;
        promoted.updated_at = "101".to_string();
        promoted.promotion = Some(PromotionEvidence {
            schema_version: 1,
            run_id: eval_passed.run_id.clone(),
            reviewer: "promotion-reviewer@example.invalid".to_string(),
            promoted_at: "101".to_string(),
            intent: intent_reference,
            candidate_diff: eval_passed
                .human_approval
                .as_ref()
                .unwrap()
                .candidate_diff
                .clone(),
            testing_evidence: ArtifactReference {
                path: testing.artifact_path.clone().unwrap(),
                digest: testing.artifact_digest.clone().unwrap(),
            },
            eval_report: ArtifactReference {
                path: report.artifact_path.clone().unwrap(),
                digest: report.artifact_digest.clone().unwrap(),
            },
            policy_decision_digest: eval_passed
                .human_approval
                .as_ref()
                .unwrap()
                .policy_decision_digest
                .clone(),
            target_head: eval_passed
                .human_approval
                .as_ref()
                .unwrap()
                .starting_head
                .clone(),
            eval_passed_run_digest: eval_passed_digest,
            eval_passed_updated_at: eval_passed.updated_at.clone(),
        });
        persist_run_with_full_compare(&workspace, &eval_passed, &promoted)
            .expect("private promotion publisher may create exact Promoted authority");
        let promoted_bytes = fs::read(workspace.run_file()).unwrap();

        let mut public_replacement = promoted.clone();
        public_replacement.status = LoopStatus::Completed;
        public_replacement.human_approval = None;
        public_replacement.promotion = None;
        state::save_run(&workspace, &public_replacement)
            .expect_err("public state writer cannot replace Promoted");
        persist_provider_exchange_record_reference(
            &workspace,
            promoted.provider_exchange_records.last().unwrap().clone(),
        )
        .expect_err("provider append cannot replace Promoted");
        reconcile_provider_exchange_state_with_validator(&workspace, &promoted, |_| Ok(()))
            .expect_err("provider reconciliation cannot replace Promoted");
        crate::validate_rerun_eligibility(&promoted, LoopStepName::OutputReview)
            .expect_err("runner rerun cannot replace Promoted");
        assert_eq!(fs::read(workspace.run_file()).unwrap(), promoted_bytes);
    }

    #[test]
    fn locked_append_validator_rejection_leaves_staged_record_unadopted() {
        let temp = tempfile::tempdir().expect("temp");
        let workspace =
            LoopWorkspace::create(&temp.path().join("runs"), "locked-validator").unwrap();
        let run = state::create_run(state::NewLoopRun {
            run_id: "locked-validator".to_string(),
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
        state::save_run(&workspace, &run).expect("initial run");
        let coordinates = ProviderExchangeCoordinates {
            run_id: run.run_id.clone(),
            step: LoopStepName::Research,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
        };
        let request =
            write_provider_exchange_request(workspace.run_directory(), &coordinates, b"research")
                .expect("request audit");
        let record = ProviderExchangeRecord {
            schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: run.run_id.clone(),
            step: coordinates.step,
            role: coordinates.role,
            step_attempt: coordinates.step_attempt,
            exchange_index: coordinates.exchange_index,
            kind: coordinates.kind,
            context_round: None,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: None,
            request,
            response: None,
            expansion: None,
            outcome: None,
        };
        let reference =
            stage_provider_exchange_record(workspace.run_directory(), &record).expect("stage");
        let before = fs::read(workspace.run_file()).expect("run bytes");

        let error = persist_provider_exchange_record_reference_with_validator(
            &workspace,
            reference.clone(),
            |_| {
                Err(ProviderExchangeError::Invalid(
                    "prospective subject rejected".to_string(),
                ))
            },
        )
        .expect_err("validator must reject before publication");

        assert!(error.to_string().contains("prospective subject rejected"));
        assert_eq!(fs::read(workspace.run_file()).unwrap(), before);
        let current = state::load_run(&workspace).expect("current run");
        assert!(current.provider_exchange_records.is_empty());
        assert_eq!(
            classify_provider_exchange_record(&workspace, &current, &reference).unwrap(),
            ProviderExchangeRecordState::Staged
        );
    }

    #[test]
    fn public_append_rejects_output_review_initial_request_before_adoption() {
        let temp = tempfile::tempdir().expect("temp");
        let workspace =
            LoopWorkspace::create(&temp.path().join("runs"), "typed-output-review").unwrap();
        let run = state::create_run(state::NewLoopRun {
            run_id: "typed-output-review".to_string(),
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
        state::save_run(&workspace, &run).expect("initial run");
        let coordinates = ProviderExchangeCoordinates {
            run_id: run.run_id.clone(),
            step: LoopStepName::OutputReview,
            role: ProviderRole::OutputReviewer,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
        };
        let request = write_provider_exchange_request(
            workspace.run_directory(),
            &coordinates,
            b"unauthenticated output review",
        )
        .expect("request audit");
        let record = ProviderExchangeRecord {
            schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: run.run_id.clone(),
            step: coordinates.step,
            role: coordinates.role,
            step_attempt: coordinates.step_attempt,
            exchange_index: coordinates.exchange_index,
            kind: coordinates.kind,
            context_round: None,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: None,
            request,
            response: None,
            expansion: None,
            outcome: None,
        };
        let reference =
            stage_provider_exchange_record(workspace.run_directory(), &record).expect("stage");
        let before = fs::read(workspace.run_file()).expect("run bytes");

        let error = persist_provider_exchange_record_reference(&workspace, reference.clone())
            .expect_err("public append must require authenticated OutputReview path");

        assert!(
            error
                .to_string()
                .contains("authenticated ProviderStepRunner path"),
            "{error}"
        );
        assert_eq!(fs::read(workspace.run_file()).unwrap(), before);
        let current = state::load_run(&workspace).expect("current run");
        assert!(current.provider_exchange_records.is_empty());
        assert_eq!(
            classify_provider_exchange_record(&workspace, &current, &reference).unwrap(),
            ProviderExchangeRecordState::Staged
        );
    }

    #[test]
    fn reconciliation_rejects_stale_candidate_authority_before_adopting_a_staged_suffix() {
        let temp = tempfile::tempdir().expect("temp");
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), "reconcile-candidate-cas")
            .expect("workspace");
        let mut authoritative = state::create_run(state::NewLoopRun {
            run_id: "reconcile-candidate-cas".to_string(),
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
        authoritative.status = LoopStatus::Completed;
        authoritative.candidate_workspace = Some(CandidateWorkspaceState {
            schema_version: 1,
            run_directory_digest: None,
            path: temp.path().join("candidate").display().to_string(),
            source_worktree_root: temp.path().join("source").display().to_string(),
            git_common_dir: temp.path().join("source/.git").display().to_string(),
            repository_identity_digest: "d".repeat(64),
            starting_head: "e".repeat(40),
            starting_tree: "f".repeat(40),
            candidate_head: "e".repeat(40),
            candidate_tree: "f".repeat(40),
            candidate_diff_digest:
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            patch_transaction: None,
            lifecycle: CandidateWorkspaceLifecycle::Active,
            cleanup_started_at: None,
            cleaned_at: None,
        });
        authoritative.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
        state::save_run(&workspace, &authoritative).expect("authoritative run");
        crate::artifacts::write_step_request(&workspace, LoopStepName::Research, 1, "research")
            .expect("conventional request");
        let coordinates = ProviderExchangeCoordinates {
            run_id: authoritative.run_id.clone(),
            step: LoopStepName::Research,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
        };
        let request =
            write_provider_exchange_request(workspace.run_directory(), &coordinates, b"research")
                .expect("request audit");
        let staged = ProviderExchangeRecord {
            schema_version: 1,
            run_id: authoritative.run_id.clone(),
            step: coordinates.step,
            role: coordinates.role,
            step_attempt: coordinates.step_attempt,
            exchange_index: coordinates.exchange_index,
            kind: coordinates.kind,
            context_round: None,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: None,
            request,
            response: None,
            expansion: None,
            outcome: None,
        };
        stage_provider_exchange_record(workspace.run_directory(), &staged).expect("staged suffix");

        let mut persisted = authoritative.clone();
        let candidate = persisted.candidate_workspace.as_mut().unwrap();
        candidate.lifecycle = CandidateWorkspaceLifecycle::Cleaning;
        candidate.cleanup_started_at = Some("cleanup-intent".to_string());
        state::save_run(&workspace, &persisted).expect("persisted Cleaning state");
        let before = std::fs::read(workspace.run_file()).expect("before bytes");

        let error = reconcile_provider_exchange_state_with_validator(
            &workspace,
            &authoritative,
            |_| Ok(()),
        )
        .expect_err("stale Active authority must not replace Cleaning");

        assert!(
            error.to_string().contains("persisted LoopRun differs"),
            "{error}"
        );
        assert_eq!(std::fs::read(workspace.run_file()).unwrap(), before);
        let current = state::load_run(&workspace).expect("current run");
        assert_eq!(current, persisted);
        assert!(current.provider_exchange_records.is_empty());
        assert_eq!(
            current.candidate_workspace.unwrap().lifecycle,
            CandidateWorkspaceLifecycle::Cleaning
        );
    }

    #[test]
    fn compare_and_publish_rejects_a_concurrent_suffix_for_nonempty_intended_state() {
        let temp = tempfile::tempdir().expect("temp");
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), "state-compare-race")
            .expect("workspace");
        let run = state::create_run(state::NewLoopRun {
            run_id: "state-compare-race".to_string(),
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
        state::save_run(&workspace, &run).expect("initial run");
        let research = ProviderExchangeCoordinates {
            run_id: run.run_id.clone(),
            step: LoopStepName::Research,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
        };
        let request =
            write_provider_exchange_request(workspace.run_directory(), &research, b"research")
                .expect("research request");
        let request_record = ProviderExchangeRecord {
            schema_version: 1,
            run_id: run.run_id.clone(),
            step: research.step,
            role: research.role,
            step_attempt: research.step_attempt,
            exchange_index: research.exchange_index,
            kind: research.kind,
            context_round: research.context_round,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: None,
            request: request.clone(),
            response: None,
            expansion: None,
            outcome: None,
        };
        let request_reference =
            stage_provider_exchange_record(workspace.run_directory(), &request_record)
                .expect("stage research request");
        persist_provider_exchange_record_reference(&workspace, request_reference.clone())
            .expect("append research request");
        let response = write_provider_exchange_response(
            workspace.run_directory(),
            &research,
            &ProviderExchangeResponseAudit::ModelResponse {
                response: ModelResponse {
                    content: serde_json::json!({
                        "role": "researcher",
                        "status": "passed",
                        "summary": "Complete.",
                        "findings": [],
                        "risks": [],
                        "next_step_recommendation": "Continue."
                    })
                    .to_string(),
                    latency_ms: 1,
                    raw_provider_metadata: serde_json::Value::Null,
                },
            },
        )
        .expect("research response");
        let response_record = ProviderExchangeRecord {
            phase: ProviderExchangePhase::Response,
            previous_record_digest: Some(request_reference.digest),
            response: Some(response),
            outcome: Some(ProviderExchangeOutcome::Passed),
            ..request_record
        };
        let response_reference =
            stage_provider_exchange_record(workspace.run_directory(), &response_record)
                .expect("stage research response");
        persist_provider_exchange_record_reference(&workspace, response_reference.clone())
            .expect("append research response");
        let mut intended = state::load_run(&workspace).expect("intended state");
        intended.updated_at = "intended-terminal-state".to_string();

        let analysis = ProviderExchangeCoordinates {
            run_id: run.run_id.clone(),
            step: LoopStepName::Analysis,
            role: ProviderRole::Analyzer,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
        };
        let analysis_request =
            write_provider_exchange_request(workspace.run_directory(), &analysis, b"analysis")
                .expect("analysis request");
        let analysis_record = ProviderExchangeRecord {
            schema_version: 1,
            run_id: run.run_id,
            step: analysis.step,
            role: analysis.role,
            step_attempt: analysis.step_attempt,
            exchange_index: analysis.exchange_index,
            kind: analysis.kind,
            context_round: analysis.context_round,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: Some(response_reference.digest),
            request: analysis_request,
            response: None,
            expansion: None,
            outcome: None,
        };
        let concurrent =
            stage_provider_exchange_record(workspace.run_directory(), &analysis_record)
                .expect("stage concurrent suffix");
        persist_provider_exchange_record_reference(&workspace, concurrent.clone())
            .expect("append concurrent suffix");

        let error = persist_run_with_provider_exchange_compare(&workspace, &intended)
            .expect_err("stale state must not erase suffix");

        assert!(error.to_string().contains("exchange head changed"));
        let current = state::load_run(&workspace).expect("current run");
        assert_eq!(current.provider_exchange_records.last(), Some(&concurrent));
        assert_ne!(current.updated_at, intended.updated_at);
    }

    #[test]
    fn pre_publish_failure_leaves_the_old_run_json_valid_and_byte_identical() {
        let temp = tempfile::tempdir().expect("temp");
        let workspace =
            LoopWorkspace::create(&temp.path().join("runs"), "atomic-run").expect("workspace");
        let run = state::create_run(state::NewLoopRun {
            run_id: "atomic-run".to_string(),
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
        state::save_run(&workspace, &run).expect("initial run");
        let original = std::fs::read(workspace.run_file()).expect("old bytes");
        let mut changed_run = run;
        changed_run.updated_at = "replacement-state".to_string();
        let mut replacement = serde_json::to_vec_pretty(&changed_run).expect("replacement");
        replacement.push(b'\n');

        let error =
            replace_run_file_atomically_with_hook(&workspace.run_file(), &replacement, || {
                Err(ProviderExchangeError::Invalid(
                    "injected pre-publish failure".to_string(),
                ))
            })
            .expect_err("injected failure");

        assert!(error.to_string().contains("injected"));
        assert_eq!(
            std::fs::read(workspace.run_file()).expect("old bytes"),
            original
        );
        state::load_run(&workspace).expect("old run remains valid");
    }

    #[test]
    fn failed_locked_rerun_reset_keeps_the_same_authority_retryable() {
        let temp = tempfile::tempdir().expect("temp");
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), "rerun-reset-retry")
            .expect("workspace");
        let mut previous = state::create_run(state::NewLoopRun {
            run_id: "rerun-reset-retry".to_string(),
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
        state::mark_step_running(&mut previous, LoopStepName::Research).expect("running");
        state::save_run(&workspace, &previous).expect("previous run");
        let previous_bytes = std::fs::read(workspace.run_file()).expect("previous bytes");
        let mut reset = previous.clone();
        state::reset_from_step(&mut reset, LoopStepName::Research).expect("reset");

        let error = persist_provider_rerun_reset_with_hook(
            &workspace,
            &previous,
            &reset,
            LoopStepName::Research,
            1,
            || {
                Err(ProviderExchangeError::Invalid(
                    "injected rerun reset publication failure".to_string(),
                ))
            },
        )
        .expect_err("injected failure");

        assert!(error.to_string().contains("injected"));
        assert_eq!(
            std::fs::read(workspace.run_file()).expect("unchanged run bytes"),
            previous_bytes
        );
        verify_rerun_authorization(&workspace, &previous, LoopStepName::Research, 1)
            .expect("immutable authorization remains valid for the unchanged head");

        persist_provider_rerun_reset(&workspace, &previous, &reset, LoopStepName::Research, 1)
            .expect("retry identical locked transaction");

        assert_eq!(state::load_run(&workspace).expect("reset run"), reset);
    }

    #[test]
    fn atomic_temp_reservation_skips_an_orphaned_name_collision() {
        let temp = tempfile::tempdir().expect("temp");
        let sequence = AtomicU64::new(0);
        let file_name = std::ffi::OsStr::new("run.json");
        let orphan = temp.path().join(format!(
            ".run.json.exchange-state.tmp-{}-0",
            std::process::id()
        ));
        std::fs::write(&orphan, b"orphan").expect("orphan");

        let (reserved, file) =
            create_run_state_temp(temp.path(), file_name, &sequence).expect("next temp");
        drop(file);

        assert!(reserved.ends_with(format!(
            ".run.json.exchange-state.tmp-{}-1",
            std::process::id()
        )));
        assert_eq!(std::fs::read(orphan).expect("orphan bytes"), b"orphan");
        std::fs::remove_file(reserved).expect("cleanup reservation");
    }
}
