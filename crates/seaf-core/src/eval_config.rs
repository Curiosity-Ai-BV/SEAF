use std::{collections::BTreeMap, error::Error, fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalConfig {
    pub evals: EvalGroup,
    #[serde(default)]
    pub thresholds: Option<serde_yaml::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalGroup {
    #[serde(default)]
    pub allow_commands: Vec<String>,
    pub required: Vec<EvalCommandConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalCommandConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
}

pub fn parse_eval_config(text: &str) -> Result<EvalConfig, EvalConfigError> {
    let config = serde_yaml::from_str(text).map_err(EvalConfigError::Parse)?;
    validate_eval_config(&config)?;
    Ok(config)
}

pub fn validate_eval_config(config: &EvalConfig) -> Result<(), EvalConfigError> {
    if config.evals.required.is_empty() {
        Err(EvalConfigError::MissingRequiredChecks)
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub enum EvalConfigError {
    Parse(serde_yaml::Error),
    MissingRequiredChecks,
}

impl fmt::Display for EvalConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => error.fmt(formatter),
            Self::MissingRequiredChecks => {
                formatter.write_str("eval config must include at least one required check")
            }
        }
    }
}

impl Error for EvalConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::MissingRequiredChecks => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_eval_config_rejects_unknown_fields() {
        let error = parse_eval_config(
            r#"evals:
  allow_commands: [cargo]
  required:
    - name: tests
      command: cargo test
      unsupported: true
"#,
        )
        .expect_err("unknown check fields must be rejected");

        assert!(error.to_string().contains("unknown field `unsupported`"));
    }

    #[test]
    fn parse_eval_config_requires_at_least_one_check() {
        let error = parse_eval_config(
            r#"evals:
  allow_commands: []
  required: []
"#,
        )
        .expect_err("empty required checks must be rejected");

        assert_eq!(
            error.to_string(),
            "eval config must include at least one required check"
        );
    }
}
