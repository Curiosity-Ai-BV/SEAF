use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Draft,
    Active,
    Paused,
    Retired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricDirection {
    Increase,
    Decrease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeDisposition {
    AutoPr,
    RequireReview,
    Forbidden,
    ForbiddenWithoutOwnerApproval,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Objective {
    pub metric: String,
    pub direction: MetricDirection,
    pub minimum_effect_size: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalSpec {
    pub goal_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub status: GoalStatus,
    pub objective: Objective,
    #[serde(default)]
    pub guardrails: BTreeMap<String, Value>,
    #[serde(default)]
    pub anti_goals: Vec<String>,
    #[serde(default)]
    pub allowed_change_types: BTreeMap<String, ChangeDisposition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub policy_id: String,
    pub default_autonomy_level: u8,
    #[serde(default)]
    pub forbidden_paths: Vec<String>,
    #[serde(default)]
    pub requires_human_review: Vec<String>,
    #[serde(default)]
    pub allowed_without_review: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCapability {
    pub agent: String,
    pub can_read: Vec<String>,
    pub can_write: Vec<String>,
    pub cannot_read: Vec<String>,
    pub cannot_write: Vec<String>,
    pub network: NetworkAccess,
    pub requires_review_for: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAccess {
    None,
    Restricted,
    Allowed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalDecision {
    Reject,
    ApproveForHumanReview,
    ApproveForRelease,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalCheck {
    pub name: String,
    pub status: CheckStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalReport {
    pub eval_report_id: String,
    pub patch_id: String,
    pub goal_id: String,
    pub passed: bool,
    pub summary: String,
    pub checks: Vec<EvalCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_delta_estimate: Option<f64>,
    pub risk_level: RiskLevel,
    pub decision: EvalDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RolloutChannel {
    Dev,
    Canary,
    Beta,
    Stable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolloutPolicy {
    pub channel: RolloutChannel,
    pub initial_percentage: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseCapsule {
    pub release_id: String,
    pub app_id: String,
    pub version: String,
    pub source_commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_task_id: Option<String>,
    pub goal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_recipe_hash: Option<String>,
    pub artifact_digest: String,
    pub eval_report_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_plan: Option<String>,
    pub rollback_plan: String,
    #[serde(default)]
    pub signatures: Vec<String>,
    pub rollout_policy: RolloutPolicy,
}
