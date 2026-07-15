use std::collections::BTreeSet;

use seaf_core::{
    EvalDecision, EvalReport, LoopExecutionMode, LoopInputDigests, LoopRun, LoopStatus,
    LoopStepName, PatchDecisionKind, Policy, PolicyDecision, PolicyDecisionReason, RiskLevel,
    TicketAutonomy, TicketContext, TicketPriority, TicketSpec, TicketStatus,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

#[test]
fn ticket_rust_contract_and_schema_fields_cannot_drift() {
    let ticket = TicketSpec {
        ticket_id: "T-1".to_string(),
        goal_id: "goal-1".to_string(),
        title: "Typed durable contracts".to_string(),
        status: TicketStatus::Ready,
        priority: TicketPriority::P1,
        problem: "Durable artifacts need one contract owner.".to_string(),
        research_questions: vec![],
        context: TicketContext {
            relevant_files: vec!["crates/seaf-core/src/models.rs".to_string()],
            forbidden_files: vec![],
        },
        autonomy: TicketAutonomy {
            level: 1,
            apply_patch: false,
            allow_shell_commands: vec![],
        },
        acceptance_criteria: vec!["Rust and schema stay aligned.".to_string()],
        eval: None,
    };

    assert_contract_schema_shape(
        &ticket,
        include_str!("../../../specs/ticket.schema.json"),
        &["eval"],
        &["eval", "research_questions"],
    );
    assert_current_legacy_and_future_version_behavior(&ticket);
}

#[test]
fn policy_rust_contract_and_schema_fields_cannot_drift() {
    let policy = Policy {
        policy_id: "policy-1".to_string(),
        default_autonomy_level: 1,
        forbidden_paths: vec!["secrets/**".to_string()],
        requires_human_review: vec!["policy_changes".to_string()],
        allowed_without_review: vec!["documentation".to_string()],
    };

    assert_contract_schema_shape(
        &policy,
        include_str!("../../../specs/policy.schema.json"),
        &[],
        &[],
    );
    assert_current_legacy_and_future_version_behavior(&policy);
}

#[test]
fn loop_run_rust_contract_and_schema_fields_cannot_drift() {
    let run = minimal_loop_run();

    let schema_text = include_str!("../../../specs/loop-run.schema.json");
    assert_contract_schema_shape(
        &run,
        schema_text,
        &[
            "candidate_workspace",
            "human_approval",
            "eval_report_path",
            "promotion",
            "latest_recovery",
        ],
        &[
            "execution_mode",
            "provider_exchange_records",
            "candidate_workspace",
            "human_approval",
            "eval_report_path",
            "promotion",
            "latest_recovery",
        ],
    );
    assert_current_legacy_and_future_version_behavior(&run);

    let schema: Value = serde_json::from_str(schema_text).expect("loop-run schema is JSON");
    assert_eq!(
        schema["properties"]["policy_decisions"]["items"]["$ref"],
        json!("policy-decision.schema.json"),
        "LoopRun policy decisions must use the shared PolicyDecision schema"
    );
}

#[test]
fn loop_run_v1_adds_only_schema_version_to_the_legacy_serialized_shape() {
    let run = minimal_loop_run();
    let current = serde_json::to_value(&run).expect("LoopRun serializes");
    assert_eq!(
        current,
        json!({
            "schema_version": 1,
            "run_id": "run-1",
            "ticket_id": "T-1",
            "goal_id": "goal-1",
            "provider": "fake",
            "model": "fake-local",
            "input_digests": {
                "ticket": "a".repeat(64),
                "policy": "b".repeat(64),
                "config": "c".repeat(64),
                "repository": "d".repeat(64)
            },
            "execution_mode": "legacy_proposal_only",
            "status": "pending",
            "current_step": "research",
            "started_at": "2026-07-15T00:00:00Z",
            "updated_at": "2026-07-15T00:00:00Z",
            "steps": [],
            "policy_decisions": [],
            "provider_exchange_records": []
        }),
        "v1 must preserve the exact legacy LoopRun shape apart from schema_version"
    );

    let mut legacy = current;
    legacy
        .as_object_mut()
        .expect("LoopRun serializes as an object")
        .remove("schema_version");
    assert_eq!(
        serde_json::from_value::<LoopRun>(legacy).expect("legacy LoopRun reads"),
        run,
        "the preserved legacy shape must still deserialize"
    );
}

fn minimal_loop_run() -> LoopRun {
    LoopRun {
        run_id: "run-1".to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "goal-1".to_string(),
        provider: "fake".to_string(),
        model: "fake-local".to_string(),
        input_digests: LoopInputDigests {
            ticket: "a".repeat(64),
            policy: "b".repeat(64),
            config: "c".repeat(64),
            repository: "d".repeat(64),
            eval_config: None,
        },
        execution_mode: LoopExecutionMode::LegacyProposalOnly,
        status: LoopStatus::Pending,
        current_step: LoopStepName::Research,
        started_at: "2026-07-15T00:00:00Z".to_string(),
        updated_at: "2026-07-15T00:00:00Z".to_string(),
        steps: vec![],
        policy_decisions: vec![],
        provider_exchange_records: vec![],
        candidate_workspace: None,
        human_approval: None,
        eval_report_path: None,
        promotion: None,
        latest_recovery: None,
    }
}

#[test]
fn policy_decision_rust_contract_and_schema_fields_cannot_drift() {
    let decision = PolicyDecision {
        patch_id: "run-1".to_string(),
        patch_sha256: format!("sha256:{}", "a".repeat(64)),
        changed_paths: vec!["docs/agent-loop.md".to_string()],
        decision: PatchDecisionKind::Allowed,
        reasons: vec![],
        requires_human_review: false,
        apply_requested: false,
        applied: false,
    };

    assert_contract_schema_shape(
        &decision,
        include_str!("../../../specs/policy-decision.schema.json"),
        &[],
        &[],
    );
    assert_current_legacy_and_future_version_behavior(&decision);
}

#[test]
fn policy_decision_nested_reason_and_enum_schema_cannot_drift() {
    let schema: Value =
        serde_json::from_str(include_str!("../../../specs/policy-decision.schema.json"))
            .expect("policy-decision schema is JSON");
    let reason_schema = &schema["properties"]["reasons"]["items"];
    let populated = PolicyDecisionReason {
        code: "policy_match".to_string(),
        message: "Policy requires review.".to_string(),
        path: Some("src/lib.rs".to_string()),
        pattern: Some("src/**".to_string()),
        details: Some("Matched the configured path rule.".to_string()),
    };
    assert_object_schema_shape(
        &populated,
        reason_schema,
        &[],
        &["path", "pattern", "details"],
    );

    let omitted = PolicyDecisionReason {
        path: None,
        pattern: None,
        details: None,
        ..populated.clone()
    };
    assert_eq!(
        serde_json::to_value(&omitted).expect("reason serializes"),
        json!({
            "code": "policy_match",
            "message": "Policy requires review."
        }),
        "canonical Rust serialization must omit absent optional reason fields"
    );

    let explicit_null: PolicyDecisionReason = serde_json::from_value(json!({
        "code": "policy_match",
        "message": "Policy requires review.",
        "path": null,
        "pattern": null,
        "details": null
    }))
    .expect("Rust contract accepts explicit null for optional reason fields");
    assert_eq!(explicit_null, omitted);
    assert_eq!(
        serde_json::to_value(explicit_null).expect("reason serializes canonically"),
        serde_json::to_value(omitted).expect("omitted reason serializes"),
        "explicit null must normalize to omitted canonical output"
    );

    for field in ["path", "pattern", "details"] {
        let variants = reason_schema["properties"][field]["anyOf"]
            .as_array()
            .expect("optional reason schema fields must allow string or null")
            .iter()
            .map(|variant| {
                variant["type"]
                    .as_str()
                    .expect("optional reason variant type")
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            variants,
            BTreeSet::from(["null", "string"]),
            "{field} must match Rust Option<String> explicit-null behavior"
        );
    }

    assert!(
        serde_json::from_value::<PolicyDecisionReason>(json!({
            "code": "policy_match",
            "message": "Policy requires review.",
            "unexpected": true
        }))
        .is_err(),
        "Rust nested reason contract must remain closed"
    );

    let rust_variants = [
        PatchDecisionKind::Allowed,
        PatchDecisionKind::RequiresHumanReview,
        PatchDecisionKind::Rejected,
    ]
    .into_iter()
    .map(|variant| {
        serde_json::to_value(variant)
            .expect("decision kind serializes")
            .as_str()
            .expect("decision kind serializes as a string")
            .to_string()
    })
    .collect::<BTreeSet<_>>();
    let schema_variants = schema["properties"]["decision"]["enum"]
        .as_array()
        .expect("decision schema enum")
        .iter()
        .map(|variant| {
            variant
                .as_str()
                .expect("decision schema variant")
                .to_string()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        rust_variants, schema_variants,
        "PatchDecisionKind Rust and schema variants drifted"
    );
}

#[test]
fn eval_report_rust_contract_and_schema_fields_cannot_drift() {
    let report = EvalReport {
        eval_report_id: "eval-1".to_string(),
        patch_id: "run-1".to_string(),
        goal_id: "goal-1".to_string(),
        passed: false,
        summary: "Not evaluated.".to_string(),
        checks: vec![],
        score_delta_estimate: None,
        risk_level: RiskLevel::High,
        decision: EvalDecision::Reject,
        loop_evidence: None,
    };

    assert_contract_schema_shape(
        &report,
        include_str!("../../../specs/eval-report.schema.json"),
        &["score_delta_estimate", "loop_evidence"],
        &["score_delta_estimate", "loop_evidence"],
    );
    assert_current_legacy_and_future_version_behavior(&report);
}

fn assert_current_legacy_and_future_version_behavior<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let current = serde_json::to_value(value).expect("durable contract serializes");
    assert_eq!(
        current["schema_version"],
        json!(1),
        "new durable artifacts must explicitly serialize the current version"
    );
    assert_eq!(
        serde_json::from_value::<T>(current.clone()).expect("current v1 artifact reads"),
        *value,
    );

    let mut legacy = current.clone();
    legacy
        .as_object_mut()
        .expect("durable contract is an object")
        .remove("schema_version");
    assert_eq!(
        serde_json::from_value::<T>(legacy).expect("legacy unversioned v0 artifact reads"),
        *value,
    );

    for unsupported in [0, 2] {
        let mut future = current.clone();
        future
            .as_object_mut()
            .expect("durable contract is an object")
            .insert("schema_version".to_string(), json!(unsupported));
        let error = serde_json::from_value::<T>(future)
            .expect_err("explicit unsupported versions must fail closed")
            .to_string();
        assert!(
            error.contains("unsupported") && error.contains("schema_version"),
            "version refusal must be actionable: {error}"
        );
    }

    let mut unknown = current;
    unknown
        .as_object_mut()
        .expect("durable contract is an object")
        .insert("unexpected".to_string(), json!(true));
    assert!(
        serde_json::from_value::<T>(unknown).is_err(),
        "durable contracts must remain closed to unknown fields"
    );
}

fn assert_contract_schema_shape<T: Serialize>(
    value: &T,
    schema_text: &str,
    omitted_serialized_fields: &[&str],
    optional_or_default_fields: &[&str],
) {
    let schema: Value = serde_json::from_str(schema_text).expect("contract schema is JSON");
    assert_eq!(
        schema["properties"]["schema_version"]["const"],
        json!(1),
        "new durable artifact schemas must require exactly version 1"
    );
    assert_object_schema_shape(
        value,
        &schema,
        omitted_serialized_fields,
        optional_or_default_fields,
    );
}

fn assert_object_schema_shape<T: Serialize>(
    value: &T,
    schema: &Value,
    omitted_serialized_fields: &[&str],
    optional_or_default_fields: &[&str],
) {
    let serialized = serde_json::to_value(value).expect("Rust contract must serialize");
    let mut rust_fields = serialized
        .as_object()
        .expect("durable contract must serialize as an object")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    rust_fields.extend(omitted_serialized_fields.iter().copied());

    let schema_fields = schema["properties"]
        .as_object()
        .expect("contract schema must declare properties")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(rust_fields, schema_fields, "Rust and schema fields drifted");

    let optional = optional_or_default_fields
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let expected_required = schema_fields
        .difference(&optional)
        .copied()
        .collect::<BTreeSet<_>>();
    let schema_required = schema["required"]
        .as_array()
        .expect("contract schema must declare required fields")
        .iter()
        .map(|field| field.as_str().expect("required field must be a string"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        expected_required, schema_required,
        "Rust required/default fields and schema drifted"
    );
    assert_eq!(
        schema["additionalProperties"],
        json!(false),
        "durable contract schemas must remain closed"
    );
}
