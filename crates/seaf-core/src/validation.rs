use std::{fmt::Display, fs, path::Path};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{GoalSpec, Policy, ReleaseCapsule};

pub type ValidationResult<T> = Result<T, ValidationReport>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

impl FieldError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
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

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
