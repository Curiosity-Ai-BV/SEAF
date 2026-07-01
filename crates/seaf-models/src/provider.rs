use std::{error::Error, fmt};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

pub trait ModelProvider {
    fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<ModelMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<Value>,
    #[serde(
        serialize_with = "serialize_finite_f32",
        deserialize_with = "deserialize_finite_f32"
    )]
    pub temperature: f32,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMessage {
    pub role: ModelMessageRole,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelMessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelResponse {
    pub content: String,
    pub latency_ms: u64,
    pub raw_provider_metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelError {
    pub kind: ModelErrorKind,
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub metadata: Value,
}

impl ModelError {
    pub fn provider(message: impl Into<String>, retryable: bool, metadata: Value) -> Self {
        Self {
            kind: ModelErrorKind::Provider,
            message: message.into(),
            retryable,
            timeout_ms: None,
            metadata,
        }
    }

    pub fn timeout(message: impl Into<String>, timeout_ms: u64, metadata: Value) -> Self {
        Self {
            kind: ModelErrorKind::Timeout,
            message: message.into(),
            retryable: true,
            timeout_ms: Some(timeout_ms),
            metadata,
        }
    }

    pub fn script_exhausted(message: impl Into<String>) -> Self {
        Self {
            kind: ModelErrorKind::ScriptExhausted,
            message: message.into(),
            retryable: false,
            timeout_ms: None,
            metadata: Value::Null,
        }
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind, self.message)
    }
}

impl Error for ModelError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelErrorKind {
    Provider,
    Timeout,
    ScriptExhausted,
}

impl fmt::Display for ModelErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Provider => "provider",
            Self::Timeout => "timeout",
            Self::ScriptExhausted => "script_exhausted",
        };
        formatter.write_str(label)
    }
}

fn serialize_finite_f32<S>(value: &f32, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if value.is_finite() {
        serializer.serialize_f32(*value)
    } else {
        Err(serde::ser::Error::custom("temperature must be finite"))
    }
}

fn deserialize_finite_f32<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    let value = f32::deserialize(deserializer)?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(serde::de::Error::custom("temperature must be finite"))
    }
}
