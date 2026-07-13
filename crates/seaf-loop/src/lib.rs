pub mod approved_eval;
mod artifact_safety;
mod artifact_storage;
pub mod artifacts;
pub mod bench;
pub mod candidate_workspace;
pub mod context;
pub mod context_expansion;
pub mod development_evidence;
pub mod eval_engine;
pub mod eval_report;
mod evaluation_attempt;
mod evaluation_storage;
pub mod final_evaluation_authority;
mod immutable_artifact;
pub mod inspect;
pub mod model_runner;
mod operator_evidence;
pub mod patch;
pub mod policy;
pub mod policy_gate;
pub mod promotion;
pub mod provider_exchange;
pub mod recovery;
pub mod role_artifact;
pub mod role_response;
mod run_persistence;
pub mod runner;
mod secret_redaction;
pub mod state;
mod storage_authority;
pub mod testing_evidence;
pub mod workspace;

pub use approved_eval::{
    execute_approved_evaluation, rerun_invalidated_evaluation, ApprovedEvaluationError,
};
pub use artifacts::ArtifactContent;
pub use bench::{
    evaluate_zero_tolerance, load_agent_bench_fixture, summarize_agent_bench_results,
    AgentBenchFixture, AgentBenchResult, AgentBenchSummary, BenchError, ZeroToleranceError,
};
pub use candidate_workspace::{
    apply_candidate_development_evidence, approve_candidate_for_testing,
    cleanup_candidate_workspace, cleanup_candidate_workspace_outcome, create_candidate_workspace,
    plan_candidate_workspace, plan_candidate_workspace_readiness, provision_candidate_workspace,
    validate_candidate_workspace, verify_candidate_patch_evidence, CandidateApprovalOutcome,
    CandidateCleanupOutcome, CandidateWorkspaceError, CandidateWorkspaceReadiness,
    VerifiedCandidatePatchEvidence, CANDIDATE_WORKSPACE_SCHEMA_VERSION,
};
pub use context::{
    pack_context, pack_context_for_ticket, pack_live_context, CandidateContextAuthority,
    CandidateContextAuthorityKind, ContextBundle, ContextError, ContextFile, ContextLimits,
    ContextManifest, ContextManifestFile, ContextPackRequest, CONTEXT_MANIFEST_FILE,
    UNTRUSTED_CONTEXT_MARKER,
};
pub use context_expansion::{
    create_context_expansion, load_context_expansion, reconstruct_context_expansion_files,
    ContextExpansionArtifact, ContextExpansionError, ContextExpansionFile, ContextExpansionRequest,
    CreatedContextExpansion, CONTEXT_EXPANSION_SCHEMA_VERSION,
};
pub use development_evidence::DevelopmentEvidence;
pub use eval_engine::{
    execute_eval_checks, plan_eval_checks, run_eval_checks, EvalCheckExecution, EvalEngineError,
    EvalPlan,
};
pub use eval_report::build_loop_eval_report;
pub use final_evaluation_authority::{
    load_verified_final_evaluation_authority, FinalEvaluationAuthorityError,
    VerifiedFinalEvaluationAuthority,
};
pub use inspect::{
    inspect_loop_run, ArtifactHistoryInspection, CandidateInspection, EvaluationPrefixInspection,
    EvidenceClassification, InputDigestInspection, InspectError, InspectionBounds,
    InspectionIntegrity, LoopInspection, ProviderAttemptInspection, ProviderExchangeInspection,
    StepInspection,
};
pub use model_runner::{ProviderPatchGateConfig, ProviderStepRunner};
pub use patch::{parse_unified_diff, ParsedPatch, PatchFile, PatchParseError};
pub use policy_gate::{
    gate_patch, patch_digest, CommandOutput, GitCommandRunner, PatchCommand, PatchCommandRunner,
    PatchDecisionKind, PatchGateError, PatchGateRequest, PolicyDecision, PolicyDecisionReason,
};
pub use promotion::{promote_evaluated_candidate, PromotionError, PromotionOutcome};
pub use provider_exchange::{
    classify_provider_exchange_record, load_provider_exchange_record,
    load_provider_exchange_request, persist_provider_exchange_record_reference,
    stage_provider_exchange_record, validate_provider_exchange_record_append,
    write_provider_exchange_request, write_provider_exchange_response, ProviderExchangeCoordinates,
    ProviderExchangeError, ProviderExchangeRecordState, ProviderExchangeResponseAudit,
    PROVIDER_EXCHANGE_SCHEMA_VERSION,
};
pub use recovery::{
    adopt_approved_evaluation, ensure_no_pending_recovery, invalidate_approved_evaluation,
    load_verified_latest_recovery, load_verified_recovery_authority_kind, revise_provider_step,
    validate_requested_recovery, EvaluationAdoptionOutcome, EvaluationInvalidationAction,
    EvaluationInvalidationAttemptV3, EvaluationInvalidationOutcome,
    EvaluationInvalidationSourceRunV3, RecoveryAction, RecoveryAttemptV1, RecoveryAuthorityKind,
    RecoveryError, RecoveryRevisionOutcome, RecoverySourceRunV1,
    EVALUATION_INVALIDATION_SCHEMA_VERSION, RECOVERY_SCHEMA_VERSION,
};
pub use role_artifact::{RoleArtifactError, ValidatedRoleArtifact};
pub use role_response::{
    parse_role_response, parse_role_response_with_repair, AgentResponse, AgentStatus,
    ContextRequest, DeveloperResponse, DeveloperStatus, Finding, ReviewDecision, ReviewIssue,
    ReviewerResponse, Role, RoleResponse, RoleResponseError, MAX_CONTEXT_REQUEST_PATHS,
    MAX_CONTEXT_REQUEST_REASON_CHARS,
};
pub use runner::{
    preflight_authoritative_run_inputs, validate_human_review_execution_barrier,
    validate_rerun_eligibility, AuthoritativeRunInputSnapshots, InitializedLoopRun, LoopRunner,
    LoopRunnerConfig, PreparedLoopRun, RunnerError, ScaffoldedLoopRun, StepOutput, StepRunner,
};
pub use seaf_core::ArtifactReference;
pub use testing_evidence::{
    TestingEvidence, TestingEvidenceError, TESTING_EVIDENCE_SCHEMA_VERSION,
};
pub use workspace::{LoopWorkspace, WorkspaceError};

#[cfg(test)]
extern crate self as seaf_loop;

#[cfg(test)]
mod legacy_provider_step_runner_tests {
    include!("test_suites/provider_step_runner.rs");
}

#[cfg(test)]
mod legacy_provider_live_context_tests {
    include!("test_suites/provider_live_context.rs");
}
