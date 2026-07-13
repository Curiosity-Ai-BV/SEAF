use seaf_core::LoopRun;
use serde::Serialize;

use crate::{secret_redaction::SecretRedactor, LoopWorkspace};

const OPERATOR_EVIDENCE_ERROR: &str = "operator evidence contains prohibited credential material";

pub(crate) struct OperatorEvidenceGuard {
    redactor: SecretRedactor,
}

impl OperatorEvidenceGuard {
    pub(crate) fn load(workspace: &LoopWorkspace, run: &LoopRun) -> Result<Self, String> {
        let expected_digest = run
            .input_digests
            .eval_config
            .as_ref()
            .ok_or_else(|| OPERATOR_EVIDENCE_ERROR.to_string())?;
        let bytes = crate::immutable_artifact::read_verified_regular_file(
            workspace.run_directory(),
            "inputs/eval-config.json",
            "operator evidence eval config",
        )
        .map_err(|_| OPERATOR_EVIDENCE_ERROR.to_string())?;
        let config: seaf_core::EvalConfig =
            serde_json::from_slice(&bytes).map_err(|_| OPERATOR_EVIDENCE_ERROR.to_string())?;
        if seaf_core::canonical_json_bytes(&config)
            .map_err(|_| OPERATOR_EVIDENCE_ERROR.to_string())?
            != bytes
            || seaf_core::canonical_sha256_digest(&config)
                .map_err(|_| OPERATOR_EVIDENCE_ERROR.to_string())?
                != *expected_digest
        {
            return Err(OPERATOR_EVIDENCE_ERROR.to_string());
        }
        seaf_core::validate_eval_config(&config)
            .map_err(|_| OPERATOR_EVIDENCE_ERROR.to_string())?;
        let redactor = SecretRedactor::from_eval_config(&config)
            .map_err(|_| OPERATOR_EVIDENCE_ERROR.to_string())?;
        Ok(Self { redactor })
    }

    pub(crate) fn validate_structural(&self, value: &str) -> Result<(), String> {
        self.validate_exact_raw_bytes(value.as_bytes())
    }

    pub(crate) fn validate_exact_raw_bytes(&self, bytes: &[u8]) -> Result<(), String> {
        match self.redactor.contains_prohibited_bytes(bytes) {
            Ok(false) => Ok(()),
            Ok(true) | Err(_) => Err(OPERATOR_EVIDENCE_ERROR.to_string()),
        }
    }

    pub(crate) fn validate_canonical_artifact<T: Serialize>(
        &self,
        value: &T,
    ) -> Result<Vec<u8>, String> {
        let bytes = seaf_core::canonical_json_bytes(value)
            .map_err(|_| OPERATOR_EVIDENCE_ERROR.to_string())?;
        self.validate_exact_raw_bytes(&bytes)?;
        Ok(bytes)
    }

    pub(crate) fn validate_future_run(&self, run: &LoopRun) -> Result<Vec<u8>, String> {
        let bytes =
            crate::state::run_file_bytes(run).map_err(|_| OPERATOR_EVIDENCE_ERROR.to_string())?;
        self.validate_exact_raw_bytes(&bytes)?;
        Ok(bytes)
    }

    pub(crate) fn validate_current_run_file(
        &self,
        workspace: &LoopWorkspace,
    ) -> Result<(), String> {
        let bytes = crate::immutable_artifact::read_verified_regular_file(
            workspace.run_directory(),
            crate::workspace::RUN_FILE,
            "operator evidence run authority",
        )
        .map_err(|_| OPERATOR_EVIDENCE_ERROR.to_string())?;
        self.validate_exact_raw_bytes(&bytes)
    }

    pub(crate) fn sanitize_reason(&self, value: &str, max: usize) -> Result<String, String> {
        let sanitized = self
            .redactor
            .redact_string(value, max)
            .map_err(|_| OPERATOR_EVIDENCE_ERROR.to_string())?;
        self.validate_exact_raw_bytes(sanitized.as_bytes())?;
        Ok(sanitized)
    }

    pub(crate) fn validate_run(&self, run: &LoopRun) -> Result<(), String> {
        if let Some(approval) = run.human_approval.as_ref() {
            self.validate_structural(&approval.reviewer)?;
        }
        if let Some(promotion) = run.promotion.as_ref() {
            self.validate_structural(&promotion.reviewer)?;
        }
        Ok(())
    }

    pub(crate) fn validate_recovery_fields(&self, actor: &str, reason: &str) -> Result<(), String> {
        self.validate_structural(actor)?;
        self.validate_exact_raw_bytes(reason.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use seaf_core::LoopInputDigests;

    use super::*;

    fn guard_for(secret: &str) -> OperatorEvidenceGuard {
        let env = BTreeMap::from([("API_TOKEN".to_string(), secret.to_string())]);
        OperatorEvidenceGuard {
            redactor: SecretRedactor::from_env_maps([&env]).unwrap(),
        }
    }

    fn run() -> LoopRun {
        crate::state::create_run(crate::state::NewLoopRun {
            run_id: "run-fixed".to_string(),
            ticket_id: "ticket-fixed".to_string(),
            goal_id: "goal-fixed".to_string(),
            provider: "fake".to_string(),
            model: "fake".to_string(),
            input_digests: LoopInputDigests {
                ticket: "a".repeat(64),
                policy: "b".repeat(64),
                config: "c".repeat(64),
                repository: "d".repeat(64),
                eval_config: Some("e".repeat(64)),
            },
        })
    }

    #[test]
    fn canonical_artifact_rejects_secret_spanning_json_key_and_scalar() {
        let guard = guard_for("\"reason\": \"clean\"");

        let error = guard
            .validate_canonical_artifact(&serde_json::json!({"reason": "clean"}))
            .unwrap_err();

        assert_eq!(error, OPERATOR_EVIDENCE_ERROR);
    }

    #[test]
    fn future_run_rejects_secret_spanning_pretty_json_field_boundary() {
        let guard = guard_for("run-fixed\",\n  \"ticket_id\": \"ticket-fixed");

        let error = guard.validate_future_run(&run()).unwrap_err();

        assert_eq!(error, OPERATOR_EVIDENCE_ERROR);
    }

    #[test]
    fn future_run_rejects_secret_equal_to_fixed_status_scalar() {
        let guard = guard_for("pending");

        let error = guard.validate_future_run(&run()).unwrap_err();

        assert_eq!(error, OPERATOR_EVIDENCE_ERROR);
    }

    #[test]
    fn exact_raw_bytes_reject_secret_spanning_redaction_marker() {
        let guard = guard_for("prefix[REDACTED]suffix");

        let error = guard
            .validate_exact_raw_bytes(b"prefix[REDACTED]suffix")
            .unwrap_err();

        assert_eq!(error, OPERATOR_EVIDENCE_ERROR);
    }
}
