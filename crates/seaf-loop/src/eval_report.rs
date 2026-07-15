use crate::PatchDecisionKind;
use seaf_core::{
    validate_loop_run, validate_ticket_spec, CheckStatus, EvalCheck, EvalDecision, EvalReport,
    LoopRun, LoopStepName, LoopStepStatus, RiskLevel, TicketSpec,
};

pub fn build_loop_eval_report(
    run: &LoopRun,
    ticket: &TicketSpec,
    command_checks: Vec<EvalCheck>,
) -> EvalReport {
    let mut checks = vec![
        schema_validation_check(run, ticket),
        patch_policy_gate_check(run),
        loop_step_check(run, LoopStepName::SpecReview, "spec_review"),
        loop_step_check(run, LoopStepName::OutputReview, "output_review"),
    ];
    checks.extend(command_checks);

    let passed = checks
        .iter()
        .all(|check| check.status == CheckStatus::Passed);

    EvalReport {
        eval_report_id: format!("eval_{}", sanitize_id(&run.run_id)),
        patch_id: run.run_id.clone(),
        goal_id: ticket.goal_id.clone(),
        passed,
        summary: if passed {
            "Loop run and required eval checks passed.".to_string()
        } else {
            "Loop run or required eval checks failed.".to_string()
        },
        checks,
        score_delta_estimate: None,
        risk_level: if passed {
            RiskLevel::Low
        } else {
            RiskLevel::High
        },
        decision: if passed {
            EvalDecision::ApproveForHumanReview
        } else {
            EvalDecision::Reject
        },
        loop_evidence: None,
    }
}

fn schema_validation_check(run: &LoopRun, ticket: &TicketSpec) -> EvalCheck {
    let mut failures = Vec::new();
    failures.extend(
        validate_loop_run(run)
            .into_iter()
            .map(|error| format!("loop_run.{}: {}", error.field, error.message)),
    );
    failures.extend(
        validate_ticket_spec(ticket)
            .into_iter()
            .map(|error| format!("ticket.{}: {}", error.field, error.message)),
    );
    if run.goal_id != ticket.goal_id {
        failures.push(format!(
            "run.goal_id must match ticket.goal_id ({} != {})",
            run.goal_id, ticket.goal_id
        ));
    }

    check(
        "schema_validation",
        failures.is_empty(),
        if failures.is_empty() {
            "Loop run and ticket schemas are valid.".to_string()
        } else {
            failures.join("; ")
        },
    )
}

fn patch_policy_gate_check(run: &LoopRun) -> EvalCheck {
    if run.policy_decisions.is_empty() {
        return check(
            "patch_policy_gate",
            false,
            "No patch policy gate decision was recorded; refusing to approve without gate evidence.",
        );
    }

    let mut rejected = Vec::new();
    let mut mismatched = Vec::new();
    for (index, decision) in run.policy_decisions.iter().enumerate() {
        match decision {
            decision if decision.patch_id != run.run_id => mismatched.push(format!(
                "policy_decisions[{index}].patch_id {} does not match run_id {}",
                decision.patch_id, run.run_id
            )),
            decision if decision.decision == PatchDecisionKind::Rejected => {
                rejected.push(format!("policy_decisions[{index}] rejected patch"))
            }
            _ => {}
        }
    }

    if !mismatched.is_empty() {
        return check("patch_policy_gate", false, mismatched.join("; "));
    }
    if !rejected.is_empty() {
        return check("patch_policy_gate", false, rejected.join("; "));
    }

    check(
        "patch_policy_gate",
        true,
        "Recorded patch policy gate decisions did not reject the patch.",
    )
}

fn loop_step_check(run: &LoopRun, step_name: LoopStepName, check_name: &str) -> EvalCheck {
    let Some(step) = run.steps.iter().find(|step| step.name == step_name) else {
        return check(
            check_name,
            false,
            format!("Loop step {step_name:?} is missing from run state."),
        );
    };

    let passed = matches!(
        step.status,
        LoopStepStatus::Passed | LoopStepStatus::Completed
    );
    check(
        check_name,
        passed,
        format!("Loop step {step_name:?} status is {:?}.", step.status),
    )
}

fn check(name: &str, passed: bool, summary: impl Into<String>) -> EvalCheck {
    EvalCheck {
        name: name.to_string(),
        status: if passed {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
        duration_ms: None,
        stdout_path: None,
        stdout_digest: None,
        stderr_path: None,
        stderr_digest: None,
        summary: Some(summary.into()),
    }
}

fn sanitize_id(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|item| {
            if item.is_ascii_alphanumeric() || item == '-' || item == '_' {
                item
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "local".to_string()
    } else {
        trimmed.to_string()
    }
}
