pub mod fake;
pub mod ollama;
pub mod provider;

pub use fake::FakeProvider;
pub use ollama::{
    OllamaConfig, OllamaHttpClient, OllamaHttpError, OllamaHttpRequest, OllamaHttpResponse,
    OllamaProvider, DEFAULT_OLLAMA_BASE_URL, PROVIDER_RESPONSE_BYTE_CAP,
};
pub use provider::{
    ModelError, ModelErrorKind, ModelMessage, ModelMessageRole, ModelProvider, ModelRequest,
    ModelResponse,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_request_serializes_all_provider_contract_fields() {
        let request = ModelRequest {
            model: "local-small".to_string(),
            system: "Return JSON only.".to_string(),
            messages: vec![ModelMessage {
                role: ModelMessageRole::User,
                content: "Draft a patch plan.".to_string(),
            }],
            response_schema: Some(serde_json::json!({
                "type": "object",
                "required": ["summary"]
            })),
            temperature: 0.2,
            timeout_ms: 30_000,
        };

        let artifact = serde_json::to_value(&request).expect("serialize request");

        assert_eq!(artifact["model"], "local-small");
        assert_eq!(artifact["system"], "Return JSON only.");
        assert_eq!(artifact["messages"][0]["role"], "user");
        assert_eq!(artifact["messages"][0]["content"], "Draft a patch plan.");
        assert_eq!(artifact["response_schema"]["required"][0], "summary");
        let temperature = artifact["temperature"].as_f64().expect("temperature");
        assert!((temperature - 0.2).abs() < f32::EPSILON.into());
        assert_eq!(artifact["timeout_ms"], 30_000);
    }

    #[test]
    fn model_request_rejects_nan_temperature_in_serialized_artifacts() {
        let request = model_request_with_temperature(f32::NAN);

        let error = serde_json::to_value(&request).unwrap_err();

        assert!(error.to_string().contains("temperature must be finite"));
    }

    #[test]
    fn model_request_rejects_infinite_temperature_in_serialized_artifacts() {
        for temperature in [f32::INFINITY, f32::NEG_INFINITY] {
            let request = model_request_with_temperature(temperature);

            let error = serde_json::to_value(&request).unwrap_err();

            assert!(error.to_string().contains("temperature must be finite"));
        }
    }

    #[test]
    fn provider_dtos_reject_unknown_fields_when_deserialized() {
        let request = serde_json::json!({
            "model": "local-small",
            "system": "Return JSON only.",
            "messages": [{ "role": "user", "content": "Draft a patch plan." }],
            "temperature": 0.0,
            "timeout_ms": 30_000,
            "unexpected": true
        });
        let response = serde_json::json!({
            "content": "done",
            "latency_ms": 10,
            "raw_provider_metadata": {},
            "unexpected": true
        });
        let error = serde_json::json!({
            "kind": "provider",
            "message": "provider failed",
            "retryable": false,
            "metadata": {},
            "unexpected": true
        });

        assert!(serde_json::from_value::<ModelRequest>(request).is_err());
        assert!(serde_json::from_value::<ModelResponse>(response).is_err());
        assert!(serde_json::from_value::<ModelError>(error).is_err());
    }

    #[test]
    fn fake_provider_replays_scripted_responses_in_order_and_records_requests() {
        let provider = FakeProvider::new(vec![
            Ok(scripted_response("first", 11)),
            Ok(scripted_response("second", 7)),
        ]);

        let first = provider
            .complete(model_request("Explain issue one."))
            .expect("first completion");
        let second = provider
            .complete(model_request("Explain issue two."))
            .expect("second completion");
        let exhausted = provider
            .complete(model_request("No script left."))
            .unwrap_err();

        assert_eq!(first.content, "first");
        assert_eq!(first.latency_ms, 11);
        assert_eq!(second.content, "second");
        assert_eq!(second.raw_provider_metadata["provider"], "fake");
        assert_eq!(exhausted.kind, ModelErrorKind::ScriptExhausted);

        let requests = provider.requests().expect("requests");
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].messages[0].content, "Explain issue one.");
        assert_eq!(requests[1].messages[0].content, "Explain issue two.");
        assert_eq!(requests[2].messages[0].content, "No script left.");
    }

    #[test]
    fn fake_provider_replays_scripted_errors_as_serializable_loop_artifacts() {
        let provider = FakeProvider::new(vec![Err(ModelError::timeout(
            "local model timed out",
            1_000,
            serde_json::json!({ "provider": "fake" }),
        ))]);

        let error = provider
            .complete(model_request("Use timeout branch."))
            .unwrap_err();
        let artifact = serde_json::to_value(&error).expect("serialize model error");

        assert_eq!(artifact["kind"], "timeout");
        assert_eq!(artifact["message"], "local model timed out");
        assert_eq!(artifact["retryable"], true);
        assert_eq!(artifact["timeout_ms"], 1_000);
        assert_eq!(artifact["metadata"]["provider"], "fake");
    }

    fn model_request(content: &str) -> ModelRequest {
        ModelRequest {
            model: "fake-local".to_string(),
            system: "Follow the ticket.".to_string(),
            messages: vec![ModelMessage {
                role: ModelMessageRole::User,
                content: content.to_string(),
            }],
            response_schema: None,
            temperature: 0.0,
            timeout_ms: 5_000,
        }
    }

    fn model_request_with_temperature(temperature: f32) -> ModelRequest {
        ModelRequest {
            temperature,
            ..model_request("Check temperature serialization.")
        }
    }

    fn scripted_response(content: &str, latency_ms: u64) -> ModelResponse {
        ModelResponse {
            content: content.to_string(),
            latency_ms,
            raw_provider_metadata: serde_json::json!({ "provider": "fake" }),
        }
    }
}
