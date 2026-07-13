use std::{collections::BTreeMap, error::Error, fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

const MAX_SENSITIVE_ENV_OCCURRENCES: usize = 64;
const MAX_SENSITIVE_ENV_VALUE_BYTES: usize = 4096;
const MAX_SENSITIVE_ENV_AGGREGATE_BYTES: usize = 65_536;

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
        return Err(EvalConfigError::MissingRequiredChecks);
    }
    let mut occurrences = 0usize;
    let mut aggregate_bytes = 0usize;
    for check in &config.evals.required {
        for (name, value) in &check.env {
            if value.is_empty() || !is_sensitive_env_name(name) {
                continue;
            }
            occurrences = occurrences
                .checked_add(1)
                .ok_or(EvalConfigError::TooManySensitiveEnvOccurrences)?;
            if occurrences > MAX_SENSITIVE_ENV_OCCURRENCES {
                return Err(EvalConfigError::TooManySensitiveEnvOccurrences);
            }
            if value.len() > MAX_SENSITIVE_ENV_VALUE_BYTES {
                return Err(EvalConfigError::SensitiveEnvValueTooLarge);
            }
            aggregate_bytes = aggregate_bytes
                .checked_add(value.len())
                .ok_or(EvalConfigError::SensitiveEnvAggregateTooLarge)?;
            if aggregate_bytes > MAX_SENSITIVE_ENV_AGGREGATE_BYTES {
                return Err(EvalConfigError::SensitiveEnvAggregateTooLarge);
            }
        }
    }
    Ok(())
}

fn is_sensitive_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD"]
        .iter()
        .any(|needle| upper.contains(needle))
}

#[derive(Debug)]
pub enum EvalConfigError {
    Parse(serde_yaml::Error),
    MissingRequiredChecks,
    TooManySensitiveEnvOccurrences,
    SensitiveEnvValueTooLarge,
    SensitiveEnvAggregateTooLarge,
}

impl fmt::Display for EvalConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => error.fmt(formatter),
            Self::MissingRequiredChecks => {
                formatter.write_str("eval config must include at least one required check")
            }
            Self::TooManySensitiveEnvOccurrences => formatter
                .write_str("eval config contains more than 64 sensitive env value occurrences"),
            Self::SensitiveEnvValueTooLarge => formatter
                .write_str("eval config contains a sensitive env value larger than 4096 bytes"),
            Self::SensitiveEnvAggregateTooLarge => {
                formatter.write_str("eval config sensitive env values exceed 65536 aggregate bytes")
            }
        }
    }
}

impl Error for EvalConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::MissingRequiredChecks
            | Self::TooManySensitiveEnvOccurrences
            | Self::SensitiveEnvValueTooLarge
            | Self::SensitiveEnvAggregateTooLarge => None,
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

    fn config_with_env(env: BTreeMap<String, String>) -> EvalConfig {
        EvalConfig {
            evals: EvalGroup {
                allow_commands: vec!["true".to_string()],
                required: vec![EvalCommandConfig {
                    name: "test".to_string(),
                    command: "true".to_string(),
                    cwd: None,
                    env,
                    timeout_ms: None,
                    max_output_bytes: None,
                }],
            },
            thresholds: None,
        }
    }

    #[test]
    fn validation_bounds_sensitive_env_corpus_before_redaction() {
        let sixty_five = (0..65)
            .map(|index| (format!("TOKEN_{index}"), "same".to_string()))
            .collect();
        assert!(matches!(
            validate_eval_config(&config_with_env(sixty_five)),
            Err(EvalConfigError::TooManySensitiveEnvOccurrences)
        ));

        let too_large = BTreeMap::from([("API_KEY".to_string(), "x".repeat(4097))]);
        assert!(matches!(
            validate_eval_config(&config_with_env(too_large)),
            Err(EvalConfigError::SensitiveEnvValueTooLarge)
        ));

        let aggregate = (0..17)
            .map(|index| {
                let length = if index == 16 { 1 } else { 4096 };
                (format!("SECRET_{index}"), "x".repeat(length))
            })
            .collect();
        assert!(matches!(
            validate_eval_config(&config_with_env(aggregate)),
            Err(EvalConfigError::SensitiveEnvAggregateTooLarge)
        ));
    }
}
