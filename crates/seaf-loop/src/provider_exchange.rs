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
        if matches!(
            current.status,
            seaf_core::LoopStatus::AwaitingHumanReview | seaf_core::LoopStatus::Approved
        ) {
            return Err(ProviderExchangeError::Invalid(
                "provider rerun reset cannot replace human review authority".to_string(),
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
        if matches!(
            run.status,
            seaf_core::LoopStatus::AwaitingHumanReview | seaf_core::LoopStatus::Approved
        ) {
            return Err(ProviderExchangeError::Invalid(
                "provider exchange history is frozen by human review authority".to_string(),
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
        if matches!(
            current.status,
            seaf_core::LoopStatus::AwaitingHumanReview | seaf_core::LoopStatus::Approved
        ) && &current != intended
        {
            return Err(ProviderExchangeError::Invalid(
                "ordinary state publication cannot replace awaiting human review or approved authority"
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
    let lock = acquire_provider_exchange_lock(workspace)?;
    let result = (|| {
        let current = state::load_run(workspace)?;
        if &current != expected {
            return Err(ProviderExchangeError::Invalid(
                "LoopRun changed before compare-and-swap publication".to_string(),
            ));
        }
        validate_current(&current)?;
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
        if matches!(
            persisted.status,
            seaf_core::LoopStatus::AwaitingHumanReview | seaf_core::LoopStatus::Approved
        ) {
            return Err(ProviderExchangeError::Invalid(
                "provider exchange reconciliation is frozen by human review authority".to_string(),
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

fn validate_run_for_atomic_publication(
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
    if matches!(
        run.status,
        seaf_core::LoopStatus::AwaitingHumanReview | seaf_core::LoopStatus::Approved
    ) {
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
        if run.status == seaf_core::LoopStatus::Approved {
            validate_approved_publication_evidence(workspace, run)?;
        }
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
            let authorized_rerun = record.kind == ProviderExchangeKind::Initial
                && record.exchange_index == 1
                && record.step_attempt > previous_attempt_for_step
                && verify_rerun_authorization(workspace, run, record.step, record.step_attempt)
                    .is_ok();
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
            verify_rerun_authorization(workspace, run, record.step, record.step_attempt)?;
        }
    } else {
        if record.kind != ProviderExchangeKind::Initial || record.exchange_index != 1 {
            return Err(ProviderExchangeError::Invalid(
                "the authoritative provider exchange history must begin with an initial request"
                    .to_string(),
            ));
        }
        if record.step_attempt > 1 {
            verify_rerun_authorization(workspace, run, record.step, record.step_attempt)?;
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

pub(crate) fn validate_recovered_conventional_attempt(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    step: LoopStepName,
    step_attempt: u32,
) -> Result<(), ProviderExchangeError> {
    if verify_rerun_authorization(workspace, run, step, step_attempt).is_ok() {
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
        CandidateWorkspaceLifecycle, CandidateWorkspaceState, HumanApprovalEvidence,
        LoopInputDigests, LoopStatus, LoopStepStatus,
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

    #[test]
    fn awaiting_human_review_freezes_ordinary_publication_and_provider_suffixes() {
        let temp = tempfile::tempdir().expect("temp");
        let workspace =
            LoopWorkspace::create(&temp.path().join("runs"), "awaiting-provider-freeze").unwrap();
        let mut run = awaiting_human_review_run(&workspace);
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
            approved_at: "approved-at".to_string(),
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
