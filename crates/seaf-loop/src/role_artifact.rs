use std::{error::Error, fmt, fs, path::Component};

use seaf_core::{canonical_json_bytes, canonical_sha256_digest, LoopStepName};
use serde::Serialize;
use serde_json::Value;

use crate::{parse_role_response, workspace::LoopWorkspace, Role, RoleResponse};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidatedRoleArtifact {
    pub run_id: String,
    pub step: LoopStepName,
    pub role: Role,
    pub response: RoleResponse,
    pub response_digest: String,
}

impl ValidatedRoleArtifact {
    pub fn new(
        run_id: impl Into<String>,
        step: LoopStepName,
        role: Role,
        response: RoleResponse,
    ) -> Result<Self, RoleArtifactError> {
        let response_digest = canonical_sha256_digest(&response)?;
        Ok(Self {
            run_id: run_id.into(),
            step,
            role,
            response,
            response_digest,
        })
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, RoleArtifactError> {
        Ok(canonical_json_bytes(self)?)
    }

    pub fn artifact_digest(&self) -> Result<String, RoleArtifactError> {
        Ok(canonical_sha256_digest(self)?)
    }

    pub fn load(
        workspace: &LoopWorkspace,
        artifact_path: &str,
        expected_digest: &str,
        expected_run_id: &str,
        expected_step: LoopStepName,
        expected_role: Role,
    ) -> Result<Self, RoleArtifactError> {
        let relative = std::path::Path::new(artifact_path);
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(RoleArtifactError::Invalid(format!(
                "artifact path is not a safe relative path: {artifact_path}"
            )));
        }
        let path = workspace.run_directory().join(relative);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            RoleArtifactError::Invalid(format!(
                "required {expected_step:?} artifact {artifact_path} could not be inspected: {error}"
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RoleArtifactError::Invalid(format!(
                "required {expected_step:?} artifact is not a real regular file: {artifact_path}"
            )));
        }
        let canonical_run = workspace.run_directory().canonicalize()?;
        let canonical_path = path.canonicalize()?;
        if !canonical_path.starts_with(&canonical_run) {
            return Err(RoleArtifactError::Invalid(format!(
                "required {expected_step:?} artifact resolves outside the loop workspace: {artifact_path}"
            )));
        }
        let bytes = fs::read(&canonical_path)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            RoleArtifactError::Invalid(format!("invalid role artifact JSON: {error}"))
        })?;
        if canonical_json_bytes(&value)? != bytes {
            return Err(RoleArtifactError::Invalid(format!(
                "{expected_step:?} role artifact is not canonical JSON"
            )));
        }
        let actual_artifact_digest = canonical_sha256_digest(&value)?;
        if actual_artifact_digest != expected_digest {
            return Err(RoleArtifactError::Invalid(format!(
                "{expected_step:?} role artifact digest mismatch"
            )));
        }
        let object = value.as_object().ok_or_else(|| {
            RoleArtifactError::Invalid("role artifact must be a JSON object".to_string())
        })?;
        let run_id = string_field(object, "run_id")?;
        let step: LoopStepName = value_field(object, "step")?;
        let role: Role = value_field(object, "role")?;
        let response_value = object.get("response").cloned().ok_or_else(|| {
            RoleArtifactError::Invalid("role artifact is missing response".to_string())
        })?;
        let response_digest = string_field(object, "response_digest")?;
        if object.len() != 5 {
            return Err(RoleArtifactError::Invalid(
                "role artifact contains unknown fields".to_string(),
            ));
        }
        if run_id != expected_run_id {
            return Err(RoleArtifactError::Invalid(format!(
                "role artifact run_id mismatch: expected {expected_run_id}, got {run_id}"
            )));
        }
        if step != expected_step {
            return Err(RoleArtifactError::Invalid(format!(
                "role artifact step mismatch: expected {expected_step:?}, got {step:?}"
            )));
        }
        if role != expected_role {
            return Err(RoleArtifactError::Invalid(format!(
                "role artifact role mismatch: expected {}, got {}",
                expected_role.as_str(),
                role.as_str()
            )));
        }
        let actual_response_digest = canonical_sha256_digest(&response_value)?;
        if response_digest != actual_response_digest {
            return Err(RoleArtifactError::Invalid(format!(
                "{expected_step:?} response digest mismatch"
            )));
        }
        let response_json = serde_json::to_string(&response_value)?;
        let response = parse_role_response(expected_role, &response_json).map_err(|error| {
            RoleArtifactError::Invalid(format!(
                "{expected_step:?} artifact response failed role validation: {error}"
            ))
        })?;
        Ok(Self {
            run_id,
            step,
            role,
            response,
            response_digest,
        })
    }
}

fn string_field(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<String, RoleArtifactError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RoleArtifactError::Invalid(format!("role artifact has invalid {field}")))
}

fn value_field<T: serde::de::DeserializeOwned>(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<T, RoleArtifactError> {
    serde_json::from_value(
        object.get(field).cloned().ok_or_else(|| {
            RoleArtifactError::Invalid(format!("role artifact is missing {field}"))
        })?,
    )
    .map_err(|error| RoleArtifactError::Invalid(format!("invalid role artifact {field}: {error}")))
}

#[derive(Debug)]
pub enum RoleArtifactError {
    Invalid(String),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for RoleArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::Io(error) => write!(formatter, "role artifact I/O error: {error}"),
            Self::Json(error) => write!(formatter, "role artifact JSON error: {error}"),
        }
    }
}

impl Error for RoleArtifactError {}

impl From<std::io::Error> for RoleArtifactError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for RoleArtifactError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
