use seaf_core::{LoopStepName, LoopStepStatus};
use seaf_models::{ModelMessage, ModelMessageRole, ModelProvider, ModelRequest};

use crate::{
    parse_role_response_with_repair,
    runner::{RunnerError, StepRunner},
    AgentStatus, DeveloperStatus, ReviewDecision, Role, RoleResponse, StepOutput,
};

pub struct ProviderStepRunner<'a, P: ModelProvider + ?Sized> {
    provider: &'a P,
    model: String,
    timeout_ms: u64,
    last_error_response: Option<String>,
}

impl<'a, P: ModelProvider + ?Sized> ProviderStepRunner<'a, P> {
    pub fn new(provider: &'a P, model: impl Into<String>, timeout_ms: u64) -> Self {
        Self {
            provider,
            model: model.into(),
            timeout_ms,
            last_error_response: None,
        }
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
}

impl<P: ModelProvider + ?Sized> StepRunner for ProviderStepRunner<'_, P> {
    fn step_request(&mut self, step: LoopStepName) -> Result<String, RunnerError> {
        let Some(role) = role_for_step(step) else {
            return Ok(no_model_request(step));
        };

        let request = self.model_request(role, role_step_prompt(step, role));
        serde_json::to_string_pretty(&request).map_err(|error| {
            RunnerError::Step(format!(
                "failed to serialize {step:?} model request: {error}"
            ))
        })
    }

    fn run_step(&mut self, step: LoopStepName, request: &str) -> Result<StepOutput, RunnerError> {
        self.last_error_response = None;

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

        Ok(StepOutput {
            response,
            artifact: None,
            status,
        })
    }

    fn error_response(&self) -> Option<&str> {
        self.last_error_response.as_deref()
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

fn role_step_prompt(step: LoopStepName, role: Role) -> String {
    format!(
        "Run the {step:?} loop step as the {}. Return only JSON matching the response schema.",
        role.as_str()
    )
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
