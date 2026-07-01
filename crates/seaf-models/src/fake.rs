use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
};

use serde_json::json;

use crate::provider::{ModelError, ModelProvider, ModelRequest, ModelResponse};

#[derive(Debug)]
pub struct FakeProvider {
    state: Mutex<FakeProviderState>,
}

#[derive(Debug)]
struct FakeProviderState {
    script: VecDeque<Result<ModelResponse, ModelError>>,
    requests: Vec<ModelRequest>,
}

impl FakeProvider {
    pub fn new(script: Vec<Result<ModelResponse, ModelError>>) -> Self {
        Self {
            state: Mutex::new(FakeProviderState {
                script: script.into_iter().collect(),
                requests: Vec::new(),
            }),
        }
    }

    pub fn requests(&self) -> Result<Vec<ModelRequest>, ModelError> {
        Ok(fake_state(&self.state)?.requests.clone())
    }
}

impl ModelProvider for FakeProvider {
    fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let mut state = fake_state(&self.state)?;
        state.requests.push(request);

        state.script.pop_front().unwrap_or_else(|| {
            Err(ModelError::script_exhausted(
                "fake provider script exhausted",
            ))
        })
    }
}

fn fake_state(
    state: &Mutex<FakeProviderState>,
) -> Result<MutexGuard<'_, FakeProviderState>, ModelError> {
    state
        .lock()
        .map_err(|_| fake_state_error("fake provider state lock poisoned"))
}

fn fake_state_error(message: &str) -> ModelError {
    ModelError::provider(message, false, json!({ "provider": "fake" }))
}
