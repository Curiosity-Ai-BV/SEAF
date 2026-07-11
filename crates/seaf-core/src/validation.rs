use std::{fmt::Display, fs, io::Read, path::Path};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CheckStatus, EvalReport, GoalSpec, LoopRun, Policy, ProjectConfig, ProviderExchangeKind,
    ProviderExchangeOutcome, ProviderExchangePhase, ProviderExchangeRecord,
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
    let errors = validate_ticket_spec(&ticket);
    if errors.is_empty() {
        Ok(ticket)
    } else {
        Err(ValidationReport::invalid("ticket", Some(path), errors))
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

    errors
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
        if decision.is_empty() {
            errors.push(FieldError::new(
                format!("policy_decisions[{index}]"),
                "must include at least one policy decision field",
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

    if let Some(eval_report_path) = &run.eval_report_path {
        require_non_empty(&mut errors, "eval_report_path", eval_report_path);
    }

    errors
}

fn validate_candidate_workspace_state(
    errors: &mut Vec<FieldError>,
    candidate: &crate::CandidateWorkspaceState,
) {
    if candidate.schema_version != 1 {
        errors.push(FieldError::new(
            "candidate_workspace.schema_version",
            "must be 1",
        ));
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
        assert!(validate_loop_run(&run).is_empty());
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
    fn loop_run_rejects_empty_policy_decision_objects() {
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
            .any(|error| error.field == "policy_decisions[0]"));
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
}
