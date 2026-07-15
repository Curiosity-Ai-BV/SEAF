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
        "research.valid_needs_context.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/research.valid_needs_context.json"
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
        "development.valid_needs_context.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/development.valid_needs_context.json"
            ))
        }
        "development.invalid_needs_context_missing_request.json" => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/model-responses/development.invalid_needs_context_missing_request.json"
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
        let object_schema = match role {
            Role::Researcher | Role::Analyzer | Role::SpecWriter => {
                status_branch(&schema, "passed")
            }
            Role::Developer => status_branch(&schema, "patch_proposed"),
            Role::SpecReviewer | Role::OutputReviewer => &schema,
        };
        assert_eq!(object_schema["type"], "object");
        assert_eq!(
            object_schema["properties"]["role"]["enum"][0],
            role.as_str()
        );
    }
}

#[test]
fn developer_prompt_requires_a_complete_git_style_diff_without_prose_or_omitted_hunks() {
    let prompt = Role::Developer.system_prompt();

    for required_component in ["diff --git", "---", "+++", "@@"] {
        assert!(
            prompt.contains(required_component),
            "Developer prompt must show the required {required_component:?} diff component"
        );
    }
    assert!(prompt.contains("complete git-style unified diff"));
    assert!(prompt.contains("Do not include prose"));
    assert!(prompt.contains("do not omit hunk headers"));
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
fn role_response_needs_context_requires_one_structured_request() {
    for (role, name) in [
        (Role::Researcher, "research.valid_needs_context.json"),
        (Role::Developer, "development.valid_needs_context.json"),
    ] {
        let parsed =
            parse_role_response(role, fixture(name)).expect("valid needs-context response");
        assert!(matches!(
            parsed,
            RoleResponse::Agent(_) | RoleResponse::Developer(_)
        ));
    }

    let missing_request = r#"{
        "role": "researcher",
        "status": "needs_context",
        "summary": "The current evidence does not establish the policy boundary.",
        "findings": [],
        "risks": [],
        "next_step_recommendation": "Load the policy module."
    }"#;
    assert!(parse_role_response(Role::Researcher, missing_request).is_err());
    assert!(parse_role_response(
        Role::Developer,
        fixture("development.invalid_needs_context_missing_request.json")
    )
    .is_err());
}

#[test]
fn context_request_schema_and_runtime_reject_the_same_invalid_contracts() {
    let valid_request = serde_json::json!({
        "paths": ["crates/seaf-loop/src/policy.rs"],
        "reason": "The policy normalizer defines the repository path boundary."
    });

    let invalid_requests = [
        (
            "empty paths",
            serde_json::json!({ "paths": [], "reason": "Needed." }),
        ),
        (
            "too many paths",
            serde_json::json!({
                "paths": (0..9).map(|index| format!("docs/{index}.md")).collect::<Vec<_>>(),
                "reason": "Needed."
            }),
        ),
        (
            "duplicate paths",
            serde_json::json!({ "paths": ["docs/a.md", "docs/a.md"], "reason": "Needed." }),
        ),
        (
            "absolute path",
            serde_json::json!({ "paths": ["/etc/passwd"], "reason": "Needed." }),
        ),
        (
            "current directory path",
            serde_json::json!({ "paths": ["."], "reason": "Needed." }),
        ),
        (
            "parent directory path",
            serde_json::json!({ "paths": [".."], "reason": "Needed." }),
        ),
        (
            "traversal path",
            serde_json::json!({ "paths": ["docs/../secret.md"], "reason": "Needed." }),
        ),
        (
            "backslash path",
            serde_json::json!({ "paths": ["docs\\secret.md"], "reason": "Needed." }),
        ),
        (
            "control path",
            serde_json::json!({ "paths": ["docs/\u{0}secret.md"], "reason": "Needed." }),
        ),
        (
            "empty reason",
            serde_json::json!({ "paths": ["docs/a.md"], "reason": " \t " }),
        ),
        (
            "control reason",
            serde_json::json!({ "paths": ["docs/a.md"], "reason": "Need\nthis." }),
        ),
        (
            "oversized reason",
            serde_json::json!({ "paths": ["docs/a.md"], "reason": "x".repeat(1025) }),
        ),
        (
            "unknown request field",
            serde_json::json!({
                "paths": ["docs/a.md"],
                "reason": "Needed.",
                "extra": true
            }),
        ),
    ];

    for role in [
        Role::Researcher,
        Role::Analyzer,
        Role::SpecWriter,
        Role::Developer,
    ] {
        assert_context_request_schema_contract(role);
        assert_runtime(
            role,
            needs_context_response(role, Some(valid_request.clone())),
            true,
        );

        for (case, request) in &invalid_requests {
            assert_runtime(
                role,
                needs_context_response(role, Some(request.clone())),
                false,
            );
            assert!(
                parse_role_response(
                    role,
                    &needs_context_response(role, Some(request.clone())).to_string()
                )
                .is_err(),
                "{role:?} runtime accepted {case}"
            );
        }

        assert_runtime(role, needs_context_response(role, None), false);
    }
}

#[test]
fn context_request_schema_and_runtime_forbid_requests_on_other_statuses() {
    let request = serde_json::json!({
        "paths": ["docs/agent-loop.md"],
        "reason": "Needed."
    });

    for (role, status) in [
        (Role::Researcher, "passed"),
        (Role::Researcher, "blocked"),
        (Role::Analyzer, "passed"),
        (Role::SpecWriter, "blocked"),
        (Role::Developer, "patch_proposed"),
        (Role::Developer, "blocked"),
    ] {
        let mut response = response_for_status(role, status);
        response["context_request"] = request.clone();
        assert_runtime(role, response, false);
    }
}

#[test]
fn agent_status_schemas_use_closed_branches_that_match_runtime() {
    for role in [Role::Researcher, Role::Analyzer, Role::SpecWriter] {
        let schema = role.response_schema();
        assert_closed_status_union(&schema);

        for status in ["passed", "blocked", "needs_context"] {
            let branch = status_branch(&schema, status);
            let mut expected_properties = vec![
                "findings",
                "next_step_recommendation",
                "risks",
                "role",
                "status",
                "summary",
            ];
            let mut expected_required = expected_properties.clone();
            if status == "needs_context" {
                expected_properties.push("context_request");
                expected_required.push("context_request");
            }

            assert_closed_branch(role, branch, &expected_properties, &expected_required);
            assert_runtime(
                role,
                response_for_status(role, status),
                status != "needs_context",
            );
        }
    }
}

#[test]
fn developer_status_schema_uses_closed_branches_that_match_runtime() {
    let role = Role::Developer;
    let schema = role.response_schema();
    assert_closed_status_union(&schema);

    for status in ["patch_proposed", "blocked", "needs_context"] {
        let branch = status_branch(&schema, status);
        let mut expected_properties = vec![
            "changed_files",
            "patch",
            "requires_human_review",
            "role",
            "status",
            "summary",
        ];
        let mut expected_required = vec![
            "changed_files",
            "requires_human_review",
            "role",
            "status",
            "summary",
        ];
        if status == "patch_proposed" {
            expected_required.push("patch");
        }
        if status == "needs_context" {
            expected_properties.push("context_request");
            expected_required.push("context_request");
        }

        assert_closed_branch(role, branch, &expected_properties, &expected_required);
        assert_runtime(
            role,
            response_for_status(role, status),
            status != "needs_context",
        );
    }
}

#[test]
fn context_request_schema_errors_are_not_repairable() {
    let mut repairs = 0;
    let invalid = needs_context_response(
        Role::Researcher,
        Some(serde_json::json!({ "paths": [], "reason": "Needed." })),
    );

    assert!(
        parse_role_response_with_repair(Role::Researcher, &invalid.to_string(), |_| {
            repairs += 1;
            fixture("research.valid_needs_context.json").to_string()
        })
        .is_err()
    );
    assert_eq!(repairs, 0);
}

fn assert_runtime(role: Role, response: serde_json::Value, expected_valid: bool) {
    assert_eq!(
        parse_role_response(role, &response.to_string()).is_ok(),
        expected_valid,
        "{role:?} runtime parity mismatch for {response}"
    );
}

fn assert_context_request_schema_contract(role: Role) {
    let schema = role.response_schema();
    let request = &status_branch(&schema, "needs_context")["properties"]["context_request"];
    assert_eq!(request["type"], "object");
    assert_eq!(request["additionalProperties"], false);
    assert_eq!(request["required"], serde_json::json!(["paths", "reason"]));
    assert_eq!(request["properties"]["paths"]["type"], "array");
    assert_eq!(request["properties"]["paths"]["minItems"], 1);
    assert_eq!(request["properties"]["paths"]["maxItems"], 8);
    assert_eq!(request["properties"]["paths"]["uniqueItems"], true);
    assert_eq!(request["properties"]["paths"]["items"]["type"], "string");
    assert_eq!(
        request["properties"]["paths"]["items"]["pattern"],
        r"^(?!/)(?![A-Za-z]:)(?!\.{1,2}(?:/|$))(?!.*\/\.{1,2}(?:/|$))(?!.*//)(?!.*[\\\u0000-\u001F\u007F-\u009F])[^/]+(?:/[^/]+)*$"
    );
    assert_eq!(request["properties"]["reason"]["type"], "string");
    assert_eq!(request["properties"]["reason"]["minLength"], 1);
    assert_eq!(request["properties"]["reason"]["maxLength"], 1024);
    assert_eq!(
        request["properties"]["reason"]["pattern"],
        r"^(?=.*\S)[^\u0000-\u001F\u007F-\u009F]*$"
    );
    assert!(schema.get("allOf").is_none());
}

fn status_branch<'a>(schema: &'a serde_json::Value, status: &str) -> &'a serde_json::Value {
    schema["oneOf"]
        .as_array()
        .expect("status-dependent schema must use oneOf")
        .iter()
        .find(|branch| branch["properties"]["status"]["enum"] == serde_json::json!([status]))
        .unwrap_or_else(|| panic!("missing schema branch for status {status}"))
}

fn assert_closed_status_union(schema: &serde_json::Value) {
    let object = schema
        .as_object()
        .expect("status-dependent schema must be an object");
    assert_eq!(object.len(), 1);
    assert_eq!(schema["oneOf"].as_array().map(Vec::len), Some(3));
}

fn assert_closed_branch(
    role: Role,
    branch: &serde_json::Value,
    expected_properties: &[&str],
    expected_required: &[&str],
) {
    assert_eq!(branch["type"], "object");
    assert_eq!(branch["additionalProperties"], false);
    assert_eq!(
        branch["properties"]["role"]["enum"],
        serde_json::json!([role.as_str()])
    );

    let mut actual_properties = branch["properties"]
        .as_object()
        .expect("closed branch properties must be an object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    actual_properties.sort_unstable();
    let mut expected_properties = expected_properties.to_vec();
    expected_properties.sort_unstable();
    assert_eq!(actual_properties, expected_properties);

    let mut actual_required = branch["required"]
        .as_array()
        .expect("closed branch required fields must be an array")
        .iter()
        .map(|field| field.as_str().expect("required field must be a string"))
        .collect::<Vec<_>>();
    actual_required.sort_unstable();
    let mut expected_required = expected_required.to_vec();
    expected_required.sort_unstable();
    assert_eq!(actual_required, expected_required);
}

fn needs_context_response(
    role: Role,
    context_request: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut response = response_for_status(role, "needs_context");
    if let Some(context_request) = context_request {
        response["context_request"] = context_request;
    }
    response
}

fn response_for_status(role: Role, status: &str) -> serde_json::Value {
    match role {
        Role::Researcher | Role::Analyzer | Role::SpecWriter => serde_json::json!({
            "role": role.as_str(),
            "status": status,
            "summary": "Summary.",
            "findings": [],
            "risks": [],
            "next_step_recommendation": "Continue."
        }),
        Role::Developer => {
            let mut response = serde_json::json!({
                "role": role.as_str(),
                "status": status,
                "summary": "Summary.",
                "changed_files": [],
                "requires_human_review": true
            });
            if status == "patch_proposed" {
                response["patch"] = serde_json::Value::String(
                    "diff --git a/docs/a.md b/docs/a.md\n--- a/docs/a.md\n+++ b/docs/a.md\n@@ -1 +1 @@\n-old\n+new\n".to_string(),
                );
            }
            response
        }
        Role::SpecReviewer | Role::OutputReviewer => {
            unreachable!("reviewers do not request context")
        }
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
        "patch": "",
        "context_request": {
            "paths": ["docs/approved-spec.md"],
            "reason": "The exact approved implementation spec is required."
        }
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
