use std::path::{Path, PathBuf};

use seaf_core::{LoopStepName, LoopStepStatus, Policy, TicketSpec};
use seaf_models::{ModelMessage, ModelMessageRole, ModelProvider, ModelRequest};

use crate::{
    context::{pack_live_context, ContextBundle, ContextPackRequest},
    parse_role_response_with_repair,
    policy_gate::{
        gate_patch, CommandOutput, PatchCommand, PatchCommandRunner, PatchDecisionKind,
        PatchGateError, PatchGateRequest, PolicyDecision,
    },
    runner::{RunnerError, StepRunner},
    workspace::{LoopWorkspace, ARTIFACTS_DIR},
    AgentStatus, DeveloperResponse, DeveloperStatus, ReviewDecision, Role, RoleResponse,
    StepOutput,
};

pub struct ProviderStepRunner<'a, P: ModelProvider + ?Sized> {
    provider: &'a P,
    model: String,
    timeout_ms: u64,
    context_pack_request: Option<ContextPackRequest>,
    context_bundle: Option<ContextBundle>,
    run_directory: Option<PathBuf>,
    run_id: Option<String>,
    patch_gate: Option<ProviderPatchGate<'a>>,
    pending_policy_decisions: Vec<PolicyDecision>,
    last_error_response: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderPatchGateConfig {
    pub repository_root: PathBuf,
    pub policy: Policy,
    pub apply_patch: bool,
    pub worktree_clean: bool,
}

impl ProviderPatchGateConfig {
    pub fn for_ticket(
        repository_root: impl Into<PathBuf>,
        ticket: &TicketSpec,
        policy: Policy,
        worktree_clean: bool,
    ) -> Self {
        Self {
            repository_root: repository_root.into(),
            policy,
            apply_patch: ticket.autonomy.apply_patch,
            worktree_clean,
        }
    }
}

struct ProviderPatchGate<'a> {
    config: ProviderPatchGateConfig,
    runner: &'a mut dyn PatchCommandRunner,
}

impl<'a, P: ModelProvider + ?Sized> ProviderStepRunner<'a, P> {
    pub fn new(provider: &'a P, model: impl Into<String>, timeout_ms: u64) -> Self {
        Self {
            provider,
            model: model.into(),
            timeout_ms,
            context_pack_request: None,
            context_bundle: None,
            run_directory: None,
            run_id: None,
            patch_gate: None,
            pending_policy_decisions: Vec::new(),
            last_error_response: None,
        }
    }

    pub fn with_context_pack_request(mut self, request: ContextPackRequest) -> Self {
        self.context_pack_request = Some(request);
        self
    }

    pub fn with_patch_gate(
        mut self,
        config: ProviderPatchGateConfig,
        runner: &'a mut dyn PatchCommandRunner,
    ) -> Self {
        self.patch_gate = Some(ProviderPatchGate { config, runner });
        self
    }

    fn model_request(&self, role: Role, user_prompt: String) -> ModelRequest {
        ModelRequest {
            model: self.model.clone(),
            system: role.system_prompt().to_string(),
            messages: vec![ModelMessage {
                role: ModelMessageRole::User,
                content: user_prompt,
            }],
            response_schema: Some(role.response_schema()),
            temperature: 0.0,
            timeout_ms: self.timeout_ms,
        }
    }

    fn gate_developer_patch(
        &mut self,
        response: &DeveloperResponse,
    ) -> Result<Option<LoopStepStatus>, RunnerError> {
        if response.status != DeveloperStatus::PatchProposed {
            return Ok(None);
        }

        let Some(patch_gate) = self.patch_gate.as_mut() else {
            return Ok(None);
        };
        let patch = response.patch.as_deref().ok_or_else(|| {
            RunnerError::Step("developer patch response was missing patch content".to_string())
        })?;
        let run_directory = self.run_directory.as_ref().ok_or_else(|| {
            RunnerError::Step(
                "patch gate requires a prepared loop workspace before development".to_string(),
            )
        })?;
        let run_id = self.run_id.as_ref().ok_or_else(|| {
            RunnerError::Step(
                "patch gate requires an authoritative loop run id before development".to_string(),
            )
        })?;
        let config = patch_gate.config.clone();
        let artifact_dir = run_directory.join(ARTIFACTS_DIR);
        let request = PatchGateRequest {
            repo_root: &config.repository_root,
            artifact_dir: &artifact_dir,
            patch_id: run_id,
            patch,
            policy: &config.policy,
            apply_patch: config.apply_patch,
        };

        let decision = if config.apply_patch && !config.worktree_clean {
            let mut guard = DirtyWorktreePatchRunner;
            gate_patch(request, &mut guard)
        } else {
            gate_patch(request, &mut *patch_gate.runner)
        }
        .map_err(|error| RunnerError::Step(format!("patch gate failed: {error}")))?;

        let status = match decision.decision {
            PatchDecisionKind::Rejected => LoopStepStatus::Failed,
            PatchDecisionKind::Allowed | PatchDecisionKind::RequiresHumanReview => {
                LoopStepStatus::Completed
            }
        };
        self.pending_policy_decisions.push(decision);
        Ok(Some(status))
    }
}

impl<P: ModelProvider + ?Sized> StepRunner for ProviderStepRunner<'_, P> {
    fn prepare_workspace(&mut self, workspace: &LoopWorkspace) -> Result<(), RunnerError> {
        self.prepare_provider_workspace(workspace, None)
    }

    fn prepare_run(&mut self, workspace: &LoopWorkspace, run_id: &str) -> Result<(), RunnerError> {
        self.prepare_provider_workspace(workspace, Some(run_id))
    }

    fn step_request(&mut self, step: LoopStepName) -> Result<String, RunnerError> {
        let Some(role) = role_for_step(step) else {
            return Ok(no_model_request(step));
        };

        let request = self.model_request(
            role,
            role_step_prompt(step, role, self.context_bundle.as_ref()),
        );
        serde_json::to_string_pretty(&request).map_err(|error| {
            RunnerError::Step(format!(
                "failed to serialize {step:?} model request: {error}"
            ))
        })
    }

    fn run_step(&mut self, step: LoopStepName, request: &str) -> Result<StepOutput, RunnerError> {
        self.last_error_response = None;
        self.pending_policy_decisions.clear();

        let Some(role) = role_for_step(step) else {
            return Ok(StepOutput::completed(no_model_response(step)));
        };

        let model_request: ModelRequest = serde_json::from_str(request).map_err(|error| {
            RunnerError::Step(format!(
                "failed to parse {step:?} model request audit: {error}"
            ))
        })?;
        let initial_response = self.provider.complete(model_request).map_err(|error| {
            RunnerError::Step(format!("provider request failed for {step:?}: {error}"))
        })?;

        let mut repair_request_audit = None;
        let mut repair_response_content = None;
        let mut repair_error = None;
        let parsed =
            parse_role_response_with_repair(role, &initial_response.content, |repair_prompt| {
                let repair_request = self.model_request(role, repair_prompt.to_string());
                repair_request_audit = serde_json::to_string_pretty(&repair_request).ok();
                match self.provider.complete(repair_request) {
                    Ok(response) => {
                        repair_response_content = Some(response.content.clone());
                        response.content
                    }
                    Err(error) => {
                        repair_error = Some(error);
                        String::new()
                    }
                }
            });

        if let Some(error) = repair_error {
            self.last_error_response = Some(match repair_request_audit {
                Some(repair_request) => {
                    repair_error_transcript(&initial_response.content, &repair_request, &error)
                }
                None => initial_response.content,
            });
            return Err(RunnerError::Step(format!(
                "provider repair request failed for {step:?}: {error}"
            )));
        }

        let parsed = match parsed {
            Ok(parsed) => parsed,
            Err(error) => {
                self.last_error_response =
                    Some(match (repair_request_audit, repair_response_content) {
                        (Some(repair_request), Some(repair_response)) => repair_transcript(
                            &initial_response.content,
                            &repair_request,
                            &repair_response,
                        ),
                        _ => initial_response.content,
                    });
                return Err(RunnerError::Step(format!(
                    "failed to parse {step:?} provider response: {error}"
                )));
            }
        };
        let status = status_for_response(&parsed);
        let response = match (repair_request_audit, repair_response_content) {
            (Some(repair_request), Some(repair_response)) => {
                repair_transcript(&initial_response.content, &repair_request, &repair_response)
            }
            _ => initial_response.content,
        };
        let gated_status = match &parsed {
            RoleResponse::Developer(response) => self.gate_developer_patch(response)?,
            RoleResponse::Agent(_) | RoleResponse::Reviewer(_) => None,
        };

        Ok(StepOutput {
            response,
            artifact: None,
            status: gated_status.unwrap_or(status),
        })
    }

    fn drain_policy_decisions(&mut self) -> Result<Vec<PolicyDecision>, RunnerError> {
        Ok(std::mem::take(&mut self.pending_policy_decisions))
    }

    fn error_response(&self) -> Option<&str> {
        self.last_error_response.as_deref()
    }
}

impl<P: ModelProvider + ?Sized> ProviderStepRunner<'_, P> {
    fn prepare_provider_workspace(
        &mut self,
        workspace: &LoopWorkspace,
        run_id: Option<&str>,
    ) -> Result<(), RunnerError> {
        self.context_bundle = None;
        self.run_directory = Some(workspace.run_directory().to_path_buf());
        self.run_id = run_id.map(str::to_string);
        let Some(request) = &self.context_pack_request else {
            return Ok(());
        };
        let mut request = request.clone();
        request.run_directory = workspace.run_directory().to_path_buf();
        let bundle = pack_live_context(&request)
            .map_err(|error| RunnerError::Step(format!("failed to pack live context: {error}")))?;
        self.context_bundle = Some(bundle);
        Ok(())
    }
}

struct DirtyWorktreePatchRunner;

impl PatchCommandRunner for DirtyWorktreePatchRunner {
    fn run(
        &mut self,
        _repo_root: &Path,
        _command: PatchCommand,
        _patch: &str,
    ) -> Result<CommandOutput, PatchGateError> {
        Ok(CommandOutput::failure(
            "worktree is not clean; refusing to run git apply for an automatic patch",
        ))
    }
}

fn role_for_step(step: LoopStepName) -> Option<Role> {
    match step {
        LoopStepName::Research => Some(Role::Researcher),
        LoopStepName::Analysis => Some(Role::Analyzer),
        LoopStepName::SpecCreation => Some(Role::SpecWriter),
        LoopStepName::SpecReview => Some(Role::SpecReviewer),
        LoopStepName::Development => Some(Role::Developer),
        LoopStepName::OutputReview => Some(Role::OutputReviewer),
        LoopStepName::Testing | LoopStepName::EvalReport => None,
    }
}

fn role_step_prompt(step: LoopStepName, role: Role, context: Option<&ContextBundle>) -> String {
    let mut prompt = format!(
        "Run the {step:?} loop step as the {}. Return only JSON matching the response schema.",
        role.as_str()
    );

    if let Some(context) = context {
        append_context_to_prompt(&mut prompt, context);
    }

    prompt
}

fn append_context_to_prompt(prompt: &mut String, context: &ContextBundle) {
    prompt.push_str("\n\nRepository context:\n");
    prompt.push_str(&context.untrusted_context_marker);
    prompt.push('\n');

    for file in &context.files {
        prompt.push_str("\ncontext file\npath: ");
        prompt.push_str(&file.path);
        prompt.push_str("\nsha256: ");
        prompt.push_str(&file.sha256);
        prompt.push_str("\ncontent:\n");
        prompt.push_str(&file.content);
        if !file.content.ends_with('\n') {
            prompt.push('\n');
        }
    }

    if !context.warnings.is_empty() {
        prompt.push_str("\ncontext warnings:\n");
        for warning in &context.warnings {
            prompt.push_str("- ");
            prompt.push_str(warning);
            prompt.push('\n');
        }
    }
}

fn status_for_response(response: &RoleResponse) -> LoopStepStatus {
    match response {
        RoleResponse::Agent(response) => match response.status {
            AgentStatus::Passed => LoopStepStatus::Completed,
            AgentStatus::Blocked | AgentStatus::NeedsContext => LoopStepStatus::Blocked,
        },
        RoleResponse::Developer(response) => match response.status {
            DeveloperStatus::PatchProposed => LoopStepStatus::Completed,
            DeveloperStatus::Blocked | DeveloperStatus::NeedsContext => LoopStepStatus::Blocked,
        },
        RoleResponse::Reviewer(response) => match response.decision {
            ReviewDecision::ApproveSpec | ReviewDecision::ApproveForTests => LoopStepStatus::Passed,
            ReviewDecision::RequestChanges => LoopStepStatus::Blocked,
            ReviewDecision::Reject => LoopStepStatus::Failed,
        },
    }
}

fn no_model_request(step: LoopStepName) -> String {
    format!("{step:?} is deterministic for this slice; no model provider call will be made.")
}

fn no_model_response(step: LoopStepName) -> String {
    format!("{step:?} completed deterministically; no model provider call was made.")
}

fn repair_transcript(
    initial_response: &str,
    repair_request: &str,
    repair_response: &str,
) -> String {
    format!(
        "initial provider response:\n{initial_response}\n\nrepair provider request:\n{repair_request}\n\nrepair provider response:\n{repair_response}"
    )
}

fn repair_error_transcript(
    initial_response: &str,
    repair_request: &str,
    repair_error: &seaf_models::ModelError,
) -> String {
    format!(
        "initial provider response:\n{initial_response}\n\nrepair provider request:\n{repair_request}\n\nrepair provider error:\n{repair_error}"
    )
}
