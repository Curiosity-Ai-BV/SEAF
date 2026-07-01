use std::sync::Arc;

use seaf_models::{
    ModelErrorKind, ModelMessage, ModelMessageRole, ModelProvider, ModelRequest, OllamaConfig,
    OllamaHttpClient, OllamaHttpError, OllamaHttpRequest, OllamaHttpResponse, OllamaProvider,
};
use serde_json::json;

#[test]
fn ollama_request_builder_uses_chat_endpoint_stream_false_schema_format_and_low_structured_temp() {
    let provider = OllamaProvider::default();

    let request = provider
        .build_chat_request(&structured_request(0.8))
        .expect("build ollama request");

    assert_eq!(request.method(), "POST");
    assert_eq!(request.url(), "http://localhost:11434/api/chat");
    assert_eq!(request.body()["model"], "gemma4:e4b-mlx");
    assert_eq!(request.body()["stream"], false);
    assert_eq!(request.body()["format"]["required"][0], "ok");
    assert_temperature(request.body(), 0.0);
    assert_eq!(request.body()["messages"][0]["role"], "system");
    assert_eq!(request.body()["messages"][1]["role"], "user");
}

#[test]
fn ollama_request_builder_preserves_unstructured_and_already_low_temperatures() {
    let provider = OllamaProvider::default();

    let unstructured = provider
        .build_chat_request(&unstructured_request(0.7))
        .expect("build unstructured request");
    let structured_low = provider
        .build_chat_request(&structured_request(0.1))
        .expect("build structured request");

    assert_temperature(unstructured.body(), 0.7);
    assert_temperature(structured_low.body(), 0.1);
}

#[test]
fn ollama_provider_extracts_non_streaming_chat_message_content() {
    let provider = OllamaProvider::with_http_client(
        OllamaConfig::default(),
        Arc::new(StaticClient::response(
            200,
            r#"{"message":{"role":"assistant","content":"{\"ok\":true}"},"eval_count":3}"#,
        )),
    );

    let response = provider
        .complete(structured_request(0.0))
        .expect("ollama response");

    assert_eq!(response.content, r#"{"ok":true}"#);
    assert_eq!(response.raw_provider_metadata["eval_count"], 3);
}

#[test]
fn ollama_provider_reports_connection_refused_as_not_running() {
    let provider = OllamaProvider::with_http_client(
        OllamaConfig::default(),
        Arc::new(StaticClient::error(OllamaHttpError::ConnectionRefused(
            "connection refused".to_string(),
        ))),
    );

    let error = provider.complete(unstructured_request(0.0)).unwrap_err();

    assert_eq!(error.kind, ModelErrorKind::Provider);
    assert!(error.retryable);
    assert!(error.message.contains("Ollama is not reachable"));
    assert!(error.message.contains("ollama serve"));
}

#[test]
fn ollama_provider_reports_timeout_with_timeout_metadata() {
    let provider = OllamaProvider::with_http_client(
        OllamaConfig::default(),
        Arc::new(StaticClient::error(OllamaHttpError::Timeout(
            "timed out".to_string(),
        ))),
    );

    let error = provider.complete(unstructured_request(0.0)).unwrap_err();

    assert_eq!(error.kind, ModelErrorKind::Timeout);
    assert!(error.retryable);
    assert_eq!(error.timeout_ms, Some(5_000));
    assert!(error.message.contains("timed out"));
}

#[test]
fn ollama_provider_reports_missing_model_with_pull_hint() {
    let provider = OllamaProvider::with_http_client(
        OllamaConfig::default(),
        Arc::new(StaticClient::response(
            404,
            r#"{"error":"model \"missing-model\" not found, try pulling it first"}"#,
        )),
    );

    let mut request = unstructured_request(0.0);
    request.model = "missing-model".to_string();
    let error = provider.complete(request).unwrap_err();

    assert_eq!(error.kind, ModelErrorKind::Provider);
    assert!(!error.retryable);
    assert!(error.message.contains("model is not installed"));
    assert!(error.message.contains("ollama pull missing-model"));
}

#[test]
fn ollama_provider_reports_generic_404_as_base_url_or_api_path_error() {
    let provider = OllamaProvider::with_http_client(
        OllamaConfig::default(),
        Arc::new(StaticClient::response(
            404,
            r#"{"error":"404 page not found"}"#,
        )),
    );

    let error = provider.complete(unstructured_request(0.0)).unwrap_err();

    assert_eq!(error.kind, ModelErrorKind::Provider);
    assert!(!error.retryable);
    assert!(error.message.contains("HTTP 404"));
    assert!(error.message.contains("base URL"));
    assert!(!error.message.contains("ollama pull"));
}

#[test]
fn ollama_provider_reports_non_json_model_content_for_structured_requests() {
    let provider = OllamaProvider::with_http_client(
        OllamaConfig::default(),
        Arc::new(StaticClient::response(
            200,
            r#"{"message":{"role":"assistant","content":"not json"}}"#,
        )),
    );

    let error = provider.complete(structured_request(0.0)).unwrap_err();

    assert_eq!(error.kind, ModelErrorKind::Provider);
    assert!(!error.retryable);
    assert!(error.message.contains("non-JSON model content"));
}

#[derive(Clone)]
struct StaticClient {
    result: Result<OllamaHttpResponse, OllamaHttpError>,
}

impl StaticClient {
    fn response(status_code: u16, body: &str) -> Self {
        Self {
            result: Ok(OllamaHttpResponse::new(status_code, body.to_string())),
        }
    }

    fn error(error: OllamaHttpError) -> Self {
        Self { result: Err(error) }
    }
}

impl OllamaHttpClient for StaticClient {
    fn send(
        &self,
        _request: OllamaHttpRequest,
        _timeout_ms: u64,
    ) -> Result<OllamaHttpResponse, OllamaHttpError> {
        self.result.clone()
    }
}

fn structured_request(temperature: f32) -> ModelRequest {
    ModelRequest {
        response_schema: Some(json!({
            "type": "object",
            "required": ["ok"],
            "properties": { "ok": { "type": "boolean" } }
        })),
        temperature,
        ..unstructured_request(temperature)
    }
}

fn unstructured_request(temperature: f32) -> ModelRequest {
    ModelRequest {
        model: "gemma4:e4b-mlx".to_string(),
        system: "Return JSON only.".to_string(),
        messages: vec![ModelMessage {
            role: ModelMessageRole::User,
            content: "Say ok.".to_string(),
        }],
        response_schema: None,
        temperature,
        timeout_ms: 5_000,
    }
}

fn assert_temperature(body: &serde_json::Value, expected: f64) {
    let actual = body["options"]["temperature"]
        .as_f64()
        .expect("temperature");
    assert!(
        (actual - expected).abs() < 0.000_001,
        "expected temperature {expected}, got {actual}"
    );
}
