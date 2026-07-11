use std::{fs, path::Component};

use seaf_core::{canonical_json_bytes, canonical_sha256_digest, LoopStepName};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    parse_unified_diff, patch_digest, workspace::LoopWorkspace, DeveloperResponse, DeveloperStatus,
    PatchDecisionKind, PatchParseError, PolicyDecision, Role, RoleArtifactError,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentEvidence {
    pub run_id: String,
    pub step: LoopStepName,
    pub role: Role,
    pub developer_response: DeveloperResponse,
    pub developer_response_digest: String,
    pub patch: String,
    pub patch_digest: String,
    pub changed_paths: Vec<String>,
    pub policy_decision: PolicyDecision,
}

impl DevelopmentEvidence {
    pub fn new(
        run_id: impl Into<String>,
        developer_response: DeveloperResponse,
        patch: impl Into<String>,
        policy_decision: PolicyDecision,
    ) -> Result<Self, RoleArtifactError> {
        let run_id = run_id.into();
        let patch = patch.into();
        let evidence = Self {
            developer_response_digest: canonical_sha256_digest(&developer_response)?,
            patch_digest: patch_digest(&patch),
            changed_paths: policy_decision.changed_paths.clone(),
            run_id,
            step: LoopStepName::Development,
            role: Role::Developer,
            developer_response,
            patch,
            policy_decision,
        };
        evidence.validate_internal_consistency()?;
        Ok(evidence)
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
                "required Development evidence {artifact_path} could not be inspected: {error}"
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RoleArtifactError::Invalid(
                "required Development evidence is not a real regular file".to_string(),
            ));
        }
        let canonical_run = workspace.run_directory().canonicalize()?;
        let canonical_path = path.canonicalize()?;
        if !canonical_path.starts_with(&canonical_run) {
            return Err(RoleArtifactError::Invalid(
                "required Development evidence resolves outside the loop workspace".to_string(),
            ));
        }
        let bytes = fs::read(canonical_path)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            RoleArtifactError::Invalid(format!("invalid Development evidence JSON: {error}"))
        })?;
        if canonical_json_bytes(&value)? != bytes {
            return Err(RoleArtifactError::Invalid(
                "Development evidence is not canonical JSON".to_string(),
            ));
        }
        if canonical_sha256_digest(&value)? != expected_digest {
            return Err(RoleArtifactError::Invalid(
                "Development evidence artifact digest mismatch".to_string(),
            ));
        }
        let evidence: Self = serde_json::from_value(value).map_err(|error| {
            RoleArtifactError::Invalid(format!("invalid Development evidence schema: {error}"))
        })?;
        if evidence.run_id != expected_run_id {
            return Err(RoleArtifactError::Invalid(format!(
                "Development evidence run_id mismatch: expected {expected_run_id}, got {}",
                evidence.run_id
            )));
        }
        evidence.validate_internal_consistency()?;
        Ok(evidence)
    }

    fn validate_internal_consistency(&self) -> Result<(), RoleArtifactError> {
        if self.step != LoopStepName::Development {
            return Err(RoleArtifactError::Invalid(
                "Development evidence step mismatch".to_string(),
            ));
        }
        if self.role != Role::Developer || self.developer_response.role != Role::Developer {
            return Err(RoleArtifactError::Invalid(
                "Development evidence role mismatch".to_string(),
            ));
        }
        if self.developer_response.status != DeveloperStatus::PatchProposed {
            return Err(RoleArtifactError::Invalid(
                "Development evidence requires a patch_proposed response".to_string(),
            ));
        }
        if self.developer_response.patch.as_deref() != Some(self.patch.as_str()) {
            return Err(RoleArtifactError::Invalid(
                "Development evidence patch does not match the validated developer response"
                    .to_string(),
            ));
        }
        if canonical_sha256_digest(&self.developer_response)? != self.developer_response_digest {
            return Err(RoleArtifactError::Invalid(
                "Development evidence developer response digest mismatch".to_string(),
            ));
        }
        if patch_digest(&self.patch) != self.patch_digest {
            return Err(RoleArtifactError::Invalid(
                "Development evidence patch digest mismatch".to_string(),
            ));
        }
        if self.policy_decision.patch_id != self.run_id
            || self.policy_decision.patch_sha256 != self.patch_digest
            || self.policy_decision.changed_paths != self.changed_paths
        {
            return Err(RoleArtifactError::Invalid(
                "Development evidence policy decision does not match its run and patch".to_string(),
            ));
        }
        self.validate_parsed_patch_alignment()?;
        Ok(())
    }

    fn validate_parsed_patch_alignment(&self) -> Result<(), RoleArtifactError> {
        match parse_unified_diff(&self.patch) {
            Ok(parsed) => {
                if parsed.changed_paths != self.changed_paths
                    || parsed.changed_paths != self.policy_decision.changed_paths
                {
                    return Err(RoleArtifactError::Invalid(
                        "Development evidence changed paths do not match the exact parsed patch"
                            .to_string(),
                    ));
                }
                let records_binary_rejection = self
                    .policy_decision
                    .reasons
                    .iter()
                    .any(|reason| reason.code == "binary_patch");
                if parsed.contains_binary_patch
                    && (self.policy_decision.decision != PatchDecisionKind::Rejected
                        || !records_binary_rejection)
                {
                    return Err(RoleArtifactError::Invalid(
                        "binary Development patch is missing its exact rejection evidence"
                            .to_string(),
                    ));
                }
                if !parsed.contains_binary_patch && records_binary_rejection {
                    return Err(RoleArtifactError::Invalid(
                        "non-binary Development patch contains substituted binary rejection evidence"
                            .to_string(),
                    ));
                }
            }
            Err(error) => {
                let expected_reason = match error {
                    PatchParseError::UnsafePath(_) => "invalid_patch_path",
                    PatchParseError::EmptyPatch
                    | PatchParseError::MalformedGitHeader(_)
                    | PatchParseError::MissingPath(_) => "invalid_patch",
                };
                if !self.changed_paths.is_empty()
                    || !self.policy_decision.changed_paths.is_empty()
                    || self.policy_decision.decision != PatchDecisionKind::Rejected
                    || !self
                        .policy_decision
                        .reasons
                        .iter()
                        .any(|reason| reason.code == expected_reason)
                {
                    return Err(RoleArtifactError::Invalid(format!(
                        "malformed Development patch is missing its exact {expected_reason} rejection evidence"
                    )));
                }
            }
        }
        Ok(())
    }
}
