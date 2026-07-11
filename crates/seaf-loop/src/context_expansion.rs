use std::{
    collections::BTreeSet,
    error::Error,
    fmt, fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use seaf_core::{canonical_json_bytes, canonical_sha256_digest, ArtifactReference, LoopStepName};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    context::{CandidateContextAuthority, ContextLimits, UNTRUSTED_CONTEXT_MARKER},
    immutable_artifact::{publish_create_only, read_verified_regular_file, ImmutableArtifactError},
    policy::{default_exclude_patterns, matching_pattern, normalize_repo_path},
    state::step_file_stem,
    ContextRequest, Role,
};

pub const CONTEXT_EXPANSION_SCHEMA_VERSION: u32 = 1;
const SOURCE_READ_BUFFER_BYTES: usize = 8 * 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextExpansionRequest {
    pub repository_root: PathBuf,
    pub run_directory: PathBuf,
    pub run_id: String,
    pub step: LoopStepName,
    pub role: Role,
    pub step_attempt: u32,
    pub context_round: u32,
    pub context_request: ContextRequest,
    pub initial_provider_request: ArtifactReference,
    pub previous_expansion: Option<ArtifactReference>,
    pub candidate_authority: Option<CandidateContextAuthority>,
    pub initial_loaded_paths: Vec<String>,
    pub initial_context_bytes: usize,
    pub ticket_forbidden_files: Vec<String>,
    pub policy_forbidden_paths: Vec<String>,
    pub default_exclude_globs: Vec<String>,
    pub limits: ContextLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextExpansionArtifact {
    pub schema_version: u32,
    pub run_id: String,
    pub step: LoopStepName,
    pub role: Role,
    pub step_attempt: u32,
    pub context_round: u32,
    pub context_request: ContextRequest,
    pub initial_provider_request: ArtifactReference,
    pub initial_loaded_paths: Vec<String>,
    pub initial_context_bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_expansion: Option<ArtifactReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_authority: Option<CandidateContextAuthority>,
    pub limits: ContextLimits,
    pub default_exclude_globs: Vec<String>,
    pub ticket_forbidden_files: Vec<String>,
    pub policy_forbidden_paths: Vec<String>,
    pub excluded_loaded_paths: Vec<String>,
    pub prior_total_context_bytes: usize,
    pub resulting_total_context_bytes: usize,
    pub untrusted_context_marker: String,
    pub files: Vec<ContextExpansionFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextExpansionFile {
    pub path: String,
    pub content: String,
    pub source_sha256: String,
    pub included_sha256: String,
    pub source_bytes: usize,
    pub included_bytes: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedContextExpansion {
    pub identity: ArtifactReference,
    pub artifact: ContextExpansionArtifact,
}

pub fn create_context_expansion(
    request: &ContextExpansionRequest,
) -> Result<CreatedContextExpansion, ContextExpansionError> {
    let prepared = PreparedRequest::new(request)?;
    verify_initial_provider_request(request, &prepared.initial_provider_request)?;
    let prior = load_prior_chain(request, &prepared)?;
    let relative_path =
        expansion_artifact_path(request.step, request.step_attempt, request.context_round);

    let repository_root = request.repository_root.canonicalize().map_err(|error| {
        ContextExpansionError::Unavailable(format!("repository root is unavailable: {error}"))
    })?;
    if !repository_root.is_dir() {
        return Err(ContextExpansionError::Unavailable(
            "repository root is not a directory".to_string(),
        ));
    }
    let mut loaded_paths = prepared.initial_loaded_paths.clone();
    for artifact in &prior {
        for file in &artifact.files {
            loaded_paths.insert(file.path.clone());
        }
    }

    let mut excluded_loaded_paths = Vec::new();
    let mut new_paths = Vec::new();
    for path in &prepared.context_request.paths {
        if loaded_paths.contains(path) {
            excluded_loaded_paths.push(path.clone());
        } else {
            new_paths.push(path.clone());
        }
    }
    if new_paths.is_empty() {
        return Err(ContextExpansionError::Safety(
            "context request contains no new context files".to_string(),
        ));
    }

    let prior_total_context_bytes = prior
        .last()
        .map_or(request.initial_context_bytes, |artifact| {
            artifact.resulting_total_context_bytes
        });
    let mut resulting_total_context_bytes = prior_total_context_bytes;
    let mut files = Vec::with_capacity(new_paths.len());
    for path in new_paths {
        reject_forbidden_path(&path, &prepared)?;
        let remaining = request
            .limits
            .max_total_bytes
            .saturating_sub(resulting_total_context_bytes);
        let retain_limit = request.limits.max_bytes_per_file.min(remaining);
        let source = read_context_source(&repository_root, &path, retain_limit)?;
        if source.source_bytes == 0 {
            return Err(ContextExpansionError::Safety(format!(
                "requested context file has zero useful bytes: {path}"
            )));
        }
        let included_bytes = utf8_prefix_len(&source.retained_prefix, source.retained_prefix.len());
        if included_bytes == 0 {
            return Err(ContextExpansionError::Safety(format!(
                "context expansion would omit requested new file {path}"
            )));
        }
        let content = String::from_utf8(source.retained_prefix[..included_bytes].to_vec())
            .expect("streaming validation guarantees a UTF-8 prefix");
        resulting_total_context_bytes += included_bytes;
        files.push(ContextExpansionFile {
            path,
            content,
            source_sha256: source.source_sha256,
            included_sha256: digest_bytes(&source.retained_prefix[..included_bytes]),
            source_bytes: source.source_bytes,
            included_bytes,
            truncated: included_bytes < source.source_bytes,
        });
    }

    let artifact = ContextExpansionArtifact {
        schema_version: CONTEXT_EXPANSION_SCHEMA_VERSION,
        run_id: request.run_id.clone(),
        step: request.step,
        role: request.role,
        step_attempt: request.step_attempt,
        context_round: request.context_round,
        context_request: prepared.context_request.clone(),
        initial_provider_request: prepared.initial_provider_request.clone(),
        initial_loaded_paths: prepared.initial_loaded_paths.iter().cloned().collect(),
        initial_context_bytes: request.initial_context_bytes,
        previous_expansion: request.previous_expansion.clone(),
        candidate_authority: request.candidate_authority.clone(),
        limits: request.limits,
        default_exclude_globs: prepared.default_exclude_globs.clone(),
        ticket_forbidden_files: prepared.ticket_forbidden_files.clone(),
        policy_forbidden_paths: prepared.policy_forbidden_paths.clone(),
        excluded_loaded_paths,
        prior_total_context_bytes,
        resulting_total_context_bytes,
        untrusted_context_marker: UNTRUSTED_CONTEXT_MARKER.to_string(),
        files,
    };
    validate_artifact_structure(&artifact)?;
    let bytes = canonical_json_bytes(&artifact)?;
    publish_create_only(&request.run_directory, &relative_path, &bytes)
        .map_err(ContextExpansionError::from_publication)?;
    Ok(CreatedContextExpansion {
        identity: ArtifactReference {
            path: relative_path,
            digest: digest_bytes(&bytes),
        },
        artifact,
    })
}

pub fn load_context_expansion(
    request: &ContextExpansionRequest,
    identity: &ArtifactReference,
) -> Result<ContextExpansionArtifact, ContextExpansionError> {
    let prepared = PreparedRequest::new(request)?;
    verify_initial_provider_request(request, &prepared.initial_provider_request)?;
    let prior = load_prior_chain(request, &prepared)?;
    let expected_path =
        expansion_artifact_path(request.step, request.step_attempt, request.context_round);
    if identity.path != expected_path {
        return Err(ContextExpansionError::Invalid(
            "context expansion artifact path does not match its identity".to_string(),
        ));
    }
    let bytes =
        read_verified_regular_file(&request.run_directory, &identity.path, "context expansion")?;
    let artifact = decode_artifact(&bytes, &identity.digest)?;
    validate_expected_artifact(&artifact, request, &prepared, &prior)?;
    Ok(artifact)
}

pub fn reconstruct_context_expansion_files(
    request: &ContextExpansionRequest,
    identity: &ArtifactReference,
) -> Result<Vec<ContextExpansionFile>, ContextExpansionError> {
    let prepared = PreparedRequest::new(request)?;
    verify_initial_provider_request(request, &prepared.initial_provider_request)?;
    let prior = load_prior_chain(request, &prepared)?;
    let current = load_context_expansion(request, identity)?;
    let mut files = Vec::new();
    for artifact in prior.into_iter().chain(std::iter::once(current)) {
        files.extend(artifact.files);
    }
    let mut paths = BTreeSet::new();
    if files.iter().any(|file| !paths.insert(file.path.clone())) {
        return Err(ContextExpansionError::Invalid(
            "context expansion chain contains duplicate file paths".to_string(),
        ));
    }
    Ok(files)
}

#[derive(Debug)]
pub enum ContextExpansionError {
    Safety(String),
    Unavailable(String),
    Invalid(String),
    AuditSafety(String),
    PublicationSafety(String),
    Collision(String),
    PublicationIo(std::io::Error),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for ContextExpansionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Safety(message) => write!(formatter, "context expansion safety error: {message}"),
            Self::Unavailable(message) => {
                write!(formatter, "context expansion source unavailable: {message}")
            }
            Self::Invalid(message) => write!(formatter, "invalid context expansion: {message}"),
            Self::AuditSafety(message) => {
                write!(formatter, "unsafe trusted context audit: {message}")
            }
            Self::PublicationSafety(message) => {
                write!(formatter, "unsafe context expansion publication: {message}")
            }
            Self::Collision(message) => write!(formatter, "context expansion collision: {message}"),
            Self::PublicationIo(error) => {
                write!(
                    formatter,
                    "context expansion publication I/O error: {error}"
                )
            }
            Self::Io(error) => write!(formatter, "context expansion I/O error: {error}"),
            Self::Json(error) => write!(formatter, "context expansion JSON error: {error}"),
        }
    }
}

impl Error for ContextExpansionError {}

impl ContextExpansionError {
    fn from_publication(error: ImmutableArtifactError) -> Self {
        match error {
            ImmutableArtifactError::Safety(message) => Self::PublicationSafety(message),
            ImmutableArtifactError::Collision(message) => Self::Collision(message),
            ImmutableArtifactError::Io(error) => Self::PublicationIo(error),
        }
    }
}

impl From<std::io::Error> for ContextExpansionError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ContextExpansionError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<ImmutableArtifactError> for ContextExpansionError {
    fn from(error: ImmutableArtifactError) -> Self {
        match error {
            ImmutableArtifactError::Safety(message) => Self::AuditSafety(message),
            ImmutableArtifactError::Collision(message) => Self::Collision(message),
            ImmutableArtifactError::Io(error) => Self::Io(error),
        }
    }
}

struct PreparedRequest {
    context_request: ContextRequest,
    initial_provider_request: ArtifactReference,
    initial_loaded_paths: BTreeSet<String>,
    default_exclude_globs: Vec<String>,
    ticket_forbidden_files: Vec<String>,
    policy_forbidden_paths: Vec<String>,
}

impl PreparedRequest {
    fn new(request: &ContextExpansionRequest) -> Result<Self, ContextExpansionError> {
        if request.run_id.trim().is_empty()
            || request.step_attempt == 0
            || request.context_round == 0
        {
            return Err(ContextExpansionError::Invalid(
                "run id, step attempt, and context round must be non-empty/non-zero".to_string(),
            ));
        }
        if expected_role(request.step) != Some(request.role) {
            return Err(ContextExpansionError::Invalid(
                "context expansion role does not match its loop step".to_string(),
            ));
        }
        if request.limits.max_bytes_per_file == 0 || request.limits.max_total_bytes == 0 {
            return Err(ContextExpansionError::Invalid(
                "context expansion limits must be non-zero".to_string(),
            ));
        }
        if request.initial_context_bytes > request.limits.max_total_bytes {
            return Err(ContextExpansionError::Invalid(
                "initial context bytes exceed the total context limit".to_string(),
            ));
        }
        validate_reference(&request.initial_provider_request)?;
        let expected_initial_paths =
            initial_provider_request_paths(request.step, request.step_attempt);
        if !expected_initial_paths.contains(&request.initial_provider_request.path) {
            return Err(ContextExpansionError::Invalid(format!(
                "initial provider request audit path mismatch: expected one of {}",
                expected_initial_paths.join(", ")
            )));
        }
        if let Some(previous) = &request.previous_expansion {
            validate_reference(previous)?;
        }
        if (request.context_round == 1) != request.previous_expansion.is_none() {
            return Err(ContextExpansionError::Invalid(
                "only context round one may omit the previous expansion link".to_string(),
            ));
        }

        let validated: ContextRequest =
            serde_json::from_value(serde_json::to_value(&request.context_request)?)?;
        let mut normalized_paths = Vec::new();
        let mut seen = BTreeSet::new();
        for path in validated.paths {
            let normalized = normalize_repo_path(&path).ok_or_else(|| {
                ContextExpansionError::Safety(format!("unsafe requested context path: {path}"))
            })?;
            if !seen.insert(normalized.clone()) {
                return Err(ContextExpansionError::Invalid(
                    "context request contains duplicate normalized paths".to_string(),
                ));
            }
            normalized_paths.push(normalized);
        }
        normalized_paths.sort();
        let context_request: ContextRequest = serde_json::from_value(serde_json::json!({
            "paths": normalized_paths,
            "reason": validated.reason,
        }))?;
        let initial_loaded_paths = normalized_set(&request.initial_loaded_paths, "initial loaded")?;
        let mut default_exclude_globs = default_exclude_patterns();
        default_exclude_globs.extend(request.default_exclude_globs.clone());
        sort_dedup(&mut default_exclude_globs);
        let mut ticket_forbidden_files = request.ticket_forbidden_files.clone();
        sort_dedup(&mut ticket_forbidden_files);
        let mut policy_forbidden_paths = request.policy_forbidden_paths.clone();
        sort_dedup(&mut policy_forbidden_paths);
        let prepared = Self {
            context_request,
            initial_provider_request: request.initial_provider_request.clone(),
            initial_loaded_paths,
            default_exclude_globs,
            ticket_forbidden_files,
            policy_forbidden_paths,
        };
        for path in &prepared.context_request.paths {
            reject_forbidden_path(path, &prepared)?;
        }
        Ok(prepared)
    }
}

fn validate_expected_artifact(
    artifact: &ContextExpansionArtifact,
    request: &ContextExpansionRequest,
    prepared: &PreparedRequest,
    prior: &[ContextExpansionArtifact],
) -> Result<(), ContextExpansionError> {
    validate_artifact_structure(artifact)?;
    if artifact.run_id != request.run_id
        || artifact.step != request.step
        || artifact.role != request.role
        || artifact.step_attempt != request.step_attempt
        || artifact.context_round != request.context_round
        || artifact.context_request != prepared.context_request
        || artifact.initial_provider_request != prepared.initial_provider_request
        || artifact.initial_loaded_paths
            != prepared
                .initial_loaded_paths
                .iter()
                .cloned()
                .collect::<Vec<_>>()
        || artifact.initial_context_bytes != request.initial_context_bytes
        || artifact.previous_expansion != request.previous_expansion
        || artifact.candidate_authority != request.candidate_authority
        || artifact.limits != request.limits
        || artifact.default_exclude_globs != prepared.default_exclude_globs
        || artifact.ticket_forbidden_files != prepared.ticket_forbidden_files
        || artifact.policy_forbidden_paths != prepared.policy_forbidden_paths
    {
        return Err(ContextExpansionError::Invalid(
            "context expansion identity, request, link, or limits mismatch".to_string(),
        ));
    }
    let expected_prior = prior.last().map_or(request.initial_context_bytes, |value| {
        value.resulting_total_context_bytes
    });
    if artifact.prior_total_context_bytes != expected_prior {
        return Err(ContextExpansionError::Invalid(
            "context expansion prior total mismatch".to_string(),
        ));
    }
    for file in &artifact.files {
        reject_forbidden_path(&file.path, prepared)?;
    }
    let mut loaded = prepared.initial_loaded_paths.clone();
    for previous in prior {
        for file in &previous.files {
            loaded.insert(file.path.clone());
        }
    }
    let expected_excluded: Vec<_> = prepared
        .context_request
        .paths
        .iter()
        .filter(|path| loaded.contains(*path))
        .cloned()
        .collect();
    if artifact.excluded_loaded_paths != expected_excluded {
        return Err(ContextExpansionError::Invalid(
            "context expansion excluded-loaded paths mismatch".to_string(),
        ));
    }
    Ok(())
}

fn validate_artifact_structure(
    artifact: &ContextExpansionArtifact,
) -> Result<(), ContextExpansionError> {
    if artifact.schema_version != CONTEXT_EXPANSION_SCHEMA_VERSION
        || artifact.step_attempt == 0
        || artifact.context_round == 0
        || artifact.untrusted_context_marker != UNTRUSTED_CONTEXT_MARKER
        || artifact.files.is_empty()
        || artifact.initial_context_bytes > artifact.prior_total_context_bytes
    {
        return Err(ContextExpansionError::Invalid(
            "context expansion schema or required content is invalid".to_string(),
        ));
    }
    validate_reference(&artifact.initial_provider_request)?;
    if let Some(previous) = &artifact.previous_expansion {
        validate_reference(previous)?;
    }
    if (artifact.context_round == 1) != artifact.previous_expansion.is_none() {
        return Err(ContextExpansionError::Invalid(
            "context expansion previous link does not match its round".to_string(),
        ));
    }
    let mut last_path: Option<&str> = None;
    let mut included_total = 0usize;
    for file in &artifact.files {
        let normalized = normalize_repo_path(&file.path).ok_or_else(|| {
            ContextExpansionError::Invalid(
                "context expansion contains an unsafe file path".to_string(),
            )
        })?;
        if normalized != file.path || last_path.is_some_and(|last| last >= file.path.as_str()) {
            return Err(ContextExpansionError::Invalid(
                "context expansion file paths are not canonical and unique".to_string(),
            ));
        }
        if file.included_bytes == 0
            || file.included_bytes != file.content.len()
            || file.included_bytes > file.source_bytes
            || file.included_bytes > artifact.limits.max_bytes_per_file
            || file.truncated != (file.included_bytes < file.source_bytes)
            || file.included_sha256 != digest_bytes(file.content.as_bytes())
            || !valid_digest(&file.source_sha256)
            || (!file.truncated && file.source_sha256 != file.included_sha256)
        {
            return Err(ContextExpansionError::Invalid(format!(
                "context expansion file metadata or digest is invalid: {}",
                file.path
            )));
        }
        included_total = included_total
            .checked_add(file.included_bytes)
            .ok_or_else(|| {
                ContextExpansionError::Invalid("context expansion byte total overflow".to_string())
            })?;
        last_path = Some(&file.path);
    }
    if artifact
        .prior_total_context_bytes
        .checked_add(included_total)
        != Some(artifact.resulting_total_context_bytes)
        || artifact.resulting_total_context_bytes > artifact.limits.max_total_bytes
    {
        return Err(ContextExpansionError::Invalid(
            "context expansion cumulative byte totals are invalid".to_string(),
        ));
    }
    if !strictly_sorted_unique(&artifact.excluded_loaded_paths)
        || !strictly_sorted_unique(&artifact.initial_loaded_paths)
        || !strictly_sorted_unique(&artifact.default_exclude_globs)
        || !strictly_sorted_unique(&artifact.ticket_forbidden_files)
        || !strictly_sorted_unique(&artifact.policy_forbidden_paths)
    {
        return Err(ContextExpansionError::Invalid(
            "context expansion unordered fields are not canonical".to_string(),
        ));
    }
    if artifact
        .initial_loaded_paths
        .iter()
        .any(|path| normalize_repo_path(path).as_deref() != Some(path.as_str()))
    {
        return Err(ContextExpansionError::Invalid(
            "context expansion initial loaded paths are not normalized".to_string(),
        ));
    }
    let represented: Vec<_> = artifact
        .excluded_loaded_paths
        .iter()
        .chain(artifact.files.iter().map(|file| &file.path))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if represented != artifact.context_request.paths {
        return Err(ContextExpansionError::Invalid(
            "context expansion does not represent every requested path exactly once".to_string(),
        ));
    }
    Ok(())
}

fn load_prior_chain(
    request: &ContextExpansionRequest,
    prepared: &PreparedRequest,
) -> Result<Vec<ContextExpansionArtifact>, ContextExpansionError> {
    let Some(mut identity) = request.previous_expansion.clone() else {
        return Ok(Vec::new());
    };
    let mut expected_round = request.context_round - 1;
    let mut reversed = Vec::new();
    loop {
        let expected_path =
            expansion_artifact_path(request.step, request.step_attempt, expected_round);
        if identity.path != expected_path {
            return Err(ContextExpansionError::Invalid(
                "previous context expansion path does not match its round".to_string(),
            ));
        }
        let bytes = read_verified_regular_file(
            &request.run_directory,
            &identity.path,
            "previous context expansion",
        )?;
        let artifact = decode_artifact(&bytes, &identity.digest)?;
        if artifact.run_id != request.run_id
            || artifact.step != request.step
            || artifact.role != request.role
            || artifact.step_attempt != request.step_attempt
            || artifact.context_round != expected_round
            || artifact.initial_provider_request != prepared.initial_provider_request
            || artifact.initial_loaded_paths
                != prepared
                    .initial_loaded_paths
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
            || artifact.initial_context_bytes != request.initial_context_bytes
            || artifact.candidate_authority != request.candidate_authority
            || artifact.limits != request.limits
            || artifact.default_exclude_globs != prepared.default_exclude_globs
            || artifact.ticket_forbidden_files != prepared.ticket_forbidden_files
            || artifact.policy_forbidden_paths != prepared.policy_forbidden_paths
        {
            return Err(ContextExpansionError::Invalid(
                "previous context expansion identity or authority mismatch".to_string(),
            ));
        }
        for path in &artifact.context_request.paths {
            reject_forbidden_path(path, prepared)?;
        }
        let next = artifact.previous_expansion.clone();
        reversed.push(artifact);
        if expected_round == 1 {
            if next.is_some() {
                return Err(ContextExpansionError::Invalid(
                    "first context expansion unexpectedly links to a predecessor".to_string(),
                ));
            }
            break;
        }
        identity = next.ok_or_else(|| {
            ContextExpansionError::Invalid("context expansion chain has a missing link".to_string())
        })?;
        expected_round -= 1;
    }
    reversed.reverse();
    let mut total = request.initial_context_bytes;
    let mut loaded = prepared.initial_loaded_paths.clone();
    for artifact in &reversed {
        if artifact.prior_total_context_bytes != total {
            return Err(ContextExpansionError::Invalid(
                "context expansion chain has inconsistent cumulative totals".to_string(),
            ));
        }
        let expected_excluded: Vec<_> = artifact
            .context_request
            .paths
            .iter()
            .filter(|path| loaded.contains(*path))
            .cloned()
            .collect();
        if artifact.excluded_loaded_paths != expected_excluded {
            return Err(ContextExpansionError::Invalid(
                "context expansion chain has false historical loaded-path exclusions".to_string(),
            ));
        }
        for file in &artifact.files {
            if !loaded.insert(file.path.clone()) {
                return Err(ContextExpansionError::Invalid(
                    "context expansion chain repeats an already loaded file".to_string(),
                ));
            }
        }
        total = artifact.resulting_total_context_bytes;
    }
    Ok(reversed)
}

fn decode_artifact(
    bytes: &[u8],
    expected_digest: &str,
) -> Result<ContextExpansionArtifact, ContextExpansionError> {
    if !valid_digest(expected_digest) || digest_bytes(bytes) != expected_digest {
        return Err(ContextExpansionError::Invalid(
            "context expansion artifact digest mismatch".to_string(),
        ));
    }
    let value: Value = serde_json::from_slice(bytes)?;
    if canonical_json_bytes(&value)? != bytes {
        return Err(ContextExpansionError::Invalid(
            "context expansion artifact bytes are not canonical JSON".to_string(),
        ));
    }
    let artifact: ContextExpansionArtifact = serde_json::from_value(value)?;
    validate_artifact_structure(&artifact)?;
    if canonical_sha256_digest(&artifact)? != expected_digest {
        return Err(ContextExpansionError::Invalid(
            "context expansion canonical artifact digest mismatch".to_string(),
        ));
    }
    Ok(artifact)
}

fn verify_initial_provider_request(
    request: &ContextExpansionRequest,
    reference: &ArtifactReference,
) -> Result<(), ContextExpansionError> {
    let bytes = read_verified_regular_file(
        &request.run_directory,
        &reference.path,
        "initial provider request",
    )?;
    if digest_bytes(&bytes) != reference.digest {
        return Err(ContextExpansionError::Invalid(
            "initial provider request audit digest mismatch".to_string(),
        ));
    }
    if let Some(expected) = &request.candidate_authority {
        let model_request: Value = serde_json::from_slice(&bytes).map_err(|error| {
            ContextExpansionError::Invalid(format!(
                "candidate-bound initial provider request is invalid JSON: {error}"
            ))
        })?;
        let role_input = model_request
            .get("messages")
            .and_then(Value::as_array)
            .and_then(|messages| messages.first())
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ContextExpansionError::Invalid(
                    "candidate-bound initial provider request has no role input".to_string(),
                )
            })?;
        let role_input: Value = serde_json::from_str(role_input).map_err(|error| {
            ContextExpansionError::Invalid(format!(
                "candidate-bound initial provider role input is invalid JSON: {error}"
            ))
        })?;
        let observed: CandidateContextAuthority = serde_json::from_value(
            role_input
                .get("repository_context_authority")
                .and_then(|authority| authority.get("candidate_authority"))
                .cloned()
                .ok_or_else(|| {
                    ContextExpansionError::Invalid(
                        "initial provider request has no candidate context authority".to_string(),
                    )
                })?,
        )?;
        if &observed != expected {
            return Err(ContextExpansionError::Invalid(
                "initial provider request candidate authority mismatch".to_string(),
            ));
        }
    }
    Ok(())
}

struct StreamedContextSource {
    source_bytes: usize,
    source_sha256: String,
    retained_prefix: Vec<u8>,
}

fn read_context_source(
    repository_root: &Path,
    repo_path: &str,
    retain_limit: usize,
) -> Result<StreamedContextSource, ContextExpansionError> {
    reject_repository_symlink_components(repository_root, repo_path)?;
    let source_path = repository_root.join(repo_path);
    let mut file = fs::OpenOptions::new()
        .read(true)
        .open(&source_path)
        .map_err(|error| {
            ContextExpansionError::Unavailable(format!(
                "requested context file {repo_path} is unavailable: {error}"
            ))
        })?;

    reject_repository_symlink_components(repository_root, repo_path)?;
    let canonical_source = source_path.canonicalize().map_err(|error| {
        ContextExpansionError::Unavailable(format!(
            "requested context file {repo_path} became unavailable: {error}"
        ))
    })?;
    if !canonical_source.starts_with(repository_root) || !canonical_source.is_file() {
        return Err(ContextExpansionError::Safety(format!(
            "requested context path is not a repository file: {repo_path}"
        )));
    }
    if !opened_file_matches_path_identity(&file, &source_path).map_err(|error| {
        ContextExpansionError::Unavailable(format!(
            "requested context file {repo_path} could not be revalidated: {error}"
        ))
    })? {
        return Err(ContextExpansionError::Safety(format!(
            "requested context file identity changed while opening: {repo_path}"
        )));
    }

    stream_context_source(&mut file, repo_path, retain_limit)
}

fn stream_context_source(
    file: &mut fs::File,
    repo_path: &str,
    retain_limit: usize,
) -> Result<StreamedContextSource, ContextExpansionError> {
    let mut hasher = Sha256::new();
    let mut retained_prefix = Vec::with_capacity(retain_limit.min(SOURCE_READ_BUFFER_BYTES));
    let mut utf8_carry = Vec::with_capacity(4);
    let mut source_bytes = 0usize;
    let mut buffer = [0u8; SOURCE_READ_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            ContextExpansionError::Unavailable(format!(
                "requested context file {repo_path} could not be read: {error}"
            ))
        })?;
        if read == 0 {
            break;
        }
        let bytes = &buffer[..read];
        source_bytes = source_bytes.checked_add(read).ok_or_else(|| {
            ContextExpansionError::Safety(format!(
                "requested context file is too large to count: {repo_path}"
            ))
        })?;
        hasher.update(bytes);
        let retain = retain_limit.saturating_sub(retained_prefix.len()).min(read);
        retained_prefix.extend_from_slice(&bytes[..retain]);

        utf8_carry.extend_from_slice(bytes);
        match std::str::from_utf8(&utf8_carry) {
            Ok(_) => utf8_carry.clear(),
            Err(error) if error.error_len().is_some() => {
                return Err(ContextExpansionError::Safety(format!(
                    "requested context file is not UTF-8 text: {repo_path}"
                )));
            }
            Err(error) => {
                let incomplete = utf8_carry.split_off(error.valid_up_to());
                utf8_carry = incomplete;
            }
        }
    }
    if !utf8_carry.is_empty() {
        return Err(ContextExpansionError::Safety(format!(
            "requested context file is not UTF-8 text: {repo_path}"
        )));
    }
    Ok(StreamedContextSource {
        source_bytes,
        source_sha256: hex::encode(hasher.finalize()),
        retained_prefix,
    })
}

fn opened_file_matches_path_identity(opened: &fs::File, path: &Path) -> std::io::Result<bool> {
    let opened_metadata = opened.metadata()?;
    let path_metadata = fs::metadata(path)?;
    Ok(metadata_identity_matches(&opened_metadata, &path_metadata))
}

#[cfg(unix)]
fn metadata_identity_matches(opened: &fs::Metadata, path: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    opened.dev() == path.dev() && opened.ino() == path.ino()
}

#[cfg(windows)]
fn metadata_identity_matches(opened: &fs::Metadata, path: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    match (
        (opened.volume_serial_number(), opened.file_index()),
        (path.volume_serial_number(), path.file_index()),
    ) {
        ((Some(opened_volume), Some(opened_index)), (Some(path_volume), Some(path_index))) => {
            opened_volume == path_volume && opened_index == path_index
        }
        _ => false,
    }
}

#[cfg(not(any(unix, windows)))]
fn metadata_identity_matches(_opened: &fs::Metadata, _path: &fs::Metadata) -> bool {
    false
}

fn reject_forbidden_path(
    path: &str,
    prepared: &PreparedRequest,
) -> Result<(), ContextExpansionError> {
    for (patterns, label) in [
        (&prepared.default_exclude_globs, "default exclude"),
        (&prepared.ticket_forbidden_files, "ticket forbidden file"),
        (&prepared.policy_forbidden_paths, "policy forbidden path"),
    ] {
        if let Some(pattern) = matching_pattern(path, patterns) {
            return Err(ContextExpansionError::Safety(format!(
                "requested context path {path} matches {label} {pattern}"
            )));
        }
    }
    Ok(())
}

fn validate_reference(reference: &ArtifactReference) -> Result<(), ContextExpansionError> {
    validate_relative_path(&reference.path)?;
    if !valid_digest(&reference.digest) {
        return Err(ContextExpansionError::Invalid(
            "artifact reference digest must be lowercase SHA-256".to_string(),
        ));
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<(), ContextExpansionError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ContextExpansionError::Safety(
            "artifact reference is not a safe relative path".to_string(),
        ));
    }
    Ok(())
}

fn expected_role(step: LoopStepName) -> Option<Role> {
    match step {
        LoopStepName::Research => Some(Role::Researcher),
        LoopStepName::Analysis => Some(Role::Analyzer),
        LoopStepName::SpecCreation => Some(Role::SpecWriter),
        LoopStepName::Development => Some(Role::Developer),
        LoopStepName::SpecReview
        | LoopStepName::OutputReview
        | LoopStepName::Testing
        | LoopStepName::EvalReport => None,
    }
}

fn expansion_artifact_path(step: LoopStepName, attempt: u32, round: u32) -> String {
    format!(
        "artifacts/{}.attempt-{attempt:03}.context-round-{round:03}.json",
        step_file_stem(step)
    )
}

fn initial_provider_request_paths(step: LoopStepName, attempt: u32) -> Vec<String> {
    let stem = step_file_stem(step);
    let legacy = if attempt == 1 {
        format!("prompts/{stem}.prompt.md")
    } else {
        format!("prompts/{stem}.attempt-{attempt:03}.prompt.md")
    };
    vec![
        legacy,
        format!("prompts/{stem}.attempt-{attempt:03}.exchange-001.initial.request.md"),
    ]
}

fn reject_repository_symlink_components(
    repository_root: &Path,
    repo_path: &str,
) -> Result<(), ContextExpansionError> {
    let mut current = repository_root.to_path_buf();
    for component in Path::new(repo_path).components() {
        let Component::Normal(component) = component else {
            return Err(ContextExpansionError::Safety(
                "requested repository path is not normalized".to_string(),
            ));
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            ContextExpansionError::Safety(format!(
                "requested context path could not be inspected: {error}"
            ))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ContextExpansionError::Safety(format!(
                "requested context path traverses a symlink: {repo_path}"
            )));
        }
    }
    Ok(())
}

fn normalized_set(
    paths: &[String],
    label: &str,
) -> Result<BTreeSet<String>, ContextExpansionError> {
    let mut normalized = BTreeSet::new();
    for path in paths {
        let path = normalize_repo_path(path)
            .ok_or_else(|| ContextExpansionError::Safety(format!("unsafe {label} path: {path}")))?;
        if !normalized.insert(path) {
            return Err(ContextExpansionError::Invalid(format!(
                "{label} paths contain a duplicate"
            )));
        }
    }
    Ok(normalized)
}

fn sort_dedup(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

fn strictly_sorted_unique(values: &[String]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn utf8_prefix_len(bytes: &[u8], max_bytes: usize) -> usize {
    let mut len = max_bytes.min(bytes.len());
    while len > 0 && std::str::from_utf8(&bytes[..len]).is_err() {
        len -= 1;
    }
    len
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::opened_file_matches_path_identity;

    #[test]
    fn opened_file_identity_rejects_a_replaced_current_path() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("source.txt");
        std::fs::write(&path, "original").expect("original");
        let opened = std::fs::File::open(&path).expect("open original");
        std::fs::rename(&path, temp.path().join("old-source.txt")).expect("rename original");
        std::fs::write(&path, "replacement").expect("replacement");

        assert!(!opened_file_matches_path_identity(&opened, &path).expect("identity check"));
    }
}
