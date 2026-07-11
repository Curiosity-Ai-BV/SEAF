use std::path::{Path, PathBuf};

use seaf_core::{
    canonical_sha256_digest, LoopRun, LoopStepName, LoopStepStatus, Policy, TicketSpec,
};
use seaf_models::{ModelMessage, ModelMessageRole, ModelProvider, ModelRequest};

use crate::{
    context::{pack_live_context, ContextBundle, ContextPackRequest},
    parse_role_response_with_repair,
    policy_gate::{
        gate_patch_proposal, CommandOutput, PatchCommand, PatchCommandRunner, PatchDecisionKind,
        PatchGateError, PatchGateRequest, PolicyDecision,
    },
    runner::{RunnerError, StepRunner},
    state::step_file_stem,
    workspace::{LoopWorkspace, ARTIFACTS_DIR},
    AgentStatus, DeveloperResponse, DeveloperStatus, DevelopmentEvidence, ReviewDecision, Role,
    RoleResponse, StepOutput, ValidatedRoleArtifact,
};

pub struct ProviderStepRunner<'a, P: ModelProvider + ?Sized> {
    provider: &'a P,
    model: String,
    timeout_ms: u64,
    context_pack_request: Option<ContextPackRequest>,
    context_bundle: Option<ContextBundle>,
    ticket: Option<TicketSpec>,
    run: Option<LoopRun>,
    early_artifacts: Vec<PersistedRoleArtifact>,
    development_evidence: Option<PersistedDevelopmentEvidence>,
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

struct PersistedRoleArtifact {
    artifact_path: String,
    artifact_digest: String,
    artifact: ValidatedRoleArtifact,
}

struct PersistedDevelopmentEvidence {
    artifact_path: String,
    artifact_digest: String,
    evidence: DevelopmentEvidence,
}

struct GatedDeveloperPatch {
    status: LoopStepStatus,
    decision: PolicyDecision,
}

impl<'a, P: ModelProvider + ?Sized> ProviderStepRunner<'a, P> {
    pub fn new(provider: &'a P, model: impl Into<String>, timeout_ms: u64) -> Self {
        Self {
            provider,
            model: model.into(),
            timeout_ms,
            context_pack_request: None,
            context_bundle: None,
            ticket: None,
            run: None,
            early_artifacts: Vec::new(),
            development_evidence: None,
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

    pub fn with_ticket(mut self, ticket: TicketSpec) -> Self {
        self.ticket = Some(ticket);
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
    ) -> Result<Option<GatedDeveloperPatch>, RunnerError> {
        if response.status != DeveloperStatus::PatchProposed {
            return Ok(None);
        }

        let Some(patch_gate) = self.patch_gate.as_mut() else {
            return Err(RunnerError::Step(
                "patch_proposed Development response requires an authoritative patch gate"
                    .to_string(),
            ));
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
            gate_patch_proposal(request, &mut guard)
        } else {
            gate_patch_proposal(request, &mut *patch_gate.runner)
        }
        .map_err(|error| RunnerError::Step(format!("patch gate failed: {error}")))?;

        let status = match decision.decision {
            PatchDecisionKind::Rejected => LoopStepStatus::Failed,
            PatchDecisionKind::Allowed | PatchDecisionKind::RequiresHumanReview => {
                LoopStepStatus::Completed
            }
        };
        self.pending_policy_decisions.push(decision.clone());
        Ok(Some(GatedDeveloperPatch { status, decision }))
    }
}

impl<P: ModelProvider + ?Sized> StepRunner for ProviderStepRunner<'_, P> {
    fn prepare_workspace(&mut self, workspace: &LoopWorkspace) -> Result<(), RunnerError> {
        self.prepare_provider_workspace(workspace, None)
    }

    fn prepare_run(&mut self, workspace: &LoopWorkspace, run: &LoopRun) -> Result<(), RunnerError> {
        let ticket = self.ticket.as_ref().ok_or_else(|| {
            RunnerError::Step(
                "prepared provider run requires the exact effective ticket".to_string(),
            )
        })?;
        validate_prepared_ticket(ticket, run)?;
        self.run = Some(run.clone());
        self.prepare_provider_workspace(workspace, Some(&run.run_id))
    }

    fn prepare_step(
        &mut self,
        workspace: &LoopWorkspace,
        run: &LoopRun,
        step: LoopStepName,
    ) -> Result<(), RunnerError> {
        self.run = Some(run.clone());
        if matches!(step, LoopStepName::Development | LoopStepName::OutputReview) {
            self.require_approved_spec_review()?;
        }
        if step == LoopStepName::OutputReview {
            self.development_evidence = Some(load_verified_development_evidence(workspace, run)?);
        }
        Ok(())
    }

    fn step_request(&mut self, step: LoopStepName) -> Result<String, RunnerError> {
        let Some(role) = role_for_step(step) else {
            return Ok(no_model_request(step));
        };

        let user_prompt = self
            .structured_role_prompt(step, role)?
            .unwrap_or_else(|| role_step_prompt(step, role, self.context_bundle.as_ref()));
        let request = self.model_request(role, user_prompt);
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
        let initial_response = match self.provider.complete(model_request) {
            Ok(response) => response,
            Err(error) => {
                self.last_error_response = Some(provider_error_transcript(step, &error));
                return Err(RunnerError::Step(format!(
                    "provider request failed for {step:?}: {error}"
                )));
            }
        };

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
        let gated_patch = match &parsed {
            RoleResponse::Developer(response) => self.gate_developer_patch(response)?,
            RoleResponse::Agent(_) | RoleResponse::Reviewer(_) => None,
        };

        let artifact = if self.ticket.is_some() && step == LoopStepName::Development {
            let run_id = self.run_id.as_deref().ok_or_else(|| {
                RunnerError::Step(
                    "Development artifact requires a prepared authoritative loop run".to_string(),
                )
            })?;
            match (&parsed, &gated_patch) {
                (RoleResponse::Developer(response), Some(gated)) => {
                    let patch = response.patch.clone().ok_or_else(|| {
                        RunnerError::Step(
                            "validated patch_proposed response is missing its patch".to_string(),
                        )
                    })?;
                    let evidence = DevelopmentEvidence::new(
                        run_id,
                        response.clone(),
                        patch,
                        gated.decision.clone(),
                    )
                    .map_err(|error| {
                        RunnerError::Step(format!("failed to build Development evidence: {error}"))
                    })?;
                    let artifact_path = format!(
                        "{ARTIFACTS_DIR}/{}.json",
                        step_file_stem(LoopStepName::Development)
                    );
                    let artifact_digest = evidence.artifact_digest().map_err(|error| {
                        RunnerError::Step(format!("failed to digest Development evidence: {error}"))
                    })?;
                    let content = crate::ArtifactContent::new(
                        "json",
                        evidence.canonical_bytes().map_err(|error| {
                            RunnerError::Step(format!(
                                "failed to serialize Development evidence: {error}"
                            ))
                        })?,
                    );
                    self.development_evidence = Some(PersistedDevelopmentEvidence {
                        artifact_path,
                        artifact_digest,
                        evidence,
                    });
                    Some(content)
                }
                (RoleResponse::Developer(_), None) => {
                    Some(validated_role_artifact_content(run_id, step, role, parsed.clone())?.1)
                }
                _ => {
                    return Err(RunnerError::Step(
                        "Development response did not validate as developer evidence".to_string(),
                    ));
                }
            }
        } else if self.ticket.is_some()
            && (is_early_role_step(step) || step == LoopStepName::OutputReview)
        {
            let run_id = self.run_id.as_deref().ok_or_else(|| {
                RunnerError::Step(format!(
                    "{step:?} role artifact requires a prepared authoritative loop run"
                ))
            })?;
            let (artifact, content) =
                validated_role_artifact_content(run_id, step, role, parsed.clone())?;
            if is_early_role_step(step) {
                self.early_artifacts
                    .retain(|existing| existing.artifact.step != step);
                self.early_artifacts.push(PersistedRoleArtifact {
                    artifact_path: format!("{ARTIFACTS_DIR}/{}.json", step_file_stem(step)),
                    artifact_digest: artifact.artifact_digest().map_err(|error| {
                        RunnerError::Step(format!(
                            "failed to digest {step:?} role artifact: {error}"
                        ))
                    })?,
                    artifact,
                });
            }
            Some(content)
        } else {
            None
        };

        Ok(StepOutput {
            response,
            artifact,
            status: gated_patch.map_or(status, |gated| gated.status),
        })
    }

    fn drain_policy_decisions(&mut self) -> Result<Vec<PolicyDecision>, RunnerError> {
        Ok(std::mem::take(&mut self.pending_policy_decisions))
    }

    fn error_response(&self) -> Option<&str> {
        self.last_error_response.as_deref()
    }
}

fn validate_prepared_ticket(ticket: &TicketSpec, run: &LoopRun) -> Result<(), RunnerError> {
    if ticket.ticket_id != run.ticket_id {
        return Err(RunnerError::Step(format!(
            "effective ticket_id mismatch: expected {}, got {}",
            run.ticket_id, ticket.ticket_id
        )));
    }
    if ticket.goal_id != run.goal_id {
        return Err(RunnerError::Step(format!(
            "effective ticket goal_id mismatch: expected {}, got {}",
            run.goal_id, ticket.goal_id
        )));
    }
    let digest = canonical_sha256_digest(ticket).map_err(|error| {
        RunnerError::Step(format!("failed to digest effective ticket: {error}"))
    })?;
    if digest != run.input_digests.ticket {
        return Err(RunnerError::Step(format!(
            "effective ticket digest mismatch: expected {}, got {digest}",
            run.input_digests.ticket
        )));
    }
    Ok(())
}

impl<P: ModelProvider + ?Sized> ProviderStepRunner<'_, P> {
    fn structured_role_prompt(
        &self,
        step: LoopStepName,
        role: Role,
    ) -> Result<Option<String>, RunnerError> {
        if step == LoopStepName::Development && self.run.is_some() && self.ticket.is_some() {
            return self.development_prompt(role).map(Some);
        }
        if step == LoopStepName::OutputReview && self.run.is_some() && self.ticket.is_some() {
            return self.output_review_prompt(role).map(Some);
        }
        if !matches!(
            step,
            LoopStepName::Research
                | LoopStepName::Analysis
                | LoopStepName::SpecCreation
                | LoopStepName::SpecReview
        ) {
            return Ok(None);
        }
        let (Some(run), Some(ticket)) = (&self.run, &self.ticket) else {
            return Ok(None);
        };
        let prerequisites = match step {
            LoopStepName::Research => serde_json::json!({}),
            LoopStepName::Analysis => serde_json::json!({
                "research": self.required_early_response(LoopStepName::Research)?,
            }),
            LoopStepName::SpecCreation => serde_json::json!({
                "research": self.required_early_response(LoopStepName::Research)?,
                "analysis": self.required_early_response(LoopStepName::Analysis)?,
            }),
            LoopStepName::SpecReview => serde_json::json!({
                "proposed_spec": self.required_early_response(LoopStepName::SpecCreation)?,
            }),
            LoopStepName::Development
            | LoopStepName::OutputReview
            | LoopStepName::Testing
            | LoopStepName::EvalReport => unreachable!("non-early step returned above"),
        };
        let prompt = serde_json::json!({
            "instructions": format!(
                "Run the {step:?} loop step as the {}. Return only JSON matching the response schema.",
                role.as_str()
            ),
            "run_id": run.run_id,
            "input_digests": run.input_digests,
            "ticket": ticket,
            "prerequisites": prerequisites,
            "repository_context": self.context_bundle.as_ref().map(context_prompt),
        });
        serde_json::to_string(&prompt).map(Some).map_err(|error| {
            RunnerError::Step(format!("failed to serialize {step:?} role input: {error}"))
        })
    }

    fn required_early_response(&self, step: LoopStepName) -> Result<&RoleResponse, RunnerError> {
        self.early_artifacts
            .iter()
            .find(|artifact| artifact.artifact.step == step)
            .map(|artifact| &artifact.artifact.response)
            .ok_or_else(|| {
                RunnerError::Step(format!(
                    "missing validated {step:?} prerequisite for the next role request"
                ))
            })
    }

    fn development_prompt(&self, role: Role) -> Result<String, RunnerError> {
        let run = self.run.as_ref().ok_or_else(|| {
            RunnerError::Step("Development request requires authoritative loop state".to_string())
        })?;
        let spec_creation =
            self.required_persisted_early_artifact(LoopStepName::SpecCreation, Role::SpecWriter)?;
        let spec_review = self.require_approved_spec_review()?;
        let prompt = serde_json::json!({
            "instructions": format!(
                "Run the Development loop step as the {}. Return only JSON matching the response schema.",
                role.as_str()
            ),
            "run_id": run.run_id,
            "input_digests": run.input_digests,
            "approved_spec": {
                "spec_creation": persisted_artifact_prompt(spec_creation)?,
                "spec_review": persisted_artifact_prompt(spec_review)?,
            },
            // Development is the one role that still needs the initially bounded source files
            // in order to construct a patch. OutputReview never receives this context.
            "repository_context": self.context_bundle.as_ref().map(context_prompt),
        });
        serde_json::to_string(&prompt).map_err(|error| {
            RunnerError::Step(format!(
                "failed to serialize Development role input: {error}"
            ))
        })
    }

    fn output_review_prompt(&self, role: Role) -> Result<String, RunnerError> {
        let run = self.run.as_ref().ok_or_else(|| {
            RunnerError::Step("OutputReview request requires authoritative loop state".to_string())
        })?;
        let development = self.development_evidence.as_ref().ok_or_else(|| {
            RunnerError::Step(
                "OutputReview request requires verified Development evidence".to_string(),
            )
        })?;
        let spec_creation =
            self.required_persisted_early_artifact(LoopStepName::SpecCreation, Role::SpecWriter)?;
        let spec_review = self.require_approved_spec_review()?;
        let prompt = serde_json::json!({
            "instructions": format!(
                "Run the OutputReview loop step as the {}. Review only the persisted policy-gated candidate evidence and return only JSON matching the response schema.",
                role.as_str()
            ),
            "run_id": run.run_id,
            "input_digests": run.input_digests,
            "approved_spec_identity": {
                "spec_creation": persisted_artifact_identity(spec_creation),
                "spec_review": persisted_artifact_identity(spec_review),
            },
            "development_evidence": {
                "artifact_path": development.artifact_path,
                "artifact_digest": development.artifact_digest,
                "artifact": development.evidence,
            },
        });
        serde_json::to_string(&prompt).map_err(|error| {
            RunnerError::Step(format!(
                "failed to serialize OutputReview role input: {error}"
            ))
        })
    }

    fn required_persisted_early_artifact(
        &self,
        step: LoopStepName,
        role: Role,
    ) -> Result<&PersistedRoleArtifact, RunnerError> {
        self.early_artifacts
            .iter()
            .find(|artifact| artifact.artifact.step == step && artifact.artifact.role == role)
            .ok_or_else(|| {
                RunnerError::Step(format!(
                    "missing validated {step:?} artifact for the Development request"
                ))
            })
    }

    fn require_approved_spec_review(&self) -> Result<&PersistedRoleArtifact, RunnerError> {
        let spec_review =
            self.required_persisted_early_artifact(LoopStepName::SpecReview, Role::SpecReviewer)?;
        match &spec_review.artifact.response {
            RoleResponse::Reviewer(response)
                if response.decision == ReviewDecision::ApproveSpec =>
            {
                Ok(spec_review)
            }
            _ => Err(RunnerError::Step(
                "Development request requires an approving SpecReview artifact".to_string(),
            )),
        }
    }

    fn prepare_provider_workspace(
        &mut self,
        workspace: &LoopWorkspace,
        run_id: Option<&str>,
    ) -> Result<(), RunnerError> {
        self.context_bundle = None;
        self.early_artifacts.clear();
        self.development_evidence = None;
        self.run_directory = Some(workspace.run_directory().to_path_buf());
        self.run_id = run_id.map(str::to_string);
        if self.ticket.is_some() {
            let run = self.run.clone().ok_or_else(|| {
                RunnerError::Step("provider runner is missing authoritative loop state".to_string())
            })?;
            for (step, role) in early_step_roles() {
                let record = run
                    .steps
                    .iter()
                    .find(|record| record.name == step)
                    .ok_or_else(|| RunnerError::Step(format!("loop state is missing {step:?}")))?;
                if !matches!(
                    record.status,
                    LoopStepStatus::Completed
                        | LoopStepStatus::Passed
                        | LoopStepStatus::Blocked
                        | LoopStepStatus::Failed
                ) {
                    continue;
                }
                let (Some(path), Some(digest)) = (
                    record.artifact_path.as_deref(),
                    record.artifact_digest.as_deref(),
                ) else {
                    return Err(RunnerError::Step(format!(
                        "completed {step:?} step is missing its paired role artifact path and digest"
                    )));
                };
                let artifact =
                    ValidatedRoleArtifact::load(workspace, path, digest, &run.run_id, step, role)
                        .map_err(|error| {
                        RunnerError::Step(format!(
                            "failed to verify {step:?} role artifact: {error}"
                        ))
                    })?;
                self.early_artifacts.push(PersistedRoleArtifact {
                    artifact_path: path.to_string(),
                    artifact_digest: digest.to_string(),
                    artifact,
                });
            }
            self.verify_downstream_artifacts(workspace, &run)?;
            if required_terminal_record(&run, LoopStepName::Development)?.is_some() {
                self.require_approved_spec_review()?;
            }
        }
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

    fn verify_downstream_artifacts(
        &mut self,
        workspace: &LoopWorkspace,
        run: &LoopRun,
    ) -> Result<(), RunnerError> {
        let development = required_terminal_record(run, LoopStepName::Development)?;
        if let Some(record) = development {
            let (path, digest) = required_artifact_pair(record)?;
            match record.status {
                LoopStepStatus::Completed | LoopStepStatus::Failed => {
                    self.development_evidence =
                        Some(load_verified_development_evidence(workspace, run)?);
                }
                LoopStepStatus::Blocked => {
                    ValidatedRoleArtifact::load(
                        workspace,
                        path,
                        digest,
                        &run.run_id,
                        LoopStepName::Development,
                        Role::Developer,
                    )
                    .map_err(|error| {
                        RunnerError::Step(format!(
                            "failed to verify Development role artifact: {error}"
                        ))
                    })?;
                }
                LoopStepStatus::Passed | LoopStepStatus::Pending | LoopStepStatus::Running => {}
            }
        }

        if let Some(record) = required_terminal_record(run, LoopStepName::OutputReview)? {
            let (path, digest) = required_artifact_pair(record)?;
            ValidatedRoleArtifact::load(
                workspace,
                path,
                digest,
                &run.run_id,
                LoopStepName::OutputReview,
                Role::OutputReviewer,
            )
            .map_err(|error| {
                RunnerError::Step(format!(
                    "failed to verify OutputReview role artifact: {error}"
                ))
            })?;
        }
        Ok(())
    }
}

fn is_early_role_step(step: LoopStepName) -> bool {
    matches!(
        step,
        LoopStepName::Research
            | LoopStepName::Analysis
            | LoopStepName::SpecCreation
            | LoopStepName::SpecReview
    )
}

fn persisted_artifact_prompt(
    persisted: &PersistedRoleArtifact,
) -> Result<serde_json::Value, RunnerError> {
    serde_json::to_value(&persisted.artifact)
        .map(|artifact| {
            serde_json::json!({
                "artifact_path": persisted.artifact_path,
                "artifact_digest": persisted.artifact_digest,
                "artifact": artifact,
            })
        })
        .map_err(|error| {
            RunnerError::Step(format!(
                "failed to serialize approved spec artifact: {error}"
            ))
        })
}

fn persisted_artifact_identity(persisted: &PersistedRoleArtifact) -> serde_json::Value {
    serde_json::json!({
        "run_id": persisted.artifact.run_id,
        "step": persisted.artifact.step,
        "role": persisted.artifact.role,
        "response_digest": persisted.artifact.response_digest,
        "artifact_path": persisted.artifact_path,
        "artifact_digest": persisted.artifact_digest,
    })
}

fn validated_role_artifact_content(
    run_id: &str,
    step: LoopStepName,
    role: Role,
    response: RoleResponse,
) -> Result<(ValidatedRoleArtifact, crate::ArtifactContent), RunnerError> {
    let artifact = ValidatedRoleArtifact::new(run_id, step, role, response).map_err(|error| {
        RunnerError::Step(format!("failed to build {step:?} role artifact: {error}"))
    })?;
    let content = crate::ArtifactContent::new(
        "json",
        artifact.canonical_bytes().map_err(|error| {
            RunnerError::Step(format!(
                "failed to serialize {step:?} role artifact: {error}"
            ))
        })?,
    );
    Ok((artifact, content))
}

fn required_terminal_record(
    run: &LoopRun,
    step: LoopStepName,
) -> Result<Option<&seaf_core::LoopStepRecord>, RunnerError> {
    let record = run
        .steps
        .iter()
        .find(|record| record.name == step)
        .ok_or_else(|| RunnerError::Step(format!("loop state is missing {step:?}")))?;
    Ok(matches!(
        record.status,
        LoopStepStatus::Completed
            | LoopStepStatus::Passed
            | LoopStepStatus::Blocked
            | LoopStepStatus::Failed
    )
    .then_some(record))
}

fn required_artifact_pair(record: &seaf_core::LoopStepRecord) -> Result<(&str, &str), RunnerError> {
    match (
        record.artifact_path.as_deref(),
        record.artifact_digest.as_deref(),
    ) {
        (Some(path), Some(digest)) => Ok((path, digest)),
        _ => Err(RunnerError::Step(format!(
            "completed {:?} step is missing its paired artifact path and digest",
            record.name
        ))),
    }
}

fn load_verified_development_evidence(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<PersistedDevelopmentEvidence, RunnerError> {
    let record = run
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::Development)
        .ok_or_else(|| RunnerError::Step("loop state is missing Development".to_string()))?;
    let (path, digest) = required_artifact_pair(record)?;
    let evidence =
        DevelopmentEvidence::load(workspace, path, digest, &run.run_id).map_err(|error| {
            RunnerError::Step(format!(
                "failed to verify Development evidence artifact: {error}"
            ))
        })?;
    let mut matching = run.policy_decisions.iter().filter(|decision| {
        decision.get("patch_id").and_then(serde_json::Value::as_str) == Some(run.run_id.as_str())
    });
    let persisted = matching.next().ok_or_else(|| {
        RunnerError::Step(
            "Development evidence is missing its persisted run policy decision".to_string(),
        )
    })?;
    if matching.next().is_some() {
        return Err(RunnerError::Step(
            "Development evidence has multiple persisted run policy decisions".to_string(),
        ));
    }
    let persisted: PolicyDecision =
        serde_json::from_value(serde_json::to_value(persisted).map_err(|error| {
            RunnerError::Step(format!(
                "failed to inspect persisted policy decision: {error}"
            ))
        })?)
        .map_err(|error| {
            RunnerError::Step(format!("invalid persisted policy decision: {error}"))
        })?;
    if persisted != evidence.policy_decision {
        return Err(RunnerError::Step(
            "Development evidence policy decision does not exactly match run state".to_string(),
        ));
    }
    Ok(PersistedDevelopmentEvidence {
        artifact_path: path.to_string(),
        artifact_digest: digest.to_string(),
        evidence,
    })
}

fn early_step_roles() -> [(LoopStepName, Role); 4] {
    [
        (LoopStepName::Research, Role::Researcher),
        (LoopStepName::Analysis, Role::Analyzer),
        (LoopStepName::SpecCreation, Role::SpecWriter),
        (LoopStepName::SpecReview, Role::SpecReviewer),
    ]
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

fn context_prompt(context: &ContextBundle) -> String {
    let mut prompt = String::new();
    append_context_to_prompt(&mut prompt, context);
    prompt
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

fn provider_error_transcript(step: LoopStepName, error: &seaf_models::ModelError) -> String {
    let error_json = serde_json::to_string_pretty(error).unwrap_or_else(|_| error.to_string());
    format!("provider request failed for {step:?}:\n{error_json}")
}
