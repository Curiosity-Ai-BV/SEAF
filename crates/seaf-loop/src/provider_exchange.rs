use std::{
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
    let lock = acquire_provider_exchange_lock(workspace)?;
    let result = (|| {
        let mut run = state::load_run(workspace)?;
        if run.provider_exchange_records.contains(&reference) {
            return Err(ProviderExchangeError::Invalid(
                "provider exchange record is already referenced".to_string(),
            ));
        }
        validate_provider_exchange_record_append(workspace, &run, &reference)?;
        run.provider_exchange_records.push(reference);
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

pub fn validate_provider_exchange_record_append(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    reference: &ProviderExchangeRecordReference,
) -> Result<(), ProviderExchangeError> {
    validate_authoritative_provider_exchange_records(workspace, run)?;
    let record = load_provider_exchange_record(workspace.run_directory(), reference)?;
    validate_append_link(workspace, run, &record)?;
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
    validate_authoritative_provider_exchange_records(workspace, run)
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
        validate_append_link(workspace, &verified_prefix, &record)?;
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
    }
    Ok(())
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
struct DerivedResponseClassification {
    outcome: ProviderExchangeOutcome,
    json_repair_eligible: bool,
}

fn derive_bound_response_classification(
    run_directory: &Path,
    record: &ProviderExchangeRecord,
) -> Result<DerivedResponseClassification, ProviderExchangeError> {
    let response = record.response.as_ref().ok_or_else(|| {
        ProviderExchangeError::Invalid("response record has no response audit".to_string())
    })?;
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
    Ok(derive_response_classification(record.role, &audit))
}

fn derive_response_classification(
    role: ProviderRole,
    audit: &ProviderExchangeResponseAudit,
) -> DerivedResponseClassification {
    let ProviderExchangeResponseAudit::ModelResponse { response } = audit else {
        return DerivedResponseClassification {
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
            return DerivedResponseClassification {
                outcome: ProviderExchangeOutcome::InvalidResponse,
                json_repair_eligible: true,
            };
        }
        Err(_) => ProviderExchangeOutcome::InvalidResponse,
    };
    DerivedResponseClassification {
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
mod tests {
    use super::*;
    use seaf_core::LoopInputDigests;

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
