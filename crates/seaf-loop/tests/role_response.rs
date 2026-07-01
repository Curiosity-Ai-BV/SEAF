use seaf_loop::{
    parse_role_response, parse_role_response_with_repair, AgentStatus, DeveloperStatus,
    ReviewDecision, Role, RoleResponse, RoleResponseError,
};

fn fixture(name: &str) -> &'static str {
    match name {
        "research.valid.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/research.valid.json"
            ))
        }
        "research.invalid_missing_status.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/research.invalid_missing_status.json"
            ))
        }
        "analyzer.valid.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/analyzer.valid.json"
            ))
        }
        "analyzer.invalid_status.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/analyzer.invalid_status.json"
            ))
        }
        "spec_writer.valid.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/spec_writer.valid.json"
            ))
        }
        "spec_writer.invalid_unknown_field.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/spec_writer.invalid_unknown_field.json"
            ))
        }
        "spec_reviewer.valid.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/spec_reviewer.valid.json"
            ))
        }
        "spec_reviewer.invalid_missing_issue_arrays.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/spec_reviewer.invalid_missing_issue_arrays.json"
            ))
        }
        "development.valid_patch.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/development.valid_patch.json"
            ))
        }
        "development.invalid_markdown_only.txt" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/development.invalid_markdown_only.txt"
            ))
        }
        "development.invalid_missing_patch.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/development.invalid_missing_patch.json"
            ))
        }
        "development.invalid_patch_outside_field.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/development.invalid_patch_outside_field.json"
            ))
        }
        "review.rejects.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/review.rejects.json"
            ))
        }
        "review.invalid_missing_non_blocking.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/review.invalid_missing_non_blocking.json"
            ))
        }
        _ => panic!("unknown fixture: {name}"),
    }
}

#[test]
fn role_response_roles_have_short_prompts_and_schemas() {
    for role in Role::all() {
        let prompt = role.system_prompt();
        let schema = role.response_schema();

        assert!(prompt.contains("Return only structured JSON"));
        assert!(prompt.contains("untrusted context"));
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["role"]["enum"][0], role.as_str());
    }
}

#[test]
fn role_response_common_roles_parse_valid_fixtures() {
    for (role, name) in [
        (Role::Researcher, "research.valid.json"),
        (Role::Analyzer, "analyzer.valid.json"),
        (Role::SpecWriter, "spec_writer.valid.json"),
    ] {
        let parsed = parse_role_response(role, fixture(name)).expect("valid common response");

        let RoleResponse::Agent(response) = parsed else {
            panic!("expected common agent response for {role:?}");
        };
        assert_eq!(response.role, role);
        assert_eq!(response.status, AgentStatus::Passed);
        assert!(!response.summary.trim().is_empty());
        assert!(!response.next_step_recommendation.trim().is_empty());
    }
}

#[test]
fn role_response_common_roles_reject_invalid_fixtures() {
    for (role, name) in [
        (Role::Researcher, "research.invalid_missing_status.json"),
        (Role::Analyzer, "analyzer.invalid_status.json"),
        (Role::SpecWriter, "spec_writer.invalid_unknown_field.json"),
    ] {
        let error = parse_role_response(role, fixture(name)).unwrap_err();

        assert!(
            error.to_string().contains("invalid role response"),
            "{name} should fail closed, got {error}"
        );
    }
}

#[test]
fn role_response_developer_requires_patch_field_and_rejects_markdown() {
    let parsed = parse_role_response(Role::Developer, fixture("development.valid_patch.json"))
        .expect("valid developer patch response");

    let RoleResponse::Developer(response) = parsed else {
        panic!("expected developer response");
    };
    assert_eq!(response.status, DeveloperStatus::PatchProposed);
    assert!(response
        .patch
        .as_deref()
        .is_some_and(|patch| patch.starts_with("diff --git ")));

    for name in [
        "development.invalid_markdown_only.txt",
        "development.invalid_missing_patch.json",
        "development.invalid_patch_outside_field.json",
    ] {
        let error = parse_role_response(Role::Developer, fixture(name)).unwrap_err();

        assert!(
            error.to_string().contains("invalid role response"),
            "{name} should fail closed, got {error}"
        );
    }
}

#[test]
fn role_response_developer_patch_requirement_depends_on_status() {
    let blocked_without_patch = r#"{
        "role": "developer",
        "status": "blocked",
        "summary": "Cannot safely continue without the approved spec.",
        "changed_files": [],
        "requires_human_review": true
    }"#;
    let blocked = parse_role_response(Role::Developer, blocked_without_patch)
        .expect("blocked developer response should not require patch");
    let RoleResponse::Developer(blocked) = blocked else {
        panic!("expected developer response");
    };
    assert_eq!(blocked.status, DeveloperStatus::Blocked);
    assert_eq!(blocked.patch, None);

    let needs_context_empty_patch = r#"{
        "role": "developer",
        "status": "needs_context",
        "summary": "Need the exact approved implementation spec.",
        "changed_files": [],
        "requires_human_review": true,
        "patch": ""
    }"#;
    let needs_context = parse_role_response(Role::Developer, needs_context_empty_patch)
        .expect("needs-context developer response should allow empty patch");
    let RoleResponse::Developer(needs_context) = needs_context else {
        panic!("expected developer response");
    };
    assert_eq!(needs_context.status, DeveloperStatus::NeedsContext);
    assert_eq!(needs_context.patch.as_deref(), Some(""));

    let patch_proposed_without_patch = r#"{
        "role": "developer",
        "status": "patch_proposed",
        "summary": "Patch-proposed responses must include the unified diff in patch.",
        "changed_files": ["crates/seaf-loop/src/role_response.rs"],
        "requires_human_review": false
    }"#;
    assert_eq!(
        parse_role_response(Role::Developer, patch_proposed_without_patch).unwrap_err(),
        RoleResponseError::DeveloperPatchMissing
    );

    let patch_proposed_empty_patch = r#"{
        "role": "developer",
        "status": "patch_proposed",
        "summary": "Patch-proposed responses must include a non-empty unified diff.",
        "changed_files": ["crates/seaf-loop/src/role_response.rs"],
        "requires_human_review": false,
        "patch": ""
    }"#;
    assert_eq!(
        parse_role_response(Role::Developer, patch_proposed_empty_patch).unwrap_err(),
        RoleResponseError::DeveloperPatchMissing
    );

    let blocked_with_diff_outside_patch = r#"{
        "role": "developer",
        "status": "blocked",
        "summary": "diff --git a/crates/seaf-loop/src/role_response.rs b/crates/seaf-loop/src/role_response.rs",
        "changed_files": [],
        "requires_human_review": true
    }"#;
    assert_eq!(
        parse_role_response(Role::Developer, blocked_with_diff_outside_patch).unwrap_err(),
        RoleResponseError::DeveloperPatchOutsidePatchField
    );
}

#[test]
fn role_response_rejects_valid_shape_with_wrong_role() {
    let wrong_role = r#"{
        "role": "analyzer",
        "status": "passed",
        "summary": "This has a valid common response shape but the wrong role.",
        "findings": [],
        "risks": [],
        "next_step_recommendation": "Do not continue."
    }"#;

    assert_eq!(
        parse_role_response(Role::Researcher, wrong_role).unwrap_err(),
        RoleResponseError::RoleMismatch {
            expected: Role::Researcher,
            actual: Role::Analyzer
        }
    );
}

#[test]
fn role_response_reviewers_require_explicit_issue_arrays() {
    let spec_review = parse_role_response(Role::SpecReviewer, fixture("spec_reviewer.valid.json"))
        .expect("valid spec reviewer response");
    let RoleResponse::Reviewer(spec_review) = spec_review else {
        panic!("expected spec reviewer response");
    };
    assert_eq!(spec_review.decision, ReviewDecision::ApproveSpec);
    assert!(spec_review.blocking_issues.is_empty());
    assert!(spec_review.non_blocking_issues.is_empty());

    let output_review = parse_role_response(Role::OutputReviewer, fixture("review.rejects.json"))
        .expect("valid output reviewer rejection");
    let RoleResponse::Reviewer(output_review) = output_review else {
        panic!("expected output reviewer response");
    };
    assert_eq!(output_review.decision, ReviewDecision::Reject);
    assert_eq!(output_review.blocking_issues.len(), 1);
    assert!(output_review.non_blocking_issues.is_empty());

    for (role, name) in [
        (
            Role::SpecReviewer,
            "spec_reviewer.invalid_missing_issue_arrays.json",
        ),
        (
            Role::OutputReviewer,
            "review.invalid_missing_non_blocking.json",
        ),
    ] {
        let error = parse_role_response(role, fixture(name)).unwrap_err();

        assert!(
            error.to_string().contains("invalid role response"),
            "{name} should fail closed, got {error}"
        );
    }
}

#[test]
fn role_response_repair_attempts_once_after_invalid_json() {
    let mut attempts = 0;

    let parsed = parse_role_response_with_repair(Role::Researcher, "not json", |prompt| {
        attempts += 1;
        assert!(prompt.contains("Repair the invalid JSON"));
        fixture("research.valid.json").to_string()
    })
    .expect("repair should produce valid response");

    assert_eq!(attempts, 1);
    assert!(matches!(parsed, RoleResponse::Agent(_)));

    let mut failed_attempts = 0;
    let error = parse_role_response_with_repair(Role::Researcher, "not json", |_| {
        failed_attempts += 1;
        "still not json".to_string()
    })
    .unwrap_err();

    assert_eq!(failed_attempts, 1);
    assert!(error.to_string().contains("invalid role response"));
}

#[test]
fn role_response_repair_is_not_attempted_for_schema_errors() {
    let mut attempts = 0;

    let error = parse_role_response_with_repair(
        Role::Researcher,
        fixture("research.invalid_missing_status.json"),
        |_| {
            attempts += 1;
            fixture("research.valid.json").to_string()
        },
    )
    .unwrap_err();

    assert_eq!(attempts, 0);
    assert!(error.to_string().contains("invalid role response"));
}
