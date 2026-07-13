mod canonical;
mod eval_config;
mod models;
pub mod templates;
mod validation;

pub use canonical::{canonical_json_bytes, canonical_sha256_digest};
pub use eval_config::{
    parse_eval_config, validate_eval_config, EvalCommandConfig, EvalConfig, EvalConfigError,
    EvalGroup,
};
pub use models::*;
pub use validation::{
    is_portable_artifact_path, load_eval_report_file, load_goal_file, load_loop_run_file,
    load_policy_file, load_project_config_file, load_release_capsule_file, load_ticket_file,
    parse_ticket_spec, sha256_digest_file, validate_eval_report, validate_goal_spec,
    validate_loop_run, validate_policy, validate_project_config, validate_provider_exchange_record,
    validate_release_capsule, validate_seaf_event, validate_ticket_spec, FieldError,
    ValidationReport, ValidationResult,
};

pub fn framework_name() -> &'static str {
    "Self-Evolving Application Framework"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_framework_name() {
        assert_eq!(framework_name(), "Self-Evolving Application Framework");
    }
}
