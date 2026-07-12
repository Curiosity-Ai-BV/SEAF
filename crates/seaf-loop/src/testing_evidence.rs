use std::{collections::BTreeSet, error::Error, fmt};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, is_portable_artifact_path, ArtifactReference,
    CheckStatus, EvalCheck, LoopRun, LoopStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workspace::LoopWorkspace;

pub const TESTING_EVIDENCE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestingEvidence {
    pub schema_version: u32,
    pub run_id: String,
    pub ticket_id: String,
    pub goal_id: String,
    pub approved_run_digest: String,
    pub ticket_digest: String,
    pub eval_config: ArtifactReference,
    pub candidate_diff: ArtifactReference,
    pub starting_head: String,
    pub human_approval_digest: String,
    pub policy_decision_digest: String,
    pub started_at: String,
    pub completed_at: String,
    pub checks: Vec<EvalCheck>,
    pub passed: bool,
}

impl TestingEvidence {
    pub fn create(
        approved_run: &LoopRun,
        started_at: impl Into<String>,
        completed_at: impl Into<String>,
        checks: Vec<EvalCheck>,
    ) -> Result<Self, TestingEvidenceError> {
        if approved_run.status != LoopStatus::Approved {
            return Err(TestingEvidenceError::invalid(
                "Testing evidence can be created only from Approved authority",
            ));
        }
        let approval = approved_run.human_approval.as_ref().ok_or_else(|| {
            TestingEvidenceError::invalid("Approved authority has no human approval evidence")
        })?;
        let eval_config_digest =
            approved_run
                .input_digests
                .eval_config
                .as_ref()
                .ok_or_else(|| {
                    TestingEvidenceError::invalid("Approved authority has no eval config digest")
                })?;
        let evidence = Self {
            schema_version: TESTING_EVIDENCE_SCHEMA_VERSION,
            run_id: approved_run.run_id.clone(),
            ticket_id: approved_run.ticket_id.clone(),
            goal_id: approved_run.goal_id.clone(),
            approved_run_digest: canonical_sha256_digest(approved_run)
                .map_err(|error| TestingEvidenceError::invalid(error.to_string()))?,
            ticket_digest: approved_run.input_digests.ticket.clone(),
            eval_config: ArtifactReference {
                path: "inputs/eval-config.json".to_string(),
                digest: eval_config_digest.clone(),
            },
            candidate_diff: approval.candidate_diff.clone(),
            starting_head: approval.starting_head.clone(),
            human_approval_digest: canonical_sha256_digest(approval)
                .map_err(|error| TestingEvidenceError::invalid(error.to_string()))?,
            policy_decision_digest: approval.policy_decision_digest.clone(),
            started_at: started_at.into(),
            completed_at: completed_at.into(),
            passed: checks
                .iter()
                .all(|check| check.status == CheckStatus::Passed),
            checks,
        };
        evidence.validate_against_approved_run(approved_run)?;
        Ok(evidence)
    }

    pub fn validate_against_approved_run(
        &self,
        approved_run: &LoopRun,
    ) -> Result<(), TestingEvidenceError> {
        self.validate()?;
        let run_errors = seaf_core::validate_loop_run(approved_run);
        if !run_errors.is_empty() || approved_run.status != LoopStatus::Approved {
            return Err(TestingEvidenceError::invalid(
                "Testing evidence requires exact valid Approved authority",
            ));
        }
        let approval = approved_run.human_approval.as_ref().ok_or_else(|| {
            TestingEvidenceError::invalid("Approved authority has no human approval evidence")
        })?;
        let approved_at = parse_canonical_unix_seconds(&approval.approved_at).ok_or_else(|| {
            TestingEvidenceError::invalid(
                "human_approval.approved_at must be canonical decimal Unix seconds within u64",
            )
        })?;
        let started_at = parse_canonical_unix_seconds(&self.started_at).ok_or_else(|| {
            TestingEvidenceError::invalid(
                "started_at must be canonical decimal Unix seconds within u64",
            )
        })?;
        if started_at < approved_at {
            return Err(TestingEvidenceError::invalid(
                "started_at must not precede human_approval.approved_at",
            ));
        }
        let eval_config_digest =
            approved_run
                .input_digests
                .eval_config
                .as_ref()
                .ok_or_else(|| {
                    TestingEvidenceError::invalid("Approved authority has no eval config digest")
                })?;
        let approved_run_digest = canonical_sha256_digest(approved_run)
            .map_err(|error| TestingEvidenceError::invalid(error.to_string()))?;
        let human_approval_digest = canonical_sha256_digest(approval)
            .map_err(|error| TestingEvidenceError::invalid(error.to_string()))?;
        if self.run_id != approved_run.run_id
            || self.ticket_id != approved_run.ticket_id
            || self.goal_id != approved_run.goal_id
            || self.approved_run_digest != approved_run_digest
            || self.ticket_digest != approved_run.input_digests.ticket
            || self.eval_config.digest != *eval_config_digest
            || self.candidate_diff != approval.candidate_diff
            || self.starting_head != approval.starting_head
            || self.human_approval_digest != human_approval_digest
            || self.policy_decision_digest != approval.policy_decision_digest
        {
            return Err(TestingEvidenceError::invalid(
                "Testing evidence bindings do not match exact Approved authority",
            ));
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), TestingEvidenceError> {
        let mut errors = Vec::new();
        if self.schema_version != TESTING_EVIDENCE_SCHEMA_VERSION {
            errors.push("schema_version must be 1".to_string());
        }
        for (field, value) in [
            ("run_id", self.run_id.as_str()),
            ("ticket_id", self.ticket_id.as_str()),
            ("goal_id", self.goal_id.as_str()),
        ] {
            if value.trim().is_empty() {
                errors.push(format!("{field} must not be empty"));
            }
        }
        for (field, digest) in [
            ("approved_run_digest", self.approved_run_digest.as_str()),
            ("ticket_digest", self.ticket_digest.as_str()),
            ("human_approval_digest", self.human_approval_digest.as_str()),
            (
                "policy_decision_digest",
                self.policy_decision_digest.as_str(),
            ),
        ] {
            validate_digest(&mut errors, field, digest);
        }
        validate_reference(&mut errors, "eval_config", &self.eval_config);
        if self.eval_config.path != "inputs/eval-config.json" {
            errors.push("eval_config.path must select inputs/eval-config.json".to_string());
        }
        validate_reference(&mut errors, "candidate_diff", &self.candidate_diff);
        if !valid_git_object_id(&self.starting_head) {
            errors.push("starting_head must be a lowercase Git object ID".to_string());
        }
        validate_timestamp(&mut errors, "started_at", &self.started_at);
        validate_timestamp(&mut errors, "completed_at", &self.completed_at);
        if timestamp_precedes(&self.completed_at, &self.started_at) {
            errors.push("completed_at must not precede started_at".to_string());
        }
        if self.checks.is_empty() {
            errors.push("checks must include at least one executed check".to_string());
        }
        let mut names = BTreeSet::new();
        let mut log_paths = BTreeSet::new();
        for (index, check) in self.checks.iter().enumerate() {
            if check.name.trim().is_empty() {
                errors.push(format!("checks[{index}].name must not be empty"));
            } else if !names.insert(check.name.as_str()) {
                errors.push(format!("checks[{index}].name is duplicated"));
            }
            validate_check_log(
                &mut errors,
                index,
                "stdout",
                &check.stdout_path,
                &check.stdout_digest,
            );
            validate_check_log(
                &mut errors,
                index,
                "stderr",
                &check.stderr_path,
                &check.stderr_digest,
            );
            for (stream, path) in [
                ("stdout", check.stdout_path.as_deref()),
                ("stderr", check.stderr_path.as_deref()),
            ] {
                if let Some(path) = path {
                    if !log_paths.insert(path) {
                        errors.push(format!(
                            "checks[{index}].{stream}_path is a duplicated log path"
                        ));
                    }
                }
            }
        }
        let aggregate = self
            .checks
            .iter()
            .all(|check| check.status == CheckStatus::Passed);
        if self.passed != aggregate {
            errors.push("passed aggregate does not match the ordered check results".to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(TestingEvidenceError::invalid(errors.join("; ")))
        }
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, TestingEvidenceError> {
        self.validate()?;
        canonical_json_bytes(self).map_err(|error| TestingEvidenceError::invalid(error.to_string()))
    }

    pub fn artifact_digest(&self) -> Result<String, TestingEvidenceError> {
        self.validate()?;
        canonical_sha256_digest(self)
            .map_err(|error| TestingEvidenceError::invalid(error.to_string()))
    }

    pub fn load(
        workspace: &LoopWorkspace,
        reference: &ArtifactReference,
        expected_run_id: &str,
    ) -> Result<Self, TestingEvidenceError> {
        if !is_portable_artifact_path(&reference.path) {
            return Err(TestingEvidenceError::invalid(
                "Testing evidence reference path is not strict portable relative spelling",
            ));
        }
        let mut reference_errors = Vec::new();
        validate_digest(
            &mut reference_errors,
            "Testing evidence reference",
            &reference.digest,
        );
        if !reference_errors.is_empty() {
            return Err(TestingEvidenceError::invalid(reference_errors.join("; ")));
        }
        let bytes = crate::immutable_artifact::read_verified_regular_file(
            workspace.run_directory(),
            &reference.path,
            "Testing evidence",
        )
        .map_err(|error| TestingEvidenceError::invalid(error.to_string()))?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            TestingEvidenceError::invalid(format!("invalid Testing evidence JSON: {error}"))
        })?;
        if canonical_json_bytes(&value)
            .map_err(|error| TestingEvidenceError::invalid(error.to_string()))?
            != bytes
        {
            return Err(TestingEvidenceError::invalid(
                "Testing evidence is not canonical JSON",
            ));
        }
        if canonical_sha256_digest(&value)
            .map_err(|error| TestingEvidenceError::invalid(error.to_string()))?
            != reference.digest
        {
            return Err(TestingEvidenceError::invalid(
                "Testing evidence artifact digest mismatch",
            ));
        }
        let evidence: Self = serde_json::from_value(value).map_err(|error| {
            TestingEvidenceError::invalid(format!("invalid Testing evidence schema: {error}"))
        })?;
        evidence.validate()?;
        if evidence.run_id != expected_run_id {
            return Err(TestingEvidenceError::invalid(format!(
                "Testing evidence run_id mismatch: expected {expected_run_id}, got {}",
                evidence.run_id
            )));
        }
        Ok(evidence)
    }

    pub fn load_for_approved_run(
        workspace: &LoopWorkspace,
        reference: &ArtifactReference,
        approved_run: &LoopRun,
    ) -> Result<Self, TestingEvidenceError> {
        let evidence = Self::load(workspace, reference, &approved_run.run_id)?;
        evidence.validate_against_approved_run(approved_run)?;
        Ok(evidence)
    }
}

fn validate_check_log(
    errors: &mut Vec<String>,
    index: usize,
    stream: &str,
    path: &Option<String>,
    digest: &Option<String>,
) {
    match (path.as_deref(), digest.as_deref()) {
        (Some(path), Some(digest)) => {
            if !is_portable_artifact_path(path) {
                errors.push(format!(
                    "checks[{index}].{stream}_path is not a safe relative path"
                ));
            }
            validate_digest(errors, &format!("checks[{index}].{stream}_digest"), digest);
        }
        _ => errors.push(format!(
            "checks[{index}].{stream} log path and digest must both be present"
        )),
    }
}

fn validate_reference(errors: &mut Vec<String>, field: &str, reference: &ArtifactReference) {
    if !is_portable_artifact_path(&reference.path) {
        errors.push(format!("{field}.path is not a safe relative path"));
    }
    validate_digest(errors, &format!("{field}.digest"), &reference.digest);
}

fn validate_digest(errors: &mut Vec<String>, field: &str, digest: &str) {
    if digest.len() != 64
        || !digest
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        errors.push(format!("{field} must be a lowercase SHA-256 digest"));
    }
}

fn validate_timestamp(errors: &mut Vec<String>, field: &str, timestamp: &str) {
    if parse_canonical_unix_seconds(timestamp).is_none() {
        errors.push(format!(
            "{field} must be canonical decimal Unix seconds within u64"
        ));
    }
}

fn timestamp_precedes(completed: &str, started: &str) -> bool {
    match (
        parse_canonical_unix_seconds(completed),
        parse_canonical_unix_seconds(started),
    ) {
        (Some(completed), Some(started)) => completed < started,
        _ => false,
    }
}

fn parse_canonical_unix_seconds(value: &str) -> Option<u64> {
    let parsed = value.parse::<u64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn valid_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestingEvidenceError {
    message: String,
}

impl TestingEvidenceError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TestingEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for TestingEvidenceError {}
