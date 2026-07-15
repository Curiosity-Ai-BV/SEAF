use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DURABLE_ARTIFACT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Default)]
enum DecodedSchemaVersion {
    #[default]
    Missing,
    Present(u32),
}

impl Serialize for DecodedSchemaVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Present(version) => serializer.serialize_u32(*version),
            Self::Missing => serializer.serialize_u32(DURABLE_ARTIFACT_SCHEMA_VERSION),
        }
    }
}

impl<'de> Deserialize<'de> for DecodedSchemaVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        u32::deserialize(deserializer).map(Self::Present)
    }
}

fn require_supported_schema_version(
    version: DecodedSchemaVersion,
    kind: &str,
) -> Result<(), String> {
    match version {
        DecodedSchemaVersion::Missing
        | DecodedSchemaVersion::Present(DURABLE_ARTIFACT_SCHEMA_VERSION) => Ok(()),
        DecodedSchemaVersion::Present(version) => Err(format!(
            "unsupported {kind} schema_version {version}; expected {DURABLE_ARTIFACT_SCHEMA_VERSION}, or omit schema_version for a legacy v0 artifact"
        )),
    }
}

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
#[serde(try_from = "PolicyWire", into = "PolicyWire")]
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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyWire {
    #[serde(default)]
    schema_version: DecodedSchemaVersion,
    policy_id: String,
    default_autonomy_level: u8,
    #[serde(default)]
    forbidden_paths: Vec<String>,
    #[serde(default)]
    requires_human_review: Vec<String>,
    #[serde(default)]
    allowed_without_review: Vec<String>,
}

impl TryFrom<PolicyWire> for Policy {
    type Error = String;

    fn try_from(wire: PolicyWire) -> Result<Self, Self::Error> {
        require_supported_schema_version(wire.schema_version, "Policy")?;
        Ok(Self {
            policy_id: wire.policy_id,
            default_autonomy_level: wire.default_autonomy_level,
            forbidden_paths: wire.forbidden_paths,
            requires_human_review: wire.requires_human_review,
            allowed_without_review: wire.allowed_without_review,
        })
    }
}

impl From<Policy> for PolicyWire {
    fn from(policy: Policy) -> Self {
        Self {
            schema_version: DecodedSchemaVersion::Present(DURABLE_ARTIFACT_SCHEMA_VERSION),
            policy_id: policy.policy_id,
            default_autonomy_level: policy.default_autonomy_level,
            forbidden_paths: policy.forbidden_paths,
            requires_human_review: policy.requires_human_review,
            allowed_without_review: policy.allowed_without_review,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PolicyDecisionWire", into = "PolicyDecisionWire")]
pub struct PolicyDecision {
    pub patch_id: String,
    pub patch_sha256: String,
    pub changed_paths: Vec<String>,
    pub decision: PatchDecisionKind,
    pub reasons: Vec<PolicyDecisionReason>,
    pub requires_human_review: bool,
    pub apply_requested: bool,
    pub applied: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyDecisionWire {
    #[serde(default)]
    schema_version: DecodedSchemaVersion,
    patch_id: String,
    patch_sha256: String,
    changed_paths: Vec<String>,
    decision: PatchDecisionKind,
    reasons: Vec<PolicyDecisionReason>,
    requires_human_review: bool,
    apply_requested: bool,
    applied: bool,
}

impl TryFrom<PolicyDecisionWire> for PolicyDecision {
    type Error = String;

    fn try_from(wire: PolicyDecisionWire) -> Result<Self, Self::Error> {
        require_supported_schema_version(wire.schema_version, "PolicyDecision")?;
        Ok(Self {
            patch_id: wire.patch_id,
            patch_sha256: wire.patch_sha256,
            changed_paths: wire.changed_paths,
            decision: wire.decision,
            reasons: wire.reasons,
            requires_human_review: wire.requires_human_review,
            apply_requested: wire.apply_requested,
            applied: wire.applied,
        })
    }
}

impl From<PolicyDecision> for PolicyDecisionWire {
    fn from(decision: PolicyDecision) -> Self {
        Self {
            schema_version: DecodedSchemaVersion::Present(DURABLE_ARTIFACT_SCHEMA_VERSION),
            patch_id: decision.patch_id,
            patch_sha256: decision.patch_sha256,
            changed_paths: decision.changed_paths,
            decision: decision.decision,
            reasons: decision.reasons,
            requires_human_review: decision.requires_human_review,
            apply_requested: decision.apply_requested,
            applied: decision.applied,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchDecisionKind {
    Allowed,
    RequiresHumanReview,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDecisionReason {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
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
    pub stdout_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalLoopEvidence {
    pub schema_version: u32,
    pub run_id: String,
    pub ticket_id: String,
    pub ticket_digest: String,
    pub eval_config: ArtifactReference,
    pub candidate_diff: ArtifactReference,
    pub starting_head: String,
    pub human_approval_digest: String,
    pub policy_decision_digest: String,
    pub testing_evidence: ArtifactReference,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "EvalReportWire", into = "EvalReportWire")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_evidence: Option<EvalLoopEvidence>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvalReportWire {
    #[serde(default)]
    schema_version: DecodedSchemaVersion,
    eval_report_id: String,
    patch_id: String,
    goal_id: String,
    passed: bool,
    summary: String,
    checks: Vec<EvalCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    score_delta_estimate: Option<f64>,
    risk_level: RiskLevel,
    decision: EvalDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    loop_evidence: Option<EvalLoopEvidence>,
}

impl TryFrom<EvalReportWire> for EvalReport {
    type Error = String;

    fn try_from(wire: EvalReportWire) -> Result<Self, Self::Error> {
        require_supported_schema_version(wire.schema_version, "EvalReport")?;
        Ok(Self {
            eval_report_id: wire.eval_report_id,
            patch_id: wire.patch_id,
            goal_id: wire.goal_id,
            passed: wire.passed,
            summary: wire.summary,
            checks: wire.checks,
            score_delta_estimate: wire.score_delta_estimate,
            risk_level: wire.risk_level,
            decision: wire.decision,
            loop_evidence: wire.loop_evidence,
        })
    }
}

impl From<EvalReport> for EvalReportWire {
    fn from(report: EvalReport) -> Self {
        Self {
            schema_version: DecodedSchemaVersion::Present(DURABLE_ARTIFACT_SCHEMA_VERSION),
            eval_report_id: report.eval_report_id,
            patch_id: report.patch_id,
            goal_id: report.goal_id,
            passed: report.passed,
            summary: report.summary,
            checks: report.checks,
            score_delta_estimate: report.score_delta_estimate,
            risk_level: report.risk_level,
            decision: report.decision,
            loop_evidence: report.loop_evidence,
        }
    }
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
#[serde(try_from = "TicketSpecWire", into = "TicketSpecWire")]
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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TicketSpecWire {
    #[serde(default)]
    schema_version: DecodedSchemaVersion,
    ticket_id: String,
    goal_id: String,
    title: String,
    status: TicketStatus,
    priority: TicketPriority,
    problem: String,
    #[serde(default)]
    research_questions: Vec<String>,
    context: TicketContext,
    autonomy: TicketAutonomy,
    acceptance_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    eval: Option<TicketEval>,
}

impl TryFrom<TicketSpecWire> for TicketSpec {
    type Error = String;

    fn try_from(wire: TicketSpecWire) -> Result<Self, Self::Error> {
        require_supported_schema_version(wire.schema_version, "TicketSpec")?;
        Ok(Self {
            ticket_id: wire.ticket_id,
            goal_id: wire.goal_id,
            title: wire.title,
            status: wire.status,
            priority: wire.priority,
            problem: wire.problem,
            research_questions: wire.research_questions,
            context: wire.context,
            autonomy: wire.autonomy,
            acceptance_criteria: wire.acceptance_criteria,
            eval: wire.eval,
        })
    }
}

impl From<TicketSpec> for TicketSpecWire {
    fn from(ticket: TicketSpec) -> Self {
        Self {
            schema_version: DecodedSchemaVersion::Present(DURABLE_ARTIFACT_SCHEMA_VERSION),
            ticket_id: ticket.ticket_id,
            goal_id: ticket.goal_id,
            title: ticket.title,
            status: ticket.status,
            priority: ticket.priority,
            problem: ticket.problem,
            research_questions: ticket.research_questions,
            context: ticket.context,
            autonomy: ticket.autonomy,
            acceptance_criteria: ticket.acceptance_criteria,
            eval: ticket.eval,
        }
    }
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
    AwaitingHumanReview,
    Approved,
    EvalPassed,
    Promoted,
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
#[serde(try_from = "LoopRunWire", into = "LoopRunWire")]
pub struct LoopRun {
    pub run_id: String,
    pub ticket_id: String,
    pub goal_id: String,
    pub provider: String,
    pub model: String,
    pub input_digests: LoopInputDigests,
    #[serde(default)]
    pub execution_mode: LoopExecutionMode,
    pub status: LoopStatus,
    pub current_step: LoopStepName,
    pub started_at: String,
    pub updated_at: String,
    pub steps: Vec<LoopStepRecord>,
    pub policy_decisions: Vec<PolicyDecision>,
    #[serde(default)]
    pub provider_exchange_records: Vec<ProviderExchangeRecordReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_workspace: Option<CandidateWorkspaceState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_approval: Option<HumanApprovalEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval_report_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion: Option<PromotionEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_recovery: Option<RecoveryReference>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoopRunWire {
    #[serde(default)]
    schema_version: DecodedSchemaVersion,
    run_id: String,
    ticket_id: String,
    goal_id: String,
    provider: String,
    model: String,
    input_digests: LoopInputDigests,
    #[serde(default)]
    execution_mode: DecodedExecutionMode,
    status: LoopStatus,
    current_step: LoopStepName,
    started_at: String,
    updated_at: String,
    steps: Vec<LoopStepRecord>,
    policy_decisions: Vec<PolicyDecision>,
    #[serde(default)]
    provider_exchange_records: Vec<ProviderExchangeRecordReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    candidate_workspace: Option<CandidateWorkspaceState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    human_approval: Option<HumanApprovalEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    eval_report_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    promotion: Option<PromotionEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest_recovery: Option<RecoveryReference>,
}

#[derive(Default)]
enum DecodedExecutionMode {
    #[default]
    Missing,
    Present(LoopExecutionMode),
}

impl<'de> Deserialize<'de> for DecodedExecutionMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        LoopExecutionMode::deserialize(deserializer).map(Self::Present)
    }
}

impl Serialize for DecodedExecutionMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Present(mode) => mode.serialize(serializer),
            Self::Missing => LoopExecutionMode::LegacyProposalOnly.serialize(serializer),
        }
    }
}

impl TryFrom<LoopRunWire> for LoopRun {
    type Error = String;

    fn try_from(wire: LoopRunWire) -> Result<Self, Self::Error> {
        require_supported_schema_version(wire.schema_version, "LoopRun")?;
        let execution_mode = match wire.execution_mode {
            DecodedExecutionMode::Present(mode) => mode,
            DecodedExecutionMode::Missing
                if matches!(
                    wire.status,
                    LoopStatus::AwaitingHumanReview
                        | LoopStatus::Approved
                        | LoopStatus::EvalPassed
                        | LoopStatus::Promoted
                ) =>
            {
                return Err(
                    "human review states require explicit isolated_candidate execution_mode"
                        .to_string(),
                );
            }
            DecodedExecutionMode::Missing
                if wire.status == LoopStatus::Failed && wire.human_approval.is_some() =>
            {
                return Err(
                    "final integrated evaluation requires explicit isolated_candidate execution_mode"
                        .to_string(),
                );
            }
            DecodedExecutionMode::Missing if wire.candidate_workspace.is_some() => {
                LoopExecutionMode::IsolatedCandidate
            }
            DecodedExecutionMode::Missing => LoopExecutionMode::LegacyProposalOnly,
        };
        Ok(LoopRun {
            run_id: wire.run_id,
            ticket_id: wire.ticket_id,
            goal_id: wire.goal_id,
            provider: wire.provider,
            model: wire.model,
            input_digests: wire.input_digests,
            execution_mode,
            status: wire.status,
            current_step: wire.current_step,
            started_at: wire.started_at,
            updated_at: wire.updated_at,
            steps: wire.steps,
            policy_decisions: wire.policy_decisions,
            provider_exchange_records: wire.provider_exchange_records,
            candidate_workspace: wire.candidate_workspace,
            human_approval: wire.human_approval,
            eval_report_path: wire.eval_report_path,
            promotion: wire.promotion,
            latest_recovery: wire.latest_recovery,
        })
    }
}

impl From<LoopRun> for LoopRunWire {
    fn from(run: LoopRun) -> Self {
        Self {
            schema_version: DecodedSchemaVersion::Present(DURABLE_ARTIFACT_SCHEMA_VERSION),
            run_id: run.run_id,
            ticket_id: run.ticket_id,
            goal_id: run.goal_id,
            provider: run.provider,
            model: run.model,
            input_digests: run.input_digests,
            execution_mode: DecodedExecutionMode::Present(run.execution_mode),
            status: run.status,
            current_step: run.current_step,
            started_at: run.started_at,
            updated_at: run.updated_at,
            steps: run.steps,
            policy_decisions: run.policy_decisions,
            provider_exchange_records: run.provider_exchange_records,
            candidate_workspace: run.candidate_workspace,
            human_approval: run.human_approval,
            eval_report_path: run.eval_report_path,
            promotion: run.promotion,
            latest_recovery: run.latest_recovery,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopExecutionMode {
    #[default]
    LegacyProposalOnly,
    IsolatedCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateWorkspaceState {
    pub schema_version: u32,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_string"
    )]
    pub run_directory_digest: Option<String>,
    pub path: String,
    pub source_worktree_root: String,
    pub git_common_dir: String,
    pub repository_identity_digest: String,
    pub starting_head: String,
    pub starting_tree: String,
    pub candidate_head: String,
    pub candidate_tree: String,
    pub candidate_diff_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_transaction: Option<CandidatePatchTransaction>,
    pub lifecycle: CandidateWorkspaceLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleaned_at: Option<String>,
}

fn deserialize_present_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidatePatchTransaction {
    pub schema_version: u32,
    pub phase: CandidatePatchPhase,
    pub intent: ArtifactReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_evidence: Option<ArtifactReference>,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidatePatchPhase {
    Applying,
    Applied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateWorkspaceLifecycle {
    Provisioning,
    Active,
    Cleaning,
    Cleaned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReference {
    pub path: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryReference {
    pub recovery_id: u32,
    pub artifact: ArtifactReference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanApprovalEvidence {
    pub schema_version: u32,
    pub run_id: String,
    pub reviewer: String,
    pub approved_at: String,
    pub candidate_diff: ArtifactReference,
    pub starting_head: String,
    pub policy_decision_digest: String,
    pub output_review: ArtifactReference,
    pub output_review_request: ProviderExchangeRecordReference,
    pub output_review_response: ProviderExchangeRecordReference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionEvidence {
    pub schema_version: u32,
    pub run_id: String,
    pub reviewer: String,
    pub promoted_at: String,
    pub intent: ArtifactReference,
    pub candidate_diff: ArtifactReference,
    pub testing_evidence: ArtifactReference,
    pub eval_report: ArtifactReference,
    pub policy_decision_digest: String,
    pub target_head: String,
    pub eval_passed_run_digest: String,
    pub eval_passed_updated_at: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval_config: Option<String>,
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
