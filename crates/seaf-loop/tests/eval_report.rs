use seaf_core::{
    CheckStatus, EvalCheck, EvalDecision, LoopStepName, LoopStepStatus, RiskLevel, TicketAutonomy,
    TicketContext, TicketPriority, TicketSpec, TicketStatus,
};
use seaf_loop::{build_loop_eval_report, state, PatchDecisionKind, PolicyDecision};

#[test]
fn eval_report_uses_loop_identity_and_includes_loop_checks() {
    let ticket = ticket();
    let mut run = passing_run(&ticket);
    run.policy_decisions
        .push(serialized_policy_decision(policy_decision(
            PatchDecisionKind::Allowed,
        )));
    let command_checks = vec![passed_check("local_loop_smoke")];

    let report = build_loop_eval_report(&run, &ticket, command_checks);

    assert!(report.passed);
    assert_eq!(report.patch_id, run.run_id);
    assert_eq!(report.goal_id, ticket.goal_id);
    assert_eq!(report.decision, EvalDecision::ApproveForHumanReview);
    assert_eq!(report.risk_level, RiskLevel::Low);

    let names = check_names(&report.checks);
    for expected in [
        "schema_validation",
        "patch_policy_gate",
        "spec_review",
        "output_review",
        "local_loop_smoke",
    ] {
        assert!(
            names.contains(&expected),
            "report should include {expected:?}; got {names:?}"
        );
    }
}

#[test]
fn eval_report_rejects_failed_patch_policy_gate() {
    let ticket = ticket();
    let mut run = passing_run(&ticket);
    run.policy_decisions
        .push(serialized_policy_decision(policy_decision(
            PatchDecisionKind::Rejected,
        )));

    let report = build_loop_eval_report(&run, &ticket, vec![passed_check("local_loop_smoke")]);

    assert!(!report.passed);
    assert_eq!(report.decision, EvalDecision::Reject);
    assert_eq!(report.risk_level, RiskLevel::High);
    let policy_check = report
        .checks
        .iter()
        .find(|check| check.name == "patch_policy_gate")
        .expect("patch policy gate check");
    assert_eq!(policy_check.status, CheckStatus::Failed);
    assert!(
        policy_check
            .summary
            .as_deref()
            .unwrap_or_default()
            .contains("rejected"),
        "summary should explain rejected policy decision: {policy_check:?}"
    );
}

#[test]
fn eval_report_rejects_policy_gate_for_different_patch_id() {
    let ticket = ticket();
    let mut run = passing_run(&ticket);
    let mut decision = policy_decision(PatchDecisionKind::Allowed);
    decision.patch_id = "other_patch".to_string();
    run.policy_decisions
        .push(serialized_policy_decision(decision));

    let report = build_loop_eval_report(&run, &ticket, vec![passed_check("local_loop_smoke")]);

    assert!(!report.passed);
    assert_eq!(report.decision, EvalDecision::Reject);
    let policy_check = report
        .checks
        .iter()
        .find(|check| check.name == "patch_policy_gate")
        .expect("patch policy gate check");
    assert_eq!(policy_check.status, CheckStatus::Failed);
    assert!(
        policy_check
            .summary
            .as_deref()
            .unwrap_or_default()
            .contains("patch_id"),
        "summary should explain mismatched patch_id: {policy_check:?}"
    );
}

fn passing_run(ticket: &TicketSpec) -> seaf_core::LoopRun {
    let mut run = state::create_run(state::NewLoopRun {
        run_id: "loop_20260701_001".to_string(),
        ticket_id: ticket.ticket_id.clone(),
        goal_id: ticket.goal_id.clone(),
        provider: "fake".to_string(),
        model: "fake-model".to_string(),
        input_digests: seaf_core::LoopInputDigests {
            ticket: "a".repeat(64),
            policy: "b".repeat(64),
            config: "c".repeat(64),
            repository: "d".repeat(64),
            eval_config: None,
        },
    });
    for step in &mut run.steps {
        step.status = LoopStepStatus::Passed;
    }
    run.current_step = LoopStepName::EvalReport;
    run
}

fn serialized_policy_decision(
    decision: PolicyDecision,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    serde_json::from_value(serde_json::to_value(decision).unwrap()).unwrap()
}

fn policy_decision(decision: PatchDecisionKind) -> PolicyDecision {
    PolicyDecision {
        patch_id: "loop_20260701_001".to_string(),
        patch_sha256: "sha256:abc123".to_string(),
        changed_paths: vec!["crates/seaf-cli/src/main.rs".to_string()],
        decision,
        reasons: Vec::new(),
        requires_human_review: false,
        apply_requested: false,
        applied: false,
    }
}

fn passed_check(name: &str) -> EvalCheck {
    EvalCheck {
        name: name.to_string(),
        status: CheckStatus::Passed,
        duration_ms: None,
        stdout_path: None,
        stderr_path: None,
        summary: Some("deterministic command check passed".to_string()),
    }
}

fn check_names(checks: &[EvalCheck]) -> Vec<&str> {
    checks.iter().map(|check| check.name.as_str()).collect()
}

fn ticket() -> TicketSpec {
    TicketSpec {
        ticket_id: "T-LOCAL-001".to_string(),
        goal_id: "local_agent_loop_mvp".to_string(),
        title: "Add a health check command to the CLI".to_string(),
        status: TicketStatus::Ready,
        priority: TicketPriority::P1,
        problem: "Developers need deterministic local-loop eval coverage.".to_string(),
        research_questions: Vec::new(),
        context: TicketContext {
            relevant_files: vec!["crates/seaf-cli/src/main.rs".to_string()],
            forbidden_files: Vec::new(),
        },
        autonomy: TicketAutonomy {
            level: 1,
            apply_patch: true,
            allow_shell_commands: Vec::new(),
        },
        acceptance_criteria: vec!["EvalReport represents loop outcome.".to_string()],
        eval: None,
    }
}
