use std::fmt::Debug;

use seaf_core::{
    ArtifactReference, CheckStatus, EvalCheck, EvalDecision, EvalLoopEvidence, EvalReport,
    HumanApprovalEvidence, LoopExecutionMode, LoopInputDigests, LoopRun, LoopStatus, LoopStepName,
    LoopStepRecord, LoopStepStatus, PatchDecisionKind, Policy, PolicyDecision,
    PolicyDecisionReason, ProviderExchangeKind, ProviderExchangePhase,
    ProviderExchangeRecordReference, ProviderRole, RecoveryReference, RiskLevel, TicketAutonomy,
    TicketContext, TicketEval, TicketPriority, TicketSpec, TicketStatus,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

#[test]
fn ticket_mapping_matches_independent_current_and_legacy_documents() {
    let expected = expected_ticket();
    let decoded =
        assert_independent_json_mapping(ticket_current_json(), ticket_legacy_json(), &expected);
    assert_eq!(decoded.ticket_id, "ticket-wire-sentinel");
    assert_eq!(decoded.goal_id, "goal-wire-sentinel");
    assert_eq!(decoded.title, "Ticket title sentinel");
    assert_eq!(decoded.status, TicketStatus::Running);
    assert_eq!(decoded.priority, TicketPriority::P2);
    assert_eq!(decoded.problem, "Ticket problem sentinel");
    assert_eq!(decoded.research_questions, ["question-one", "question-two"]);
    assert_eq!(decoded.context.relevant_files, ["src/alpha.rs"]);
    assert_eq!(decoded.context.forbidden_files, ["secrets/beta.txt"]);
    assert_eq!(decoded.autonomy.level, 3);
    assert!(decoded.autonomy.apply_patch);
    assert_eq!(decoded.autonomy.allow_shell_commands, ["cargo test"]);
    assert_eq!(decoded.acceptance_criteria, ["criterion-one"]);
    assert_eq!(
        decoded.eval.as_ref().map(|eval| eval.config.as_str()),
        Some("eval/config-sentinel.yaml")
    );
}

#[test]
fn policy_mapping_matches_independent_current_and_legacy_documents() {
    let expected = expected_policy();
    let decoded =
        assert_independent_json_mapping(policy_current_json(), policy_legacy_json(), &expected);
    assert_eq!(decoded.policy_id, "policy-wire-sentinel");
    assert_eq!(decoded.default_autonomy_level, 4);
    assert_eq!(decoded.forbidden_paths, ["forbidden/alpha/**"]);
    assert_eq!(decoded.requires_human_review, ["review-category-sentinel"]);
    assert_eq!(
        decoded.allowed_without_review,
        ["allowed-category-sentinel"]
    );
}

#[test]
fn policy_decision_mapping_matches_independent_current_and_legacy_documents() {
    let expected = expected_policy_decision();
    let decoded = assert_independent_json_mapping(
        policy_decision_current_json(),
        policy_decision_legacy_json(),
        &expected,
    );
    assert_eq!(decoded.patch_id, "patch-wire-sentinel");
    assert_eq!(decoded.patch_sha256, format!("sha256:{}", "1".repeat(64)));
    assert_eq!(decoded.changed_paths, ["src/alpha.rs", "docs/beta.md"]);
    assert_eq!(decoded.decision, PatchDecisionKind::RequiresHumanReview);
    assert_eq!(decoded.reasons.len(), 2);
    assert_eq!(decoded.reasons[0].path.as_deref(), Some("src/alpha.rs"));
    assert_eq!(decoded.reasons[0].pattern.as_deref(), Some("src/**"));
    assert_eq!(
        decoded.reasons[0].details.as_deref(),
        Some("details-wire-sentinel")
    );
    assert_eq!(decoded.reasons[1].path, None);
    assert_eq!(decoded.reasons[1].pattern, None);
    assert_eq!(decoded.reasons[1].details, None);
    assert!(decoded.requires_human_review);
    assert!(decoded.apply_requested);
    assert!(!decoded.applied);
}

#[test]
fn eval_report_mapping_matches_independent_current_and_legacy_documents() {
    let expected = expected_eval_report();
    let decoded = assert_independent_json_mapping(
        eval_report_current_json(),
        eval_report_legacy_json(),
        &expected,
    );
    assert_eq!(decoded.eval_report_id, "eval-report-wire-sentinel");
    assert_eq!(decoded.patch_id, "eval-patch-wire-sentinel");
    assert_eq!(decoded.goal_id, "eval-goal-wire-sentinel");
    assert!(decoded.passed);
    assert_eq!(decoded.summary, "eval-summary-wire-sentinel");
    assert_eq!(decoded.checks.len(), 1);
    assert_eq!(decoded.checks[0].name, "check-name-wire-sentinel");
    assert_eq!(decoded.checks[0].status, CheckStatus::Passed);
    assert_eq!(decoded.checks[0].duration_ms, Some(321));
    assert_eq!(decoded.score_delta_estimate, Some(0.375));
    assert_eq!(decoded.risk_level, RiskLevel::Medium);
    assert_eq!(decoded.decision, EvalDecision::ApproveForHumanReview);
    assert_eq!(
        decoded
            .loop_evidence
            .as_ref()
            .map(|evidence| evidence.run_id.as_str()),
        Some("evidence-run-wire-sentinel")
    );
}

#[test]
fn loop_run_mapping_matches_independent_current_and_legacy_documents() {
    let expected = expected_loop_run();
    let decoded =
        assert_independent_json_mapping(loop_run_current_json(), loop_run_legacy_json(), &expected);
    assert_eq!(decoded.run_id, "loop-run-wire-sentinel");
    assert_eq!(decoded.ticket_id, "loop-ticket-wire-sentinel");
    assert_eq!(decoded.goal_id, "loop-goal-wire-sentinel");
    assert_eq!(decoded.provider, "loop-provider-wire-sentinel");
    assert_eq!(decoded.model, "loop-model-wire-sentinel");
    assert_eq!(decoded.execution_mode, LoopExecutionMode::IsolatedCandidate);
    assert_eq!(decoded.status, LoopStatus::Approved);
    assert_eq!(decoded.current_step, LoopStepName::Testing);
    assert_eq!(decoded.steps.len(), 1);
    assert_eq!(decoded.steps[0].name, LoopStepName::Research);
    assert_eq!(decoded.policy_decisions.len(), 1);
    assert_eq!(
        decoded.policy_decisions[0].patch_id,
        "loop-policy-patch-wire-sentinel"
    );
    assert_eq!(decoded.provider_exchange_records.len(), 1);
    assert_eq!(
        decoded.provider_exchange_records[0].path,
        "provider/record-wire-sentinel.json"
    );
    assert_eq!(
        decoded
            .human_approval
            .as_ref()
            .map(|approval| approval.reviewer.as_str()),
        Some("reviewer-wire-sentinel")
    );
    assert_eq!(
        decoded.eval_report_path.as_deref(),
        Some("eval/report-wire-sentinel.json")
    );
    assert_eq!(
        decoded
            .latest_recovery
            .as_ref()
            .map(|recovery| recovery.recovery_id),
        Some(17)
    );
    assert!(decoded.candidate_workspace.is_none());
    assert!(decoded.promotion.is_none());
}

#[test]
fn ticket_and_policy_yaml_versions_accept_legacy_and_v1_but_refuse_unsupported() {
    assert_yaml_versions(TICKET_YAML_BODY, &expected_ticket());
    assert_yaml_versions(POLICY_YAML_BODY, &expected_policy());
}

fn assert_independent_json_mapping<T>(current: Value, legacy: Value, expected: &T) -> T
where
    T: DeserializeOwned + Serialize + PartialEq + Debug,
{
    let decoded_current: T =
        serde_json::from_value(current.clone()).expect("independent current JSON reads");
    assert_eq!(
        &decoded_current, expected,
        "current JSON field mapping drifted"
    );

    let decoded_legacy: T = serde_json::from_value(legacy).expect("independent legacy JSON reads");
    assert_eq!(
        &decoded_legacy, expected,
        "legacy JSON field mapping drifted"
    );

    assert_eq!(
        serde_json::to_value(expected).expect("public value serializes"),
        current,
        "serialized fields must equal the independently authored current JSON"
    );

    decoded_current
}

fn assert_yaml_versions<T>(body: &str, expected: &T)
where
    T: DeserializeOwned + PartialEq + Debug,
{
    let legacy: T = serde_yaml::from_str(body).expect("missing legacy YAML version reads");
    assert_eq!(&legacy, expected);

    let explicit_current: T = serde_yaml::from_str(&format!("schema_version: 1\n{body}"))
        .expect("explicit current YAML version reads");
    assert_eq!(&explicit_current, expected);

    for unsupported in [0, 2] {
        let error = serde_yaml::from_str::<T>(&format!("schema_version: {unsupported}\n{body}"))
            .expect_err("unsupported YAML version must fail closed")
            .to_string();
        assert!(
            error.contains("unsupported")
                && error.contains("schema_version")
                && error.contains(&unsupported.to_string()),
            "YAML version refusal must identify the unsupported schema_version: {error}"
        );
    }
}

fn expected_ticket() -> TicketSpec {
    TicketSpec {
        ticket_id: "ticket-wire-sentinel".to_string(),
        goal_id: "goal-wire-sentinel".to_string(),
        title: "Ticket title sentinel".to_string(),
        status: TicketStatus::Running,
        priority: TicketPriority::P2,
        problem: "Ticket problem sentinel".to_string(),
        research_questions: vec!["question-one".to_string(), "question-two".to_string()],
        context: TicketContext {
            relevant_files: vec!["src/alpha.rs".to_string()],
            forbidden_files: vec!["secrets/beta.txt".to_string()],
        },
        autonomy: TicketAutonomy {
            level: 3,
            apply_patch: true,
            allow_shell_commands: vec!["cargo test".to_string()],
        },
        acceptance_criteria: vec!["criterion-one".to_string()],
        eval: Some(TicketEval {
            config: "eval/config-sentinel.yaml".to_string(),
        }),
    }
}

fn ticket_current_json() -> Value {
    json!({
        "schema_version": 1,
        "ticket_id": "ticket-wire-sentinel",
        "goal_id": "goal-wire-sentinel",
        "title": "Ticket title sentinel",
        "status": "running",
        "priority": "p2",
        "problem": "Ticket problem sentinel",
        "research_questions": ["question-one", "question-two"],
        "context": {
            "relevant_files": ["src/alpha.rs"],
            "forbidden_files": ["secrets/beta.txt"]
        },
        "autonomy": {
            "level": 3,
            "apply_patch": true,
            "allow_shell_commands": ["cargo test"]
        },
        "acceptance_criteria": ["criterion-one"],
        "eval": { "config": "eval/config-sentinel.yaml" }
    })
}

fn ticket_legacy_json() -> Value {
    json!({
        "ticket_id": "ticket-wire-sentinel",
        "goal_id": "goal-wire-sentinel",
        "title": "Ticket title sentinel",
        "status": "running",
        "priority": "p2",
        "problem": "Ticket problem sentinel",
        "research_questions": ["question-one", "question-two"],
        "context": {
            "relevant_files": ["src/alpha.rs"],
            "forbidden_files": ["secrets/beta.txt"]
        },
        "autonomy": {
            "level": 3,
            "apply_patch": true,
            "allow_shell_commands": ["cargo test"]
        },
        "acceptance_criteria": ["criterion-one"],
        "eval": { "config": "eval/config-sentinel.yaml" }
    })
}

fn expected_policy() -> Policy {
    Policy {
        policy_id: "policy-wire-sentinel".to_string(),
        default_autonomy_level: 4,
        forbidden_paths: vec!["forbidden/alpha/**".to_string()],
        requires_human_review: vec!["review-category-sentinel".to_string()],
        allowed_without_review: vec!["allowed-category-sentinel".to_string()],
    }
}

fn policy_current_json() -> Value {
    json!({
        "schema_version": 1,
        "policy_id": "policy-wire-sentinel",
        "default_autonomy_level": 4,
        "forbidden_paths": ["forbidden/alpha/**"],
        "requires_human_review": ["review-category-sentinel"],
        "allowed_without_review": ["allowed-category-sentinel"]
    })
}

fn policy_legacy_json() -> Value {
    json!({
        "policy_id": "policy-wire-sentinel",
        "default_autonomy_level": 4,
        "forbidden_paths": ["forbidden/alpha/**"],
        "requires_human_review": ["review-category-sentinel"],
        "allowed_without_review": ["allowed-category-sentinel"]
    })
}

fn expected_policy_decision() -> PolicyDecision {
    PolicyDecision {
        patch_id: "patch-wire-sentinel".to_string(),
        patch_sha256: format!("sha256:{}", "1".repeat(64)),
        changed_paths: vec!["src/alpha.rs".to_string(), "docs/beta.md".to_string()],
        decision: PatchDecisionKind::RequiresHumanReview,
        reasons: vec![
            PolicyDecisionReason {
                code: "reason-code-one".to_string(),
                message: "reason-message-one".to_string(),
                path: Some("src/alpha.rs".to_string()),
                pattern: Some("src/**".to_string()),
                details: Some("details-wire-sentinel".to_string()),
            },
            PolicyDecisionReason {
                code: "reason-code-two".to_string(),
                message: "reason-message-two".to_string(),
                path: None,
                pattern: None,
                details: None,
            },
        ],
        requires_human_review: true,
        apply_requested: true,
        applied: false,
    }
}

fn policy_decision_current_json() -> Value {
    json!({
        "schema_version": 1,
        "patch_id": "patch-wire-sentinel",
        "patch_sha256": format!("sha256:{}", "1".repeat(64)),
        "changed_paths": ["src/alpha.rs", "docs/beta.md"],
        "decision": "requires_human_review",
        "reasons": [
            {
                "code": "reason-code-one",
                "message": "reason-message-one",
                "path": "src/alpha.rs",
                "pattern": "src/**",
                "details": "details-wire-sentinel"
            },
            {
                "code": "reason-code-two",
                "message": "reason-message-two"
            }
        ],
        "requires_human_review": true,
        "apply_requested": true,
        "applied": false
    })
}

fn policy_decision_legacy_json() -> Value {
    json!({
        "patch_id": "patch-wire-sentinel",
        "patch_sha256": format!("sha256:{}", "1".repeat(64)),
        "changed_paths": ["src/alpha.rs", "docs/beta.md"],
        "decision": "requires_human_review",
        "reasons": [
            {
                "code": "reason-code-one",
                "message": "reason-message-one",
                "path": "src/alpha.rs",
                "pattern": "src/**",
                "details": "details-wire-sentinel"
            },
            {
                "code": "reason-code-two",
                "message": "reason-message-two"
            }
        ],
        "requires_human_review": true,
        "apply_requested": true,
        "applied": false
    })
}

fn expected_eval_report() -> EvalReport {
    EvalReport {
        eval_report_id: "eval-report-wire-sentinel".to_string(),
        patch_id: "eval-patch-wire-sentinel".to_string(),
        goal_id: "eval-goal-wire-sentinel".to_string(),
        passed: true,
        summary: "eval-summary-wire-sentinel".to_string(),
        checks: vec![EvalCheck {
            name: "check-name-wire-sentinel".to_string(),
            status: CheckStatus::Passed,
            duration_ms: Some(321),
            stdout_path: Some("logs/stdout-wire-sentinel.log".to_string()),
            stdout_digest: Some(format!("sha256:{}", "2".repeat(64))),
            stderr_path: Some("logs/stderr-wire-sentinel.log".to_string()),
            stderr_digest: Some(format!("sha256:{}", "3".repeat(64))),
            summary: Some("check-summary-wire-sentinel".to_string()),
        }],
        score_delta_estimate: Some(0.375),
        risk_level: RiskLevel::Medium,
        decision: EvalDecision::ApproveForHumanReview,
        loop_evidence: Some(EvalLoopEvidence {
            schema_version: 9,
            run_id: "evidence-run-wire-sentinel".to_string(),
            ticket_id: "evidence-ticket-wire-sentinel".to_string(),
            ticket_digest: format!("sha256:{}", "4".repeat(64)),
            eval_config: artifact("evidence/eval-config.json", '5'),
            candidate_diff: artifact("evidence/candidate.diff", '6'),
            starting_head: "starting-head-wire-sentinel".to_string(),
            human_approval_digest: format!("sha256:{}", "7".repeat(64)),
            policy_decision_digest: format!("sha256:{}", "8".repeat(64)),
            testing_evidence: artifact("evidence/testing.json", '9'),
        }),
    }
}

fn eval_report_current_json() -> Value {
    json!({
        "schema_version": 1,
        "eval_report_id": "eval-report-wire-sentinel",
        "patch_id": "eval-patch-wire-sentinel",
        "goal_id": "eval-goal-wire-sentinel",
        "passed": true,
        "summary": "eval-summary-wire-sentinel",
        "checks": [{
            "name": "check-name-wire-sentinel",
            "status": "passed",
            "duration_ms": 321,
            "stdout_path": "logs/stdout-wire-sentinel.log",
            "stdout_digest": format!("sha256:{}", "2".repeat(64)),
            "stderr_path": "logs/stderr-wire-sentinel.log",
            "stderr_digest": format!("sha256:{}", "3".repeat(64)),
            "summary": "check-summary-wire-sentinel"
        }],
        "score_delta_estimate": 0.375,
        "risk_level": "medium",
        "decision": "approve_for_human_review",
        "loop_evidence": {
            "schema_version": 9,
            "run_id": "evidence-run-wire-sentinel",
            "ticket_id": "evidence-ticket-wire-sentinel",
            "ticket_digest": format!("sha256:{}", "4".repeat(64)),
            "eval_config": artifact_json("evidence/eval-config.json", '5'),
            "candidate_diff": artifact_json("evidence/candidate.diff", '6'),
            "starting_head": "starting-head-wire-sentinel",
            "human_approval_digest": format!("sha256:{}", "7".repeat(64)),
            "policy_decision_digest": format!("sha256:{}", "8".repeat(64)),
            "testing_evidence": artifact_json("evidence/testing.json", '9')
        }
    })
}

fn eval_report_legacy_json() -> Value {
    json!({
        "eval_report_id": "eval-report-wire-sentinel",
        "patch_id": "eval-patch-wire-sentinel",
        "goal_id": "eval-goal-wire-sentinel",
        "passed": true,
        "summary": "eval-summary-wire-sentinel",
        "checks": [{
            "name": "check-name-wire-sentinel",
            "status": "passed",
            "duration_ms": 321,
            "stdout_path": "logs/stdout-wire-sentinel.log",
            "stdout_digest": format!("sha256:{}", "2".repeat(64)),
            "stderr_path": "logs/stderr-wire-sentinel.log",
            "stderr_digest": format!("sha256:{}", "3".repeat(64)),
            "summary": "check-summary-wire-sentinel"
        }],
        "score_delta_estimate": 0.375,
        "risk_level": "medium",
        "decision": "approve_for_human_review",
        "loop_evidence": {
            "schema_version": 9,
            "run_id": "evidence-run-wire-sentinel",
            "ticket_id": "evidence-ticket-wire-sentinel",
            "ticket_digest": format!("sha256:{}", "4".repeat(64)),
            "eval_config": artifact_json("evidence/eval-config.json", '5'),
            "candidate_diff": artifact_json("evidence/candidate.diff", '6'),
            "starting_head": "starting-head-wire-sentinel",
            "human_approval_digest": format!("sha256:{}", "7".repeat(64)),
            "policy_decision_digest": format!("sha256:{}", "8".repeat(64)),
            "testing_evidence": artifact_json("evidence/testing.json", '9')
        }
    })
}

fn expected_loop_run() -> LoopRun {
    let request = provider_reference(
        "approval-request-run-sentinel",
        LoopStepName::OutputReview,
        ProviderRole::OutputReviewer,
        5,
        7,
        ProviderExchangeKind::ContextRetry,
        Some(11),
        ProviderExchangePhase::Request,
        "provider/approval-request.json",
        'a',
    );
    let response = provider_reference(
        "approval-response-run-sentinel",
        LoopStepName::SpecReview,
        ProviderRole::SpecReviewer,
        6,
        8,
        ProviderExchangeKind::JsonRepair,
        Some(12),
        ProviderExchangePhase::Response,
        "provider/approval-response.json",
        'b',
    );

    LoopRun {
        run_id: "loop-run-wire-sentinel".to_string(),
        ticket_id: "loop-ticket-wire-sentinel".to_string(),
        goal_id: "loop-goal-wire-sentinel".to_string(),
        provider: "loop-provider-wire-sentinel".to_string(),
        model: "loop-model-wire-sentinel".to_string(),
        input_digests: LoopInputDigests {
            ticket: "c".repeat(64),
            policy: "d".repeat(64),
            config: "e".repeat(64),
            repository: "f".repeat(64),
            eval_config: Some("0".repeat(64)),
        },
        execution_mode: LoopExecutionMode::IsolatedCandidate,
        status: LoopStatus::Approved,
        current_step: LoopStepName::Testing,
        started_at: "2026-07-15T01:02:03Z".to_string(),
        updated_at: "2026-07-15T04:05:06Z".to_string(),
        steps: vec![LoopStepRecord {
            name: LoopStepName::Research,
            status: LoopStepStatus::Completed,
            artifact_path: Some("artifacts/research-wire-sentinel.json".to_string()),
            artifact_digest: Some("1".repeat(64)),
        }],
        policy_decisions: vec![PolicyDecision {
            patch_id: "loop-policy-patch-wire-sentinel".to_string(),
            patch_sha256: format!("sha256:{}", "2".repeat(64)),
            changed_paths: vec!["loop/changed-wire-sentinel.rs".to_string()],
            decision: PatchDecisionKind::Allowed,
            reasons: vec![PolicyDecisionReason {
                code: "loop-reason-code".to_string(),
                message: "loop-reason-message".to_string(),
                path: Some("loop/changed-wire-sentinel.rs".to_string()),
                pattern: None,
                details: Some("loop-reason-details".to_string()),
            }],
            requires_human_review: false,
            apply_requested: true,
            applied: true,
        }],
        provider_exchange_records: vec![provider_reference(
            "provider-record-run-sentinel",
            LoopStepName::Development,
            ProviderRole::Developer,
            3,
            4,
            ProviderExchangeKind::Initial,
            Some(10),
            ProviderExchangePhase::Response,
            "provider/record-wire-sentinel.json",
            '3',
        )],
        candidate_workspace: None,
        human_approval: Some(HumanApprovalEvidence {
            schema_version: 7,
            run_id: "approval-run-wire-sentinel".to_string(),
            reviewer: "reviewer-wire-sentinel".to_string(),
            approved_at: "2026-07-15T03:04:05Z".to_string(),
            candidate_diff: artifact("approval/candidate.diff", '4'),
            starting_head: "approval-starting-head-sentinel".to_string(),
            policy_decision_digest: "5".repeat(64),
            output_review: artifact("approval/output-review.json", '6'),
            output_review_request: request,
            output_review_response: response,
        }),
        eval_report_path: Some("eval/report-wire-sentinel.json".to_string()),
        promotion: None,
        latest_recovery: Some(RecoveryReference {
            recovery_id: 17,
            artifact: artifact("recovery/reference-wire-sentinel.json", '7'),
        }),
    }
}

fn loop_run_current_json() -> Value {
    json!({
        "schema_version": 1,
        "run_id": "loop-run-wire-sentinel",
        "ticket_id": "loop-ticket-wire-sentinel",
        "goal_id": "loop-goal-wire-sentinel",
        "provider": "loop-provider-wire-sentinel",
        "model": "loop-model-wire-sentinel",
        "input_digests": {
            "ticket": "c".repeat(64),
            "policy": "d".repeat(64),
            "config": "e".repeat(64),
            "repository": "f".repeat(64),
            "eval_config": "0".repeat(64)
        },
        "execution_mode": "isolated_candidate",
        "status": "approved",
        "current_step": "testing",
        "started_at": "2026-07-15T01:02:03Z",
        "updated_at": "2026-07-15T04:05:06Z",
        "steps": [{
            "name": "research",
            "status": "completed",
            "artifact_path": "artifacts/research-wire-sentinel.json",
            "artifact_digest": "1".repeat(64)
        }],
        "policy_decisions": [{
            "schema_version": 1,
            "patch_id": "loop-policy-patch-wire-sentinel",
            "patch_sha256": format!("sha256:{}", "2".repeat(64)),
            "changed_paths": ["loop/changed-wire-sentinel.rs"],
            "decision": "allowed",
            "reasons": [{
                "code": "loop-reason-code",
                "message": "loop-reason-message",
                "path": "loop/changed-wire-sentinel.rs",
                "details": "loop-reason-details"
            }],
            "requires_human_review": false,
            "apply_requested": true,
            "applied": true
        }],
        "provider_exchange_records": [{
            "run_id": "provider-record-run-sentinel",
            "step": "development",
            "role": "developer",
            "step_attempt": 3,
            "exchange_index": 4,
            "kind": "initial",
            "context_round": 10,
            "phase": "response",
            "path": "provider/record-wire-sentinel.json",
            "digest": "3".repeat(64)
        }],
        "human_approval": {
            "schema_version": 7,
            "run_id": "approval-run-wire-sentinel",
            "reviewer": "reviewer-wire-sentinel",
            "approved_at": "2026-07-15T03:04:05Z",
            "candidate_diff": artifact_json("approval/candidate.diff", '4'),
            "starting_head": "approval-starting-head-sentinel",
            "policy_decision_digest": "5".repeat(64),
            "output_review": artifact_json("approval/output-review.json", '6'),
            "output_review_request": provider_reference_json(
                "approval-request-run-sentinel",
                "output_review",
                "output_reviewer",
                5,
                7,
                "context_retry",
                11,
                "request",
                "provider/approval-request.json",
                'a'
            ),
            "output_review_response": provider_reference_json(
                "approval-response-run-sentinel",
                "spec_review",
                "spec_reviewer",
                6,
                8,
                "json_repair",
                12,
                "response",
                "provider/approval-response.json",
                'b'
            )
        },
        "eval_report_path": "eval/report-wire-sentinel.json",
        "latest_recovery": {
            "recovery_id": 17,
            "artifact": artifact_json("recovery/reference-wire-sentinel.json", '7')
        }
    })
}

fn loop_run_legacy_json() -> Value {
    json!({
        "run_id": "loop-run-wire-sentinel",
        "ticket_id": "loop-ticket-wire-sentinel",
        "goal_id": "loop-goal-wire-sentinel",
        "provider": "loop-provider-wire-sentinel",
        "model": "loop-model-wire-sentinel",
        "input_digests": {
            "ticket": "c".repeat(64),
            "policy": "d".repeat(64),
            "config": "e".repeat(64),
            "repository": "f".repeat(64),
            "eval_config": "0".repeat(64)
        },
        "execution_mode": "isolated_candidate",
        "status": "approved",
        "current_step": "testing",
        "started_at": "2026-07-15T01:02:03Z",
        "updated_at": "2026-07-15T04:05:06Z",
        "steps": [{
            "name": "research",
            "status": "completed",
            "artifact_path": "artifacts/research-wire-sentinel.json",
            "artifact_digest": "1".repeat(64)
        }],
        "policy_decisions": [{
            "patch_id": "loop-policy-patch-wire-sentinel",
            "patch_sha256": format!("sha256:{}", "2".repeat(64)),
            "changed_paths": ["loop/changed-wire-sentinel.rs"],
            "decision": "allowed",
            "reasons": [{
                "code": "loop-reason-code",
                "message": "loop-reason-message",
                "path": "loop/changed-wire-sentinel.rs",
                "details": "loop-reason-details"
            }],
            "requires_human_review": false,
            "apply_requested": true,
            "applied": true
        }],
        "provider_exchange_records": [{
            "run_id": "provider-record-run-sentinel",
            "step": "development",
            "role": "developer",
            "step_attempt": 3,
            "exchange_index": 4,
            "kind": "initial",
            "context_round": 10,
            "phase": "response",
            "path": "provider/record-wire-sentinel.json",
            "digest": "3".repeat(64)
        }],
        "human_approval": {
            "schema_version": 7,
            "run_id": "approval-run-wire-sentinel",
            "reviewer": "reviewer-wire-sentinel",
            "approved_at": "2026-07-15T03:04:05Z",
            "candidate_diff": artifact_json("approval/candidate.diff", '4'),
            "starting_head": "approval-starting-head-sentinel",
            "policy_decision_digest": "5".repeat(64),
            "output_review": artifact_json("approval/output-review.json", '6'),
            "output_review_request": provider_reference_json(
                "approval-request-run-sentinel",
                "output_review",
                "output_reviewer",
                5,
                7,
                "context_retry",
                11,
                "request",
                "provider/approval-request.json",
                'a'
            ),
            "output_review_response": provider_reference_json(
                "approval-response-run-sentinel",
                "spec_review",
                "spec_reviewer",
                6,
                8,
                "json_repair",
                12,
                "response",
                "provider/approval-response.json",
                'b'
            )
        },
        "eval_report_path": "eval/report-wire-sentinel.json",
        "latest_recovery": {
            "recovery_id": 17,
            "artifact": artifact_json("recovery/reference-wire-sentinel.json", '7')
        }
    })
}

fn artifact(path: &str, digest_char: char) -> ArtifactReference {
    ArtifactReference {
        path: path.to_string(),
        digest: digest_char.to_string().repeat(64),
    }
}

fn artifact_json(path: &str, digest_char: char) -> Value {
    json!({
        "path": path,
        "digest": digest_char.to_string().repeat(64)
    })
}

#[allow(clippy::too_many_arguments)]
fn provider_reference(
    run_id: &str,
    step: LoopStepName,
    role: ProviderRole,
    step_attempt: u32,
    exchange_index: u32,
    kind: ProviderExchangeKind,
    context_round: Option<u32>,
    phase: ProviderExchangePhase,
    path: &str,
    digest_char: char,
) -> ProviderExchangeRecordReference {
    ProviderExchangeRecordReference {
        run_id: run_id.to_string(),
        step,
        role,
        step_attempt,
        exchange_index,
        kind,
        context_round,
        phase,
        path: path.to_string(),
        digest: digest_char.to_string().repeat(64),
    }
}

#[allow(clippy::too_many_arguments)]
fn provider_reference_json(
    run_id: &str,
    step: &str,
    role: &str,
    step_attempt: u32,
    exchange_index: u32,
    kind: &str,
    context_round: u32,
    phase: &str,
    path: &str,
    digest_char: char,
) -> Value {
    json!({
        "run_id": run_id,
        "step": step,
        "role": role,
        "step_attempt": step_attempt,
        "exchange_index": exchange_index,
        "kind": kind,
        "context_round": context_round,
        "phase": phase,
        "path": path,
        "digest": digest_char.to_string().repeat(64)
    })
}

const TICKET_YAML_BODY: &str = r#"ticket_id: ticket-wire-sentinel
goal_id: goal-wire-sentinel
title: Ticket title sentinel
status: running
priority: p2
problem: Ticket problem sentinel
research_questions:
  - question-one
  - question-two
context:
  relevant_files:
    - src/alpha.rs
  forbidden_files:
    - secrets/beta.txt
autonomy:
  level: 3
  apply_patch: true
  allow_shell_commands:
    - cargo test
acceptance_criteria:
  - criterion-one
eval:
  config: eval/config-sentinel.yaml
"#;

const POLICY_YAML_BODY: &str = r#"policy_id: policy-wire-sentinel
default_autonomy_level: 4
forbidden_paths:
  - forbidden/alpha/**
requires_human_review:
  - review-category-sentinel
allowed_without_review:
  - allowed-category-sentinel
"#;
