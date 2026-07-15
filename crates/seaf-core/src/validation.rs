use std::{collections::BTreeSet, fmt::Display, fs, io::Read, path::Path};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CheckStatus, EvalReport, GoalSpec, LoopRun, Policy, PolicyDecision, ProjectConfig,
    ProviderExchangeKind, ProviderExchangeOutcome, ProviderExchangePhase, ProviderExchangeRecord,
    ProviderExchangeRecordReference, ProviderRole, ReleaseCapsule, SeafEvent, TicketSpec,
};

pub type ValidationResult<T> = Result<T, ValidationReport>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

impl FieldError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub valid: bool,
    pub errors: Vec<FieldError>,
}

impl ValidationReport {
    pub fn valid(kind: impl Into<String>, path: Option<&Path>) -> Self {
        Self {
            kind: kind.into(),
            path: path.map(display_path),
            valid: true,
            errors: Vec::new(),
        }
    }

    pub fn invalid(kind: impl Into<String>, path: Option<&Path>, errors: Vec<FieldError>) -> Self {
        Self {
            kind: kind.into(),
            path: path.map(display_path),
            valid: false,
            errors,
        }
    }
}

pub fn load_goal_file(path: &Path) -> ValidationResult<GoalSpec> {
    let goal = load_struct::<GoalSpec>("goal", path)?;
    let errors = validate_goal_spec(&goal);
    if errors.is_empty() {
        Ok(goal)
    } else {
        Err(ValidationReport::invalid("goal", Some(path), errors))
    }
}

pub fn load_policy_file(path: &Path) -> ValidationResult<Policy> {
    let policy = load_struct::<Policy>("policy", path)?;
    let errors = validate_policy(&policy);
    if errors.is_empty() {
        Ok(policy)
    } else {
        Err(ValidationReport::invalid("policy", Some(path), errors))
    }
}

pub fn load_project_config_file(path: &Path) -> ValidationResult<ProjectConfig> {
    let config = load_struct::<ProjectConfig>("project_config", path)?;
    let errors = validate_project_config(&config);
    if errors.is_empty() {
        Ok(config)
    } else {
        Err(ValidationReport::invalid(
            "project_config",
            Some(path),
            errors,
        ))
    }
}

pub fn load_release_capsule_file(path: &Path) -> ValidationResult<ReleaseCapsule> {
    let capsule = load_struct::<ReleaseCapsule>("release_capsule", path)?;
    let errors = validate_release_capsule(&capsule);
    if errors.is_empty() {
        Ok(capsule)
    } else {
        Err(ValidationReport::invalid(
            "release_capsule",
            Some(path),
            errors,
        ))
    }
}

pub fn load_eval_report_file(path: &Path) -> ValidationResult<EvalReport> {
    let report = load_struct::<EvalReport>("eval_report", path)?;
    let errors = validate_eval_report(&report);
    if errors.is_empty() {
        Ok(report)
    } else {
        Err(ValidationReport::invalid("eval_report", Some(path), errors))
    }
}

pub fn load_ticket_file(path: &Path) -> ValidationResult<TicketSpec> {
    let ticket = load_struct::<TicketSpec>("ticket", path)?;
    validate_parsed_ticket(ticket, Some(path))
}

pub fn parse_ticket_spec(text: &str) -> ValidationResult<TicketSpec> {
    let ticket = serde_yaml::from_str(text).map_err(|error| {
        ValidationReport::invalid(
            "ticket",
            None,
            vec![FieldError::new("document", error.to_string())],
        )
    })?;
    validate_parsed_ticket(ticket, None)
}

fn validate_parsed_ticket(ticket: TicketSpec, path: Option<&Path>) -> ValidationResult<TicketSpec> {
    let errors = validate_ticket_spec(&ticket);
    if errors.is_empty() {
        Ok(ticket)
    } else {
        Err(ValidationReport::invalid("ticket", path, errors))
    }
}

pub fn load_loop_run_file(path: &Path) -> ValidationResult<LoopRun> {
    let run = load_struct::<LoopRun>("loop_run", path)?;
    let errors = validate_loop_run(&run);
    if errors.is_empty() {
        Ok(run)
    } else {
        Err(ValidationReport::invalid("loop_run", Some(path), errors))
    }
}

pub fn validate_goal_spec(goal: &GoalSpec) -> Vec<FieldError> {
    let mut errors = Vec::new();
    require_non_empty(&mut errors, "goal_id", &goal.goal_id);
    require_non_empty(&mut errors, "name", &goal.name);
    require_non_empty(&mut errors, "objective.metric", &goal.objective.metric);

    if !goal.objective.minimum_effect_size.is_finite() || goal.objective.minimum_effect_size <= 0.0
    {
        errors.push(FieldError::new(
            "objective.minimum_effect_size",
            "must be a finite number greater than 0",
        ));
    }

    if goal.guardrails.is_empty() {
        errors.push(FieldError::new(
            "guardrails",
            "must define at least one non-regression guardrail",
        ));
    }

    if goal.allowed_change_types.is_empty() {
        errors.push(FieldError::new(
            "allowed_change_types",
            "must define at least one allowed, review-required, or forbidden change type",
        ));
    }

    if let Some(rollout) = &goal.rollout {
        if !rollout.is_object() {
            errors.push(FieldError::new("rollout", "must be an object when present"));
        }
    }

    for (index, anti_goal) in goal.anti_goals.iter().enumerate() {
        require_non_empty(&mut errors, format!("anti_goals[{index}]"), anti_goal);
    }

    for change_type in goal.allowed_change_types.keys() {
        require_non_empty(&mut errors, "allowed_change_types.<key>", change_type);
    }

    errors
}

pub fn validate_policy(policy: &Policy) -> Vec<FieldError> {
    let mut errors = Vec::new();
    require_non_empty(&mut errors, "policy_id", &policy.policy_id);

    if policy.default_autonomy_level > 4 {
        errors.push(FieldError::new(
            "default_autonomy_level",
            "must be between 0 and 4",
        ));
    }

    validate_non_empty_list(
        &mut errors,
        "forbidden_paths",
        &policy.forbidden_paths,
        "must include protected paths agents cannot modify directly",
    );
    validate_non_empty_list(
        &mut errors,
        "requires_human_review",
        &policy.requires_human_review,
        "must include change types that require human review",
    );
    validate_non_empty_list(
        &mut errors,
        "allowed_without_review",
        &policy.allowed_without_review,
        "must include low-risk change types or be intentionally omitted in a later schema",
    );

    errors
}

pub fn validate_policy_decision(decision: &PolicyDecision) -> Vec<FieldError> {
    let mut errors = Vec::new();
    require_non_empty(&mut errors, "patch_id", &decision.patch_id);
    validate_sha256_digest(&mut errors, "patch_sha256", &decision.patch_sha256);
    for (index, path) in decision.changed_paths.iter().enumerate() {
        require_non_empty(&mut errors, format!("changed_paths[{index}]"), path);
    }
    for (index, reason) in decision.reasons.iter().enumerate() {
        require_non_empty(&mut errors, format!("reasons[{index}].code"), &reason.code);
        require_non_empty(
            &mut errors,
            format!("reasons[{index}].message"),
            &reason.message,
        );
    }
    errors
}

pub fn validate_project_config(config: &ProjectConfig) -> Vec<FieldError> {
    let mut errors = Vec::new();
    validate_safe_relative_path(&mut errors, "policy_path", &config.policy_path);
    errors
}

pub fn validate_release_capsule(capsule: &ReleaseCapsule) -> Vec<FieldError> {
    let mut errors = Vec::new();
    require_non_empty(&mut errors, "release_id", &capsule.release_id);
    require_non_empty(&mut errors, "app_id", &capsule.app_id);
    require_non_empty(&mut errors, "version", &capsule.version);
    require_non_empty(&mut errors, "source_commit", &capsule.source_commit);
    require_non_empty(&mut errors, "goal_id", &capsule.goal_id);
    require_non_empty(&mut errors, "rollback_plan", &capsule.rollback_plan);
    validate_sha256_digest(&mut errors, "artifact_digest", &capsule.artifact_digest);
    validate_sha256_digest(
        &mut errors,
        "eval_report_digest",
        &capsule.eval_report_digest,
    );

    if capsule.rollout_policy.initial_percentage > 100 {
        errors.push(FieldError::new(
            "rollout_policy.initial_percentage",
            "must be between 0 and 100",
        ));
    }

    errors
}

pub fn validate_eval_report(report: &EvalReport) -> Vec<FieldError> {
    let mut errors = Vec::new();
    require_non_empty(&mut errors, "eval_report_id", &report.eval_report_id);
    require_non_empty(&mut errors, "patch_id", &report.patch_id);
    require_non_empty(&mut errors, "goal_id", &report.goal_id);
    require_non_empty(&mut errors, "summary", &report.summary);

    if report.checks.is_empty() {
        errors.push(FieldError::new(
            "checks",
            "must include at least one executed check",
        ));
    }

    if report.passed
        && report
            .checks
            .iter()
            .any(|check| check.status != CheckStatus::Passed)
    {
        errors.push(FieldError::new(
            "passed",
            "cannot be true unless every check passed",
        ));
    }

    if report.decision == crate::EvalDecision::Reject && report.passed {
        errors.push(FieldError::new(
            "decision",
            "cannot reject an EvalReport that is marked passed",
        ));
    }

    if !report.passed && report.decision != crate::EvalDecision::Reject {
        errors.push(FieldError::new(
            "decision",
            "must reject an EvalReport that is marked failed",
        ));
    }

    if report.risk_level == crate::RiskLevel::High && report.passed {
        errors.push(FieldError::new(
            "risk_level",
            "cannot be high when an EvalReport is marked passed",
        ));
    }

    for (index, check) in report.checks.iter().enumerate() {
        require_non_empty(&mut errors, format!("checks[{index}].name"), &check.name);
        validate_optional_log_digest(
            &mut errors,
            index,
            "stdout",
            check.stdout_path.as_deref(),
            check.stdout_digest.as_deref(),
        );
        validate_optional_log_digest(
            &mut errors,
            index,
            "stderr",
            check.stderr_path.as_deref(),
            check.stderr_digest.as_deref(),
        );
    }

    if let Some(evidence) = &report.loop_evidence {
        validate_eval_loop_evidence(&mut errors, report, evidence);
        let mut log_paths = BTreeSet::new();
        for (index, check) in report.checks.iter().enumerate() {
            for (stream, path, digest) in [
                ("stdout", &check.stdout_path, &check.stdout_digest),
                ("stderr", &check.stderr_path, &check.stderr_digest),
            ] {
                if path.is_none() || digest.is_none() {
                    errors.push(FieldError::new(
                        format!("checks[{index}].{stream}_digest"),
                        "integrated loop evaluation requires an exact log path and digest pair",
                    ));
                }
                if path
                    .as_deref()
                    .is_some_and(|path| !is_portable_artifact_path(path))
                {
                    errors.push(FieldError::new(
                        format!("checks[{index}].{stream}_path"),
                        "integrated log path must use strict portable relative artifact spelling",
                    ));
                }
                if let Some(path) = path {
                    if !log_paths.insert(path.as_str()) {
                        errors.push(FieldError::new(
                            format!("checks[{index}].{stream}_path"),
                            "integrated log paths must be unique",
                        ));
                    }
                }
            }
        }
    }

    errors
}

fn validate_optional_log_digest(
    errors: &mut Vec<FieldError>,
    index: usize,
    stream: &str,
    path: Option<&str>,
    digest: Option<&str>,
) {
    if let Some(path) = path {
        require_non_empty(errors, format!("checks[{index}].{stream}_path"), path);
    }
    if let Some(digest) = digest {
        validate_lowercase_sha256_digest(
            errors,
            &format!("checks[{index}].{stream}_digest"),
            digest,
        );
        if path.is_none() {
            errors.push(FieldError::new(
                format!("checks[{index}].{stream}_path"),
                "is required when a log digest is present",
            ));
        }
    }
}

fn validate_eval_loop_evidence(
    errors: &mut Vec<FieldError>,
    report: &EvalReport,
    evidence: &crate::EvalLoopEvidence,
) {
    if evidence.schema_version != 1 {
        errors.push(FieldError::new("loop_evidence.schema_version", "must be 1"));
    }
    if evidence.run_id != report.patch_id {
        errors.push(FieldError::new(
            "loop_evidence.run_id",
            "must match patch_id",
        ));
    }
    require_non_empty(errors, "loop_evidence.ticket_id", &evidence.ticket_id);
    validate_lowercase_sha256_digest(
        errors,
        "loop_evidence.ticket_digest",
        &evidence.ticket_digest,
    );
    validate_artifact_reference(errors, "loop_evidence.eval_config", &evidence.eval_config);
    if evidence.eval_config.path != "inputs/eval-config.json" {
        errors.push(FieldError::new(
            "loop_evidence.eval_config.path",
            "must select inputs/eval-config.json",
        ));
    }
    validate_artifact_reference(
        errors,
        "loop_evidence.candidate_diff",
        &evidence.candidate_diff,
    );
    if !is_portable_artifact_path(&evidence.candidate_diff.path) {
        errors.push(FieldError::new(
            "loop_evidence.candidate_diff.path",
            "must use strict portable relative artifact spelling",
        ));
    }
    if !matches!(evidence.starting_head.len(), 40 | 64)
        || !evidence
            .starting_head
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        errors.push(FieldError::new(
            "loop_evidence.starting_head",
            "must be a lowercase 40- or 64-character Git object ID",
        ));
    }
    validate_lowercase_sha256_digest(
        errors,
        "loop_evidence.human_approval_digest",
        &evidence.human_approval_digest,
    );
    validate_lowercase_sha256_digest(
        errors,
        "loop_evidence.policy_decision_digest",
        &evidence.policy_decision_digest,
    );
    validate_artifact_reference(
        errors,
        "loop_evidence.testing_evidence",
        &evidence.testing_evidence,
    );
    if !is_portable_artifact_path(&evidence.testing_evidence.path) {
        errors.push(FieldError::new(
            "loop_evidence.testing_evidence.path",
            "must use strict portable relative artifact spelling",
        ));
    }
}

pub fn is_portable_artifact_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains(['\\', ':'])
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
                })
        })
}

pub fn validate_seaf_event(event: &SeafEvent) -> Vec<FieldError> {
    let mut errors = Vec::new();
    require_non_empty(&mut errors, "event_id", &event.event_id);
    require_non_empty(&mut errors, "name", &event.name);
    require_non_empty(&mut errors, "timestamp", &event.timestamp);
    require_non_empty(&mut errors, "source", &event.source);

    if !event.payload.is_object() {
        errors.push(FieldError::new("payload", "must be an object"));
    }

    errors
}

pub fn validate_ticket_spec(ticket: &TicketSpec) -> Vec<FieldError> {
    let mut errors = Vec::new();
    require_non_empty(&mut errors, "ticket_id", &ticket.ticket_id);
    require_non_empty(&mut errors, "goal_id", &ticket.goal_id);
    require_non_empty(&mut errors, "title", &ticket.title);
    require_non_empty(&mut errors, "problem", &ticket.problem);

    validate_list_entries(
        &mut errors,
        "research_questions",
        &ticket.research_questions,
    );
    validate_list_entries(
        &mut errors,
        "context.relevant_files",
        &ticket.context.relevant_files,
    );
    validate_list_entries(
        &mut errors,
        "context.forbidden_files",
        &ticket.context.forbidden_files,
    );

    if ticket.autonomy.level > 4 {
        errors.push(FieldError::new("autonomy.level", "must be between 0 and 4"));
    }
    validate_list_entries(
        &mut errors,
        "autonomy.allow_shell_commands",
        &ticket.autonomy.allow_shell_commands,
    );

    validate_non_empty_list(
        &mut errors,
        "acceptance_criteria",
        &ticket.acceptance_criteria,
        "must include at least one acceptance criterion",
    );

    if let Some(eval) = &ticket.eval {
        require_non_empty(&mut errors, "eval.config", &eval.config);
    }

    errors
}

pub fn validate_loop_run(run: &LoopRun) -> Vec<FieldError> {
    let mut errors = Vec::new();
    require_non_empty(&mut errors, "run_id", &run.run_id);
    require_non_empty(&mut errors, "ticket_id", &run.ticket_id);
    require_non_empty(&mut errors, "goal_id", &run.goal_id);
    require_non_empty(&mut errors, "provider", &run.provider);
    require_non_empty(&mut errors, "model", &run.model);
    validate_lowercase_sha256_digest(
        &mut errors,
        "input_digests.ticket",
        &run.input_digests.ticket,
    );
    validate_lowercase_sha256_digest(
        &mut errors,
        "input_digests.policy",
        &run.input_digests.policy,
    );
    validate_lowercase_sha256_digest(
        &mut errors,
        "input_digests.config",
        &run.input_digests.config,
    );
    validate_lowercase_sha256_digest(
        &mut errors,
        "input_digests.repository",
        &run.input_digests.repository,
    );
    if let Some(eval_config) = &run.input_digests.eval_config {
        validate_lowercase_sha256_digest(&mut errors, "input_digests.eval_config", eval_config);
    }
    require_non_empty(&mut errors, "started_at", &run.started_at);
    require_non_empty(&mut errors, "updated_at", &run.updated_at);

    for (index, step) in run.steps.iter().enumerate() {
        if let Some(artifact_path) = &step.artifact_path {
            require_non_empty(
                &mut errors,
                format!("steps[{index}].artifact_path"),
                artifact_path,
            );
        }
        if let Some(artifact_digest) = &step.artifact_digest {
            let field = format!("steps[{index}].artifact_digest");
            validate_lowercase_sha256_digest(&mut errors, &field, artifact_digest);
        }
        if step.artifact_path.is_some() != step.artifact_digest.is_some() {
            errors.push(FieldError::new(
                format!("steps[{index}].artifact_path"),
                "artifact_path and artifact_digest must either both be present or both be absent",
            ));
            errors.push(FieldError::new(
                format!("steps[{index}].artifact_digest"),
                "artifact_path and artifact_digest must either both be present or both be absent",
            ));
        }
    }

    for (index, decision) in run.policy_decisions.iter().enumerate() {
        for error in validate_policy_decision(decision) {
            errors.push(FieldError::new(
                format!("policy_decisions[{index}].{}", error.field),
                error.message,
            ));
        }
    }

    validate_provider_exchange_references(&mut errors, run);

    if let Some(candidate) = &run.candidate_workspace {
        validate_candidate_workspace_state(&mut errors, candidate);
        if candidate.repository_identity_digest != run.input_digests.repository {
            errors.push(FieldError::new(
                "candidate_workspace.repository_identity_digest",
                "must match input_digests.repository",
            ));
        }
        if run.status == crate::LoopStatus::Running
            && candidate.lifecycle != crate::CandidateWorkspaceLifecycle::Active
        {
            errors.push(FieldError::new(
                "candidate_workspace.lifecycle",
                "must remain active while the LoopRun is running",
            ));
        }
        if run.status == crate::LoopStatus::Pending
            && !matches!(
                candidate.lifecycle,
                crate::CandidateWorkspaceLifecycle::Provisioning
                    | crate::CandidateWorkspaceLifecycle::Active
            )
        {
            errors.push(FieldError::new(
                "candidate_workspace.lifecycle",
                "must be provisioning or active while the LoopRun is pending",
            ));
        }
        if candidate.lifecycle == crate::CandidateWorkspaceLifecycle::Provisioning
            && (run.status != crate::LoopStatus::Pending
                || run.current_step != crate::LoopStepName::Research
                || run.steps.iter().any(|step| {
                    step.status != crate::LoopStepStatus::Pending
                        || step.artifact_path.is_some()
                        || step.artifact_digest.is_some()
                })
                || !run.policy_decisions.is_empty()
                || !run.provider_exchange_records.is_empty()
                || run.eval_report_path.is_some())
        {
            errors.push(FieldError::new(
                "candidate_workspace.lifecycle",
                "provisioning is valid only for an untouched pending Research run",
            ));
        }
    }
    match (run.execution_mode, run.candidate_workspace.is_some()) {
        (crate::LoopExecutionMode::LegacyProposalOnly, true) => errors.push(FieldError::new(
            "candidate_workspace",
            "must be absent for legacy_proposal_only execution",
        )),
        (crate::LoopExecutionMode::IsolatedCandidate, false) => errors.push(FieldError::new(
            "candidate_workspace",
            "is required for isolated_candidate execution",
        )),
        _ => {}
    }

    match run.status {
        crate::LoopStatus::AwaitingHumanReview => {
            validate_awaiting_human_review_run(&mut errors, run);
            if run.human_approval.is_some() {
                errors.push(FieldError::new(
                    "human_approval",
                    "must be absent before exact human approval publication",
                ));
            }
        }
        crate::LoopStatus::Approved => {
            validate_awaiting_human_review_run(&mut errors, run);
            validate_human_approval_evidence(&mut errors, run);
        }
        crate::LoopStatus::EvalPassed => {
            validate_final_eval_run(&mut errors, run, true);
            validate_human_approval_evidence(&mut errors, run);
        }
        crate::LoopStatus::Promoted => {
            validate_final_eval_run(&mut errors, run, true);
            validate_human_approval_evidence(&mut errors, run);
            validate_promotion_evidence(&mut errors, run);
        }
        crate::LoopStatus::Failed if run.human_approval.is_some() => {
            validate_final_eval_run(&mut errors, run, false);
            validate_human_approval_evidence(&mut errors, run);
        }
        _ if run.human_approval.is_some() => errors.push(FieldError::new(
            "human_approval",
            "is valid only for approved or final integrated evaluation authority",
        )),
        _ => {}
    }

    if run.status != crate::LoopStatus::Promoted && run.promotion.is_some() {
        errors.push(FieldError::new(
            "promotion",
            "is valid only for promoted authority",
        ));
    }

    if let Some(recovery) = &run.latest_recovery {
        if recovery.recovery_id == 0 {
            errors.push(FieldError::new(
                "latest_recovery.recovery_id",
                "must be at least 1",
            ));
        }
        validate_artifact_reference(&mut errors, "latest_recovery.artifact", &recovery.artifact);
        let expected = format!("artifacts/recovery-{:03}.json", recovery.recovery_id);
        if recovery.artifact.path != expected {
            errors.push(FieldError::new(
                "latest_recovery.artifact.path",
                format!("must be {expected}"),
            ));
        }
        if run.execution_mode != crate::LoopExecutionMode::IsolatedCandidate
            || run.candidate_workspace.is_none()
        {
            errors.push(FieldError::new(
                "latest_recovery",
                "requires isolated_candidate authority",
            ));
        }
    }

    if let Some(eval_report_path) = &run.eval_report_path {
        require_non_empty(&mut errors, "eval_report_path", eval_report_path);
    }

    errors
}

fn validate_promotion_evidence(errors: &mut Vec<FieldError>, run: &LoopRun) {
    let Some(evidence) = run.promotion.as_ref() else {
        errors.push(FieldError::new(
            "promotion",
            "promoted authority requires promotion evidence",
        ));
        return;
    };
    if evidence.schema_version != 1 {
        errors.push(FieldError::new("promotion.schema_version", "must be 1"));
    }
    if evidence.run_id != run.run_id {
        errors.push(FieldError::new("promotion.run_id", "must match run_id"));
    }
    require_non_empty(errors, "promotion.reviewer", &evidence.reviewer);
    if evidence.reviewer.len() > 256
        || evidence.reviewer.trim() != evidence.reviewer
        || evidence.reviewer.chars().any(char::is_control)
    {
        errors.push(FieldError::new(
            "promotion.reviewer",
            "must be 1..=256 bytes with no surrounding whitespace or control characters",
        ));
    }
    for (field, value) in [
        ("promotion.promoted_at", evidence.promoted_at.as_str()),
        (
            "promotion.eval_passed_updated_at",
            evidence.eval_passed_updated_at.as_str(),
        ),
    ] {
        require_non_empty(errors, field, value);
        if value
            .parse::<u64>()
            .map_or(true, |parsed| parsed.to_string() != value)
        {
            errors.push(FieldError::new(
                field,
                "must be canonical decimal Unix seconds",
            ));
        }
    }
    if evidence
        .promoted_at
        .parse::<u64>()
        .ok()
        .zip(evidence.eval_passed_updated_at.parse::<u64>().ok())
        .is_some_and(|(promoted, evaluated)| promoted < evaluated)
    {
        errors.push(FieldError::new(
            "promotion.promoted_at",
            "must not precede the exact EvalPassed predecessor",
        ));
    }
    validate_artifact_reference(errors, "promotion.intent", &evidence.intent);
    if evidence.intent.path != "artifacts/09-promotion.intent.json" {
        errors.push(FieldError::new(
            "promotion.intent.path",
            "must select the canonical promotion intent",
        ));
    }
    if evidence.promoted_at != run.updated_at {
        errors.push(FieldError::new(
            "promotion.promoted_at",
            "must match the promoted LoopRun updated_at timestamp",
        ));
    }
    validate_artifact_reference(errors, "promotion.candidate_diff", &evidence.candidate_diff);
    validate_artifact_reference(
        errors,
        "promotion.testing_evidence",
        &evidence.testing_evidence,
    );
    validate_artifact_reference(errors, "promotion.eval_report", &evidence.eval_report);
    validate_lowercase_sha256_digest(
        errors,
        "promotion.policy_decision_digest",
        &evidence.policy_decision_digest,
    );
    validate_lowercase_sha256_digest(
        errors,
        "promotion.eval_passed_run_digest",
        &evidence.eval_passed_run_digest,
    );
    if !matches!(evidence.target_head.len(), 40 | 64)
        || !evidence
            .target_head
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        errors.push(FieldError::new(
            "promotion.target_head",
            "must be a lowercase Git object ID",
        ));
    }
    let approval = run.human_approval.as_ref();
    let testing = run
        .steps
        .iter()
        .find(|step| step.name == crate::LoopStepName::Testing);
    let report = run
        .steps
        .iter()
        .find(|step| step.name == crate::LoopStepName::EvalReport);
    if approval.is_none_or(|approval| {
        evidence.candidate_diff != approval.candidate_diff
            || evidence.policy_decision_digest != approval.policy_decision_digest
            || evidence.target_head != approval.starting_head
    }) {
        errors.push(FieldError::new(
            "promotion.candidate_diff",
            "must preserve the exact approved candidate, policy decision, and target HEAD",
        ));
    }
    if testing.is_none_or(|step| {
        step.artifact_path.as_deref() != Some(evidence.testing_evidence.path.as_str())
            || step.artifact_digest.as_deref() != Some(evidence.testing_evidence.digest.as_str())
    }) {
        errors.push(FieldError::new(
            "promotion.testing_evidence",
            "must match the final Testing artifact",
        ));
    }
    if report.is_none_or(|step| {
        step.artifact_path.as_deref() != Some(evidence.eval_report.path.as_str())
            || step.artifact_digest.as_deref() != Some(evidence.eval_report.digest.as_str())
    }) {
        errors.push(FieldError::new(
            "promotion.eval_report",
            "must match the final EvalReport artifact",
        ));
    }
    let mut predecessor = run.clone();
    predecessor.status = crate::LoopStatus::EvalPassed;
    predecessor.updated_at = evidence.eval_passed_updated_at.clone();
    predecessor.promotion = None;
    if crate::canonical_sha256_digest(&predecessor)
        .map_or(true, |digest| digest != evidence.eval_passed_run_digest)
    {
        errors.push(FieldError::new(
            "promotion.eval_passed_run_digest",
            "must bind the exact EvalPassed predecessor",
        ));
    }
}

fn validate_final_eval_run(errors: &mut Vec<FieldError>, run: &LoopRun, passed: bool) {
    if run.execution_mode != crate::LoopExecutionMode::IsolatedCandidate {
        errors.push(FieldError::new(
            "execution_mode",
            "final integrated evaluation requires isolated_candidate execution",
        ));
    }
    if run.current_step != crate::LoopStepName::EvalReport {
        errors.push(FieldError::new(
            "current_step",
            "final integrated evaluation must stop at EvalReport",
        ));
    }
    if run.input_digests.eval_config.is_none() {
        errors.push(FieldError::new(
            "input_digests.eval_config",
            "final integrated evaluation requires immutable eval config authority",
        ));
    }
    match run.candidate_workspace.as_ref() {
        Some(candidate)
            if candidate.schema_version == 2
                && (!passed
                    || candidate.lifecycle == crate::CandidateWorkspaceLifecycle::Active)
                && candidate
                    .patch_transaction
                    .as_ref()
                    .is_some_and(|transaction| {
                        transaction.phase == crate::CandidatePatchPhase::Applied
                    }) => {}
        _ => errors.push(FieldError::new(
            "candidate_workspace",
            "final integrated evaluation requires v2 candidate authority with an Applied transaction; passing authority must remain active",
        )),
    }

    let expected_names = [
        crate::LoopStepName::Research,
        crate::LoopStepName::Analysis,
        crate::LoopStepName::SpecCreation,
        crate::LoopStepName::SpecReview,
        crate::LoopStepName::Development,
        crate::LoopStepName::OutputReview,
        crate::LoopStepName::Testing,
        crate::LoopStepName::EvalReport,
    ];
    if run.steps.len() != expected_names.len()
        || run
            .steps
            .iter()
            .zip(expected_names)
            .any(|(record, expected)| record.name != expected)
    {
        errors.push(FieldError::new(
            "steps",
            "final integrated evaluation requires the exact ordered eight-step chain without duplicates",
        ));
        return;
    }

    for (index, record) in run.steps.iter().take(6).enumerate() {
        let valid = match record.name {
            crate::LoopStepName::Development => record.status == crate::LoopStepStatus::Completed,
            crate::LoopStepName::OutputReview => record.status == crate::LoopStepStatus::Passed,
            _ => matches!(
                record.status,
                crate::LoopStepStatus::Completed | crate::LoopStepStatus::Passed
            ),
        };
        if !valid {
            errors.push(FieldError::new(
                format!("steps[{index}].status"),
                "must preserve a successful pre-evaluation prefix",
            ));
        }
    }

    let terminal_status = if passed {
        crate::LoopStepStatus::Passed
    } else {
        crate::LoopStepStatus::Failed
    };
    for index in [6, 7] {
        let record = &run.steps[index];
        if record.status != terminal_status {
            errors.push(FieldError::new(
                format!("steps[{index}].status"),
                format!("must be {terminal_status:?} for this final evaluation outcome"),
            ));
        }
        if record.artifact_path.is_none() || record.artifact_digest.is_none() {
            errors.push(FieldError::new(
                format!("steps[{index}].artifact_path"),
                "final evaluation steps require an artifact path and digest",
            ));
        }
        if record
            .artifact_path
            .as_deref()
            .is_some_and(|path| !is_portable_artifact_path(path))
        {
            errors.push(FieldError::new(
                format!("steps[{index}].artifact_path"),
                "final evaluation artifact path must use strict portable relative spelling",
            ));
        }
    }
    if run.eval_report_path.as_deref() != run.steps[7].artifact_path.as_deref() {
        errors.push(FieldError::new(
            "eval_report_path",
            "must exactly match the EvalReport step artifact path",
        ));
    }
}

fn validate_human_approval_evidence(errors: &mut Vec<FieldError>, run: &LoopRun) {
    let Some(approval) = run.human_approval.as_ref() else {
        errors.push(FieldError::new(
            "human_approval",
            "approved requires exact human approval evidence",
        ));
        return;
    };
    if approval.schema_version != 1 {
        errors.push(FieldError::new(
            "human_approval.schema_version",
            "must be 1",
        ));
    }
    if approval.run_id != run.run_id {
        errors.push(FieldError::new(
            "human_approval.run_id",
            "must match run_id",
        ));
    }
    require_non_empty(errors, "human_approval.reviewer", &approval.reviewer);
    if approval.reviewer.len() > 256 {
        errors.push(FieldError::new(
            "human_approval.reviewer",
            "must not exceed 256 bytes",
        ));
    }
    if approval.reviewer.chars().any(char::is_control) {
        errors.push(FieldError::new(
            "human_approval.reviewer",
            "must not contain control characters",
        ));
    }
    require_non_empty(errors, "human_approval.approved_at", &approval.approved_at);
    if approval.approved_at.len() > 64 {
        errors.push(FieldError::new(
            "human_approval.approved_at",
            "must not exceed 64 bytes",
        ));
    }
    require_non_empty(
        errors,
        "human_approval.candidate_diff.path",
        &approval.candidate_diff.path,
    );
    validate_lowercase_sha256_digest(
        errors,
        "human_approval.candidate_diff.digest",
        &approval.candidate_diff.digest,
    );
    validate_lowercase_sha256_digest(
        errors,
        "human_approval.policy_decision_digest",
        &approval.policy_decision_digest,
    );
    require_non_empty(
        errors,
        "human_approval.output_review.path",
        &approval.output_review.path,
    );
    validate_lowercase_sha256_digest(
        errors,
        "human_approval.output_review.digest",
        &approval.output_review.digest,
    );
    if !matches!(approval.starting_head.len(), 40 | 64)
        || !approval
            .starting_head
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        errors.push(FieldError::new(
            "human_approval.starting_head",
            "must be a lowercase Git object ID",
        ));
    }
    if let Some(candidate) = run.candidate_workspace.as_ref() {
        if approval.starting_head != candidate.starting_head {
            errors.push(FieldError::new(
                "human_approval.starting_head",
                "must match the candidate starting HEAD",
            ));
        }
        if approval.candidate_diff.digest != candidate.candidate_diff_digest {
            errors.push(FieldError::new(
                "human_approval.candidate_diff.digest",
                "must match the candidate diff digest",
            ));
        }
    }
    let Some(output_review) = run
        .steps
        .iter()
        .find(|record| record.name == crate::LoopStepName::OutputReview)
    else {
        return;
    };
    if output_review.artifact_path.as_deref() != Some(&approval.output_review.path)
        || output_review.artifact_digest.as_deref() != Some(&approval.output_review.digest)
    {
        errors.push(FieldError::new(
            "human_approval.output_review",
            "must match the current OutputReview artifact",
        ));
    }
    if !run
        .provider_exchange_records
        .contains(&approval.output_review_request)
        || !run
            .provider_exchange_records
            .contains(&approval.output_review_response)
    {
        errors.push(FieldError::new(
            "human_approval.output_review_request",
            "approval exchange references must belong to the authoritative ledger",
        ));
    }
    let request = &approval.output_review_request;
    let response = &approval.output_review_response;
    if request.run_id != run.run_id
        || request.step != crate::LoopStepName::OutputReview
        || request.role != ProviderRole::OutputReviewer
        || request.kind != ProviderExchangeKind::Initial
        || request.exchange_index != 1
        || request.phase != ProviderExchangePhase::Request
        || response.run_id != run.run_id
        || response.step != crate::LoopStepName::OutputReview
        || response.role != ProviderRole::OutputReviewer
        || response.phase != ProviderExchangePhase::Response
        || request.step_attempt != response.step_attempt
        || run.provider_exchange_records.last() != Some(response)
    {
        errors.push(FieldError::new(
            "human_approval.output_review_response",
            "must bind the latest OutputReview attempt's initial request and terminal response",
        ));
    }
    let authoritative = run
        .policy_decisions
        .iter()
        .filter(|decision| decision.patch_id == run.run_id)
        .collect::<Vec<_>>();
    if authoritative.len() != 1
        || crate::canonical_sha256_digest(authoritative[0])
            .map_or(true, |digest| digest != approval.policy_decision_digest)
    {
        errors.push(FieldError::new(
            "human_approval.policy_decision_digest",
            "must select the unique authoritative Development policy decision",
        ));
    }
}

fn validate_awaiting_human_review_run(errors: &mut Vec<FieldError>, run: &LoopRun) {
    if run.execution_mode != crate::LoopExecutionMode::IsolatedCandidate {
        errors.push(FieldError::new(
            "execution_mode",
            "awaiting_human_review is valid only for isolated_candidate execution",
        ));
    }
    if run.current_step != crate::LoopStepName::Testing {
        errors.push(FieldError::new(
            "current_step",
            "awaiting_human_review must stop at Testing before it runs",
        ));
    }
    match run.candidate_workspace.as_ref() {
        Some(candidate)
            if candidate.schema_version == 2
                && candidate.lifecycle == crate::CandidateWorkspaceLifecycle::Active
                && candidate
                    .patch_transaction
                    .as_ref()
                    .is_some_and(|transaction| {
                        transaction.phase == crate::CandidatePatchPhase::Applied
                    }) => {}
        _ => errors.push(FieldError::new(
            "candidate_workspace",
            "awaiting_human_review requires active v2 candidate authority with an Applied transaction",
        )),
    }

    validate_awaiting_step(
        errors,
        run,
        crate::LoopStepName::Development,
        crate::LoopStepStatus::Completed,
        None,
    );
    validate_awaiting_step(
        errors,
        run,
        crate::LoopStepName::OutputReview,
        crate::LoopStepStatus::Passed,
        Some(true),
    );
    validate_awaiting_step(
        errors,
        run,
        crate::LoopStepName::Testing,
        crate::LoopStepStatus::Pending,
        Some(false),
    );
    validate_awaiting_step(
        errors,
        run,
        crate::LoopStepName::EvalReport,
        crate::LoopStepStatus::Pending,
        Some(false),
    );
    if run.eval_report_path.is_some() {
        errors.push(FieldError::new(
            "eval_report_path",
            "must be absent before approved Testing and EvalReport execute",
        ));
    }
    validate_awaiting_output_review_ledger(errors, run);
}

fn validate_awaiting_output_review_ledger(errors: &mut Vec<FieldError>, run: &LoopRun) {
    let Some(last) = run.provider_exchange_records.last() else {
        errors.push(FieldError::new(
            "provider_exchange_records",
            "awaiting_human_review requires authenticated OutputReview request and response evidence",
        ));
        return;
    };
    if last.step != crate::LoopStepName::OutputReview
        || last.role != ProviderRole::OutputReviewer
        || last.phase != ProviderExchangePhase::Response
        || last.step_attempt == 0
    {
        errors.push(FieldError::new(
            "provider_exchange_records",
            "awaiting_human_review requires the ledger to end in an OutputReview OutputReviewer response",
        ));
        return;
    }
    let has_initial_request = run.provider_exchange_records.iter().any(|reference| {
        reference.step == crate::LoopStepName::OutputReview
            && reference.role == ProviderRole::OutputReviewer
            && reference.step_attempt == last.step_attempt
            && reference.kind == ProviderExchangeKind::Initial
            && reference.phase == ProviderExchangePhase::Request
    });
    if !has_initial_request {
        errors.push(FieldError::new(
            "provider_exchange_records",
            "awaiting_human_review requires the same OutputReview attempt's Initial request",
        ));
    }
}

fn validate_awaiting_step(
    errors: &mut Vec<FieldError>,
    run: &LoopRun,
    name: crate::LoopStepName,
    expected_status: crate::LoopStepStatus,
    requires_artifact: Option<bool>,
) {
    let matches = run
        .steps
        .iter()
        .enumerate()
        .filter(|(_, record)| record.name == name)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        errors.push(FieldError::new(
            "steps",
            format!("awaiting_human_review requires exactly one {name:?} record"),
        ));
        return;
    }
    let (index, record) = matches[0];
    if record.status != expected_status {
        errors.push(FieldError::new(
            format!("steps[{index}].status"),
            format!("must be {expected_status:?} while awaiting human review"),
        ));
    }
    let has_pair = record.artifact_path.is_some() && record.artifact_digest.is_some();
    if requires_artifact.is_some_and(|required| has_pair != required) {
        errors.push(FieldError::new(
            format!("steps[{index}].artifact_path"),
            if requires_artifact == Some(true) {
                "must preserve the reviewed OutputReview artifact and digest"
            } else {
                "must be absent before this step executes"
            },
        ));
    }
}

fn validate_candidate_workspace_state(
    errors: &mut Vec<FieldError>,
    candidate: &crate::CandidateWorkspaceState,
) {
    match candidate.schema_version {
        1 => {
            if candidate.run_directory_digest.is_some() {
                errors.push(FieldError::new(
                    "candidate_workspace.run_directory_digest",
                    "must be absent for candidate schema version 1",
                ));
            }
        }
        2 => match candidate.run_directory_digest.as_deref() {
            Some(digest) => validate_lowercase_sha256_digest(
                errors,
                "candidate_workspace.run_directory_digest",
                digest,
            ),
            None => errors.push(FieldError::new(
                "candidate_workspace.run_directory_digest",
                "is required for candidate schema version 2",
            )),
        },
        _ => errors.push(FieldError::new(
            "candidate_workspace.schema_version",
            "must be 1 or 2",
        )),
    }
    for (field, value) in [
        ("path", &candidate.path),
        ("source_worktree_root", &candidate.source_worktree_root),
        ("git_common_dir", &candidate.git_common_dir),
    ] {
        require_non_empty(errors, format!("candidate_workspace.{field}"), value);
        if !std::path::Path::new(value).is_absolute() {
            errors.push(FieldError::new(
                format!("candidate_workspace.{field}"),
                "must be an absolute path",
            ));
        }
    }
    validate_lowercase_sha256_digest(
        errors,
        "candidate_workspace.repository_identity_digest",
        &candidate.repository_identity_digest,
    );
    validate_lowercase_sha256_digest(
        errors,
        "candidate_workspace.candidate_diff_digest",
        &candidate.candidate_diff_digest,
    );
    for (field, value) in [
        ("starting_head", &candidate.starting_head),
        ("starting_tree", &candidate.starting_tree),
        ("candidate_head", &candidate.candidate_head),
        ("candidate_tree", &candidate.candidate_tree),
    ] {
        if !matches!(value.len(), 40 | 64)
            || !value
                .chars()
                .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
        {
            errors.push(FieldError::new(
                format!("candidate_workspace.{field}"),
                "must be a lowercase 40- or 64-character Git object ID",
            ));
        }
    }
    match candidate.lifecycle {
        crate::CandidateWorkspaceLifecycle::Provisioning
            if candidate.cleanup_started_at.is_some() || candidate.cleaned_at.is_some() =>
        {
            errors.push(FieldError::new(
                "candidate_workspace.cleaned_at",
                "cleanup timestamps must be absent while the candidate is provisioning",
            ));
        }
        crate::CandidateWorkspaceLifecycle::Active
            if candidate.cleanup_started_at.is_some() || candidate.cleaned_at.is_some() =>
        {
            errors.push(FieldError::new(
                "candidate_workspace.cleaned_at",
                "cleanup timestamps must be absent while the candidate is active",
            ));
        }
        crate::CandidateWorkspaceLifecycle::Cleaning => {
            if candidate
                .cleanup_started_at
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
                || candidate.cleaned_at.is_some()
            {
                errors.push(FieldError::new(
                    "candidate_workspace.cleanup_started_at",
                    "must be present only while cleanup is in progress",
                ));
            }
        }
        crate::CandidateWorkspaceLifecycle::Cleaned => {
            if candidate
                .cleanup_started_at
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
                || candidate
                    .cleaned_at
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
            {
                errors.push(FieldError::new(
                    "candidate_workspace.cleaned_at",
                    "cleanup start and completion timestamps must be present after cleanup",
                ));
            }
        }
        crate::CandidateWorkspaceLifecycle::Provisioning
        | crate::CandidateWorkspaceLifecycle::Active => {}
    }
    if candidate.lifecycle == crate::CandidateWorkspaceLifecycle::Provisioning
        && candidate.patch_transaction.is_some()
    {
        errors.push(FieldError::new(
            "candidate_workspace.patch_transaction",
            "must be absent while provisioning",
        ));
    }
    if candidate.candidate_head != candidate.starting_head {
        errors.push(FieldError::new(
            "candidate_workspace.candidate_head",
            "must equal starting_head because candidate commits are not authorized",
        ));
    }
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    match &candidate.patch_transaction {
        None => require_pristine_candidate_evidence(errors, candidate, EMPTY_SHA256),
        Some(transaction) => {
            if transaction.schema_version != 1 {
                errors.push(FieldError::new(
                    "candidate_workspace.patch_transaction.schema_version",
                    "must be 1",
                ));
            }
            validate_artifact_reference(
                errors,
                "candidate_workspace.patch_transaction.intent",
                &transaction.intent,
            );
            require_non_empty(
                errors,
                "candidate_workspace.patch_transaction.started_at",
                &transaction.started_at,
            );
            match transaction.phase {
                crate::CandidatePatchPhase::Applying => {
                    require_pristine_candidate_evidence(errors, candidate, EMPTY_SHA256);
                    if transaction.applied_evidence.is_some() || transaction.applied_at.is_some() {
                        errors.push(FieldError::new(
                            "candidate_workspace.patch_transaction.applied_evidence",
                            "must be absent while patch application is in progress",
                        ));
                    }
                }
                crate::CandidatePatchPhase::Applied => {
                    match &transaction.applied_evidence {
                        Some(reference) => validate_artifact_reference(
                            errors,
                            "candidate_workspace.patch_transaction.applied_evidence",
                            reference,
                        ),
                        None => errors.push(FieldError::new(
                            "candidate_workspace.patch_transaction.applied_evidence",
                            "is required after patch application",
                        )),
                    }
                    if transaction
                        .applied_at
                        .as_deref()
                        .unwrap_or("")
                        .trim()
                        .is_empty()
                    {
                        errors.push(FieldError::new(
                            "candidate_workspace.patch_transaction.applied_at",
                            "is required after patch application",
                        ));
                    }
                    if candidate.candidate_tree == candidate.starting_tree {
                        errors.push(FieldError::new(
                            "candidate_workspace.candidate_tree",
                            "must differ from starting_tree after patch application",
                        ));
                    }
                    if candidate.candidate_diff_digest == EMPTY_SHA256 {
                        errors.push(FieldError::new(
                            "candidate_workspace.candidate_diff_digest",
                            "must be non-empty after patch application",
                        ));
                    }
                }
            }
        }
    }
}

fn require_pristine_candidate_evidence(
    errors: &mut Vec<FieldError>,
    candidate: &crate::CandidateWorkspaceState,
    empty_sha256: &str,
) {
    if candidate.candidate_tree != candidate.starting_tree {
        errors.push(FieldError::new(
            "candidate_workspace.candidate_tree",
            "must equal starting_tree before patch application completes",
        ));
    }
    if candidate.candidate_diff_digest != empty_sha256 {
        errors.push(FieldError::new(
            "candidate_workspace.candidate_diff_digest",
            "must be the empty SHA-256 before patch application completes",
        ));
    }
}

pub fn validate_provider_exchange_record(record: &ProviderExchangeRecord) -> Vec<FieldError> {
    let mut errors = Vec::new();
    if record.schema_version != 1 {
        errors.push(FieldError::new("schema_version", "must be 1"));
    }
    require_non_empty(&mut errors, "run_id", &record.run_id);
    validate_exchange_identity(
        &mut errors,
        record.step,
        record.role,
        record.step_attempt,
        record.exchange_index,
    );
    validate_artifact_reference(&mut errors, "request", &record.request);
    if let Some(response) = &record.response {
        validate_artifact_reference(&mut errors, "response", response);
    }
    if let Some(expansion) = &record.expansion {
        validate_artifact_reference(&mut errors, "expansion", expansion);
    }
    if let Some(digest) = &record.previous_record_digest {
        validate_lowercase_sha256_digest(&mut errors, "previous_record_digest", digest);
    }
    if record.context_round == Some(0) {
        errors.push(FieldError::new(
            "context_round",
            "must be at least 1 when present",
        ));
    }

    match record.phase {
        ProviderExchangePhase::Request => {
            if record.response.is_some() || record.outcome.is_some() {
                errors.push(FieldError::new(
                    "phase",
                    "request records must not contain a response or outcome",
                ));
            }
        }
        ProviderExchangePhase::Response => {
            if record.previous_record_digest.is_none() {
                errors.push(FieldError::new(
                    "previous_record_digest",
                    "response records must link their request record",
                ));
            }
            if record.response.is_none() || record.outcome.is_none() {
                errors.push(FieldError::new(
                    "phase",
                    "response records require a response and parsed outcome",
                ));
            }
        }
    }

    match record.kind {
        ProviderExchangeKind::ContextRetry | ProviderExchangeKind::JsonRepair => {
            let requires_context = record.kind == ProviderExchangeKind::ContextRetry;
            if requires_context
                && (record.context_round.is_none() || record.context_round == Some(0))
            {
                errors.push(FieldError::new(
                    "context_round",
                    "context retry records require a nonzero context round",
                ));
            }
            if requires_context && record.expansion.is_none() {
                errors.push(FieldError::new(
                    "expansion",
                    "context retry records require an expansion",
                ));
            } else if record.context_round.is_some() != record.expansion.is_some() {
                errors.push(FieldError::new(
                    "expansion",
                    "context round and expansion must either both be present or both be absent",
                ));
            } else if let (Some(round), Some(expansion)) = (record.context_round, &record.expansion)
            {
                let expected = format!(
                    "artifacts/{}.attempt-{:03}.context-round-{round:03}.json",
                    exchange_step_stem(record.step),
                    record.step_attempt
                );
                if expansion.path != expected {
                    errors.push(FieldError::new(
                        "expansion.path",
                        format!("must be {expected}"),
                    ));
                }
            }
        }
        ProviderExchangeKind::Initial
            if record.expansion.is_some() || record.context_round.is_some() =>
        {
            errors.push(FieldError::new(
                "context_round",
                "initial records must not contain context expansion identity",
            ));
        }
        _ => {}
    }

    if let Some(outcome) = record.outcome {
        if !outcome_matches_role(record.role, outcome) {
            errors.push(FieldError::new(
                "outcome",
                "parsed outcome is not valid for this provider role",
            ));
        }
    }
    errors
}

fn validate_provider_exchange_references(errors: &mut Vec<FieldError>, run: &LoopRun) {
    let mut finished_groups = Vec::new();
    let mut current_group = None;
    for (index, reference) in run.provider_exchange_records.iter().enumerate() {
        let field = format!("provider_exchange_records[{index}]");
        validate_exchange_identity(
            errors,
            reference.step,
            reference.role,
            reference.step_attempt,
            reference.exchange_index,
        );
        if reference.run_id != run.run_id {
            errors.push(FieldError::new(
                format!("{field}.run_id"),
                "must match the loop run",
            ));
        }
        validate_lowercase_sha256_digest(errors, &format!("{field}.digest"), &reference.digest);
        if reference.context_round == Some(0) {
            errors.push(FieldError::new(
                format!("{field}.context_round"),
                "must be at least 1 when present",
            ));
        }
        let expected_path = exchange_record_path(reference);
        if reference.path != expected_path {
            errors.push(FieldError::new(
                format!("{field}.path"),
                format!("must be {expected_path}"),
            ));
        }
        match reference.kind {
            ProviderExchangeKind::ContextRetry
                if reference.context_round.is_none() || reference.context_round == Some(0) =>
            {
                errors.push(FieldError::new(
                    format!("{field}.context_round"),
                    "context retry references require a nonzero context round",
                ));
            }
            ProviderExchangeKind::Initial if reference.context_round.is_some() => {
                errors.push(FieldError::new(
                    format!("{field}.context_round"),
                    "initial references must not contain a context round",
                ));
            }
            _ => {}
        }

        let group = (reference.step, reference.step_attempt);
        if current_group != Some(group) {
            if index > 0
                && run.provider_exchange_records[index - 1].phase != ProviderExchangePhase::Response
            {
                errors.push(FieldError::new(
                    field.clone(),
                    "a new exchange group cannot start before the prior response",
                ));
            }
            if let Some(previous) = current_group.replace(group) {
                finished_groups.push(previous);
            }
            if finished_groups.contains(&group) {
                errors.push(FieldError::new(
                    field.clone(),
                    "exchange group is reordered",
                ));
            }
            if reference.exchange_index != 1
                || reference.phase != ProviderExchangePhase::Request
                || reference.kind != ProviderExchangeKind::Initial
            {
                errors.push(FieldError::new(
                    field.clone(),
                    "an exchange group must start with its initial request at index 1",
                ));
            }
        } else if let Some(previous) = index
            .checked_sub(1)
            .map(|prior| &run.provider_exchange_records[prior])
        {
            match reference.phase {
                ProviderExchangePhase::Response => {
                    if previous.phase != ProviderExchangePhase::Request
                        || reference.exchange_index != previous.exchange_index
                        || reference.kind != previous.kind
                        || reference.context_round != previous.context_round
                        || reference.role != previous.role
                    {
                        errors.push(FieldError::new(
                            field.clone(),
                            "a response must immediately follow its matching request",
                        ));
                    }
                }
                ProviderExchangePhase::Request => {
                    if previous.phase != ProviderExchangePhase::Response
                        || reference.exchange_index != previous.exchange_index.saturating_add(1)
                    {
                        errors.push(FieldError::new(
                            field.clone(),
                            "the next request must follow the prior response without gaps",
                        ));
                    }
                }
            }
        }
    }
}

fn validate_exchange_identity(
    errors: &mut Vec<FieldError>,
    step: crate::LoopStepName,
    role: ProviderRole,
    step_attempt: u32,
    exchange_index: u32,
) {
    if step_attempt == 0 {
        errors.push(FieldError::new("step_attempt", "must be at least 1"));
    }
    if exchange_index == 0 {
        errors.push(FieldError::new("exchange_index", "must be at least 1"));
    }
    let expected = match step {
        crate::LoopStepName::Research => Some(ProviderRole::Researcher),
        crate::LoopStepName::Analysis => Some(ProviderRole::Analyzer),
        crate::LoopStepName::SpecCreation => Some(ProviderRole::SpecWriter),
        crate::LoopStepName::SpecReview => Some(ProviderRole::SpecReviewer),
        crate::LoopStepName::Development => Some(ProviderRole::Developer),
        crate::LoopStepName::OutputReview => Some(ProviderRole::OutputReviewer),
        crate::LoopStepName::Testing | crate::LoopStepName::EvalReport => None,
    };
    if expected != Some(role) {
        errors.push(FieldError::new("role", "does not match the loop step"));
    }
}

fn validate_artifact_reference(
    errors: &mut Vec<FieldError>,
    field: &str,
    reference: &crate::ArtifactReference,
) {
    validate_safe_relative_path(errors, &format!("{field}.path"), &reference.path);
    validate_lowercase_sha256_digest(errors, &format!("{field}.digest"), &reference.digest);
}

fn outcome_matches_role(role: ProviderRole, outcome: ProviderExchangeOutcome) -> bool {
    use ProviderExchangeOutcome as Outcome;
    matches!(outcome, Outcome::InvalidResponse | Outcome::ProviderFailure)
        || match role {
            ProviderRole::Researcher | ProviderRole::Analyzer | ProviderRole::SpecWriter => {
                matches!(
                    outcome,
                    Outcome::Passed | Outcome::Blocked | Outcome::NeedsContext
                )
            }
            ProviderRole::Developer => matches!(
                outcome,
                Outcome::PatchProposed | Outcome::Blocked | Outcome::NeedsContext
            ),
            ProviderRole::SpecReviewer => {
                matches!(
                    outcome,
                    Outcome::ApproveSpec | Outcome::RequestChanges | Outcome::Reject
                )
            }
            ProviderRole::OutputReviewer => matches!(
                outcome,
                Outcome::ApproveForTests | Outcome::RequestChanges | Outcome::Reject
            ),
        }
}

fn exchange_record_path(reference: &ProviderExchangeRecordReference) -> String {
    let phase = match reference.phase {
        ProviderExchangePhase::Request => "request",
        ProviderExchangePhase::Response => "response",
    };
    let kind = match reference.kind {
        ProviderExchangeKind::Initial => "initial",
        ProviderExchangeKind::JsonRepair => "json-repair",
        ProviderExchangeKind::ContextRetry => "context-retry",
    };
    format!(
        "artifacts/{}.attempt-{:03}.exchange-{:03}.{kind}.{phase}.record.json",
        exchange_step_stem(reference.step),
        reference.step_attempt,
        reference.exchange_index
    )
}

fn exchange_step_stem(step: crate::LoopStepName) -> String {
    let index = match step {
        crate::LoopStepName::Research => 1,
        crate::LoopStepName::Analysis => 2,
        crate::LoopStepName::SpecCreation => 3,
        crate::LoopStepName::SpecReview => 4,
        crate::LoopStepName::Development => 5,
        crate::LoopStepName::OutputReview => 6,
        crate::LoopStepName::Testing => 7,
        crate::LoopStepName::EvalReport => 8,
    };
    let slug = match step {
        crate::LoopStepName::Research => "research",
        crate::LoopStepName::Analysis => "analysis",
        crate::LoopStepName::SpecCreation => "spec",
        crate::LoopStepName::SpecReview => "spec-review",
        crate::LoopStepName::Development => "development",
        crate::LoopStepName::OutputReview => "output-review",
        crate::LoopStepName::Testing => "testing",
        crate::LoopStepName::EvalReport => "eval-report",
    };
    format!("{index:02}-{slug}")
}

pub fn sha256_digest_file(path: &Path) -> Result<String, std::io::Error> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn load_struct<T>(kind: &str, path: &Path) -> ValidationResult<T>
where
    T: DeserializeOwned,
{
    let contents = fs::read_to_string(path).map_err(|err| {
        ValidationReport::invalid(
            kind,
            Some(path),
            vec![FieldError::new(
                "file",
                format!("could not read file: {err}"),
            )],
        )
    })?;

    let extension = path.extension().and_then(|value| value.to_str());
    match extension {
        Some("json") => serde_json::from_str(&contents).map_err(parse_error(kind, path)),
        _ => serde_yaml::from_str(&contents).map_err(parse_error(kind, path)),
    }
}

fn parse_error<'a, E>(kind: &'a str, path: &'a Path) -> impl FnOnce(E) -> ValidationReport + 'a
where
    E: Display,
{
    move |err| {
        ValidationReport::invalid(
            kind,
            Some(path),
            vec![FieldError::new(
                "file",
                format!("could not parse file: {err}"),
            )],
        )
    }
}

fn require_non_empty(errors: &mut Vec<FieldError>, field: impl Into<String>, value: &str) {
    if value.trim().is_empty() {
        errors.push(FieldError::new(field, "must not be empty"));
    }
}

fn validate_safe_relative_path(errors: &mut Vec<FieldError>, field: &str, value: &str) {
    if value.trim().is_empty() {
        errors.push(FieldError::new(field, "must not be empty"));
        return;
    }

    if value != value.trim() || value.chars().any(char::is_control) {
        errors.push(FieldError::new(
            field,
            "must be an unambiguous relative path",
        ));
        return;
    }

    let portable = value.replace('\\', "/");
    let bytes = portable.as_bytes();
    let has_windows_prefix = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if portable.starts_with('/') || has_windows_prefix {
        errors.push(FieldError::new(field, "must be a relative path"));
        return;
    }

    if portable.split('/').any(|component| component == "..") {
        errors.push(FieldError::new(field, "must not contain parent traversal"));
    }
}

fn validate_non_empty_list(
    errors: &mut Vec<FieldError>,
    field: &str,
    values: &[String],
    empty_message: &str,
) {
    if values.is_empty() {
        errors.push(FieldError::new(field, empty_message));
        return;
    }

    for (index, value) in values.iter().enumerate() {
        require_non_empty(errors, format!("{field}[{index}]"), value);
    }
}

fn validate_list_entries(errors: &mut Vec<FieldError>, field: &str, values: &[String]) {
    for (index, value) in values.iter().enumerate() {
        require_non_empty(errors, format!("{field}[{index}]"), value);
    }
}

fn validate_sha256_digest(errors: &mut Vec<FieldError>, field: &str, value: &str) {
    let digest = value.strip_prefix("sha256:");
    if digest.is_none() {
        errors.push(FieldError::new(field, "must start with sha256:"));
        return;
    }

    let digest = digest.unwrap();
    if digest.len() != 64 || !digest.chars().all(|item| item.is_ascii_hexdigit()) {
        errors.push(FieldError::new(
            field,
            "must contain a 64-character hexadecimal SHA-256 digest",
        ));
    }
}

fn validate_lowercase_sha256_digest(errors: &mut Vec<FieldError>, field: &str, value: &str) {
    if value.len() != 64
        || !value
            .chars()
            .all(|item| item.is_ascii_digit() || matches!(item, 'a'..='f'))
    {
        errors.push(FieldError::new(
            field,
            "must be a lowercase 64-character hexadecimal SHA-256 digest",
        ));
    }
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{canonical_json_bytes, canonical_sha256_digest};
    use serde::ser::{SerializeMap, Serializer};

    const POLICY_GATE_REVIEW_CATEGORIES: &[&str] = &[
        "dependency_changes",
        "database_migrations",
        "auth_code",
        "payment_code",
        "privacy_sensitive_code",
        "network_permission_changes",
        "ci_changes",
        "eval_changes",
        "policy_changes",
        "updater_changes",
        "signing_changes",
    ];

    #[test]
    fn valid_goal_example_loads() {
        let goal: GoalSpec = serde_yaml::from_str(crate::templates::ADAPTIVE_GOAL_YAML)
            .expect("template should parse");

        assert_eq!(goal.goal_id, "reduce_time_to_first_note");
        assert!(validate_goal_spec(&goal).is_empty());
    }

    #[test]
    fn invalid_goal_reports_actionable_fields() {
        let goal: GoalSpec = serde_yaml::from_str(
            r#"goal_id: ""
name: Missing objective details
status: active
objective:
  metric: ""
  direction: increase
  minimum_effect_size: 0
guardrails: {}
allowed_change_types:
  updater_code: auto_pr
"#,
        )
        .expect("goal should parse");
        let errors = validate_goal_spec(&goal);
        let fields: Vec<&str> = errors.iter().map(|error| error.field.as_str()).collect();

        assert!(fields.contains(&"goal_id"));
        assert!(fields.contains(&"objective.metric"));
        assert!(fields.contains(&"objective.minimum_effect_size"));
    }

    #[test]
    fn invalid_goal_rejects_unknown_fields() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let goal_path = temp_dir.path().join("adaptive.yaml");
        std::fs::write(
            &goal_path,
            format!(
                "{}\nunexpected_policy_escape: true\n",
                crate::templates::ADAPTIVE_GOAL_YAML
            ),
        )
        .expect("write goal");

        let report = load_goal_file(&goal_path).unwrap_err();

        assert!(report.errors.iter().any(
            |error| error.field == "file" && error.message.contains("unexpected_policy_escape")
        ));
    }

    #[test]
    fn invalid_goal_rejects_nan_effect_size() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let goal_path = temp_dir.path().join("adaptive.yaml");
        std::fs::write(
            &goal_path,
            r#"goal_id: reduce_time_to_first_note
name: Reduce time to first note
status: active
objective:
  metric: median_time_between.app_opened_and.note_created
  direction: decrease
  minimum_effect_size: .nan
guardrails:
  no_new_permissions: true
allowed_change_types:
  copy_changes: auto_pr
"#,
        )
        .expect("write goal");

        let report = load_goal_file(&goal_path).unwrap_err();

        assert!(report
            .errors
            .iter()
            .any(|error| error.field == "objective.minimum_effect_size"));
    }

    #[test]
    fn valid_policy_example_loads() {
        let policy: Policy = serde_json::from_str(crate::templates::DEFAULT_POLICY_JSON)
            .expect("template should parse");

        assert_eq!(policy.default_autonomy_level, 1);
        assert!(validate_policy(&policy).is_empty());
    }

    #[test]
    fn default_and_example_policies_require_review_for_every_policy_gate_category() {
        let policy_sources = [
            ("default template", crate::templates::DEFAULT_POLICY_JSON),
            (
                "adaptive-notes example",
                include_str!("../../../examples/adaptive-notes/seaf.policy.json"),
            ),
        ];

        for (name, source) in policy_sources {
            let policy: Policy = serde_json::from_str(source).expect("policy should parse");
            let expected_categories: std::collections::BTreeSet<&str> =
                POLICY_GATE_REVIEW_CATEGORIES.iter().copied().collect();
            let review_categories: std::collections::BTreeSet<&str> = policy
                .requires_human_review
                .iter()
                .map(String::as_str)
                .collect();
            let missing_categories: Vec<&str> = expected_categories
                .difference(&review_categories)
                .copied()
                .collect();
            let unexpected_categories: Vec<&str> = review_categories
                .difference(&expected_categories)
                .copied()
                .collect();

            assert!(
                missing_categories.is_empty(),
                "{name} policy must require human review for every policy-gate category; missing: {missing_categories:?}"
            );
            assert!(
                unexpected_categories.is_empty(),
                "{name} policy must not add review defaults outside policy-gate categories; unexpected: {unexpected_categories:?}"
            );
            assert!(validate_policy(&policy).is_empty());
        }
    }

    #[test]
    fn invalid_policy_reports_agent_guard_fields() {
        let policy: Policy = serde_json::from_str(
            r#"{
  "policy_id": "",
  "default_autonomy_level": 9,
  "forbidden_paths": [],
  "requires_human_review": [""],
  "allowed_without_review": []
}"#,
        )
        .expect("policy should parse");
        let errors = validate_policy(&policy);
        let fields: Vec<&str> = errors.iter().map(|error| error.field.as_str()).collect();

        assert!(fields.contains(&"policy_id"));
        assert!(fields.contains(&"default_autonomy_level"));
        assert!(fields.contains(&"forbidden_paths"));
        assert!(fields.contains(&"requires_human_review[0]"));
    }

    #[test]
    fn release_capsule_rejects_digest_mismatch() {
        let capsule = ReleaseCapsule {
            release_id: "rel_0.1.0".to_string(),
            app_id: "dev.seaf.notes".to_string(),
            version: "0.1.0".to_string(),
            source_commit: "abc123".to_string(),
            agent_task_id: None,
            goal_id: "reduce_time_to_first_note".to_string(),
            build_recipe_hash: None,
            artifact_digest: "sha256:not-a-digest".to_string(),
            eval_report_digest: format!("sha256:{}", "a".repeat(64)),
            migration_plan: None,
            rollback_plan: "rollback/0.0.9".to_string(),
            signatures: Vec::new(),
            rollout_policy: crate::RolloutPolicy {
                channel: crate::RolloutChannel::Canary,
                initial_percentage: 5,
            },
        };

        let errors = validate_release_capsule(&capsule);

        assert!(errors.iter().any(|error| error.field == "artifact_digest"));
    }

    #[test]
    fn valid_release_capsule_example_loads() {
        let capsule: ReleaseCapsule = serde_json::from_str(
            r#"{
  "release_id": "rel_0.1.0",
  "app_id": "dev.seaf.adaptive-notes",
  "version": "0.1.0",
  "source_commit": "abc123",
  "agent_task_id": "task_reduce_time_to_first_note_001",
  "goal_id": "reduce_time_to_first_note",
  "build_recipe_hash": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
  "artifact_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "eval_report_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "rollback_plan": "rollback/0.0.9",
  "signatures": ["dev-placeholder-signature"],
  "rollout_policy": {
    "channel": "canary",
    "initial_percentage": 5
  }
}"#,
        )
        .expect("capsule should parse");

        assert_eq!(capsule.goal_id, "reduce_time_to_first_note");
        assert!(validate_release_capsule(&capsule).is_empty());
    }

    #[test]
    fn agent_capability_requires_access_lists() {
        let error = serde_json::from_str::<crate::AgentCapability>(
            r#"{
  "agent": "patch-agent",
  "network": "restricted"
}"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("can_read"));
    }

    #[test]
    fn eval_report_requires_checks() {
        let error = serde_json::from_str::<crate::EvalReport>(
            r#"{
  "eval_report_id": "eval_01",
  "patch_id": "patch_01",
  "goal_id": "reduce_time_to_first_note",
  "passed": true,
  "summary": "Missing checks should fail closed.",
  "risk_level": "low",
  "decision": "approve_for_release"
}"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("checks"));
    }

    #[test]
    fn failed_eval_report_must_reject() {
        let report: EvalReport = serde_json::from_str(
            r#"{
  "eval_report_id": "eval_01",
  "patch_id": "patch_01",
  "goal_id": "reduce_time_to_first_note",
  "passed": false,
  "summary": "Failed report should not be approved.",
  "checks": [
    { "name": "unit_tests", "status": "failed" }
  ],
  "risk_level": "high",
  "decision": "approve_for_release"
}"#,
        )
        .expect("report should parse");

        let errors = validate_eval_report(&report);

        assert!(errors.iter().any(|error| error.field == "decision"));
    }

    #[test]
    fn integrated_eval_report_accepts_exact_bound_log_evidence() {
        let report = integrated_eval_report_fixture();

        assert!(validate_eval_report(&report).is_empty());
    }

    fn integrated_eval_report_fixture() -> EvalReport {
        serde_json::from_value(serde_json::json!({
            "eval_report_id": "eval_run-1",
            "patch_id": "run-1",
            "goal_id": "goal-1",
            "passed": true,
            "summary": "Integrated evaluation passed.",
            "checks": [{
                "name": "unit",
                "status": "passed",
                "duration_ms": 10,
                "stdout_path": "artifacts/eval/unit.stdout.log",
                "stdout_digest": "1".repeat(64),
                "stderr_path": "artifacts/eval/unit.stderr.log",
                "stderr_digest": "2".repeat(64)
            }],
            "risk_level": "low",
            "decision": "approve_for_human_review",
            "loop_evidence": {
                "schema_version": 1,
                "run_id": "run-1",
                "ticket_id": "ticket-1",
                "ticket_digest": "3".repeat(64),
                "eval_config": {
                    "path": "inputs/eval-config.json",
                    "digest": "4".repeat(64)
                },
                "candidate_diff": {
                    "path": "artifacts/candidate-patch.applied.diff",
                    "digest": "5".repeat(64)
                },
                "starting_head": "6".repeat(40),
                "human_approval_digest": "7".repeat(64),
                "policy_decision_digest": "8".repeat(64),
                "testing_evidence": {
                    "path": "artifacts/07-testing.json",
                    "digest": "9".repeat(64)
                }
            }
        }))
        .expect("integrated EvalReport should deserialize")
    }

    #[test]
    fn integrated_eval_report_rejects_missing_log_digest_pair() {
        let mut report = integrated_eval_report_fixture();
        report.checks[0].stdout_digest = None;

        let fields = validate_eval_report(&report)
            .into_iter()
            .map(|error| error.field)
            .collect::<Vec<_>>();

        assert!(fields.contains(&"checks[0].stdout_digest".to_string()));
    }

    #[test]
    fn integrated_eval_report_rejects_nonportable_log_and_reference_paths() {
        let mut report = integrated_eval_report_fixture();
        report.checks[0].stdout_path = Some(r"artifacts\eval\unit.stdout.log".to_string());
        let evidence = report.loop_evidence.as_mut().unwrap();
        evidence.candidate_diff.path = "C:/candidate.diff".to_string();
        evidence.testing_evidence.path = "artifacts//07-testing.json".to_string();

        let fields = validate_eval_report(&report)
            .into_iter()
            .map(|error| error.field)
            .collect::<Vec<_>>();

        for expected in [
            "checks[0].stdout_path",
            "loop_evidence.candidate_diff.path",
            "loop_evidence.testing_evidence.path",
        ] {
            assert!(fields.contains(&expected.to_string()), "{expected}");
        }
    }

    #[test]
    fn integrated_eval_report_rejects_reused_log_paths() {
        let mut report = integrated_eval_report_fixture();
        report.checks[0].stderr_path = report.checks[0].stdout_path.clone();

        let fields = validate_eval_report(&report)
            .into_iter()
            .map(|error| error.field)
            .collect::<Vec<_>>();

        assert!(fields.contains(&"checks[0].stderr_path".to_string()));
    }

    #[test]
    fn integrated_eval_report_rejects_substituted_run_and_authority_digests() {
        let mut report = integrated_eval_report_fixture();
        let evidence = report.loop_evidence.as_mut().unwrap();
        evidence.run_id = "other-run".to_string();
        evidence.ticket_digest = "UPPER".to_string();
        evidence.eval_config.path.clear();
        evidence.candidate_diff.digest = "sha256:bad".to_string();
        evidence.starting_head = "not-a-head".to_string();
        evidence.human_approval_digest = "bad".to_string();
        evidence.policy_decision_digest = "bad".to_string();
        evidence.testing_evidence.digest = "bad".to_string();

        let fields = validate_eval_report(&report)
            .into_iter()
            .map(|error| error.field)
            .collect::<Vec<_>>();

        for expected in [
            "loop_evidence.run_id",
            "loop_evidence.ticket_digest",
            "loop_evidence.eval_config.path",
            "loop_evidence.candidate_diff.digest",
            "loop_evidence.starting_head",
            "loop_evidence.human_approval_digest",
            "loop_evidence.policy_decision_digest",
            "loop_evidence.testing_evidence.digest",
        ] {
            assert!(fields.contains(&expected.to_string()), "{expected}");
        }
    }

    #[test]
    fn standalone_eval_report_remains_valid_without_log_digests_or_loop_evidence() {
        let report: EvalReport = serde_json::from_value(serde_json::json!({
            "eval_report_id": "standalone",
            "patch_id": "patch",
            "goal_id": "goal",
            "passed": true,
            "summary": "Standalone compatibility.",
            "checks": [{
                "name": "unit",
                "status": "passed",
                "stdout_path": "/tmp/unit.stdout.log",
                "stderr_path": "/tmp/unit.stderr.log"
            }],
            "risk_level": "low",
            "decision": "approve_for_release"
        }))
        .unwrap();

        assert!(validate_eval_report(&report).is_empty());
    }

    #[test]
    fn public_eval_report_schema_exposes_closed_optional_integrated_evidence() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../specs/eval-report.schema.json")).unwrap();

        assert_eq!(
            schema["properties"]["checks"]["items"]["properties"]["stdout_digest"]["pattern"],
            "^[a-f0-9]{64}$"
        );
        assert_eq!(
            schema["properties"]["loop_evidence"]["properties"]["schema_version"]["const"],
            1
        );
        assert_eq!(
            schema["properties"]["loop_evidence"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn event_requires_object_payload() {
        let event = SeafEvent {
            event_id: "evt_1".to_string(),
            name: "note.created".to_string(),
            timestamp: "2026-06-30T00:00:00.000Z".to_string(),
            source: "adaptive-notes".to_string(),
            privacy_level: crate::PrivacyLevel::Aggregated,
            payload: serde_json::json!("raw text"),
        };

        let errors = validate_seaf_event(&event);

        assert!(errors.iter().any(|error| error.field == "payload"));
    }

    #[test]
    fn invalid_goal_rejects_non_object_rollout() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let goal_path = temp_dir.path().join("adaptive.yaml");
        std::fs::write(
            &goal_path,
            r#"goal_id: reduce_time_to_first_note
name: Reduce time to first note
status: active
objective:
  metric: median_time_between.app_opened_and.note_created
  direction: decrease
  minimum_effect_size: 0.15
guardrails:
  no_new_permissions: true
allowed_change_types:
  copy_changes: auto_pr
rollout: canary
"#,
        )
        .expect("write goal");

        let report = load_goal_file(&goal_path).unwrap_err();

        assert!(report.errors.iter().any(|error| error.field == "rollout"));
    }

    #[test]
    fn valid_ticket_fixture_loads() {
        let ticket_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/local-loop/tickets/add-health-command.yaml");
        let ticket = load_ticket_file(&ticket_path).expect("demo ticket fixture should load");

        assert_eq!(ticket.ticket_id, "T-LOCAL-001");
        assert!(validate_ticket_spec(&ticket).is_empty());
    }

    #[test]
    fn valid_project_config_fixture_loads() {
        let config_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/local-loop/project-configs/valid-project-config.json");
        let config =
            load_project_config_file(&config_path).expect("project config fixture should load");

        assert_eq!(config.policy_path, "seaf.policy.json");
        assert!(validate_project_config(&config).is_empty());
    }

    #[test]
    fn project_config_unknown_fields_fail_closed() {
        let config_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/local-loop/project-configs/invalid-unknown-field.json");
        let report = load_project_config_file(&config_path).unwrap_err();

        assert!(report
            .errors
            .iter()
            .any(|error| error.field == "file" && error.message.contains("unexpected_escape")));
    }

    #[test]
    fn project_config_rejects_empty_absolute_and_parent_traversal_policy_paths() {
        let fixtures = [
            "invalid-empty-policy-path.json",
            "invalid-absolute-policy-path.json",
            "invalid-parent-traversal-policy-path.json",
        ];

        for fixture in fixtures {
            let config_path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/local-loop/project-configs")
                .join(fixture);
            let report = load_project_config_file(&config_path).unwrap_err();

            assert!(
                report
                    .errors
                    .iter()
                    .any(|error| error.field == "policy_path"),
                "{fixture} should fail policy_path validation: {:?}",
                report.errors
            );
        }
    }

    #[test]
    fn project_config_runtime_and_schema_reject_control_characters() {
        let fixtures = [
            "invalid-newline-policy-path.json",
            "invalid-nul-policy-path.json",
            "invalid-control-policy-path.json",
        ];

        for fixture in fixtures {
            let config_path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/local-loop/project-configs")
                .join(fixture);
            let report = load_project_config_file(&config_path).unwrap_err();

            assert!(
                report
                    .errors
                    .iter()
                    .any(|error| error.field == "policy_path"),
                "{fixture} should fail policy_path validation: {:?}",
                report.errors
            );
        }

        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../specs/project-config.schema.json"))
                .expect("project config schema");
        let pattern = schema["properties"]["policy_path"]["pattern"]
            .as_str()
            .expect("policy_path pattern");
        assert!(
            pattern.contains(r"\u0000-\u001F") && pattern.contains(r"\u007F-\u009F"),
            "schema must reject the same control-character ranges as Rust: {pattern}"
        );
    }

    #[test]
    fn canonical_typed_serialization_and_digest_are_deterministic() {
        let config = ProjectConfig {
            policy_path: "seaf.policy.json".to_string(),
        };

        let canonical = canonical_json_bytes(&config).expect("canonical config");
        let digest = canonical_sha256_digest(&config).expect("canonical config digest");

        assert_eq!(
            canonical,
            b"{\n  \"policy_path\": \"seaf.policy.json\"\n}".to_vec()
        );
        assert_eq!(
            digest,
            "a4e211c86b6ca52f6b08601a02dc523fe1d4c8dd0a5ab1a68bf1e7fab9bb1be5"
        );
    }

    #[test]
    fn canonical_serialization_recursively_sorts_semantically_equivalent_objects() {
        let ascending = OrderedObject { reverse: false };
        let descending = OrderedObject { reverse: true };

        let ascending_bytes = canonical_json_bytes(&ascending).expect("ascending canonical JSON");
        let descending_bytes =
            canonical_json_bytes(&descending).expect("descending canonical JSON");

        assert_eq!(ascending_bytes, descending_bytes);
        assert_eq!(
            canonical_sha256_digest(&ascending).expect("ascending digest"),
            canonical_sha256_digest(&descending).expect("descending digest")
        );
    }

    struct OrderedObject {
        reverse: bool,
    }

    impl Serialize for OrderedObject {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut map = serializer.serialize_map(Some(2))?;
            if self.reverse {
                map.serialize_entry("zeta", &3)?;
                map.serialize_entry("nested", &OrderedNested { reverse: true })?;
            } else {
                map.serialize_entry("nested", &OrderedNested { reverse: false })?;
                map.serialize_entry("zeta", &3)?;
            }
            map.end()
        }
    }

    struct OrderedNested {
        reverse: bool,
    }

    impl Serialize for OrderedNested {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut map = serializer.serialize_map(Some(2))?;
            if self.reverse {
                map.serialize_entry("beta", &2)?;
                map.serialize_entry("alpha", &1)?;
            } else {
                map.serialize_entry("alpha", &1)?;
                map.serialize_entry("beta", &2)?;
            }
            map.end()
        }
    }

    #[test]
    fn invalid_ticket_reports_contract_fields() {
        let ticket_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/local-loop/tickets/invalid-empty-ticket.yaml");
        let report = load_ticket_file(&ticket_path).unwrap_err();
        let errors = report.errors;
        let fields: Vec<&str> = errors.iter().map(|error| error.field.as_str()).collect();

        assert!(fields.contains(&"ticket_id"));
        assert!(fields.contains(&"goal_id"));
        assert!(fields.contains(&"title"));
        assert!(fields.contains(&"problem"));
        assert!(fields.contains(&"context.relevant_files[0]"));
        assert!(fields.contains(&"context.forbidden_files[0]"));
        assert!(fields.contains(&"autonomy.level"));
        assert!(fields.contains(&"autonomy.allow_shell_commands[0]"));
        assert!(fields.contains(&"acceptance_criteria"));
        assert!(fields.contains(&"eval.config"));
    }

    #[test]
    fn ticket_unknown_fields_fail_closed() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let ticket_path = temp_dir.path().join("ticket.yaml");
        std::fs::write(
            &ticket_path,
            r#"ticket_id: T-LOCAL-001
goal_id: local_agent_loop_mvp
title: Add a local health check command
status: ready
priority: p1
problem: Keep the local loop safe.
context:
  relevant_files:
    - crates/seaf-core/src/models.rs
  forbidden_files: []
autonomy:
  level: 1
  apply_patch: true
acceptance_criteria:
  - Unknown fields must not be accepted.
unexpected_escape: true
"#,
        )
        .expect("write ticket");

        let report = load_ticket_file(&ticket_path).unwrap_err();

        assert!(report
            .errors
            .iter()
            .any(|error| error.field == "file" && error.message.contains("unexpected_escape")));
    }

    #[test]
    fn valid_loop_run_fixture_loads() {
        let run_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/local-loop/runs/valid-loop-run.json");
        let run = load_loop_run_file(&run_path).expect("loop run fixture should load");

        assert_eq!(run.run_id, "loop_20260701_001");
        assert_eq!(run.input_digests.repository, "d".repeat(64));
        assert_eq!(run.input_digests.eval_config, None);
        assert!(validate_loop_run(&run).is_empty());
    }

    #[test]
    fn unsupported_versions_fail_without_mutating_durable_input_files() {
        let temp_dir = tempfile::tempdir().expect("temp dir");

        let policy_path = temp_dir.path().join("policy.json");
        let policy_bytes = future_version_bytes(crate::templates::DEFAULT_POLICY_JSON);
        std::fs::write(&policy_path, &policy_bytes).expect("write policy");
        let policy_error = load_policy_file(&policy_path).expect_err("future policy must fail");
        assert_version_error(&policy_error);
        assert_eq!(std::fs::read(&policy_path).unwrap(), policy_bytes);

        let ticket_path = temp_dir.path().join("ticket.json");
        let ticket = load_ticket_file(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/local-loop/tickets/add-health-command.yaml"),
        )
        .expect("ticket fixture");
        let ticket_bytes = future_version_value(&ticket);
        std::fs::write(&ticket_path, &ticket_bytes).expect("write ticket");
        let ticket_error = load_ticket_file(&ticket_path).expect_err("future ticket must fail");
        assert_version_error(&ticket_error);
        assert_eq!(std::fs::read(&ticket_path).unwrap(), ticket_bytes);

        let run_path = temp_dir.path().join("run.json");
        let run = load_loop_run_file(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/local-loop/runs/valid-loop-run.json"),
        )
        .expect("run fixture");
        let run_bytes = future_version_value(&run);
        std::fs::write(&run_path, &run_bytes).expect("write run");
        let run_error = load_loop_run_file(&run_path).expect_err("future run must fail");
        assert_version_error(&run_error);
        assert_eq!(std::fs::read(&run_path).unwrap(), run_bytes);

        let report_path = temp_dir.path().join("eval-report.json");
        let report: EvalReport = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "eval_report_id": "eval-version-refusal",
            "patch_id": "patch-version-refusal",
            "goal_id": "goal-version-refusal",
            "passed": false,
            "summary": "Future versions must not mutate source bytes.",
            "checks": [{"name": "version", "status": "failed"}],
            "risk_level": "high",
            "decision": "reject"
        }))
        .expect("current report");
        let report_bytes = future_version_value(&report);
        std::fs::write(&report_path, &report_bytes).expect("write report");
        let report_error =
            load_eval_report_file(&report_path).expect_err("future EvalReport must fail");
        assert_version_error(&report_error);
        assert_eq!(std::fs::read(&report_path).unwrap(), report_bytes);
    }

    fn future_version_bytes(current: &str) -> Vec<u8> {
        let value: serde_json::Value = serde_json::from_str(current).expect("current JSON");
        future_version_json(value)
    }

    fn future_version_value<T: Serialize>(value: &T) -> Vec<u8> {
        future_version_json(serde_json::to_value(value).expect("current artifact serializes"))
    }

    fn future_version_json(mut value: serde_json::Value) -> Vec<u8> {
        value
            .as_object_mut()
            .expect("durable artifact is an object")
            .insert("schema_version".to_string(), serde_json::json!(2));
        serde_json::to_vec_pretty(&value).expect("future fixture serializes")
    }

    fn assert_version_error(report: &ValidationReport) {
        assert!(report.errors.iter().any(|error| {
            error.field == "file"
                && error.message.contains("unsupported")
                && error.message.contains("schema_version")
        }));
    }

    #[test]
    fn loop_run_requires_effective_input_digests() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let run_path = temp_dir.path().join("loop-run.json");
        std::fs::write(
            &run_path,
            r#"{
  "run_id": "loop_20260701_001",
  "ticket_id": "T-LOCAL-001",
  "goal_id": "local_agent_loop_mvp",
  "provider": "ollama",
  "model": "gemma4:e4b-mlx",
  "status": "running",
  "current_step": "development",
  "started_at": "2026-07-01T12:00:00Z",
  "updated_at": "2026-07-01T12:12:00Z",
  "steps": [],
  "policy_decisions": [],
  "eval_report_path": null
}"#,
        )
        .expect("write loop run");

        let report = load_loop_run_file(&run_path).unwrap_err();

        assert!(report
            .errors
            .iter()
            .any(|error| error.field == "file" && error.message.contains("input_digests")));
    }

    #[test]
    fn loop_run_rejects_malformed_effective_input_digests() {
        let run: LoopRun = serde_json::from_str(
            r#"{
  "run_id": "loop_20260701_001",
  "ticket_id": "T-LOCAL-001",
  "goal_id": "local_agent_loop_mvp",
  "provider": "ollama",
  "model": "gemma4:e4b-mlx",
  "input_digests": {
    "ticket": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    "policy": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "config": "cccc",
    "repository": "dddd"
  },
  "status": "running",
  "current_step": "development",
  "started_at": "2026-07-01T12:00:00Z",
  "updated_at": "2026-07-01T12:12:00Z",
  "steps": [],
  "policy_decisions": [],
  "eval_report_path": null
}"#,
        )
        .expect("loop run should parse");
        let errors = validate_loop_run(&run);
        let fields: Vec<&str> = errors.iter().map(|error| error.field.as_str()).collect();

        assert!(fields.contains(&"input_digests.ticket"));
        assert!(fields.contains(&"input_digests.policy"));
        assert!(fields.contains(&"input_digests.config"));
        assert!(fields.contains(&"input_digests.repository"));
    }

    #[test]
    fn loop_run_requires_artifact_path_and_digest_as_an_integrity_pair() {
        let mut run: LoopRun = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/local-loop/runs/valid-loop-run.json"
        )))
        .expect("valid loop run");
        run.steps[0].artifact_digest = None;

        let errors = validate_loop_run(&run);
        let fields: Vec<&str> = errors.iter().map(|error| error.field.as_str()).collect();

        assert!(fields.contains(&"steps[0].artifact_path"));
        assert!(fields.contains(&"steps[0].artifact_digest"));

        let mut reverse: LoopRun = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/local-loop/runs/valid-loop-run.json"
        )))
        .expect("valid loop run");
        reverse.steps[0].artifact_path = None;
        let reverse_errors = validate_loop_run(&reverse);
        let reverse_fields: Vec<&str> = reverse_errors
            .iter()
            .map(|error| error.field.as_str())
            .collect();
        assert!(reverse_fields.contains(&"steps[0].artifact_path"));
        assert!(reverse_fields.contains(&"steps[0].artifact_digest"));
    }

    #[test]
    fn loop_run_rejects_malformed_artifact_digest() {
        let mut run: LoopRun = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/local-loop/runs/valid-loop-run.json"
        )))
        .expect("valid loop run");
        run.steps[0].artifact_digest = Some("sha256:not-canonical".to_string());

        let errors = validate_loop_run(&run);

        assert!(errors
            .iter()
            .any(|error| error.field == "steps[0].artifact_digest"));
    }

    #[test]
    fn loop_run_schema_rejects_string_null_artifact_pair_in_both_directions() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../specs/loop-run.schema.json"))
                .expect("loop run schema");
        let step = &schema["properties"]["steps"]["items"];
        let branches = step["oneOf"]
            .as_array()
            .expect("artifact integrity must use exclusive schema branches");

        let paired_strings = branches
            .iter()
            .find(|branch| {
                branch["properties"]["artifact_path"]["type"] == "string"
                    && branch["properties"]["artifact_digest"]["type"] == "string"
            })
            .expect("schema branch requiring both artifact strings");
        assert_eq!(
            paired_strings["required"],
            serde_json::json!(["artifact_path", "artifact_digest"]),
            "a string path with null/absent digest and a string digest with null/absent path must both fail"
        );

        let paired_nulls = branches
            .iter()
            .find(|branch| {
                branch["properties"]["artifact_path"]["type"] == "null"
                    && branch["properties"]["artifact_digest"]["type"] == "null"
            })
            .expect("schema branch requiring both artifact nulls");
        assert_eq!(
            paired_nulls["required"],
            serde_json::json!(["artifact_path", "artifact_digest"])
        );

        assert!(
            branches.iter().any(|branch| {
                branch["not"]["anyOf"]
                    == serde_json::json!([
                        { "required": ["artifact_path"] },
                        { "required": ["artifact_digest"] }
                    ])
            }),
            "schema must retain the valid both-absent representation"
        );
    }

    #[test]
    fn awaiting_human_review_status_is_closed_and_has_schema_shape_constraints() {
        assert_eq!(
            serde_json::to_value(crate::LoopStatus::AwaitingHumanReview).unwrap(),
            serde_json::json!("awaiting_human_review")
        );
        assert!(serde_json::from_str::<crate::LoopStatus>(r#""awaiting_approval""#).is_err());
        let missing_mode = serde_json::json!({
            "run_id": "waiting",
            "ticket_id": "T-1",
            "goal_id": "G-1",
            "provider": "fake",
            "model": "fake",
            "input_digests": {
                "ticket": "a".repeat(64),
                "policy": "b".repeat(64),
                "config": "c".repeat(64),
                "repository": "d".repeat(64)
            },
            "status": "awaiting_human_review",
            "current_step": "testing",
            "started_at": "1",
            "updated_at": "2",
            "steps": [],
            "policy_decisions": [],
            "candidate_workspace": null
        });
        assert!(serde_json::from_value::<crate::LoopRun>(missing_mode)
            .unwrap_err()
            .to_string()
            .contains("explicit isolated_candidate"));

        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../specs/loop-run.schema.json"))
                .expect("loop run schema");
        assert!(schema["properties"]["status"]["enum"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("awaiting_human_review")));
        let awaiting = schema["allOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|branch| {
                branch["if"]["properties"]["status"]["const"]
                    == serde_json::json!("awaiting_human_review")
            })
            .expect("awaiting state must have a public schema branch");
        assert_eq!(
            awaiting["then"]["properties"]["execution_mode"]["const"],
            serde_json::json!("isolated_candidate")
        );
        assert_eq!(
            awaiting["then"]["properties"]["current_step"]["const"],
            serde_json::json!("testing")
        );
        assert_eq!(
            awaiting["then"]["properties"]["candidate_workspace"]["properties"]["lifecycle"]
                ["const"],
            serde_json::json!("active")
        );
        assert_eq!(
            awaiting["then"]["properties"]["eval_report_path"]["type"],
            serde_json::json!("null")
        );
        assert!(awaiting["then"]["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("provider_exchange_records")));
        let ledger = &awaiting["then"]["properties"]["provider_exchange_records"];
        assert_eq!(ledger["minItems"], 2);
        let ledger_rules = ledger["allOf"].as_array().unwrap();
        assert!(ledger_rules.iter().any(|rule| {
            rule["contains"]["properties"]
                == serde_json::json!({
                    "step": { "const": "output_review" },
                    "role": { "const": "output_reviewer" },
                    "kind": { "const": "initial" },
                    "phase": { "const": "request" }
                })
        }));
        assert!(ledger_rules.iter().any(|rule| {
            rule["contains"]["properties"]
                == serde_json::json!({
                    "step": { "const": "output_review" },
                    "role": { "const": "output_reviewer" },
                    "phase": { "const": "response" }
                })
        }));
        let steps = &awaiting["then"]["properties"]["steps"];
        let occurrence_rules = steps["allOf"]
            .as_array()
            .expect("awaiting steps must close duplicate occurrences by name");
        let shape_rules = steps["items"]["allOf"]
            .as_array()
            .expect("awaiting steps must validate every occurrence by name");
        for (name, status) in [
            ("development", "completed"),
            ("output_review", "passed"),
            ("testing", "pending"),
            ("eval_report", "pending"),
        ] {
            let occurrence = occurrence_rules
                .iter()
                .find(|rule| rule["contains"]["properties"]["name"]["const"] == name)
                .expect("name-only occurrence rule");
            assert_eq!(
                occurrence["contains"]["required"],
                serde_json::json!(["name"])
            );
            assert_eq!(
                occurrence["contains"]["properties"]
                    .as_object()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(occurrence["minContains"], 1);
            assert_eq!(occurrence["maxContains"], 1);

            let shape = shape_rules
                .iter()
                .find(|rule| rule["if"]["properties"]["name"]["const"] == name)
                .expect("per-name shape rule");
            assert_eq!(shape["if"]["required"], serde_json::json!(["name"]));
            assert_eq!(shape["then"]["properties"]["status"]["const"], status);
        }
        let output_shape = shape_rules
            .iter()
            .find(|rule| rule["if"]["properties"]["name"]["const"] == "output_review")
            .unwrap();
        assert_eq!(
            output_shape["then"]["required"],
            serde_json::json!(["artifact_path", "artifact_digest"])
        );
        for name in ["testing", "eval_report"] {
            let shape = shape_rules
                .iter()
                .find(|rule| rule["if"]["properties"]["name"]["const"] == name)
                .unwrap();
            assert_eq!(shape["then"]["properties"]["artifact_path"]["type"], "null");
            assert_eq!(
                shape["then"]["properties"]["artifact_digest"]["type"],
                "null"
            );
        }
    }

    #[test]
    fn approved_status_requires_closed_exact_human_evidence_and_pending_testing_shape() {
        let run = approved_run_fixture();
        assert_eq!(
            serde_json::to_value(crate::LoopStatus::Approved).unwrap(),
            serde_json::json!("approved")
        );
        assert!(validate_loop_run(&run).is_empty());
        let mut missing_mode = serde_json::to_value(&run).unwrap();
        missing_mode
            .as_object_mut()
            .unwrap()
            .remove("execution_mode");
        assert!(serde_json::from_value::<crate::LoopRun>(missing_mode)
            .unwrap_err()
            .to_string()
            .contains("explicit isolated_candidate"));

        let mut missing = run.clone();
        missing.human_approval = None;
        assert!(validate_loop_run(&missing)
            .iter()
            .any(|error| error.field == "human_approval"));
        let mut smuggled = run.clone();
        smuggled.status = crate::LoopStatus::AwaitingHumanReview;
        assert!(validate_loop_run(&smuggled)
            .iter()
            .any(|error| error.field == "human_approval"));
        let mut malformed = run.clone();
        malformed.human_approval.as_mut().unwrap().schema_version = 2;
        malformed.human_approval.as_mut().unwrap().run_id = "other".to_string();
        malformed
            .human_approval
            .as_mut()
            .unwrap()
            .candidate_diff
            .digest = "f".repeat(64);
        malformed.human_approval.as_mut().unwrap().starting_head = "a".repeat(40);
        malformed
            .human_approval
            .as_mut()
            .unwrap()
            .output_review_request
            .step_attempt = 2;
        malformed
            .human_approval
            .as_mut()
            .unwrap()
            .policy_decision_digest = "0".repeat(64);
        let fields = validate_loop_run(&malformed)
            .into_iter()
            .map(|error| error.field)
            .collect::<Vec<_>>();
        for expected in [
            "human_approval.schema_version",
            "human_approval.run_id",
            "human_approval.candidate_diff.digest",
            "human_approval.starting_head",
            "human_approval.output_review_response",
            "human_approval.policy_decision_digest",
        ] {
            assert!(fields.iter().any(|field| field == expected), "{expected}");
        }
        let mut duplicate_policy = run.clone();
        duplicate_policy
            .policy_decisions
            .push(duplicate_policy.policy_decisions[0].clone());
        assert!(validate_loop_run(&duplicate_policy)
            .iter()
            .any(|error| error.field == "human_approval.policy_decision_digest"));
        let mut testing_ran = run.clone();
        testing_ran
            .steps
            .iter_mut()
            .find(|step| step.name == crate::LoopStepName::Testing)
            .unwrap()
            .status = crate::LoopStepStatus::Running;
        assert!(validate_loop_run(&testing_ran)
            .iter()
            .any(|error| error.field.ends_with(".status")));

        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../specs/loop-run.schema.json")).unwrap();
        assert!(schema["properties"]["status"]["enum"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("approved")));
        assert_eq!(
            schema["properties"]["human_approval"]["anyOf"][0]["properties"]["schema_version"]
                ["const"],
            1
        );
        let approved = schema["allOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|branch| branch["if"]["properties"]["status"]["const"] == "approved")
            .expect("Approved must have a public schema branch");
        assert_eq!(approved["then"]["allOf"][0]["$ref"], "#/allOf/0/then");
    }

    #[test]
    fn eval_passed_status_serializes_as_closed_terminal_authority() {
        assert_eq!(
            serde_json::to_value(crate::LoopStatus::EvalPassed).unwrap(),
            serde_json::json!("eval_passed")
        );
    }

    #[test]
    fn eval_passed_requires_exact_approval_bound_terminal_chain() {
        let run = final_eval_run_fixture(true);
        assert!(validate_loop_run(&run).is_empty());

        let mut missing_approval = run.clone();
        missing_approval.human_approval = None;
        assert!(validate_loop_run(&missing_approval)
            .iter()
            .any(|error| error.field == "human_approval"));

        let mut mismatched_report = run.clone();
        mismatched_report.eval_report_path = Some("artifacts/substituted.json".to_string());
        assert!(validate_loop_run(&mismatched_report)
            .iter()
            .any(|error| error.field == "eval_report_path"));

        let mut missing_testing_digest = run.clone();
        missing_testing_digest
            .steps
            .iter_mut()
            .find(|step| step.name == crate::LoopStepName::Testing)
            .unwrap()
            .artifact_digest = None;
        assert!(validate_loop_run(&missing_testing_digest)
            .iter()
            .any(|error| error.field.contains("artifact")));

        let mut duplicate = run;
        duplicate.steps.push(duplicate.steps[7].clone());
        assert!(validate_loop_run(&duplicate)
            .iter()
            .any(|error| error.field == "steps"));

        let mut nonportable = final_eval_run_fixture(true);
        nonportable.steps[6].artifact_path = Some(r"artifacts\07-testing.json".to_string());
        assert!(validate_loop_run(&nonportable)
            .iter()
            .any(|error| error.field == "steps[6].artifact_path"));
    }

    #[test]
    fn promoted_requires_exact_eval_passed_predecessor_and_closed_fresh_evidence() {
        let mut predecessor = final_eval_run_fixture(true);
        predecessor.updated_at = "10".to_string();
        let predecessor_digest = canonical_sha256_digest(&predecessor).unwrap();
        let approval = predecessor.human_approval.as_ref().unwrap();
        let mut promoted = predecessor.clone();
        promoted.status = crate::LoopStatus::Promoted;
        promoted.updated_at = "11".to_string();
        promoted.promotion = Some(crate::PromotionEvidence {
            schema_version: 1,
            run_id: promoted.run_id.clone(),
            reviewer: "fresh-reviewer@example.invalid".to_string(),
            promoted_at: "11".to_string(),
            intent: crate::ArtifactReference {
                path: "artifacts/09-promotion.intent.json".to_string(),
                digest: "9".repeat(64),
            },
            candidate_diff: approval.candidate_diff.clone(),
            testing_evidence: crate::ArtifactReference {
                path: "artifacts/07-testing.json".to_string(),
                digest: "7".repeat(64),
            },
            eval_report: crate::ArtifactReference {
                path: "artifacts/08-eval-report.json".to_string(),
                digest: "8".repeat(64),
            },
            policy_decision_digest: approval.policy_decision_digest.clone(),
            target_head: approval.starting_head.clone(),
            eval_passed_run_digest: predecessor_digest,
            eval_passed_updated_at: predecessor.updated_at.clone(),
        });
        assert_eq!(
            serde_json::to_value(crate::LoopStatus::Promoted).unwrap(),
            serde_json::json!("promoted")
        );
        assert!(validate_loop_run(&promoted).is_empty());

        let mut substituted = promoted.clone();
        substituted
            .promotion
            .as_mut()
            .unwrap()
            .eval_passed_run_digest = "f".repeat(64);
        assert!(validate_loop_run(&substituted)
            .iter()
            .any(|error| error.field == "promotion.eval_passed_run_digest"));
        let mut stale_time = promoted.clone();
        stale_time.promotion.as_mut().unwrap().promoted_at = "9".to_string();
        stale_time.updated_at = "9".to_string();
        assert!(validate_loop_run(&stale_time)
            .iter()
            .any(|error| error.field == "promotion.promoted_at"));
        let mut smuggled = predecessor;
        smuggled.promotion = promoted.promotion;
        assert!(validate_loop_run(&smuggled)
            .iter()
            .any(|error| error.field == "promotion"));

        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../specs/loop-run.schema.json")).unwrap();
        assert!(schema["properties"]["status"]["enum"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("promoted")));
        assert!(schema["allOf"].as_array().unwrap().iter().any(|branch| {
            branch["if"]["properties"]["status"]["const"] == "promoted"
                && branch["then"]["allOf"][0]["$ref"] == "#/allOf/3/then"
        }));
    }

    #[test]
    fn public_loop_schema_exposes_eval_passed_and_reported_failure_branches() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../specs/loop-run.schema.json")).unwrap();
        assert!(schema["properties"]["status"]["enum"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("eval_passed")));
        assert!(schema["allOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|branch| branch["if"]["properties"]["status"]["const"] == "eval_passed"));
        assert!(schema["allOf"].as_array().unwrap().iter().any(|branch| {
            branch["if"]["properties"]["status"]["const"] == "failed"
                && branch["if"]["required"]
                    .as_array()
                    .is_some_and(|required| required.contains(&serde_json::json!("human_approval")))
        }));
    }

    #[test]
    fn approval_bound_reported_eval_failure_has_one_exact_terminal_shape() {
        let run = final_eval_run_fixture(false);
        assert!(validate_loop_run(&run).is_empty());

        let mut malformed = run;
        malformed
            .steps
            .iter_mut()
            .find(|step| step.name == crate::LoopStepName::EvalReport)
            .unwrap()
            .status = crate::LoopStepStatus::Passed;
        assert!(validate_loop_run(&malformed)
            .iter()
            .any(|error| error.field.ends_with(".status")));
    }

    #[test]
    fn historical_failed_run_without_human_approval_remains_compatible() {
        let mut run = approved_run_fixture();
        run.status = crate::LoopStatus::Failed;
        run.current_step = crate::LoopStepName::Development;
        run.human_approval = None;

        assert!(validate_loop_run(&run).is_empty());
    }

    fn final_eval_run_fixture(passed: bool) -> crate::LoopRun {
        let mut run = approved_run_fixture();
        run.input_digests.eval_config = Some("0".repeat(64));
        run.status = if passed {
            crate::LoopStatus::EvalPassed
        } else {
            crate::LoopStatus::Failed
        };
        run.current_step = crate::LoopStepName::EvalReport;
        let testing = run
            .steps
            .iter_mut()
            .find(|step| step.name == crate::LoopStepName::Testing)
            .unwrap();
        testing.status = if passed {
            crate::LoopStepStatus::Passed
        } else {
            crate::LoopStepStatus::Failed
        };
        testing.artifact_path = Some("artifacts/07-testing.json".to_string());
        testing.artifact_digest = Some("7".repeat(64));
        let report = run
            .steps
            .iter_mut()
            .find(|step| step.name == crate::LoopStepName::EvalReport)
            .unwrap();
        report.status = if passed {
            crate::LoopStepStatus::Passed
        } else {
            crate::LoopStepStatus::Failed
        };
        report.artifact_path = Some("artifacts/08-eval-report.json".to_string());
        report.artifact_digest = Some("8".repeat(64));
        run.eval_report_path = report.artifact_path.clone();
        run
    }

    fn approved_run_fixture() -> crate::LoopRun {
        let run_id = "approved-run".to_string();
        let request = crate::ProviderExchangeRecordReference {
            run_id: run_id.clone(),
            step: crate::LoopStepName::OutputReview,
            role: crate::ProviderRole::OutputReviewer,
            step_attempt: 1,
            exchange_index: 1,
            kind: crate::ProviderExchangeKind::Initial,
            context_round: None,
            phase: crate::ProviderExchangePhase::Request,
            path: "artifacts/06-output-review.attempt-001.exchange-001.initial.request.record.json"
                .to_string(),
            digest: "1".repeat(64),
        };
        let response = crate::ProviderExchangeRecordReference {
            phase: crate::ProviderExchangePhase::Response,
            path:
                "artifacts/06-output-review.attempt-001.exchange-001.initial.response.record.json"
                    .to_string(),
            digest: "2".repeat(64),
            ..request.clone()
        };
        let policy = crate::PolicyDecision {
            patch_id: run_id.clone(),
            patch_sha256: format!("sha256:{}", "0".repeat(64)),
            changed_paths: vec!["src/lib.rs".to_string()],
            decision: crate::PatchDecisionKind::Allowed,
            reasons: Vec::new(),
            requires_human_review: false,
            apply_requested: false,
            applied: false,
        };
        let policy_digest = canonical_sha256_digest(&policy).unwrap();
        let steps = [
            crate::LoopStepName::Research,
            crate::LoopStepName::Analysis,
            crate::LoopStepName::SpecCreation,
            crate::LoopStepName::SpecReview,
            crate::LoopStepName::Development,
            crate::LoopStepName::OutputReview,
            crate::LoopStepName::Testing,
            crate::LoopStepName::EvalReport,
        ]
        .into_iter()
        .map(|name| crate::LoopStepRecord {
            name,
            status: match name {
                crate::LoopStepName::OutputReview => crate::LoopStepStatus::Passed,
                crate::LoopStepName::Testing | crate::LoopStepName::EvalReport => {
                    crate::LoopStepStatus::Pending
                }
                _ => crate::LoopStepStatus::Completed,
            },
            artifact_path: (name == crate::LoopStepName::OutputReview)
                .then(|| "artifacts/06-output-review.json".to_string()),
            artifact_digest: (name == crate::LoopStepName::OutputReview).then(|| "6".repeat(64)),
        })
        .collect();
        crate::LoopRun {
            run_id: run_id.clone(),
            ticket_id: "T-1".to_string(),
            goal_id: "G-1".to_string(),
            provider: "fake".to_string(),
            model: "fake".to_string(),
            input_digests: crate::LoopInputDigests {
                ticket: "a".repeat(64),
                policy: "b".repeat(64),
                config: "c".repeat(64),
                repository: "d".repeat(64),
                eval_config: None,
            },
            execution_mode: crate::LoopExecutionMode::IsolatedCandidate,
            status: crate::LoopStatus::Approved,
            current_step: crate::LoopStepName::Testing,
            started_at: "started".to_string(),
            updated_at: "approved".to_string(),
            steps,
            policy_decisions: vec![policy],
            provider_exchange_records: vec![request.clone(), response.clone()],
            candidate_workspace: Some(crate::CandidateWorkspaceState {
                schema_version: 2,
                run_directory_digest: Some("9".repeat(64)),
                path: "/tmp/candidate".to_string(),
                source_worktree_root: "/tmp/source".to_string(),
                git_common_dir: "/tmp/source/.git".to_string(),
                repository_identity_digest: "d".repeat(64),
                starting_head: "e".repeat(40),
                starting_tree: "f".repeat(40),
                candidate_head: "e".repeat(40),
                candidate_tree: "1".repeat(40),
                candidate_diff_digest: "3".repeat(64),
                patch_transaction: Some(crate::CandidatePatchTransaction {
                    schema_version: 1,
                    phase: crate::CandidatePatchPhase::Applied,
                    intent: crate::ArtifactReference {
                        path: "artifacts/intent.json".to_string(),
                        digest: "4".repeat(64),
                    },
                    applied_evidence: Some(crate::ArtifactReference {
                        path: "artifacts/applied.json".to_string(),
                        digest: "5".repeat(64),
                    }),
                    started_at: "started".to_string(),
                    applied_at: Some("applied".to_string()),
                }),
                lifecycle: crate::CandidateWorkspaceLifecycle::Active,
                cleanup_started_at: None,
                cleaned_at: None,
            }),
            human_approval: Some(crate::HumanApprovalEvidence {
                schema_version: 1,
                run_id,
                reviewer: "reviewer@example.invalid".to_string(),
                approved_at: "approved".to_string(),
                candidate_diff: crate::ArtifactReference {
                    path: "artifacts/candidate-patch.applied.diff".to_string(),
                    digest: "3".repeat(64),
                },
                starting_head: "e".repeat(40),
                policy_decision_digest: policy_digest,
                output_review: crate::ArtifactReference {
                    path: "artifacts/06-output-review.json".to_string(),
                    digest: "6".repeat(64),
                },
                output_review_request: request,
                output_review_response: response,
            }),
            eval_report_path: None,
            promotion: None,
            latest_recovery: None,
        }
    }

    #[test]
    fn invalid_loop_run_reports_contract_fields() {
        let run: LoopRun = serde_json::from_str(
            r#"{
  "run_id": "",
  "ticket_id": "",
  "goal_id": "",
  "provider": "",
  "model": "",
  "input_digests": {
    "ticket": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "policy": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "config": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    "repository": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
  },
  "status": "running",
  "current_step": "development",
  "started_at": "",
  "updated_at": "",
  "steps": [
    {
      "name": "research",
      "status": "passed",
      "artifact_path": ""
    }
  ],
  "policy_decisions": [],
  "eval_report_path": ""
}"#,
        )
        .expect("loop run should parse");
        let errors = validate_loop_run(&run);
        let fields: Vec<&str> = errors.iter().map(|error| error.field.as_str()).collect();

        assert!(fields.contains(&"run_id"));
        assert!(fields.contains(&"ticket_id"));
        assert!(fields.contains(&"goal_id"));
        assert!(fields.contains(&"provider"));
        assert!(fields.contains(&"model"));
        assert!(fields.contains(&"started_at"));
        assert!(fields.contains(&"updated_at"));
        assert!(fields.contains(&"steps[0].artifact_path"));
        assert!(fields.contains(&"eval_report_path"));
    }

    #[test]
    fn loop_run_rejects_policy_decisions_missing_typed_contract_fields() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let run_path = temp_dir.path().join("loop-run.json");
        std::fs::write(
            &run_path,
            r#"{
  "run_id": "loop_20260701_001",
  "ticket_id": "T-LOCAL-001",
  "goal_id": "local_agent_loop_mvp",
  "provider": "ollama",
  "model": "gemma4:e4b-mlx",
  "input_digests": {
    "ticket": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "policy": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "config": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    "repository": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
  },
  "status": "running",
  "current_step": "development",
  "started_at": "2026-07-01T12:00:00Z",
  "updated_at": "2026-07-01T12:12:00Z",
  "steps": [],
  "policy_decisions": [{}],
  "eval_report_path": null
}"#,
        )
        .expect("write loop run");

        let report = load_loop_run_file(&run_path).unwrap_err();

        assert!(report
            .errors
            .iter()
            .any(|error| error.field == "file" && error.message.contains("patch_id")));
    }

    #[test]
    fn loop_run_unknown_fields_fail_closed() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let run_path = temp_dir.path().join("loop-run.json");
        std::fs::write(
            &run_path,
            r#"{
  "run_id": "loop_20260701_001",
  "ticket_id": "T-LOCAL-001",
  "goal_id": "local_agent_loop_mvp",
  "provider": "ollama",
  "model": "gemma4:e4b-mlx",
  "input_digests": {
    "ticket": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "policy": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "config": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    "repository": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
  },
  "status": "running",
  "current_step": "development",
  "started_at": "2026-07-01T12:00:00Z",
  "updated_at": "2026-07-01T12:12:00Z",
  "steps": [],
  "policy_decisions": [],
  "eval_report_path": null,
  "unexpected_escape": true
}"#,
        )
        .expect("write loop run");

        let report = load_loop_run_file(&run_path).unwrap_err();

        assert!(report
            .errors
            .iter()
            .any(|error| error.field == "file" && error.message.contains("unexpected_escape")));
    }

    #[test]
    fn loop_run_accepts_one_closed_latest_recovery_reference_without_breaking_old_shape() {
        let mut run = serde_json::to_value(approved_run_fixture()).expect("run value");
        run["status"] = serde_json::json!("blocked");
        run["current_step"] = serde_json::json!("output_review");
        run.as_object_mut()
            .expect("run object")
            .remove("human_approval");
        run["latest_recovery"] = serde_json::json!({
            "recovery_id": 1,
            "artifact": {
                "path": "artifacts/recovery-001.json",
                "digest": "a".repeat(64)
            }
        });

        let decoded: LoopRun =
            serde_json::from_value(run).expect("latest recovery is a backward-compatible field");

        assert!(validate_loop_run(&decoded).is_empty());
    }
}
