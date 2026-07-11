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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub policy_path: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyLevel {
    Public,
    Aggregated,
    Private,
    Sensitive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeafEvent {
    pub event_id: String,
    pub name: String,
    pub timestamp: String,
    pub source: String,
    pub privacy_level: PrivacyLevel,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalSeverity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signal {
    pub signal_id: String,
    pub source: String,
    #[serde(rename = "type")]
    pub signal_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_goal_id: Option<String>,
    pub summary: String,
    pub severity: SignalSeverity,
    pub privacy_level: PrivacyLevel,
    pub evidence: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskBrief {
    pub task_id: String,
    pub goal_id: String,
    pub objective: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<Signal>,
    pub constraints: AgentTaskConstraints,
    pub relevant_files: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub generated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskConstraints {
    pub default_autonomy_level: u8,
    pub forbidden_paths: Vec<String>,
    pub requires_human_review: Vec<String>,
    pub allowed_without_review: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketStatus {
    Draft,
    Ready,
    Running,
    Blocked,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketPriority {
    P0,
    P1,
    P2,
    P3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketSpec {
    pub ticket_id: String,
    pub goal_id: String,
    pub title: String,
    pub status: TicketStatus,
    pub priority: TicketPriority,
    pub problem: String,
    #[serde(default)]
    pub research_questions: Vec<String>,
    pub context: TicketContext,
    pub autonomy: TicketAutonomy,
    pub acceptance_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval: Option<TicketEval>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketContext {
    pub relevant_files: Vec<String>,
    pub forbidden_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketAutonomy {
    pub level: u8,
    pub apply_patch: bool,
    #[serde(default)]
    pub allow_shell_commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketEval {
    pub config: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopStatus {
    Pending,
    Running,
    Blocked,
    Failed,
    Passed,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopStepStatus {
    Pending,
    Running,
    Blocked,
    Failed,
    Passed,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopStepName {
    Research,
    Analysis,
    SpecCreation,
    SpecReview,
    Development,
    OutputReview,
    Testing,
    EvalReport,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopRun {
    pub run_id: String,
    pub ticket_id: String,
    pub goal_id: String,
    pub provider: String,
    pub model: String,
    pub input_digests: LoopInputDigests,
    pub status: LoopStatus,
    pub current_step: LoopStepName,
    pub started_at: String,
    pub updated_at: String,
    pub steps: Vec<LoopStepRecord>,
    pub policy_decisions: Vec<BTreeMap<String, Value>>,
    #[serde(default)]
    pub provider_exchange_records: Vec<ProviderExchangeRecordReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval_report_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReference {
    pub path: String,
    pub digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRole {
    Researcher,
    Analyzer,
    SpecWriter,
    SpecReviewer,
    Developer,
    OutputReviewer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderExchangeKind {
    Initial,
    JsonRepair,
    ContextRetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderExchangePhase {
    Request,
    Response,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderExchangeOutcome {
    Passed,
    Blocked,
    NeedsContext,
    PatchProposed,
    ApproveSpec,
    ApproveForTests,
    RequestChanges,
    Reject,
    InvalidResponse,
    ProviderFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderExchangeRecord {
    pub schema_version: u32,
    pub run_id: String,
    pub step: LoopStepName,
    pub role: ProviderRole,
    pub step_attempt: u32,
    pub exchange_index: u32,
    pub kind: ProviderExchangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_round: Option<u32>,
    pub phase: ProviderExchangePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_record_digest: Option<String>,
    pub request: ArtifactReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<ArtifactReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion: Option<ArtifactReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ProviderExchangeOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderExchangeRecordReference {
    pub run_id: String,
    pub step: LoopStepName,
    pub role: ProviderRole,
    pub step_attempt: u32,
    pub exchange_index: u32,
    pub kind: ProviderExchangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_round: Option<u32>,
    pub phase: ProviderExchangePhase,
    pub path: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopInputDigests {
    pub ticket: String,
    pub policy: String,
    pub config: String,
    pub repository: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopStepRecord {
    pub name: LoopStepName,
    pub status: LoopStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_digest: Option<String>,
}
