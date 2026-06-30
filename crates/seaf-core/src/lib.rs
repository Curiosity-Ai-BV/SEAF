mod models;
pub mod templates;
mod validation;

pub use models::*;
pub use validation::{
    load_goal_file, load_policy_file, load_release_capsule_file, validate_goal_spec,
    validate_policy, validate_release_capsule, FieldError, ValidationReport, ValidationResult,
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
