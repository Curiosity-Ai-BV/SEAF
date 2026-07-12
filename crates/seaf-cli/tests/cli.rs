use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use seaf_core::{canonical_json_bytes, canonical_sha256_digest, templates, Policy, ProjectConfig};

#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};

#[test]
fn validates_goal_json_output() {
    let output = seaf()
        .args([
            "goal",
            "validate",
            example_path("adaptive.yaml").to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run seaf");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"valid\": true"));
}

#[test]
fn invalid_goal_fails_loudly() {
    let output = seaf()
        .args([
            "goal",
            "validate",
            example_path("invalid-adaptive.yaml").to_str().unwrap(),
        ])
        .output()
        .expect("run seaf");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("goal_id"));
    assert!(stderr.contains("objective.metric"));
}

#[test]
fn invalid_policy_fails_loudly() {
    let output = seaf()
        .args([
            "policy",
            "validate",
            example_path("invalid-seaf.policy.json").to_str().unwrap(),
        ])
        .output()
        .expect("run seaf");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("forbidden_paths"));
    assert!(stderr.contains("requires_human_review"));
}

#[test]
fn init_creates_templates_and_refuses_overwrite() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let root = temp_dir.path();

    let first = seaf()
        .args(["init", "--path", root.to_str().unwrap(), "--json"])
        .output()
        .expect("run init");
    assert!(first.status.success());
    assert!(root.join("adaptive.yaml").exists());
    assert!(root.join("seaf.policy.json").exists());
    assert!(root.join(".seaf/loops/current/contract.md").exists());

    let second = seaf()
        .args(["init", "--path", root.to_str().unwrap()])
        .output()
        .expect("run init again");
    assert!(!second.status.success());
    let stderr = String::from_utf8(second.stderr).expect("utf8 stderr");
    assert!(stderr.contains("already exists"));
}

#[test]
fn task_brief_generates_json_and_markdown_artifacts() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let output_dir = temp_dir.path().join("tasks");

    let output = seaf()
        .args([
            "task",
            "brief",
            "--goal",
            example_path("adaptive.yaml").to_str().unwrap(),
            "--policy",
            example_path("seaf.policy.json").to_str().unwrap(),
            "--output-dir",
            output_dir.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run task brief");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"task_id\": \"task_reduce_time_to_first_note\""));
    assert!(output_dir
        .join("task_reduce_time_to_first_note/agent-task.json")
        .exists());
    assert!(output_dir
        .join("task_reduce_time_to_first_note/agent-task.md")
        .exists());
}

#[test]
fn release_verify_rejects_bad_digest() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let capsule_path = temp_dir.path().join("release-capsule.json");
    fs::write(
        &capsule_path,
        r#"{
  "release_id": "rel_0.1.0",
  "app_id": "dev.seaf.notes",
  "version": "0.1.0",
  "source_commit": "abc123",
  "goal_id": "reduce_time_to_first_note",
  "artifact_digest": "sha256:not-a-digest",
  "eval_report_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "rollback_plan": "rollback/0.0.9",
  "rollout_policy": {
    "channel": "canary",
    "initial_percentage": 5
  }
}"#,
    )
    .expect("write capsule");

    let output = seaf()
        .args(["release", "verify", capsule_path.to_str().unwrap()])
        .output()
        .expect("run release verify");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("artifact_digest"));
}

#[test]
fn eval_run_writes_passing_report() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::write(
        &config_path,
        r#"evals:
  allow_commands:
    - printf
  required:
    - name: smoke
      command: "printf ok"
"#,
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--goal-id",
            "reduce_time_to_first_note",
            "--patch-id",
            "patch_smoke",
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"passed\": true"));
    assert!(stdout.contains("\"decision\": \"approve_for_human_review\""));
    assert!(report_path.exists());
    assert!(temp_dir.path().join("logs/smoke.stdout.log").exists());
}

#[test]
fn eval_run_accepts_initialized_template_thresholds() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let root = temp_dir.path();
    let init = seaf()
        .args(["init", "--path", root.to_str().unwrap()])
        .output()
        .expect("run init");
    assert!(init.status.success());

    let report_path = root.join("eval-report.json");
    let output = seaf()
        .args([
            "eval",
            "run",
            root.join("seaf.evals.yaml").to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(report_path.exists(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"eval_report_id\""));
}

#[test]
fn eval_run_fails_closed_when_required_check_fails() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::write(
        &config_path,
        r#"evals:
  allow_commands:
    - "false"
  required:
    - name: fail
      command: "false"
"#,
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"passed\": false"));
    assert!(stdout.contains("\"decision\": \"reject\""));
    assert!(report_path.exists());
}

#[test]
fn eval_run_accepts_local_loop_config_with_explicit_ids() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let report_path = temp_dir.path().join("eval-report.json");

    let output = seaf()
        .args([
            "eval",
            "run",
            local_loop_eval_path().to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--goal-id",
            "local_agent_loop_mvp",
            "--patch-id",
            "test",
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"patch_id\": \"test\""));
    assert!(stdout.contains("\"goal_id\": \"local_agent_loop_mvp\""));
    assert!(stdout.contains("\"passed\": true"));
    assert!(report_path.exists());
}

#[test]
fn eval_run_loop_mode_uses_run_and_ticket_identity() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let run_path = temp_dir.path().join("run.json");
    let report_path = temp_dir.path().join("eval-report.json");
    write_passing_loop_run_file(&run_path, "loop_cli_001");

    let output = seaf()
        .args([
            "eval",
            "run",
            local_loop_eval_path().to_str().unwrap(),
            "--loop-run",
            run_path.to_str().unwrap(),
            "--ticket",
            local_loop_ticket_path().to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run loop eval");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("eval report json");
    assert_eq!(report["patch_id"], "loop_cli_001");
    assert_eq!(report["goal_id"], "local_agent_loop_mvp");
    assert_eq!(report["decision"], "approve_for_human_review");
    let check_names: Vec<&str> = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|check| check["name"].as_str().expect("check name"))
        .collect();
    for expected in [
        "schema_validation",
        "patch_policy_gate",
        "spec_review",
        "output_review",
        "local_loop_smoke",
    ] {
        assert!(
            check_names.contains(&expected),
            "report should include {expected:?}; got {check_names:?}"
        );
    }
    assert!(report_path.exists());
}

#[test]
fn eval_run_loop_mode_requires_ticket_with_loop_run() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let run_path = temp_dir.path().join("run.json");
    write_passing_loop_run_file(&run_path, "loop_cli_001");

    let output = seaf()
        .args([
            "eval",
            "run",
            local_loop_eval_path().to_str().unwrap(),
            "--loop-run",
            run_path.to_str().unwrap(),
        ])
        .output()
        .expect("run loop eval");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("--loop-run and --ticket must be provided together"));
}

#[test]
fn eval_run_loop_mode_validates_artifacts_before_running_checks() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let marker_path = temp_dir.path().join("marker");
    let report_path = temp_dir.path().join("eval-report.json");
    let run_path = temp_dir.path().join("invalid-run.json");
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - printf
  required:
    - name: side_effect
      command: "printf touched > {}"
"#,
            marker_path.display()
        ),
    )
    .expect("write eval config");
    fs::write(&run_path, "{").expect("write invalid run");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--loop-run",
            run_path.to_str().unwrap(),
            "--ticket",
            local_loop_ticket_path().to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run loop eval");

    assert!(!output.status.success());
    assert!(!marker_path.exists(), "eval command should not run");
    assert!(!report_path.exists(), "EvalReport should not be written");
    assert!(
        !temp_dir.path().join("logs").exists(),
        "logs should not be created before artifact validation"
    );
}

#[test]
fn model_check_json_reports_invalid_ollama_base_url_without_live_server() {
    let output = seaf()
        .args([
            "model",
            "check",
            "--provider",
            "ollama",
            "--model",
            "local-model",
            "--base-url",
            "ftp://localhost:11434/api",
            "--json",
        ])
        .output()
        .expect("run model check");

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"ok\": false"));
    assert!(stdout.contains("unsupported Ollama base URL"));
}

#[test]
fn ticket_validate_accepts_valid_ticket_json() {
    let output = seaf()
        .args([
            "ticket",
            "validate",
            local_loop_ticket_path().to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run ticket validate");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"kind\": \"ticket\""));
    assert!(stdout.contains("\"valid\": true"));
}

#[test]
fn ticket_validate_rejects_invalid_ticket_with_actionable_fields() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let ticket_path = temp_dir.path().join("invalid-ticket.yaml");
    fs::write(
        &ticket_path,
        r#"ticket_id: ""
goal_id: ""
title: ""
status: ready
priority: p2
problem: ""
context:
  relevant_files:
    - ""
  forbidden_files: []
autonomy:
  level: 5
  apply_patch: true
acceptance_criteria: []
"#,
    )
    .expect("write invalid ticket");

    let output = seaf()
        .args(["ticket", "validate", ticket_path.to_str().unwrap()])
        .output()
        .expect("run ticket validate");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("ticket_id"));
    assert!(stderr.contains("autonomy.level"));
    assert!(stderr.contains("acceptance_criteria"));
}

#[test]
fn loop_run_refuses_dirty_worktree_unless_allowed() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path();
    init_git_repo(repo);
    fs::write(repo.join("untracked.txt"), "dirty").expect("write dirty file");
    let runs_root = repo.join("runs");

    let denied = seaf_in(repo)
        .args([
            "loop",
            "run",
            "--ticket",
            local_loop_ticket_path().to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            "dirty-denied",
            "--json",
        ])
        .output()
        .expect("run loop");

    assert!(!denied.status.success());
    assert!(!runs_root.join("dirty-denied").exists());
    let stderr = String::from_utf8(denied.stderr).expect("utf8 stderr");
    assert!(stderr.contains("dirty git working tree"));

    let allowed = seaf_in(repo)
        .args([
            "loop",
            "run",
            "--ticket",
            local_loop_ticket_path().to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            "dirty-allowed",
            "--allow-dirty",
            "--json",
        ])
        .output()
        .expect("run loop with allow dirty");

    assert!(allowed.status.success());
    let stdout = String::from_utf8(allowed.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"run_id\": \"dirty-allowed\""));
    assert!(stdout.contains("\"status\": \"completed\""));
    assert!(runs_root.join("dirty-allowed/run.json").exists());
}

#[test]
fn loop_run_rejects_traversal_run_id_before_workspace_creation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");

    let output = seaf()
        .args([
            "loop",
            "run",
            "--ticket",
            local_loop_ticket_path().to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            "../escaped",
            "--allow-dirty",
        ])
        .output()
        .expect("run loop");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("invalid run ID"));
    assert!(!temp_dir.path().join("escaped").exists());
    assert!(!runs_root.exists());
}

#[test]
fn loop_run_rejects_absolute_run_id_before_workspace_creation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let escaped = temp_dir.path().join("absolute-run-id");

    let output = seaf()
        .args([
            "loop",
            "run",
            "--ticket",
            local_loop_ticket_path().to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            escaped.to_str().unwrap(),
            "--allow-dirty",
        ])
        .output()
        .expect("run loop");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("invalid run ID"));
    assert!(!escaped.exists());
    assert!(!runs_root.exists());
}

#[test]
fn loop_run_accepts_valid_stable_run_id() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = repo.join("runs");

    let output = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            local_loop_ticket_path().to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            "stable_123-run",
            "--allow-dirty",
            "--json",
        ])
        .output()
        .expect("run loop");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"run_id\": \"stable_123-run\""));
    assert!(runs_root.join("stable_123-run/run.json").exists());
}

#[test]
fn loop_run_fake_uses_provider_artifacts_and_real_policy_decision() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let run_id = "cli-loop-json";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);

    let run = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "fake",
            "--model",
            "fake-model",
            "--json",
        ])
        .output()
        .expect("run loop");

    assert!(run.status.success());
    let stdout = String::from_utf8(run.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"status\": \"completed\""));
    let run_dir = runs_root.join(run_id);
    assert!(run_dir.join("run.json").exists());
    assert!(
        run_dir.join("context-manifest.json").exists(),
        "fake provider runs must use the same live context packing path as live providers"
    );

    let request: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(run_dir.join("prompts/01-research.prompt.md"))
            .expect("research request audit"),
    )
    .expect("research request should be a serialized ModelRequest");
    assert_eq!(request["model"], "fake-model");
    assert!(request["response_schema"].is_object());
    assert!(request["messages"][0]["content"]
        .as_str()
        .expect("user prompt")
        .contains("UNTRUSTED_REPOSITORY_CONTEXT"));

    let research_response =
        fs::read_to_string(run_dir.join("responses/01-research.raw.txt")).expect("response audit");
    assert!(
        research_response.contains("\"role\":\"researcher\"")
            || research_response.contains("\"role\": \"researcher\""),
        "provider responses should be structured role JSON, got {research_response}"
    );

    let persisted_run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(run_dir.join("run.json")).expect("run json"))
            .expect("persisted run json");
    let effective_ticket =
        seaf_core::load_ticket_file(&ticket_path).expect("effective provider ticket");
    let effective_policy: Policy =
        serde_json::from_str(templates::DEFAULT_POLICY_JSON).expect("default policy");
    let effective_config = ProjectConfig {
        policy_path: "seaf.policy.json".to_string(),
    };
    assert_eq!(
        persisted_run["input_digests"]["ticket"],
        canonical_sha256_digest(&effective_ticket).expect("ticket digest")
    );
    assert_eq!(
        persisted_run["input_digests"]["policy"],
        canonical_sha256_digest(&effective_policy).expect("policy digest")
    );
    assert_eq!(
        persisted_run["input_digests"]["config"],
        canonical_sha256_digest(&effective_config).expect("effective config digest")
    );
    assert_eq!(persisted_run["execution_mode"], "isolated_candidate");
    assert_eq!(persisted_run["candidate_workspace"]["lifecycle"], "active");
    let role_input: serde_json::Value =
        serde_json::from_str(request["messages"][0]["content"].as_str().unwrap())
            .expect("structured role input");
    assert_eq!(
        role_input["repository_context_authority"]["candidate_authority"]["kind"],
        "isolated_candidate"
    );
    let decisions = persisted_run["policy_decisions"]
        .as_array()
        .expect("policy decisions");
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0]["patch_id"], run_id);
    assert_eq!(decisions[0]["decision"], "requires_human_review");
    assert_eq!(decisions[0]["apply_requested"], true);
    assert_eq!(decisions[0]["applied"], false);
    assert_eq!(
        decisions[0]["changed_paths"][0],
        "examples/local-loop/evals/fake-provider-smoke.txt"
    );
    assert!(
        run_dir
            .join("artifacts/cli-loop-json.policy-decision.json")
            .exists(),
        "real patch gate decisions should be persisted as artifacts"
    );

    let status = seaf()
        .args([
            "loop",
            "status",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run loop status");
    assert!(status.status.success());
    let stdout = String::from_utf8(status.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"run_id\": \"cli-loop-json\""));
    assert!(stdout.contains("\"status\": \"completed\""));

    let resume = seaf()
        .args([
            "loop",
            "resume",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run loop resume");
    assert!(resume.status.success());
    let stdout = String::from_utf8(resume.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"command\": \"resume\""));
    assert!(stdout.contains("\"status\": \"completed\""));
}

#[test]
fn loop_cleanup_json_removes_only_a_terminal_candidate_and_reports_cleaned_authority() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = temp_dir.path().join("runs");
    let run_id = "cli-loop-cleanup";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    run_fake_provider(&repo, &ticket_path, &runs_root, run_id, &[]);

    let run_dir = runs_root.join(run_id);
    let before = read_run_json(&run_dir);
    assert_eq!(before["status"], "completed");
    assert_eq!(before["candidate_workspace"]["lifecycle"], "active");
    let candidate_path = PathBuf::from(
        before["candidate_workspace"]["path"]
            .as_str()
            .expect("candidate path"),
    );
    assert!(candidate_path.is_dir());
    let registration_before = git_worktree_registration(&repo);
    assert!(
        registration_before
            .windows(candidate_path.as_os_str().as_encoded_bytes().len())
            .any(|window| window == candidate_path.as_os_str().as_encoded_bytes()),
        "candidate must be registered before cleanup"
    );
    let source_git_before = git_evidence(&repo);
    let source_bytes_before = fs::read(repo.join("seaf.policy.json")).expect("source bytes");

    let cleanup = seaf_in(&repo)
        .args([
            "loop",
            "cleanup",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("clean terminal candidate");

    assert!(cleanup.status.success(), "{cleanup:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&cleanup.stdout).expect("cleanup report JSON");
    assert_eq!(
        report.as_object().expect("cleanup report object").len(),
        7,
        "cleanup JSON is a dedicated closed operation report"
    );
    assert_eq!(report["command"], "cleanup");
    assert_eq!(report["run_id"], run_id);
    assert_eq!(report["status"], "completed");
    assert_eq!(report["candidate_lifecycle"], "cleaned");
    assert_eq!(
        report["candidate_path"],
        candidate_path.display().to_string()
    );
    assert_eq!(
        report["run_directory"],
        run_dir.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(
        report["run_file"],
        run_dir
            .join("run.json")
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
    assert!(!candidate_path.exists());
    assert!(
        !git_worktree_registration(&repo)
            .windows(candidate_path.as_os_str().as_encoded_bytes().len())
            .any(|window| window == candidate_path.as_os_str().as_encoded_bytes()),
        "cleanup must remove the exact candidate registration"
    );
    assert_eq!(
        read_run_json(&run_dir)["candidate_workspace"]["lifecycle"],
        "cleaned"
    );
    assert_eq!(git_evidence(&repo), source_git_before);
    assert_eq!(
        fs::read(repo.join("seaf.policy.json")).expect("source bytes after cleanup"),
        source_bytes_before
    );

    let cleaned_run_bytes = fs::read(run_dir.join("run.json")).expect("cleaned run bytes");
    let repeated = seaf_in(&repo)
        .args([
            "loop",
            "cleanup",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("repeat terminal cleanup");
    assert!(repeated.status.success(), "{repeated:?}");
    let repeated_report: serde_json::Value =
        serde_json::from_slice(&repeated.stdout).expect("repeated cleanup report JSON");
    assert_eq!(repeated_report, report);
    assert_eq!(
        fs::read(run_dir.join("run.json")).expect("run bytes after repeated cleanup"),
        cleaned_run_bytes,
        "idempotent cleanup must not republish or alter retained evidence"
    );
    assert_eq!(git_evidence(&repo), source_git_before);
}

#[test]
fn loop_cleanup_refuses_an_active_run_without_mutating_run_source_or_candidate() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = temp_dir.path().join("runs");
    let run_id = "cli-loop-cleanup-active";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    run_fake_provider(&repo, &ticket_path, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    mark_loop_run_pending_from_analysis(&run_dir.join("run.json"));
    let candidate_path = PathBuf::from(
        read_run_json(&run_dir)["candidate_workspace"]["path"]
            .as_str()
            .expect("candidate path"),
    );
    let run_before = read_tree_bytes(&run_dir);
    let source_before = git_evidence(&repo);
    let candidate_before = git_evidence(&candidate_path);
    let registration_before = git_worktree_registration(&repo);

    let cleanup = seaf_in(&repo)
        .args([
            "loop",
            "cleanup",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("refuse active cleanup");

    assert!(!cleanup.status.success(), "active cleanup must fail");
    assert!(
        cleanup.stdout.is_empty(),
        "failed cleanup must not emit a success report"
    );
    let stderr = String::from_utf8(cleanup.stderr).expect("stderr");
    assert!(stderr.contains("active run"), "{stderr}");
    assert_eq!(read_tree_bytes(&run_dir), run_before);
    assert_eq!(git_evidence(&repo), source_before);
    assert_eq!(git_evidence(&candidate_path), candidate_before);
    assert_eq!(git_worktree_registration(&repo), registration_before);
    assert!(candidate_path.is_dir());
}

#[test]
fn loop_cleanup_uses_the_current_repository_as_witness_and_rejects_another_repo() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let source_repo = temp_dir.path().join("source");
    let other_repo = temp_dir.path().join("other");
    fs::create_dir_all(&source_repo).expect("source repo dir");
    fs::create_dir_all(&other_repo).expect("other repo dir");
    init_git_repo(&source_repo);
    init_git_repo(&other_repo);
    let runs_root = temp_dir.path().join("runs");
    let run_id = "cli-loop-cleanup-wrong-repo";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    run_fake_provider(&source_repo, &ticket_path, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    let candidate_path = PathBuf::from(
        read_run_json(&run_dir)["candidate_workspace"]["path"]
            .as_str()
            .expect("candidate path"),
    );
    let run_before = read_tree_bytes(&run_dir);
    let source_before = git_evidence(&source_repo);
    let other_before = git_evidence(&other_repo);
    let candidate_before = git_evidence(&candidate_path);

    let cleanup = seaf_in(&other_repo)
        .args([
            "loop",
            "cleanup",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
        ])
        .output()
        .expect("reject cleanup from another repository");

    assert!(
        !cleanup.status.success(),
        "wrong repository cleanup must fail"
    );
    let stderr = String::from_utf8(cleanup.stderr).expect("stderr");
    assert!(
        stderr.contains("source worktree") || stderr.contains("Git common directory"),
        "{stderr}"
    );
    assert_eq!(read_tree_bytes(&run_dir), run_before);
    assert_eq!(git_evidence(&source_repo), source_before);
    assert_eq!(git_evidence(&other_repo), other_before);
    assert_eq!(git_evidence(&candidate_path), candidate_before);
    assert!(candidate_path.is_dir());
}

#[test]
fn loop_cleanup_rejects_a_copied_run_before_touching_its_original_candidate() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = temp_dir.path().join("runs");
    let copied_runs_root = temp_dir.path().join("copied-runs");
    let run_id = "cli-loop-cleanup-copied";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    run_fake_provider(&repo, &ticket_path, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    let copied_run_dir = copied_runs_root.join(run_id);
    copy_directory(&run_dir, &copied_run_dir);
    let candidate_path = PathBuf::from(
        read_run_json(&run_dir)["candidate_workspace"]["path"]
            .as_str()
            .expect("candidate path"),
    );
    let original_run_before = read_tree_bytes(&run_dir);
    let copied_run_before = read_tree_bytes(&copied_run_dir);
    let source_before = git_evidence(&repo);
    let candidate_before = git_evidence(&candidate_path);

    let cleanup = seaf_in(&repo)
        .args([
            "loop",
            "cleanup",
            "--run-id",
            run_id,
            "--runs-root",
            copied_runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("reject copied run cleanup");

    assert!(!cleanup.status.success(), "copied run cleanup must fail");
    assert!(
        cleanup.stdout.is_empty(),
        "failed cleanup must not emit a success report"
    );
    let stderr = String::from_utf8(cleanup.stderr).expect("stderr");
    assert!(stderr.contains("run directory"), "{stderr}");
    assert_eq!(read_tree_bytes(&run_dir), original_run_before);
    assert_eq!(read_tree_bytes(&copied_run_dir), copied_run_before);
    assert_eq!(git_evidence(&repo), source_before);
    assert_eq!(git_evidence(&candidate_path), candidate_before);
    assert!(candidate_path.is_dir());
}

#[test]
fn loop_cleanup_rejects_a_persisted_run_id_mismatch_before_any_lock_or_checkout_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = temp_dir.path().join("runs");
    let run_id = "cli-loop-cleanup-run-id";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    run_fake_provider(&repo, &ticket_path, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    let mut run = read_run_json(&run_dir);
    let candidate_path = PathBuf::from(
        run["candidate_workspace"]["path"]
            .as_str()
            .expect("candidate path"),
    );
    run["run_id"] = serde_json::json!("other-safe-run");
    run["provider_exchange_records"] = serde_json::json!([]);
    for decision in run["policy_decisions"]
        .as_array_mut()
        .expect("policy decisions")
    {
        decision["patch_id"] = serde_json::json!("other-safe-run");
    }
    fs::write(
        run_dir.join("run.json"),
        serde_json::to_vec_pretty(&run).expect("serialize mismatched run"),
    )
    .expect("persist mismatched run");
    let authority_root = temp_dir.path().join("seaf-test-tmp");
    let run_before = read_tree_bytes(&run_dir);
    let authority_before = read_tree_bytes(&authority_root);
    let source_before = git_evidence(&repo);
    let candidate_before = git_evidence(&candidate_path);
    let registration_before = git_worktree_registration(&repo);

    let cleanup = seaf_in(&repo)
        .args([
            "loop",
            "cleanup",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("reject persisted run ID mismatch");

    assert!(!cleanup.status.success(), "mismatched run ID must fail");
    let stderr = String::from_utf8(cleanup.stderr).expect("stderr");
    assert!(stderr.contains("run ID"), "{stderr}");
    assert_eq!(read_tree_bytes(&run_dir), run_before);
    assert_eq!(read_tree_bytes(&authority_root), authority_before);
    assert_eq!(git_evidence(&repo), source_before);
    assert_eq!(git_evidence(&candidate_path), candidate_before);
    assert_eq!(git_worktree_registration(&repo), registration_before);
    assert!(candidate_path.is_dir());
}

#[test]
fn loop_cleanup_ignores_inherited_git_redirection_when_resolving_the_caller_repository() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let source_repo = temp_dir.path().join("source");
    let caller_repo = temp_dir.path().join("caller");
    fs::create_dir_all(&source_repo).expect("source repo dir");
    fs::create_dir_all(&caller_repo).expect("caller repo dir");
    init_git_repo(&source_repo);
    init_git_repo(&caller_repo);
    let runs_root = temp_dir.path().join("runs");
    let run_id = "cli-loop-cleanup-git-env";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    run_fake_provider(&source_repo, &ticket_path, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    let candidate_path = PathBuf::from(
        read_run_json(&run_dir)["candidate_workspace"]["path"]
            .as_str()
            .expect("candidate path"),
    );
    let authority_root = temp_dir.path().join("seaf-test-tmp");
    let run_before = read_tree_bytes(&run_dir);
    let authority_before = read_tree_bytes(&authority_root);
    let source_before = git_evidence(&source_repo);
    let caller_before = git_evidence(&caller_repo);
    let candidate_before = git_evidence(&candidate_path);
    let registration_before = git_worktree_registration(&source_repo);

    let cleanup = seaf_in(&caller_repo)
        .env("GIT_DIR", source_repo.join(".git"))
        .env("GIT_WORK_TREE", &source_repo)
        .env("GIT_COMMON_DIR", source_repo.join(".git"))
        .env("GIT_INDEX_FILE", source_repo.join(".git/index"))
        .env("GIT_OBJECT_DIRECTORY", source_repo.join(".git/objects"))
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.fsmonitor")
        .env("GIT_CONFIG_VALUE_0", "false")
        .args([
            "loop",
            "cleanup",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("reject inherited Git redirection");

    assert!(
        !cleanup.status.success(),
        "redirected caller witness must fail"
    );
    let stderr = String::from_utf8(cleanup.stderr).expect("stderr");
    assert!(stderr.contains("source worktree"), "{stderr}");
    assert_eq!(read_tree_bytes(&run_dir), run_before);
    assert_eq!(read_tree_bytes(&authority_root), authority_before);
    assert_eq!(git_evidence(&source_repo), source_before);
    assert_eq!(git_evidence(&caller_repo), caller_before);
    assert_eq!(git_evidence(&candidate_path), candidate_before);
    assert_eq!(git_worktree_registration(&source_repo), registration_before);
    assert!(candidate_path.is_dir());
}

#[test]
fn loop_cleanup_rejects_invalid_and_missing_targets_without_creating_workspace_state() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = temp_dir.path().join("runs");

    let invalid = seaf_in(&repo)
        .args([
            "loop",
            "cleanup",
            "--run-id",
            "../escaped",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("reject traversal run id");
    assert!(!invalid.status.success());
    assert!(String::from_utf8(invalid.stderr)
        .expect("stderr")
        .contains("invalid run ID"));
    assert!(!runs_root.exists());

    fs::create_dir(&runs_root).expect("empty runs root");
    let source_before = git_evidence(&repo);
    let missing = seaf_in(&repo)
        .args([
            "loop",
            "cleanup",
            "--run-id",
            "missing-run",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("reject missing run");
    assert!(!missing.status.success());
    assert!(String::from_utf8(missing.stderr)
        .expect("stderr")
        .contains("run directory does not exist"));
    assert!(fs::read_dir(&runs_root)
        .expect("read runs root")
        .next()
        .is_none());
    assert_eq!(git_evidence(&repo), source_before);
}

#[test]
fn loop_help_exposes_cleanup_as_an_explicit_operation() {
    let output = seaf().args(["loop", "--help"]).output().expect("loop help");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("stdout");
    for command in ["run", "status", "resume", "smoke", "bench", "cleanup"] {
        assert!(
            stdout
                .lines()
                .any(|line| line.trim_start().starts_with(command)),
            "missing {command} in {stdout}"
        );
    }
}

#[test]
fn loop_run_project_config_policy_changes_fake_gating_and_explicit_policy_wins() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(repo.join("config/policies")).expect("config policies dir");
    init_git_repo(&repo);
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let config_path = repo.join("config/seaf.config.json");
    fs::write(
        &config_path,
        r#"{"policy_path":"policies/reject-smoke.json"}"#,
    )
    .expect("write config");
    write_policy(
        &repo.join("config/policies/reject-smoke.json"),
        &["examples/local-loop/evals/fake-provider-smoke.txt"],
        &[],
    );
    write_policy(
        &repo.join("explicit-policy.json"),
        &[],
        &["examples/local-loop/evals/fake-provider-smoke.txt"],
    );

    let configured = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            repo.join("runs").to_str().unwrap(),
            "--run-id",
            "configured-rejection",
            "--config",
            config_path.to_str().unwrap(),
            "--allow-dirty",
            "--json",
        ])
        .output()
        .expect("run with project config");
    assert!(configured.status.success(), "{configured:?}");
    assert_eq!(
        read_run_json(&repo.join("runs/configured-rejection"))["policy_decisions"][0]["decision"],
        "rejected",
        "the config-relative custom policy must drive the real fake-provider patch gate"
    );

    let overridden = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            repo.join("runs").to_str().unwrap(),
            "--run-id",
            "explicit-policy-wins",
            "--config",
            config_path.to_str().unwrap(),
            "--policy",
            repo.join("explicit-policy.json").to_str().unwrap(),
            "--allow-dirty",
            "--json",
        ])
        .output()
        .expect("run with explicit policy override");
    assert!(overridden.status.success(), "{overridden:?}");
    assert_eq!(
        read_run_json(&repo.join("runs/explicit-policy-wins"))["policy_decisions"][0]["decision"],
        "requires_human_review",
        "--policy must override the policy named by --config"
    );
    let snapshotted_config: ProjectConfig = serde_json::from_slice(
        &fs::read(repo.join("runs/explicit-policy-wins/inputs/config.json"))
            .expect("effective config snapshot"),
    )
    .expect("effective config");
    assert_eq!(
        snapshotted_config.policy_path, "explicit-policy.json",
        "the effective config snapshot must record the policy authority that actually won"
    );
}

#[test]
fn loop_run_explicit_policy_bypasses_malformed_discovered_config() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    fs::write(
        repo.join("seaf.config.json"),
        r#"{"policy_path":"../unsafe.json"}"#,
    )
    .expect("malformed discovered config");
    let explicit_policy = repo.join("explicit-policy.json");
    write_policy(
        &explicit_policy,
        &[],
        &["examples/local-loop/evals/fake-provider-smoke.txt"],
    );
    let runs_root = repo.join("runs");

    let output = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            write_provider_loop_ticket(temp_dir.path(), true)
                .to_str()
                .unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            "explicit-policy-skips-discovery",
            "--policy",
            explicit_policy.to_str().unwrap(),
            "--allow-dirty",
            "--json",
        ])
        .output()
        .expect("run with explicit policy and malformed discovered config");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        read_run_json(&runs_root.join("explicit-policy-skips-discovery"))["policy_decisions"][0]
            ["decision"],
        "requires_human_review"
    );
}

#[test]
fn loop_run_persists_canonical_effective_inputs_and_matching_digests() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(repo.join("config/policies")).expect("config policies dir");
    init_git_repo(&repo);
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let config_path = repo.join("config/seaf.config.json");
    fs::write(&config_path, r#"{"policy_path":"policies/effective.json"}"#).expect("write config");
    let policy_path = repo.join("config/policies/effective.json");
    write_policy(&policy_path, &["private/**"], &["eval_changes"]);
    let runs_root = repo.join("runs");
    let run_id = "canonical-input-snapshots";

    let output = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--config",
            config_path.to_str().unwrap(),
            "--allow-dirty",
            "--json",
        ])
        .output()
        .expect("run with effective inputs");
    assert!(output.status.success(), "{output:?}");

    let run_dir = runs_root.join(run_id);
    let effective_ticket = seaf_core::load_ticket_file(&ticket_path).expect("ticket");
    let effective_policy = seaf_core::load_policy_file(&policy_path).expect("policy");
    let effective_config = ProjectConfig {
        policy_path: "policies/effective.json".to_string(),
    };
    let expected = [
        (
            "ticket",
            canonical_json_bytes(&effective_ticket).expect("ticket canonical bytes"),
            canonical_sha256_digest(&effective_ticket).expect("ticket digest"),
        ),
        (
            "policy",
            canonical_json_bytes(&effective_policy).expect("policy canonical bytes"),
            canonical_sha256_digest(&effective_policy).expect("policy digest"),
        ),
        (
            "config",
            canonical_json_bytes(&effective_config).expect("config canonical bytes"),
            canonical_sha256_digest(&effective_config).expect("config digest"),
        ),
    ];
    let run = read_run_json(&run_dir);
    for (kind, bytes, digest) in expected {
        assert_eq!(
            fs::read(run_dir.join("inputs").join(format!("{kind}.json"))).expect("input snapshot"),
            bytes,
            "{kind} snapshot must contain canonical effective bytes"
        );
        assert_eq!(run["input_digests"][kind], digest);
    }
    let repository_snapshot =
        fs::read(run_dir.join("inputs/repository.json")).expect("repository identity snapshot");
    let repository_identity: serde_json::Value =
        serde_json::from_slice(&repository_snapshot).expect("repository identity");
    assert_eq!(
        repository_snapshot,
        canonical_json_bytes(&repository_identity).expect("canonical repository identity")
    );
    assert_eq!(
        run["input_digests"]["repository"],
        canonical_sha256_digest(&repository_identity).expect("repository identity digest")
    );
}

#[test]
fn loop_run_invalid_or_missing_authority_has_zero_workspace_or_provider_side_effects() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_empty_git_repo(&repo);
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let invalid_config = repo.join("invalid-config.json");
    fs::write(&invalid_config, r#"{"policy_path":"../escape.json"}"#).expect("invalid config");
    let invalid_policy = repo.join("invalid-policy.json");
    fs::write(&invalid_policy, r#"{"policy_id":""}"#).expect("invalid policy");
    let invalid_policy_config = repo.join("invalid-policy-config.json");
    fs::write(
        &invalid_policy_config,
        r#"{"policy_path":"invalid-policy.json"}"#,
    )
    .expect("invalid policy config");
    let valid_explicit_policy = repo.join("valid-explicit-policy.json");
    write_policy(&valid_explicit_policy, &[], &[]);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind provider probe");
    listener
        .set_nonblocking(true)
        .expect("nonblocking provider probe");
    let base_url = format!("http://{}", listener.local_addr().expect("probe address"));
    let runs_root = repo.join("runs");
    let cases = [
        ("no-authority", None, None),
        (
            "missing-config",
            Some(repo.join("missing-config.json")),
            None,
        ),
        ("invalid-config", Some(invalid_config.clone()), None),
        (
            "invalid-explicit-config-with-policy",
            Some(invalid_config),
            Some(valid_explicit_policy),
        ),
        ("config-invalid-policy", Some(invalid_policy_config), None),
        ("explicit-invalid-policy", None, Some(invalid_policy)),
    ];

    for (run_id, config_path, policy_path) in cases {
        let mut command = seaf_in(&repo);
        command.args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "ollama",
            "--model",
            "unused-model",
            "--base-url",
            &base_url,
            "--allow-dirty",
        ]);
        if let Some(config_path) = config_path {
            command.args(["--config", config_path.to_str().unwrap()]);
        }
        if let Some(policy_path) = policy_path {
            command.args(["--policy", policy_path.to_str().unwrap()]);
        }
        let output = command.output().expect("run invalid authority case");
        assert!(!output.status.success(), "{run_id} must fail closed");
        assert!(
            !runs_root.join(run_id).exists(),
            "{run_id} must fail before workspace creation"
        );
    }
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "authority preflight must fail before contacting the provider"
    );
}

#[cfg(unix)]
#[test]
fn loop_run_rejects_config_relative_policy_symlink_escape_before_workspace_creation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(repo.join("config")).expect("config dir");
    init_git_repo(&repo);
    let outside_policy = temp_dir.path().join("outside-policy.json");
    write_policy(&outside_policy, &[], &[]);
    symlink(&outside_policy, repo.join("config/escaped-policy.json")).expect("policy symlink");
    let config_path = repo.join("config/seaf.config.json");
    fs::write(&config_path, r#"{"policy_path":"escaped-policy.json"}"#).expect("write config");
    let runs_root = repo.join("runs");

    let output = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            write_provider_loop_ticket(temp_dir.path(), false)
                .to_str()
                .unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            "symlink-escape",
            "--config",
            config_path.to_str().unwrap(),
            "--allow-dirty",
        ])
        .output()
        .expect("run with escaped policy symlink");
    assert!(!output.status.success());
    assert!(!runs_root.join("symlink-escape").exists());
    let stderr = String::from_utf8(output.stderr).expect("stderr");
    assert!(stderr.contains("outside the git repository"), "{stderr}");
}

#[test]
fn loop_run_mocked_ollama_uses_discovered_project_policy_for_gating() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(repo.join("policies")).expect("policies dir");
    init_git_repo(&repo);
    fs::write(
        repo.join("seaf.config.json"),
        r#"{"policy_path":"policies/reject-smoke.json"}"#,
    )
    .expect("root project config");
    write_policy(
        &repo.join("policies/reject-smoke.json"),
        &["examples/local-loop/evals/fake-provider-smoke.txt"],
        &[],
    );
    let runs_root = repo.join("runs");
    let base_url = start_fake_ollama_server_sequence(provider_loop_model_responses());

    let output = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            write_provider_loop_ticket(temp_dir.path(), true)
                .to_str()
                .unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            "ollama-discovered-policy",
            "--provider",
            "ollama",
            "--model",
            "mocked-ollama",
            "--base-url",
            &base_url,
            "--allow-dirty",
            "--json",
        ])
        .output()
        .expect("run mocked Ollama with discovered config");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        read_run_json(&runs_root.join("ollama-discovered-policy"))["policy_decisions"][0]
            ["decision"],
        "rejected"
    );
}

#[test]
fn loop_run_ollama_completes_against_fake_server_with_provider_artifacts() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let run_id = "cli-loop-ollama";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let base_url = start_fake_ollama_server_sequence(provider_loop_model_responses());

    let output = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "ollama",
            "--model",
            "fake-ollama-model",
            "--base-url",
            &base_url,
            "--timeout-ms",
            "1000",
            "--json",
        ])
        .output()
        .expect("run ollama loop");

    assert!(output.status.success(), "{output:?}");
    let run_dir = runs_root.join(run_id);
    let request: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(run_dir.join("prompts/01-research.prompt.md"))
            .expect("research request audit"),
    )
    .expect("research request should be a serialized ModelRequest");
    assert_eq!(request["model"], "fake-ollama-model");
    assert_eq!(request["timeout_ms"], 1000);
    assert!(run_dir.join("context-manifest.json").exists());

    let persisted_run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(run_dir.join("run.json")).expect("run json"))
            .expect("persisted run json");
    assert_eq!(persisted_run["provider"], "ollama");
    assert_eq!(persisted_run["status"], "completed");
    assert_eq!(persisted_run["policy_decisions"][0]["patch_id"], run_id);
}

#[test]
fn loop_resume_rejects_completed_analysis_rerun_before_ticket_or_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let run_id = "cli-loop-resume-provider";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);

    let run = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "fake",
            "--model",
            "fake-model",
            "--json",
        ])
        .output()
        .expect("run loop");
    assert!(run.status.success(), "{run:?}");

    let run_dir = runs_root.join(run_id);
    assert!(
        run_dir.join("ticket.snapshot.json").exists(),
        "provider-backed runs must persist the original ticket content used for resume checks"
    );
    let before = read_tree_bytes(&run_dir);

    let missing_ticket = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--rerun-from",
            "analysis",
            "--json",
        ])
        .output()
        .expect("resume loop without ticket");
    assert!(!missing_ticket.status.success());
    let stderr = String::from_utf8(missing_ticket.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("start a new run"),
        "forbidden rerun must reject before ticket handling, got {stderr}"
    );
    assert_eq!(read_tree_bytes(&run_dir), before);

    let resume = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--rerun-from",
            "analysis",
            "--json",
        ])
        .output()
        .expect("resume loop with ticket");
    assert!(!resume.status.success(), "{resume:?}");
    let stderr = String::from_utf8(resume.stderr).expect("utf8 stderr");
    assert!(stderr.contains("start a new run"), "{stderr}");
    assert_eq!(read_tree_bytes(&run_dir), before);
}

#[test]
fn loop_resume_rejects_completed_research_rerun_before_any_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let run_id = "cli-loop-audited-rerun";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    run_fake_provider(&repo, &ticket_path, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    let before = read_run_json(&run_dir);
    let before_tree = read_tree_bytes(&run_dir);
    let source_before = git_evidence(&repo);
    let candidate = PathBuf::from(
        before["candidate_workspace"]["path"]
            .as_str()
            .expect("candidate path"),
    );
    let candidate_before = git_evidence(&candidate);

    let rerun = resume_provider_run(
        &repo,
        &ticket_path,
        &runs_root,
        run_id,
        &["--rerun-from", "research", "--json"],
    );

    assert!(!rerun.status.success(), "{rerun:?}");
    let stderr = String::from_utf8(rerun.stderr).expect("stderr");
    assert!(stderr.contains("start a new run"), "{stderr}");
    assert_eq!(read_tree_bytes(&run_dir), before_tree);
    assert_eq!(git_evidence(&repo), source_before);
    assert_eq!(git_evidence(&candidate), candidate_before);
}

#[test]
fn loop_cli_rerun_preserves_context_cap_across_attempts() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    for name in ["one.txt", "two.txt", "three.txt"] {
        fs::write(repo.join(name), format!("{name} bytes\n")).expect("context file");
    }
    commit_all(&repo, "Add context cap fixtures");
    let runs_root = repo.join("runs");
    let run_id = "cli-context-cap-rerun";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let initial_responses = vec![
        provider_needs_context_response("researcher", "one.txt"),
        provider_needs_context_response("researcher", "two.txt"),
        serde_json::json!({
            "role": "researcher",
            "status": "blocked",
            "summary": "Research cannot continue with the available context.",
            "findings": [],
            "risks": [],
            "next_step_recommendation": "Resolve the missing evidence."
        })
        .to_string(),
    ];
    let initial_url = start_fake_ollama_server_sequence(initial_responses);
    let initial = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "ollama",
            "--model",
            "mocked-ollama",
            "--base-url",
            &initial_url,
            "--allow-dirty",
        ])
        .output()
        .expect("initial context run");
    assert!(initial.status.success(), "{initial:?}");
    let run_dir = runs_root.join(run_id);
    let before = read_run_json(&run_dir);
    assert_eq!(before["status"], "blocked");
    assert_eq!(before["current_step"], "research");
    assert!(before["candidate_workspace"]["patch_transaction"].is_null());
    let before_records = before["provider_exchange_records"]
        .as_array()
        .expect("records")
        .len();

    let rerun_url = start_fake_ollama_server_sequence(vec![provider_needs_context_response(
        "researcher",
        "three.txt",
    )]);
    let rerun = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--base-url",
            &rerun_url,
            "--rerun-from",
            "research",
        ])
        .output()
        .expect("rerun at cap");

    assert!(rerun.status.success(), "{rerun:?}");
    let after = read_run_json(&run_dir);
    assert_eq!(after["status"], "blocked");
    assert_eq!(
        after["provider_exchange_records"]
            .as_array()
            .expect("records")
            .len(),
        before_records + 2,
        "the cap-denied request records only its initial exchange and makes no retry"
    );
    assert!(!run_dir
        .join("artifacts/01-research.attempt-002.context-round-001.json")
        .exists());
}

#[test]
fn loop_resume_reruns_only_output_review_as_attempt_two_without_overwriting_attempt_one() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let run_id = "cli-output-review-rerun";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    run_fake_provider(&repo, &ticket_path, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    let before = read_run_json(&run_dir);
    let attempt_one = before["provider_exchange_records"]
        .as_array()
        .expect("provider records")
        .iter()
        .filter(|record| record["step"] == "output_review")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(attempt_one.len(), 2);
    assert!(attempt_one.iter().all(|record| record["step_attempt"] == 1));

    let rerun = resume_provider_run(
        &repo,
        &ticket_path,
        &runs_root,
        run_id,
        &["--rerun-from", "output-review", "--json"],
    );

    assert!(rerun.status.success(), "{rerun:?}");
    let after = read_run_json(&run_dir);
    assert_eq!(after["status"], "completed");
    let output_review_records = after["provider_exchange_records"]
        .as_array()
        .expect("provider records")
        .iter()
        .filter(|record| record["step"] == "output_review")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(&output_review_records[..attempt_one.len()], attempt_one);
    assert_eq!(output_review_records.len(), 4);
    assert!(output_review_records[attempt_one.len()..]
        .iter()
        .all(|record| record["step_attempt"] == 2));
    assert!(run_dir
        .join("prompts/06-output-review.attempt-001.exchange-001.initial.request.md")
        .exists());
    assert!(run_dir
        .join("prompts/06-output-review.attempt-002.exchange-001.initial.request.md")
        .exists());
}

#[test]
fn loop_cli_resume_continues_a_consistent_durable_request_and_rejects_later_tampering() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    fs::write(
        repo.join("recovery-context.txt"),
        "recovery context bytes\n",
    )
    .expect("recovery context");
    commit_all(&repo, "Add recovery context");
    let runs_root = repo.join("runs");
    let run_id = "cli-durable-request-resume";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let mut initial_responses = vec![
        provider_needs_context_response("researcher", "recovery-context.txt"),
        agent_response("researcher", "Research complete.", "Continue."),
    ];
    initial_responses.extend(provider_loop_model_responses().into_iter().skip(1));
    let initial_url = start_fake_ollama_server_sequence(initial_responses);
    let initial = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "ollama",
            "--model",
            "mocked-ollama",
            "--base-url",
            &initial_url,
            "--allow-dirty",
        ])
        .output()
        .expect("initial run");
    assert!(initial.status.success(), "{initial:?}");
    let run_dir = runs_root.join(run_id);
    let exchange_request_path =
        "prompts/06-output-review.attempt-001.exchange-001.initial.request.md";
    let response_audit_path =
        "responses/06-output-review.attempt-001.exchange-001.initial.response.json";
    let response_record_path =
        "artifacts/06-output-review.attempt-001.exchange-001.initial.response.record.json";
    let audited_request: serde_json::Value = serde_json::from_slice(
        &fs::read(run_dir.join(exchange_request_path)).expect("exchange request"),
    )
    .expect("request JSON");
    let expected_user_input = audited_request["messages"][0]["content"]
        .as_str()
        .expect("audited user input")
        .to_string();
    let mut interrupted = read_run_json(&run_dir);
    interrupted["provider_exchange_records"]
        .as_array_mut()
        .expect("records")
        .pop();
    interrupted["status"] = serde_json::json!("running");
    interrupted["current_step"] = serde_json::json!("output_review");
    for step in interrupted["steps"].as_array_mut().expect("steps") {
        match step["name"].as_str().expect("step name") {
            "output_review" => {
                step["status"] = serde_json::json!("running");
                step.as_object_mut().expect("step").remove("artifact_path");
                step.as_object_mut()
                    .expect("step")
                    .remove("artifact_digest");
            }
            "testing" | "eval_report" => {
                step["status"] = serde_json::json!("pending");
                step.as_object_mut().expect("step").remove("artifact_path");
                step.as_object_mut()
                    .expect("step")
                    .remove("artifact_digest");
            }
            _ => {}
        }
    }
    fs::remove_file(run_dir.join(response_audit_path)).expect("remove uncommitted response audit");
    fs::remove_file(run_dir.join(response_record_path))
        .expect("remove uncommitted response record");
    fs::write(
        run_dir.join("run.json"),
        serde_json::to_vec_pretty(&interrupted).expect("interrupted run"),
    )
    .expect("write interrupted run");

    let (resume_url, captured) =
        start_recording_fake_ollama_server_sequence(vec![reviewer_response(
            "output_reviewer",
            "approve_for_tests",
            "Recovered exact durable request.",
        )]);
    let resume = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--base-url",
            &resume_url,
        ])
        .output()
        .expect("resume durable request");
    assert!(resume.status.success(), "{resume:?}");
    let requests = captured.lock().expect("captured requests");
    assert_eq!(requests.len(), 1);
    let header_end = find_header_end(&requests[0]).expect("HTTP headers");
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0][header_end + 4..]).expect("Ollama request JSON");
    assert_eq!(
        body["messages"]
            .as_array()
            .expect("messages")
            .last()
            .expect("user message")["content"],
        expected_user_input
    );
    drop(requests);
    assert_eq!(read_run_json(&run_dir)["status"], "completed");

    for relative in [
        "prompts/01-research.attempt-001.exchange-001.initial.request.md",
        "responses/01-research.attempt-001.exchange-001.initial.response.json",
        "artifacts/01-research.attempt-001.exchange-001.initial.request.record.json",
        "artifacts/01-research.attempt-001.context-round-001.json",
    ] {
        let path = run_dir.join(relative);
        let original = fs::read(&path).expect("tamper target");
        fs::write(&path, "tampered durable artifact").expect("tamper artifact");
        let before = read_tree_bytes(&run_dir);
        let (probe, probe_url) = provider_call_probe();
        let rejected = seaf_in(&repo)
            .args([
                "loop",
                "resume",
                "--ticket",
                ticket_path.to_str().unwrap(),
                "--runs-root",
                runs_root.to_str().unwrap(),
                "--run-id",
                run_id,
                "--base-url",
                &probe_url,
                "--rerun-from",
                "research",
            ])
            .output()
            .expect("tampered resume");
        assert!(!rejected.status.success(), "{relative}");
        assert_no_provider_call(&probe);
        assert_eq!(read_tree_bytes(&run_dir), before, "{relative}");
        fs::write(path, original).expect("restore fixture authority");
    }
}

#[test]
fn loop_cli_resume_reconstructs_context_retry_from_old_bytes_after_repository_change() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let context_path = repo.join("recovery-context.txt");
    fs::write(&context_path, "old accepted recovery bytes\n").expect("context");
    commit_all(&repo, "Add recovery context");
    let runs_root = repo.join("runs");
    let run_id = "cli-context-recovery-old-bytes";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let mut responses = vec![
        provider_needs_context_response("researcher", "recovery-context.txt"),
        agent_response("researcher", "Research complete.", "Continue."),
    ];
    responses.extend(provider_loop_model_responses().into_iter().skip(1));
    let initial_url = start_fake_ollama_server_sequence(responses);
    let initial = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "ollama",
            "--model",
            "mocked-ollama",
            "--base-url",
            &initial_url,
            "--allow-dirty",
        ])
        .output()
        .expect("initial run");
    assert!(initial.status.success(), "{initial:?}");
    let run_dir = runs_root.join(run_id);
    mark_loop_run_pending_from_step(&run_dir.join("run.json"), "analysis");
    let retry_path = "prompts/01-research.attempt-001.exchange-002.context-retry.request.md";
    let retry: serde_json::Value =
        serde_json::from_slice(&fs::read(run_dir.join(retry_path)).expect("retry request"))
            .expect("retry JSON");
    let expected_expansion_message = retry["messages"][1]["content"]
        .as_str()
        .expect("expansion message")
        .to_string();
    let expansion: serde_json::Value = serde_json::from_slice(
        &fs::read(run_dir.join("artifacts/01-research.attempt-001.context-round-001.json"))
            .expect("candidate context expansion"),
    )
    .expect("candidate context expansion JSON");
    let persisted = read_run_json(&run_dir);
    let candidate_root = PathBuf::from(
        persisted["candidate_workspace"]["path"]
            .as_str()
            .expect("candidate path"),
    );
    assert_eq!(
        expansion["candidate_authority"]["kind"],
        "isolated_candidate"
    );
    assert_eq!(
        expansion["files"][0]["content"],
        fs::read_to_string(candidate_root.join("recovery-context.txt")).unwrap(),
        "NeedsContext must read candidate bytes"
    );
    let preserved = [
        "prompts/01-research.attempt-001.exchange-001.initial.request.md",
        "responses/01-research.attempt-001.exchange-001.initial.response.json",
        "artifacts/01-research.attempt-001.exchange-001.initial.request.record.json",
        "artifacts/01-research.attempt-001.exchange-001.initial.response.record.json",
        "artifacts/01-research.attempt-001.context-round-001.json",
        retry_path,
        "artifacts/01-research.attempt-001.exchange-002.context-retry.request.record.json",
    ];
    for directory in ["prompts", "responses", "artifacts"] {
        for entry in fs::read_dir(run_dir.join(directory)).expect("exchange directory") {
            let entry = entry.expect("exchange entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let belongs_to_later_step = ["02-", "03-", "04-", "05-", "06-", "07-", "08-"]
                .iter()
                .any(|prefix| name.starts_with(prefix));
            if belongs_to_later_step
                || ((name.contains(".exchange-") || name.contains(".context-round-"))
                    && !preserved.contains(&format!("{directory}/{name}").as_str()))
            {
                fs::remove_file(entry.path()).expect("remove post-crash exchange file");
            }
        }
    }
    let mut interrupted = read_run_json(&run_dir);
    interrupted["provider_exchange_records"]
        .as_array_mut()
        .expect("records")
        .truncate(3);
    interrupted["status"] = serde_json::json!("running");
    interrupted["current_step"] = serde_json::json!("research");
    interrupted["policy_decisions"] = serde_json::json!([]);
    for step in interrupted["steps"].as_array_mut().expect("steps") {
        step["status"] = if step["name"] == "research" {
            serde_json::json!("running")
        } else {
            serde_json::json!("pending")
        };
        step.as_object_mut().expect("step").remove("artifact_path");
        step.as_object_mut()
            .expect("step")
            .remove("artifact_digest");
    }
    fs::write(
        run_dir.join("run.json"),
        serde_json::to_vec_pretty(&interrupted).expect("interrupted run"),
    )
    .expect("write interrupted run");
    fs::write(&context_path, "changed live repository bytes\n").expect("mutate repository");
    let mut resume_responses = vec![agent_response(
        "researcher",
        "Recovered research.",
        "Continue.",
    )];
    resume_responses.extend(provider_loop_model_responses().into_iter().skip(1));
    let (resume_url, captured) = start_recording_fake_ollama_server_sequence(resume_responses);
    let resume = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--base-url",
            &resume_url,
        ])
        .output()
        .expect("resume context request");

    assert!(resume.status.success(), "{resume:?}");
    let requests = captured.lock().expect("requests");
    let header_end = find_header_end(&requests[0]).expect("headers");
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0][header_end + 4..]).expect("request body");
    assert_eq!(
        body["messages"]
            .as_array()
            .expect("messages")
            .last()
            .expect("expansion message")["content"],
        expected_expansion_message
    );
    let body_text = body.to_string();
    assert!(body_text.contains("old accepted recovery bytes"));
    assert!(!body_text.contains("changed live repository bytes"));
}

#[test]
fn loop_resume_provider_run_rejects_mutated_same_identity_ticket() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = temp_dir.path().join("runs");
    let run_id = "cli-loop-resume-ticket-digest";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let mutated_ticket_path = write_provider_loop_mutated_same_identity_ticket(temp_dir.path());

    let run = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "fake",
            "--model",
            "fake-model",
            "--json",
        ])
        .output()
        .expect("run loop");
    assert!(run.status.success(), "{run:?}");

    let run_path = runs_root.join(run_id).join("run.json");
    mark_loop_run_pending_from_analysis(&run_path);

    let resume = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--ticket",
            mutated_ticket_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("resume loop with mutated ticket");
    assert!(
        !resume.status.success(),
        "resume must bind to the original ticket content, not just ids"
    );
    let stderr = String::from_utf8(resume.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("does not match the original provider run ticket snapshot"),
        "content mismatch should explain the snapshot trust-boundary failure, got {stderr}"
    );
    assert_eq!(
        git_status_porcelain(&repo),
        "",
        "mutated ticket content must not let resume mutate the worktree"
    );
}

#[test]
fn loop_resume_repairs_missing_exact_provider_ticket_snapshot_before_provider_recovery() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = temp_dir.path().join("runs");
    let run_id = "cli-loop-resume-missing-snapshot";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);

    let run = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "fake",
            "--model",
            "fake-model",
            "--json",
        ])
        .output()
        .expect("run loop");
    assert!(run.status.success(), "{run:?}");

    let run_path = runs_root.join(run_id).join("run.json");
    mark_loop_run_pending_from_analysis(&run_path);
    let snapshot_path = runs_root.join(run_id).join("ticket.snapshot.json");
    let expected_snapshot = fs::read(&snapshot_path).expect("ticket snapshot");
    fs::remove_file(&snapshot_path).expect("remove provider ticket snapshot");

    let resume = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("resume loop without ticket snapshot");
    assert!(resume.status.success(), "{resume:?}");
    assert_eq!(fs::read(snapshot_path).unwrap(), expected_snapshot);
    assert_eq!(
        git_status_porcelain(&repo),
        "",
        "unverifiable ticket content must not let resume mutate the worktree"
    );
}

#[test]
fn loop_resume_rejects_mutated_same_path_policy_before_any_side_effect() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let run_id = "resume-mutated-policy";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let policy_path = repo.join("explicit-policy.json");
    write_policy(&policy_path, &[], &["dependency_changes"]);
    let base_url = start_fake_ollama_server_sequence(provider_loop_model_responses());

    let run = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "ollama",
            "--model",
            "mocked-ollama",
            "--base-url",
            &base_url,
            "--policy",
            policy_path.to_str().unwrap(),
            "--allow-dirty",
        ])
        .output()
        .expect("start explicit-policy run");
    assert!(run.status.success(), "{run:?}");

    let run_dir = runs_root.join(run_id);
    mark_loop_run_pending_from_analysis(&run_dir.join("run.json"));
    write_policy(
        &policy_path,
        &["examples/local-loop/evals/fake-provider-smoke.txt"],
        &[],
    );
    let before = read_tree_bytes(&run_dir);
    let worktree_before = git_status_porcelain(&repo);
    let (probe, probe_url) = provider_call_probe();

    let resume = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--policy",
            policy_path.to_str().unwrap(),
            "--base-url",
            &probe_url,
        ])
        .output()
        .expect("resume with mutated policy");

    assert!(!resume.status.success());
    let stderr = String::from_utf8(resume.stderr).expect("stderr");
    assert!(
        stderr.contains("policy") && stderr.contains("start a new run"),
        "policy mismatch must give actionable recovery guidance, got {stderr}"
    );
    assert_eq!(read_tree_bytes(&run_dir), before);
    assert_eq!(git_status_porcelain(&repo), worktree_before);
    assert_no_provider_call(&probe);
}

#[test]
fn loop_resume_rejects_mutated_config_even_when_effective_policy_is_unchanged() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(repo.join("policies")).expect("policies dir");
    init_git_repo(&repo);
    let policy_a = repo.join("policies/a.json");
    let policy_b = repo.join("policies/b.json");
    write_policy(&policy_a, &[], &[]);
    fs::copy(&policy_a, &policy_b).expect("copy identical policy");
    let config_path = repo.join("seaf.config.json");
    fs::write(&config_path, r#"{"policy_path":"policies/a.json"}"#).expect("config");
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let runs_root = repo.join("runs");
    let run_id = "resume-mutated-config";

    run_fake_provider(
        &repo,
        &ticket_path,
        &runs_root,
        run_id,
        &["--config", config_path.to_str().unwrap()],
    );
    let run_dir = runs_root.join(run_id);
    mark_loop_run_pending_from_analysis(&run_dir.join("run.json"));
    fs::write(&config_path, r#"{"policy_path":"policies/b.json"}"#).expect("mutated config");
    let before = read_tree_bytes(&run_dir);

    let resume = resume_provider_run(
        &repo,
        &ticket_path,
        &runs_root,
        run_id,
        &["--config", config_path.to_str().unwrap()],
    );

    assert!(!resume.status.success());
    let stderr = String::from_utf8(resume.stderr).expect("stderr");
    assert!(
        stderr.contains("config") && stderr.contains("start a new run"),
        "config mismatch must give actionable recovery guidance, got {stderr}"
    );
    assert_eq!(read_tree_bytes(&run_dir), before);
}

#[test]
fn loop_resume_repairs_missing_input_but_rejects_noncanonical_collision() {
    for case in ["missing-policy", "noncanonical-config"] {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let repo = temp_dir.path().join("repo");
        fs::create_dir_all(&repo).expect("repo dir");
        init_git_repo(&repo);
        let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
        let runs_root = repo.join("runs");

        run_fake_provider(&repo, &ticket_path, &runs_root, case, &[]);
        let run_dir = runs_root.join(case);
        mark_loop_run_pending_from_analysis(&run_dir.join("run.json"));
        let removed = match case {
            "missing-policy" => {
                let path = run_dir.join("inputs/policy.json");
                let bytes = fs::read(&path).unwrap();
                fs::remove_file(path).expect("remove policy snapshot");
                Some(bytes)
            }
            "noncanonical-config" => {
                let path = run_dir.join("inputs/config.json");
                let mut bytes = fs::read(&path).expect("config snapshot");
                bytes.push(b'\n');
                fs::write(path, bytes).expect("tamper config snapshot");
                None
            }
            _ => unreachable!(),
        };
        let before = read_tree_bytes(&run_dir);

        let resume = resume_provider_run(&repo, &ticket_path, &runs_root, case, &[]);

        if let Some(expected) = removed {
            assert!(resume.status.success(), "{resume:?}");
            assert_eq!(
                fs::read(run_dir.join("inputs/policy.json")).unwrap(),
                expected
            );
        } else {
            assert!(!resume.status.success(), "{resume:?}");
            let stderr = String::from_utf8(resume.stderr).expect("stderr");
            assert!(
                stderr.contains("collision"),
                "noncanonical snapshot must collide: {stderr}"
            );
            assert_eq!(read_tree_bytes(&run_dir), before);
        }
    }
}

#[test]
fn loop_resume_rejects_input_digest_mismatch_without_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let runs_root = repo.join("runs");
    let run_id = "resume-run-digest-mismatch";
    run_fake_provider(&repo, &ticket_path, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    mark_loop_run_pending_from_analysis(&run_dir.join("run.json"));
    let mut run = read_run_json(&run_dir);
    run["input_digests"]["policy"] = serde_json::json!("0".repeat(64));
    fs::write(
        run_dir.join("run.json"),
        serde_json::to_vec_pretty(&run).expect("serialize run"),
    )
    .expect("write mismatched digest");
    let before = read_tree_bytes(&run_dir);

    let resume = resume_provider_run(&repo, &ticket_path, &runs_root, run_id, &[]);

    assert!(!resume.status.success());
    let stderr = String::from_utf8(resume.stderr).expect("stderr");
    assert!(
        stderr.contains("policy digest") && stderr.contains("start a new run"),
        "run digest mismatch must be actionable, got {stderr}"
    );
    assert_eq!(read_tree_bytes(&run_dir), before);
}

#[test]
fn loop_resume_accepts_matching_explicit_policy_authority() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let policy_path = repo.join("explicit-policy.json");
    write_policy(
        &policy_path,
        &["examples/local-loop/evals/fake-provider-smoke.txt"],
        &[],
    );
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let runs_root = repo.join("runs");
    let run_id = "resume-explicit-policy";
    let authority = ["--policy", policy_path.to_str().unwrap()];
    run_fake_provider(&repo, &ticket_path, &runs_root, run_id, &authority);
    mark_loop_run_pending_from_analysis(&runs_root.join(run_id).join("run.json"));

    let resume = resume_provider_run(
        &repo,
        &ticket_path,
        &runs_root,
        run_id,
        &[
            "--policy",
            policy_path.to_str().unwrap(),
            "--rerun-from",
            "analysis",
        ],
    );

    assert!(resume.status.success(), "{resume:?}");
    assert_eq!(
        read_run_json(&runs_root.join(run_id))["policy_decisions"][0]["decision"],
        "rejected",
        "resume must keep using the verified explicit policy"
    );
}

#[test]
fn loop_resume_mocked_ollama_uses_verified_project_policy() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(repo.join("policies")).expect("policies dir");
    init_git_repo(&repo);
    let policy_path = repo.join("policies/reject-smoke.json");
    write_policy(
        &policy_path,
        &["examples/local-loop/evals/fake-provider-smoke.txt"],
        &[],
    );
    fs::write(
        repo.join("seaf.config.json"),
        r#"{"policy_path":"policies/reject-smoke.json"}"#,
    )
    .expect("project config");
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let runs_root = repo.join("runs");
    let run_id = "resume-ollama-verified-policy";
    let initial_url = start_fake_ollama_server_sequence(provider_loop_model_responses());
    let run = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "ollama",
            "--model",
            "mocked-ollama",
            "--base-url",
            &initial_url,
            "--allow-dirty",
        ])
        .output()
        .expect("initial Ollama run");
    assert!(run.status.success(), "{run:?}");
    mark_loop_run_pending_from_step(&runs_root.join(run_id).join("run.json"), "development");
    let resume_url = start_fake_ollama_server_sequence(
        provider_loop_model_responses()
            .into_iter()
            .skip(4)
            .collect(),
    );

    let resume = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--base-url",
            &resume_url,
            "--rerun-from",
            "development",
        ])
        .output()
        .expect("resume Ollama run");

    assert!(resume.status.success(), "{resume:?}");
    assert_eq!(
        read_run_json(&runs_root.join(run_id))["policy_decisions"][0]["decision"],
        "rejected",
        "mocked Ollama resume must gate with the verified project policy, not a compiled fallback"
    );
}

#[test]
fn loop_resume_rejects_policy_path_outside_repository_without_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let outside_policy = temp_dir.path().join("outside-policy.json");
    write_policy(&outside_policy, &[], &[]);
    let runs_root = repo.join("runs");
    let run_id = "resume-unsafe-policy";
    run_fake_provider(&repo, &ticket_path, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    mark_loop_run_pending_from_analysis(&run_dir.join("run.json"));
    let before = read_tree_bytes(&run_dir);

    let resume = resume_provider_run(
        &repo,
        &ticket_path,
        &runs_root,
        run_id,
        &["--policy", outside_policy.to_str().unwrap()],
    );

    assert!(!resume.status.success());
    let stderr = String::from_utf8(resume.stderr).expect("stderr");
    assert!(stderr.contains("outside the git repository"), "{stderr}");
    assert_eq!(read_tree_bytes(&run_dir), before);
}

#[test]
fn loop_resume_rejects_run_copied_to_another_repository_without_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let source_repo = temp_dir.path().join("source");
    let destination_repo = temp_dir.path().join("destination");
    fs::create_dir_all(&source_repo).expect("source repo");
    fs::create_dir_all(&destination_repo).expect("destination repo");
    init_git_repo(&source_repo);
    init_git_repo(&destination_repo);
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let source_runs = source_repo.join("runs");
    let destination_runs = destination_repo.join("runs");
    let run_id = "resume-copied-repository";
    run_fake_provider(&source_repo, &ticket_path, &source_runs, run_id, &[]);
    let source_run = source_runs.join(run_id);
    assert!(
        source_run.join("inputs/repository.json").is_file(),
        "new runs must persist repository identity"
    );
    mark_loop_run_pending_from_analysis(&source_run.join("run.json"));
    copy_directory(&source_run, &destination_runs.join(run_id));
    let destination_run = destination_runs.join(run_id);
    let before = read_tree_bytes(&destination_run);

    let resume = resume_provider_run(
        &destination_repo,
        &ticket_path,
        &destination_runs,
        run_id,
        &[],
    );

    assert!(!resume.status.success());
    let stderr = String::from_utf8(resume.stderr).expect("stderr");
    assert!(
        stderr.contains("repository identity") && stderr.contains("start a new run"),
        "copied run must explain repository binding, got {stderr}"
    );
    assert_eq!(read_tree_bytes(&destination_run), before);
}

#[test]
fn loop_resume_rejects_copied_run_when_repository_snapshot_is_rewritten_for_destination() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let source_repo = temp_dir.path().join("source");
    let destination_repo = temp_dir.path().join("destination");
    fs::create_dir_all(&source_repo).expect("source repo");
    fs::create_dir_all(&destination_repo).expect("destination repo");
    init_git_repo(&source_repo);
    init_git_repo(&destination_repo);
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let source_runs = source_repo.join("runs");
    let destination_runs = destination_repo.join("runs");
    let run_id = "resume-rewritten-repository-snapshot";
    run_fake_provider(&source_repo, &ticket_path, &source_runs, run_id, &[]);
    let source_run = source_runs.join(run_id);
    mark_loop_run_pending_from_analysis(&source_run.join("run.json"));
    copy_directory(&source_run, &destination_runs.join(run_id));
    let destination_run = destination_runs.join(run_id);
    fs::write(
        destination_run.join("inputs/repository.json"),
        canonical_json_bytes(&repository_identity_json(&destination_repo))
            .expect("canonical destination repository identity"),
    )
    .expect("rewrite repository snapshot for destination");
    let before = read_tree_bytes(&destination_run);

    let resume = resume_provider_run(
        &destination_repo,
        &ticket_path,
        &destination_runs,
        run_id,
        &[],
    );

    assert!(!resume.status.success());
    let stderr = String::from_utf8(resume.stderr).expect("stderr");
    assert!(
        stderr.contains("repository") && stderr.contains("digest"),
        "rewritten repository snapshot must not sever the run binding, got {stderr}"
    );
    assert_eq!(read_tree_bytes(&destination_run), before);
}

#[test]
fn loop_resume_provider_run_rejects_ticket_identity_mismatch() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = temp_dir.path().join("runs");
    let run_id = "cli-loop-resume-identity";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let mismatch_ticket_path = write_provider_loop_mismatched_identity_ticket(temp_dir.path());

    let run = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "fake",
            "--model",
            "fake-model",
            "--json",
        ])
        .output()
        .expect("run loop");
    assert!(run.status.success(), "{run:?}");

    mark_loop_run_pending_from_analysis(&runs_root.join(run_id).join("run.json"));

    let resume = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--ticket",
            mismatch_ticket_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("resume loop with mismatched ticket");
    assert!(
        !resume.status.success(),
        "resume must bind a provider-backed run to its original ticket identity"
    );
    let stderr = String::from_utf8(resume.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("ticket_id") && stderr.contains("goal_id"),
        "identity mismatch should report both persisted trust-boundary fields, got {stderr}"
    );
}

#[test]
fn loop_run_fake_from_subdirectory_uses_committed_candidate_context_not_dirty_source() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    let subdir = repo.join("crates/example");
    fs::create_dir_all(&subdir).expect("subdir");
    fs::create_dir_all(repo.join("docs")).expect("docs dir");
    fs::write(
        repo.join("docs/root-context.txt"),
        "root-owned context for provider loop",
    )
    .expect("write root context");
    init_git_repo(&repo);
    let runs_root = temp_dir.path().join("runs");
    let run_id = "cli-loop-subdir-root";
    let ticket_path = write_provider_loop_ticket_with_relevant_file(
        temp_dir.path(),
        "docs/root-context.txt",
        false,
    );

    let run = seaf_in(&subdir)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "fake",
            "--model",
            "fake-model",
            "--allow-dirty",
            "--json",
        ])
        .output()
        .expect("run loop from subdirectory");
    assert!(run.status.success(), "{run:?}");

    let manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(runs_root.join(run_id).join("context-manifest.json"))
            .expect("context manifest"),
    )
    .expect("context manifest json");
    let files = manifest["files"].as_array().expect("manifest files");
    assert!(
        files.is_empty(),
        "dirty source-only bytes must not enter candidate context"
    );
    assert!(
        manifest["warnings"]
            .as_array()
            .expect("manifest warnings")
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap()
                .contains("skipped missing context file")),
        "candidate context should report the uncommitted source-only file as missing"
    );
}

#[test]
fn loop_bench_fake_json_reports_agent_bench_summary() {
    let output = seaf()
        .args([
            "loop",
            "bench",
            "--provider",
            "fake",
            "--fixture",
            agent_bench_fixture_path().to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run loop bench");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("bench summary json");
    assert_eq!(summary["ticket_count"], 5);
    assert_eq!(summary["schema_valid_rate"], 1.0);
    assert_eq!(summary["repair_success_rate"], 0.2);
    assert_eq!(summary["patch_apply_rate"], 0.6);
    assert_eq!(summary["eval_pass_rate"], 1.0);
    assert_eq!(summary["forbidden_violation_count"], 0);
    assert_eq!(summary["eval_weakening_accepted_count"], 0);
    assert_eq!(summary["median_latency_ms"], 120);
}

#[test]
fn loop_bench_json_still_emits_summary_when_zero_tolerance_fails() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    write_bench_fixture_with_violation(temp_dir.path());

    let output = seaf()
        .args([
            "loop",
            "bench",
            "--provider",
            "fake",
            "--fixture",
            temp_dir.path().to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run loop bench");

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("bench summary json");
    assert_eq!(summary["forbidden_violation_count"], 1);
    assert_eq!(summary["eval_weakening_accepted_count"], 1);
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("forbidden_violation_count=1"));
    assert!(stderr.contains("eval_weakening_accepted_count=1"));
}

#[test]
fn loop_bench_ollama_validates_base_url_without_live_server() {
    let output = seaf()
        .args([
            "loop",
            "bench",
            "--provider",
            "ollama",
            "--model",
            "local-model",
            "--base-url",
            "ftp://localhost:11434/api",
            "--fixture",
            agent_bench_fixture_path().to_str().unwrap(),
        ])
        .output()
        .expect("run loop bench");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("unsupported Ollama base URL"));
}

#[test]
fn loop_bench_ollama_rejects_negative_smoke_response() {
    let base_url = start_fake_ollama_server(r#"{"ok":false}"#);

    let output = seaf()
        .args([
            "loop",
            "bench",
            "--provider",
            "ollama",
            "--model",
            "local-model",
            "--base-url",
            &base_url,
            "--fixture",
            agent_bench_fixture_path().to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run loop bench");

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(
        stdout.trim().is_empty(),
        "negative Ollama smoke should not emit success summary: {stdout}"
    );
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("ok == true"));
}

#[test]
fn loop_resume_json_rejects_invalid_run_file_without_scaffolding_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_dir = runs_root.join("invalid-json");
    fs::create_dir_all(&run_dir).expect("create run dir");
    fs::write(run_dir.join("run.json"), "{").expect("write invalid run");

    let output = seaf()
        .args([
            "loop",
            "resume",
            "--run-id",
            "invalid-json",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run loop resume");

    assert_loop_run_validation_failure(output);
    assert!(!run_dir.join("prompts").exists());
    assert!(!run_dir.join("responses").exists());
    assert!(!run_dir.join("artifacts").exists());
    assert!(!run_dir.join("context-manifest.json").exists());
    assert!(!run_dir.join("log.md").exists());
}

#[test]
fn loop_resume_json_rejects_missing_run_file_without_scaffolding_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");

    let output = seaf()
        .args([
            "loop",
            "resume",
            "--run-id",
            "missing-run",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run loop resume");

    assert_loop_run_validation_failure(output);
    assert!(!runs_root.join("missing-run").exists());
}

#[test]
fn loop_status_json_rejects_invalid_persisted_run_id_without_reporting_escaped_paths() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_dir = runs_root.join("safe");
    fs::create_dir_all(&run_dir).expect("create run dir");
    write_loop_run_file(&run_dir.join("run.json"), "../escaped");

    let output = seaf()
        .args([
            "loop",
            "status",
            "--run-id",
            "safe",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run loop status");

    let report = parse_loop_run_validation_failure(output);
    assert_eq!(report["errors"][0]["field"], "run_id");
    assert!(report.get("run_file").is_none());
    assert!(!report.to_string().contains("escaped"));
}

#[test]
fn loop_resume_json_rejects_mismatched_persisted_run_id_without_scaffolding_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    let run_dir = runs_root.join("safe");
    fs::create_dir_all(&run_dir).expect("create run dir");
    write_loop_run_file(&run_dir.join("run.json"), "other-safe");

    let output = seaf()
        .args([
            "loop",
            "resume",
            "--run-id",
            "safe",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run loop resume");

    let report = parse_loop_run_validation_failure(output);
    assert_eq!(report["errors"][0]["field"], "run_id");
    assert!(!run_dir.join("prompts").exists());
    assert!(!run_dir.join("responses").exists());
    assert!(!run_dir.join("artifacts").exists());
    assert!(!run_dir.join("context-manifest.json").exists());
    assert!(!run_dir.join("log.md").exists());
}

#[test]
fn loop_smoke_produces_json_artifacts_without_ollama() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");

    let output = seaf()
        .args([
            "loop",
            "smoke",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run loop smoke");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("json report");
    let run_id = report["run_id"].as_str().expect("run_id");
    assert_eq!(report["command"], "smoke");
    assert_eq!(report["status"], "completed");
    assert!(runs_root.join(run_id).join("run.json").exists());
}

#[test]
fn eval_run_loop_mode_accepts_product_path_loop_run() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path();
    init_git_repo(repo);
    let runs_root = repo.join("runs");
    let run_id = "loop-eval-product";
    let eval_report_path = repo.join("eval-report.json");

    let loop_run = seaf_in(repo)
        .args([
            "loop",
            "run",
            "--ticket",
            local_loop_ticket_path().to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--provider",
            "fake",
            "--model",
            "fake-model",
            "--json",
        ])
        .output()
        .expect("run loop");
    assert!(loop_run.status.success(), "{loop_run:?}");

    let run_path = runs_root.join(run_id).join("run.json");
    let persisted_run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&run_path).expect("run json"))
            .expect("persisted run json");
    assert!(
        !persisted_run["policy_decisions"]
            .as_array()
            .expect("policy decisions")
            .is_empty(),
        "product path loop run should persist policy gate evidence"
    );
    let decision = &persisted_run["policy_decisions"][0];
    assert_eq!(decision["patch_id"], run_id);
    assert_eq!(decision["decision"], "requires_human_review");
    assert_eq!(decision["apply_requested"], true);
    assert_eq!(decision["applied"], false);

    let output = seaf()
        .args([
            "eval",
            "run",
            local_loop_eval_path().to_str().unwrap(),
            "--loop-run",
            run_path.to_str().unwrap(),
            "--ticket",
            local_loop_ticket_path().to_str().unwrap(),
            "--output",
            eval_report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("eval report json");
    assert_eq!(report["patch_id"], run_id);
    assert_eq!(report["goal_id"], "local_agent_loop_mvp");
    assert_eq!(report["passed"], true);
    assert_eq!(report["decision"], "approve_for_human_review");
}

#[test]
fn eval_run_loop_mode_rejects_missing_real_policy_evidence() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let ticket_path = temp_dir.path().join("ticket.yaml");
    let run_path = temp_dir.path().join("run.json");
    let eval_report_path = temp_dir.path().join("eval-report.json");
    let run_id = "loop-missing-policy-evidence";

    fs::write(
        &config_path,
        r#"evals:
  allow_commands:
    - printf
  required:
    - name: smoke
      command: "printf ok"
"#,
    )
    .expect("write eval config");
    fs::write(
        &ticket_path,
        r#"ticket_id: T-LOCAL-001
goal_id: local_agent_loop_mvp
title: Add a health check command to the CLI
status: ready
priority: p1
problem: "Exercise missing policy evidence refusal."
context:
  relevant_files: []
  forbidden_files: []
autonomy:
  level: 1
  apply_patch: false
  allow_shell_commands:
    - printf
acceptance_criteria:
  - Existing tests pass.
"#,
    )
    .expect("write ticket");
    write_passing_loop_run_file(&run_path, run_id);
    let mut persisted_run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&run_path).expect("run json"))
            .expect("persisted run json");
    *persisted_run
        .get_mut("policy_decisions")
        .expect("policy decisions") = serde_json::json!([]);
    fs::write(
        &run_path,
        serde_json::to_string_pretty(&persisted_run).expect("serialize missing-evidence run"),
    )
    .expect("write missing-evidence run");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--loop-run",
            run_path.to_str().unwrap(),
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--output",
            eval_report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("eval report json");
    assert_eq!(report["patch_id"], run_id);
    assert_eq!(report["passed"], false);
    assert_eq!(report["decision"], "reject");
    let policy_check = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["name"] == "patch_policy_gate")
        .expect("policy gate check");
    assert_eq!(policy_check["status"], "failed");
    assert!(policy_check["summary"]
        .as_str()
        .expect("summary")
        .contains("No patch policy gate decision was recorded"));
}

#[test]
fn release_prepare_and_verify_bind_artifact_and_eval_digests() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let artifact_path = temp_dir.path().join("artifact.txt");
    let eval_report_path = temp_dir.path().join("eval-report.json");
    let capsule_path = temp_dir.path().join("release-capsule.json");
    fs::write(&artifact_path, "artifact").expect("write artifact");
    fs::write(
        &eval_report_path,
        r#"{
  "eval_report_id": "eval_reduce_time_to_first_note",
  "patch_id": "patch_smoke",
  "goal_id": "reduce_time_to_first_note",
  "passed": true,
  "summary": "All required eval checks passed.",
  "checks": [
    { "name": "smoke", "status": "passed" }
  ],
  "risk_level": "low",
  "decision": "approve_for_human_review"
}"#,
    )
    .expect("write eval report");

    let prepare = seaf()
        .args([
            "release",
            "prepare",
            "--app-id",
            "dev.seaf.adaptive-notes",
            "--version",
            "0.1.0",
            "--source-commit",
            "abc123",
            "--artifact",
            artifact_path.to_str().unwrap(),
            "--eval-report",
            eval_report_path.to_str().unwrap(),
            "--rollback-plan",
            "rollback/0.0.9",
            "--output",
            capsule_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run release prepare");
    assert!(prepare.status.success());
    assert!(capsule_path.exists());

    let verify = seaf()
        .args([
            "release",
            "verify",
            capsule_path.to_str().unwrap(),
            "--artifact",
            artifact_path.to_str().unwrap(),
            "--eval-report",
            eval_report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run release verify");
    assert!(verify.status.success());

    fs::write(&artifact_path, "tampered").expect("tamper artifact");
    let tampered = seaf()
        .args([
            "release",
            "verify",
            capsule_path.to_str().unwrap(),
            "--artifact",
            artifact_path.to_str().unwrap(),
            "--eval-report",
            eval_report_path.to_str().unwrap(),
        ])
        .output()
        .expect("run release verify");
    assert!(!tampered.status.success());
    let stderr = String::from_utf8(tampered.stderr).expect("utf8 stderr");
    assert!(stderr.contains("artifact_digest"));
}

#[test]
fn release_prepare_rejects_contradictory_eval_report() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let artifact_path = temp_dir.path().join("artifact.txt");
    let eval_report_path = temp_dir.path().join("eval-report.json");
    let capsule_path = temp_dir.path().join("release-capsule.json");
    fs::write(&artifact_path, "artifact").expect("write artifact");
    fs::write(
        &eval_report_path,
        r#"{
  "eval_report_id": "eval_reduce_time_to_first_note",
  "patch_id": "patch_smoke",
  "goal_id": "reduce_time_to_first_note",
  "passed": true,
  "summary": "Contradictory report should be rejected.",
  "checks": [
    { "name": "smoke", "status": "passed" }
  ],
  "risk_level": "high",
  "decision": "reject"
}"#,
    )
    .expect("write eval report");

    let output = seaf()
        .args([
            "release",
            "prepare",
            "--app-id",
            "dev.seaf.adaptive-notes",
            "--version",
            "0.1.0",
            "--source-commit",
            "abc123",
            "--artifact",
            artifact_path.to_str().unwrap(),
            "--eval-report",
            eval_report_path.to_str().unwrap(),
            "--rollback-plan",
            "rollback/0.0.9",
            "--output",
            capsule_path.to_str().unwrap(),
        ])
        .output()
        .expect("run release prepare");

    assert!(!output.status.success());
    assert!(!capsule_path.exists());
}

#[test]
fn release_verify_accepts_valid_capsule_structure() {
    let output = seaf()
        .args([
            "release",
            "verify",
            example_path("release-capsule.json").to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run release verify");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"valid\": true"));
}

#[cfg(unix)]
fn write_executable_script(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write script");
    let mut permissions = fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod script");
}

#[cfg(unix)]
fn compile_descendant_pipe_helper(root: &Path) -> PathBuf {
    let source_path = root.join("descendant-pipe-helper.rs");
    let helper_path = root.join("descendant-pipe-helper");
    fs::write(
        &source_path,
        r#"
use std::{
    fs,
    io::{self, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

extern "C" {
    fn setsid() -> i32;
}

fn main() {
    if std::env::args().any(|arg| arg == "--hold-pipes") {
        unsafe {
            setsid();
        }
        let marker = std::env::args().nth(2).unwrap();
        let stop = std::env::args().nth(3).unwrap();
        let exited = std::env::args().nth(4).unwrap();
        fs::write(marker, "ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !Path::new(&stop).exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        fs::write(exited, "exited").unwrap();
        return;
    }

    let marker = std::env::args().nth(1).unwrap();
    let stop = std::env::args().nth(2).unwrap();
    let exited = std::env::args().nth(3).unwrap();
    Command::new(std::env::current_exe().unwrap())
        .arg("--hold-pipes")
        .arg(&marker)
        .arg(&stop)
        .arg(&exited)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_millis(500);
    while !Path::new(&marker).exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(1200));
    println!("direct-child-done");
    io::stdout().flush().unwrap();
}
"#,
    )
    .expect("write descendant pipe helper source");
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let output = Command::new(rustc)
        .args([
            source_path.to_str().unwrap(),
            "-o",
            helper_path.to_str().unwrap(),
        ])
        .output()
        .expect("compile descendant pipe helper");
    assert!(
        output.status.success(),
        "compile descendant pipe helper failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    helper_path
}

#[test]
fn eval_run_rejects_shell_metacharacters_without_side_effects() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let marker_path = temp_dir.path().join("marker");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - printf
  required:
    - name: shell_meta
      command: "printf touched > {}"
"#,
            marker_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("shell metacharacter"), "{stderr}");
    assert!(
        !marker_path.exists(),
        "shell metacharacter command must not run"
    );
    assert!(
        !report_path.exists(),
        "rejected eval should not write report"
    );
    assert!(
        !temp_dir.path().join("logs/shell_meta.stdout.log").exists(),
        "rejected eval should not write check logs"
    );
}

#[test]
fn eval_run_loop_mode_rejects_command_not_allowed_by_ticket() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let ticket_path = temp_dir.path().join("ticket.yaml");
    let run_path = temp_dir.path().join("run.json");
    let report_path = temp_dir.path().join("eval-report.json");
    write_passing_loop_run_file(&run_path, "loop_cli_001");
    fs::write(
        &config_path,
        r#"evals:
  allow_commands:
    - printf
  required:
    - name: smoke
      command: "printf ok"
"#,
    )
    .expect("write eval config");
    fs::write(
        &ticket_path,
        r#"ticket_id: T-LOCAL-001
goal_id: local_agent_loop_mvp
title: Add a health check command to the CLI
status: ready
priority: p1
problem: "Exercise eval command allowlists."
context:
  relevant_files:
    - crates/seaf-cli/src/main.rs
  forbidden_files: []
autonomy:
  level: 1
  apply_patch: true
  allow_shell_commands:
    - pnpm typecheck
acceptance_criteria:
  - Existing tests pass.
"#,
    )
    .expect("write ticket");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--loop-run",
            run_path.to_str().unwrap(),
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("ticket autonomy"), "{stderr}");
    assert!(
        !report_path.exists(),
        "disallowed eval should not write report"
    );
    assert!(
        !temp_dir.path().join("logs/smoke.stdout.log").exists(),
        "disallowed eval should not write check logs"
    );
}

#[test]
fn eval_run_rejects_command_not_allowed_by_eval_config_without_side_effects() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let marker_path = temp_dir.path().join("marker");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::write(
        &config_path,
        r#"evals:
  allow_commands:
    - printf
  required:
    - name: disallowed
      command: "touch marker"
"#,
    )
    .expect("write eval config");

    let output = seaf_in(temp_dir.path())
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("eval allow_commands"), "{stderr}");
    assert!(!marker_path.exists(), "disallowed command must not run");
    assert!(
        !report_path.exists(),
        "rejected eval should not write report"
    );
    assert!(
        !temp_dir.path().join("logs/disallowed.stdout.log").exists(),
        "rejected eval should not write per-check logs"
    );
}

#[cfg(unix)]
#[test]
fn eval_run_rejects_path_env_before_it_can_hijack_allowed_command() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let fake_bin = temp_dir.path().join("fake-bin");
    let marker_path = temp_dir.path().join("marker");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::create_dir_all(&fake_bin).expect("fake bin dir");
    write_executable_script(
        &fake_bin.join("cargo"),
        &format!(
            r#"#!/bin/sh
touch {}
"#,
            marker_path.display()
        ),
    );
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - cargo
  required:
    - name: path_hijack
      command: "cargo --version"
      env:
        PATH: {}
"#,
            fake_bin.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("PATH"), "{stderr}");
    assert!(
        !marker_path.exists(),
        "per-check PATH must not redirect an allowed command"
    );
    assert!(
        !report_path.exists(),
        "rejected eval should not write report"
    );
    assert!(
        !temp_dir.path().join("logs/path_hijack.stdout.log").exists(),
        "rejected eval should not write per-check logs"
    );
}

#[cfg(unix)]
#[test]
fn eval_run_ignores_inherited_path_when_resolving_allowed_command() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let fake_bin = temp_dir.path().join("fake-bin");
    let marker_path = temp_dir.path().join("marker");
    let report_path = temp_dir.path().join("eval-report.json");
    let original_path = std::env::var("PATH").expect("PATH");
    fs::create_dir_all(&fake_bin).expect("fake bin dir");
    write_executable_script(
        &fake_bin.join("cargo"),
        &format!(
            r#"#!/bin/sh
touch {}
"#,
            marker_path.display()
        ),
    );
    fs::write(
        &config_path,
        r#"evals:
  allow_commands:
    - cargo
  required:
    - name: inherited_path_hijack
      command: "cargo --version"
"#,
    )
    .expect("write eval config");

    let output = seaf()
        .env("PATH", format!("{}:{original_path}", fake_bin.display()))
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success(), "{output:?}");
    assert!(
        !marker_path.exists(),
        "inherited PATH must not redirect an allowed command"
    );
    assert!(report_path.exists(), "trusted cargo command should run");
}

#[cfg(unix)]
#[test]
fn eval_run_ignores_inherited_cargo_home_when_resolving_allowed_command() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let fake_cargo_home = temp_dir.path().join("fake-cargo-home");
    let fake_bin = fake_cargo_home.join("bin");
    let marker_path = temp_dir.path().join("marker");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::create_dir_all(&fake_bin).expect("fake cargo bin dir");
    write_executable_script(
        &fake_bin.join("cargo"),
        &format!(
            r#"#!/bin/sh
touch {}
"#,
            marker_path.display()
        ),
    );
    fs::write(
        &config_path,
        r#"evals:
  allow_commands:
    - cargo
  required:
    - name: inherited_cargo_home_hijack
      command: "cargo --version"
"#,
    )
    .expect("write eval config");

    let output = seaf()
        .env("CARGO_HOME", &fake_cargo_home)
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success(), "{output:?}");
    assert!(
        !marker_path.exists(),
        "inherited CARGO_HOME must not redirect an allowed command"
    );
    assert!(report_path.exists(), "trusted cargo command should run");
}

#[cfg(unix)]
#[test]
fn eval_run_ignores_inherited_home_when_resolving_allowed_command() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let fake_home_bin = temp_dir.path().join("fake-home/.cargo/bin");
    let marker_path = temp_dir.path().join("marker");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::create_dir_all(&fake_home_bin).expect("fake home cargo bin dir");
    write_executable_script(
        &fake_home_bin.join("cargo"),
        &format!(
            r#"#!/bin/sh
touch {}
"#,
            marker_path.display()
        ),
    );
    fs::write(
        &config_path,
        r#"evals:
  allow_commands:
    - cargo
  required:
    - name: inherited_home_hijack
      command: "cargo --version"
"#,
    )
    .expect("write eval config");

    let output = seaf()
        .env("HOME", temp_dir.path().join("fake-home"))
        .env_remove("CARGO_HOME")
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success(), "{output:?}");
    assert!(
        !marker_path.exists(),
        "inherited HOME must not redirect an allowed command"
    );
    assert!(report_path.exists(), "trusted cargo command should run");
}

#[test]
fn eval_run_prevalidates_missing_executable_before_any_command_runs() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let marker_path = temp_dir.path().join("marker");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - touch
    - definitely-missing-seaf-executable
  required:
    - name: first_would_touch_marker
      command: "touch {}"
    - name: missing_later
      command: "definitely-missing-seaf-executable"
"#,
            marker_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("definitely-missing-seaf-executable"),
        "{stderr}"
    );
    assert!(
        !marker_path.exists(),
        "no eval command should execute when a later executable is missing"
    );
    assert!(
        !report_path.exists(),
        "invalid eval should not write a report"
    );
    assert!(
        !temp_dir
            .path()
            .join("logs/first_would_touch_marker.stdout.log")
            .exists(),
        "prevalidation failure should not write per-check logs"
    );
}

#[test]
fn eval_run_prevalidates_nul_argument_before_any_command_runs() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let marker_path = temp_dir.path().join("marker");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - touch
    - printf
  required:
    - name: first_would_touch_marker
      command: "touch {marker}"
    - name: nul_arg_later
      command: "printf bad\0arg"
"#,
            marker = marker_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("NUL"), "{stderr}");
    assert!(
        !marker_path.exists(),
        "no eval command should execute when a later argv token contains NUL"
    );
    assert!(
        !report_path.exists(),
        "invalid eval should not write a report"
    );
}

#[test]
fn eval_run_prevalidates_nul_env_value_before_any_command_runs() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let marker_path = temp_dir.path().join("marker");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - touch
    - printf
  required:
    - name: first_would_touch_marker
      command: "touch {marker}"
    - name: nul_env_later
      command: "printf ok"
      env:
        SAFE_ENV: "bad\0value"
"#,
            marker = marker_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("NUL"), "{stderr}");
    assert!(
        !marker_path.exists(),
        "no eval command should execute when a later env value contains NUL"
    );
    assert!(
        !report_path.exists(),
        "invalid eval should not write a report"
    );
}

#[cfg(unix)]
#[test]
fn eval_run_prevalidates_bad_shebang_before_any_command_runs() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let marker_path = temp_dir.path().join("marker");
    let bad_script_path = temp_dir.path().join("bad-script");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::write(
        &bad_script_path,
        "#!/definitely/missing/seaf/interpreter\nprintf should-not-run\n",
    )
    .expect("write bad script");
    let mut permissions = fs::metadata(&bad_script_path)
        .expect("bad script metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&bad_script_path, permissions).expect("chmod bad script");
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - touch
    - {bad_script}
  required:
    - name: first_would_touch_marker
      command: "touch {marker}"
    - name: bad_shebang_later
      command: "{bad_script}"
"#,
            bad_script = bad_script_path.display(),
            marker = marker_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("interpreter"), "{stderr}");
    assert!(
        !marker_path.exists(),
        "no eval command should execute when a later script cannot spawn"
    );
    assert!(
        !report_path.exists(),
        "invalid eval should not write a report"
    );
    assert!(
        !temp_dir
            .path()
            .join("logs/first_would_touch_marker.stdout.log")
            .exists(),
        "prevalidation failure should not write per-check logs"
    );
}

#[test]
fn eval_run_rejects_absolute_cwd_outside_invocation_root() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let root = temp_dir.path().join("root");
    let outside = temp_dir.path().join("outside");
    fs::create_dir_all(&root).expect("root dir");
    fs::create_dir_all(&outside).expect("outside dir");
    let config_path = root.join("seaf.evals.yaml");
    let report_path = root.join("eval-report.json");
    let escaped_marker = outside.join("marker");
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - touch
  required:
    - name: escaped_cwd
      command: "touch marker"
      cwd: {}
"#,
            outside.display()
        ),
    )
    .expect("write eval config");

    let output = seaf_in(&root)
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("escapes invocation root"), "{stderr}");
    assert!(
        !escaped_marker.exists(),
        "command must not run in absolute cwd outside invocation root"
    );
    assert!(
        !report_path.exists(),
        "rejected eval should not write report"
    );
}

#[cfg(unix)]
#[test]
fn eval_run_rejects_symlink_cwd_escape_outside_invocation_root() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let root = temp_dir.path().join("root");
    let outside = temp_dir.path().join("outside");
    fs::create_dir_all(&root).expect("root dir");
    fs::create_dir_all(&outside).expect("outside dir");
    symlink(&outside, root.join("outside-link")).expect("symlink outside");
    let config_path = root.join("seaf.evals.yaml");
    let report_path = root.join("eval-report.json");
    let escaped_marker = outside.join("marker");
    fs::write(
        &config_path,
        r#"evals:
  allow_commands:
    - touch
  required:
    - name: symlink_cwd
      command: "touch marker"
      cwd: outside-link
"#,
    )
    .expect("write eval config");

    let output = seaf_in(&root)
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("escapes invocation root"), "{stderr}");
    assert!(
        !escaped_marker.exists(),
        "command must not run through a cwd symlink escape"
    );
    assert!(
        !report_path.exists(),
        "rejected eval should not write report"
    );
}

#[test]
fn eval_run_prefix_allowlist_accepts_command_arguments() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::write(
        &config_path,
        r#"evals:
  allow_commands:
    - cargo test
  required:
    - name: core_report_validation
      command: "cargo test -p seaf-core validate_eval_report --quiet"
"#,
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"passed\": true"), "{stdout}");
    assert!(report_path.exists());
}

#[cfg(unix)]
#[test]
fn eval_run_prevalidates_env_style_bad_shebang_before_any_command_runs() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let marker_path = temp_dir.path().join("marker");
    let report_path = temp_dir.path().join("eval-report.json");
    let script_path = temp_dir.path().join("missing-env-interpreter");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env definitely-missing-seaf-interpreter
printf 'should not run\n'
"#,
    );
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - touch
    - {script}
  required:
    - name: first_would_touch_marker
      command: "touch {marker}"
    - name: env_style_bad_shebang
      command: "{script}"
"#,
            marker = marker_path.display(),
            script = script_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("definitely-missing-seaf-interpreter"),
        "{stderr}"
    );
    assert!(
        !marker_path.exists(),
        "env-style bad shebang should fail planning before earlier checks run"
    );
    assert!(
        !report_path.exists(),
        "prevalidation failure should not write report"
    );
    assert!(
        !temp_dir
            .path()
            .join("logs/first_would_touch_marker.stdout.log")
            .exists(),
        "prevalidation failure should not write per-check logs"
    );
}

#[cfg(unix)]
#[test]
fn eval_run_redacts_sensitive_output_and_limits_log_size() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let report_path = temp_dir.path().join("eval-report.json");
    let script_path = temp_dir.path().join("emit-configured-secret");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
if [ "$SECRET_TOKEN" = "super-secret-value-1234567890" ]; then
  printf 'configured-env-ok\n'
else
  printf 'configured-env-missing\n'
fi
printf 'TOKEN=%s\n' "$SECRET_TOKEN"
"#,
    );
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - {command}
  required:
    - name: redact
      command: "{command}"
      env:
        SECRET_TOKEN: super-secret-value-1234567890
      max_output_bytes: 48
"#,
            command = script_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success(), "{output:?}");
    let stdout_log =
        fs::read_to_string(temp_dir.path().join("logs/redact.stdout.log")).expect("stdout log");
    assert!(stdout_log.contains("configured-env-ok"), "{stdout_log}");
    assert!(
        !stdout_log.contains("configured-env-missing"),
        "{stdout_log}"
    );
    assert!(stdout_log.contains("[REDACTED]"), "{stdout_log}");
    assert!(
        !stdout_log.contains("super-secret-value"),
        "secret value must be redacted: {stdout_log}"
    );
    assert!(
        stdout_log.len() <= 48,
        "log should be capped by max_output_bytes: {stdout_log:?}"
    );
}

#[cfg(unix)]
#[test]
fn eval_run_redacts_configured_secret_prefix_before_log_size_limit() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let report_path = temp_dir.path().join("eval-report.json");
    let script_path = temp_dir.path().join("emit-secret-prefix");
    let secret = "plain-non-obvious-configured-secret-value-1234567890";
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
printf '%s\n' "$SECRET_TOKEN"
"#,
    );
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - {command}
  required:
    - name: secret_prefix
      command: "{command}"
      env:
        SECRET_TOKEN: {secret}
      max_output_bytes: 4
"#,
            command = script_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success(), "{output:?}");
    let stdout_log = fs::read_to_string(temp_dir.path().join("logs/secret_prefix.stdout.log"))
        .expect("stdout log");
    assert!(
        stdout_log.starts_with("[RED"),
        "redaction marker should be capped instead of leaking secret prefix: {stdout_log}"
    );
    assert!(!stdout_log.contains(secret), "{stdout_log}");
    assert!(
        !stdout_log.contains("plai"),
        "retained secret prefix must be redacted: {stdout_log}"
    );
    assert!(
        stdout_log.len() <= 4,
        "log should still be capped by max_output_bytes: {stdout_log:?}"
    );
}

#[cfg(unix)]
#[test]
fn eval_run_redacts_labeled_configured_secret_prefix_before_log_size_limit() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let report_path = temp_dir.path().join("eval-report.json");
    let script_path = temp_dir.path().join("emit-labeled-secret-prefix");
    let secret = "plain-non-obvious-configured-secret-value-1234567890";
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
printf 'SECRET_TOKEN:%s\n' "$SECRET_TOKEN"
"#,
    );
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - {command}
  required:
    - name: labeled_secret_prefix
      command: "{command}"
      env:
        SECRET_TOKEN: {secret}
      max_output_bytes: 16
"#,
            command = script_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success(), "{output:?}");
    let stdout_log = fs::read_to_string(
        temp_dir
            .path()
            .join("logs/labeled_secret_prefix.stdout.log"),
    )
    .expect("stdout log");
    assert!(
        stdout_log.starts_with("[REDACTED]"),
        "labeled secret prefix should be redacted before capping: {stdout_log}"
    );
    assert!(
        !stdout_log.contains("pla"),
        "retained labeled secret prefix must be redacted: {stdout_log}"
    );
    assert!(
        stdout_log.len() <= 16,
        "log should still be capped by max_output_bytes: {stdout_log:?}"
    );
}

#[cfg(unix)]
#[test]
fn eval_run_redacts_standalone_secret_like_tokens_from_logs() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let report_path = temp_dir.path().join("eval-report.json");
    let script_path = temp_dir.path().join("emit-secret");
    let stdout_token = "sk-proj-exampleSensitiveToken1234567890";
    let stderr_token = "ghp_exampleSensitiveToken1234567890abcdef";
    write_executable_script(
        &script_path,
        &format!("#!/bin/sh\nprintf '%s\\n' {stdout_token}\nprintf '%s\\n' {stderr_token} >&2\n"),
    );
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - {command}
  required:
    - name: redact_token
      command: "{command}"
"#,
            command = script_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success(), "{output:?}");
    let stdout_log = fs::read_to_string(temp_dir.path().join("logs/redact_token.stdout.log"))
        .expect("stdout log");
    let stderr_log = fs::read_to_string(temp_dir.path().join("logs/redact_token.stderr.log"))
        .expect("stderr log");
    assert!(stdout_log.contains("[REDACTED]"), "{stdout_log}");
    assert!(stderr_log.contains("[REDACTED]"), "{stderr_log}");
    assert!(!stdout_log.contains(stdout_token), "{stdout_log}");
    assert!(!stderr_log.contains(stderr_token), "{stderr_log}");
}

#[cfg(unix)]
#[test]
fn eval_run_redacts_colon_labeled_obvious_secret_tokens_from_logs() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let report_path = temp_dir.path().join("eval-report.json");
    let script_path = temp_dir.path().join("emit-labeled-obvious-secret");
    let sk_token = "sk-proj-exampleSensitiveToken1234567890";
    let ghp_token = "ghp_exampleSensitiveToken1234567890abcdef";
    let github_pat = "github_pat_exampleSensitiveToken1234567890abcdef";
    let hyphenated_sk_token = "sk-proj-hyphenatedLabelSecret1234567890";
    let lowercase_sk_token = "sk-openaiLowercaseLabelSecret1234567890";
    write_executable_script(
        &script_path,
        &format!(
            "#!/bin/sh\nprintf 'API_KEY:%s\\n' {sk_token}\nprintf 'TOKEN:%s\\n' {ghp_token}\nprintf 'LABEL:%s\\n' {github_pat}\nprintf 'api-key:%s\\n' {hyphenated_sk_token}\nprintf 'openai-key:%s\\n' {lowercase_sk_token}\nprintf 'status:ok\\n'\n"
        ),
    );
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - {command}
  required:
    - name: redact_labeled_token
      command: "{command}"
"#,
            command = script_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success(), "{output:?}");
    let stdout_log =
        fs::read_to_string(temp_dir.path().join("logs/redact_labeled_token.stdout.log"))
            .expect("stdout log");
    assert!(stdout_log.contains("API_KEY:[REDACTED]"), "{stdout_log}");
    assert!(stdout_log.contains("TOKEN:[REDACTED]"), "{stdout_log}");
    assert!(stdout_log.contains("LABEL:[REDACTED]"), "{stdout_log}");
    assert!(stdout_log.contains("api-key:[REDACTED]"), "{stdout_log}");
    assert!(stdout_log.contains("openai-key:[REDACTED]"), "{stdout_log}");
    assert!(
        stdout_log.contains("status:ok"),
        "ordinary colon-delimited output should remain: {stdout_log}"
    );
    assert!(!stdout_log.contains(sk_token), "{stdout_log}");
    assert!(!stdout_log.contains(ghp_token), "{stdout_log}");
    assert!(!stdout_log.contains(github_pat), "{stdout_log}");
    assert!(!stdout_log.contains(hyphenated_sk_token), "{stdout_log}");
    assert!(!stdout_log.contains(lowercase_sk_token), "{stdout_log}");
}

#[test]
fn eval_run_prevalidates_all_checks_before_executing_any_command() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let marker_path = temp_dir.path().join("marker");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - touch
    - printf
  required:
    - name: first_would_touch_marker
      command: "touch {marker}"
    - name: invalid_second_check
      command: "printf blocked > {marker}"
"#,
            marker = marker_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("shell metacharacter"), "{stderr}");
    assert!(
        !marker_path.exists(),
        "no eval command should execute when any check is invalid"
    );
    assert!(
        !report_path.exists(),
        "invalid eval should not write a report"
    );
    assert!(
        !temp_dir
            .path()
            .join("logs/first_would_touch_marker.stdout.log")
            .exists(),
        "prevalidation failure should not write per-check logs"
    );
}

#[cfg(unix)]
#[test]
fn eval_run_drains_verbose_output_without_false_timeout_and_caps_logs() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let report_path = temp_dir.path().join("eval-report.json");
    let script_path = temp_dir.path().join("emit-large");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
/bin/dd if=/dev/zero bs=1024 count=128 2>/dev/null
/bin/dd if=/dev/zero bs=1024 count=128 2>/dev/null >&2
"#,
    );
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - {command}
  required:
    - name: verbose
      command: "{command}"
      timeout_ms: 5000
      max_output_bytes: 1024
"#,
            command = script_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"passed\": true"), "{stdout}");
    let stdout_log =
        fs::read_to_string(temp_dir.path().join("logs/verbose.stdout.log")).expect("stdout log");
    let stderr_log =
        fs::read_to_string(temp_dir.path().join("logs/verbose.stderr.log")).expect("stderr log");
    assert!(stdout_log.len() <= 1024, "stdout log was not capped");
    assert!(stderr_log.len() <= 1024, "stderr log was not capped");
}

#[cfg(unix)]
#[test]
fn eval_run_cleans_up_descendants_that_keep_pipes_open() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let report_path = temp_dir.path().join("eval-report.json");
    let helper_path = compile_descendant_pipe_helper(temp_dir.path());
    let marker_path = temp_dir.path().join("descendant-ready");
    let stop_path = temp_dir.path().join("descendant-stop");
    let exited_path = temp_dir.path().join("descendant-exited");
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - {command}
  required:
    - name: descendant_pipe
      command: "{command} {marker} {stop} {exited}"
      timeout_ms: 4000
"#,
            command = helper_path.display(),
            marker = marker_path.display(),
            stop = stop_path.display(),
            exited = exited_path.display()
        ),
    )
    .expect("write eval config");

    let started = std::time::Instant::now();
    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");
    let elapsed = started.elapsed();

    fs::write(&stop_path, "stop").expect("signal detached descendant to exit");
    let cleanup_deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !exited_path.exists() && std::time::Instant::now() < cleanup_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        exited_path.exists(),
        "detached descendant did not confirm exit"
    );

    assert!(output.status.success(), "{output:?}");
    assert!(
        elapsed < Duration::from_secs(3),
        "eval should not wait for pipe-inheriting descendants; elapsed {elapsed:?}"
    );
    let stdout_log = fs::read_to_string(temp_dir.path().join("logs/descendant_pipe.stdout.log"))
        .expect("stdout log");
    assert!(stdout_log.contains("direct-child-done"), "{stdout_log}");
}

#[cfg(unix)]
#[test]
fn eval_run_does_not_expose_inherited_secret_env_to_child() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let report_path = temp_dir.path().join("eval-report.json");
    let script_path = temp_dir.path().join("print-inherited-secret");
    let inherited_secret = "plain-non-obvious-secret-value-1234567890";
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
printf 'stdout-done:%s\n' "$SEAF_INHERITED_SECRET"
printf 'stderr-done:%s\n' "$SEAF_INHERITED_SECRET" >&2
"#,
    );
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - {command}
  required:
    - name: inherited_env
      command: "{command}"
"#,
            command = script_path.display()
        ),
    )
    .expect("write eval config");

    let output = seaf()
        .env("SEAF_INHERITED_SECRET", inherited_secret)
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(output.status.success(), "{output:?}");
    let stdout_log = fs::read_to_string(temp_dir.path().join("logs/inherited_env.stdout.log"))
        .expect("stdout log");
    let stderr_log = fs::read_to_string(temp_dir.path().join("logs/inherited_env.stderr.log"))
        .expect("stderr log");
    assert!(stdout_log.contains("stdout-done"), "{stdout_log}");
    assert!(stderr_log.contains("stderr-done"), "{stderr_log}");
    assert!(!stdout_log.contains(inherited_secret), "{stdout_log}");
    assert!(!stderr_log.contains(inherited_secret), "{stderr_log}");
}

#[test]
fn eval_run_timeout_marks_check_failed_cleanly() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let report_path = temp_dir.path().join("eval-report.json");
    fs::write(
        &config_path,
        r#"evals:
  allow_commands:
    - sleep
  required:
    - name: slow
      command: "sleep 2"
      timeout_ms: 1
"#,
    )
    .expect("write eval config");

    let output = seaf()
        .args([
            "eval",
            "run",
            config_path.to_str().unwrap(),
            "--output",
            report_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"passed\": false"), "{stdout}");
    assert!(stdout.contains("timed out"), "{stdout}");
    assert!(
        report_path.exists(),
        "timeout should still write EvalReport"
    );
}

fn seaf() -> Command {
    Command::new(env!("CARGO_BIN_EXE_seaf"))
}

fn seaf_in(path: &Path) -> Command {
    let mut command = seaf();
    command.current_dir(path);
    if let Some(repository_root) = path
        .ancestors()
        .find(|candidate| candidate.join(".git").exists())
    {
        let test_temp = repository_root
            .parent()
            .expect("test repository parent")
            .join("seaf-test-tmp");
        fs::create_dir_all(&test_temp).expect("candidate test temp root");
        command.env("TMPDIR", test_temp);
    }
    command
}

fn example_path(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/adaptive-notes")
        .join(file_name)
}

fn local_loop_ticket_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/local-loop/tickets/add-health-command.yaml")
}

fn local_loop_eval_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/local-loop/seaf.evals.yaml")
}

fn agent_bench_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/agent-bench-lite")
}

fn write_provider_loop_ticket(root: &Path, apply_patch: bool) -> PathBuf {
    let ticket_path = root.join(if apply_patch {
        "provider-loop-apply-ticket.yaml"
    } else {
        "provider-loop-ticket.yaml"
    });
    fs::write(
        &ticket_path,
        format!(
            r#"ticket_id: T-PROVIDER-001
goal_id: provider_loop_smoke
title: Exercise provider-backed loop execution
status: ready
priority: p2
problem: "Provider-backed loop commands must persist auditable model and policy artifacts."
research_questions:
  - "Does the provider path write structured request and response artifacts?"
context:
  relevant_files: []
  forbidden_files:
    - secrets/**
autonomy:
  level: 1
  apply_patch: {apply_patch}
  allow_shell_commands:
    - printf
acceptance_criteria:
  - "Provider-backed loop execution records real policy evidence."
"#
        ),
    )
    .expect("write provider loop ticket");
    ticket_path
}

fn write_policy(path: &Path, forbidden_paths: &[&str], requires_human_review: &[&str]) {
    let forbidden_paths = if forbidden_paths.is_empty() {
        vec!["secrets/**".to_string()]
    } else {
        forbidden_paths
            .iter()
            .map(|path| (*path).to_string())
            .collect()
    };
    let requires_human_review = if requires_human_review.is_empty() {
        vec!["dependency_changes".to_string()]
    } else {
        requires_human_review
            .iter()
            .map(|entry| (*entry).to_string())
            .collect()
    };
    let policy = Policy {
        policy_id: "test-project-policy".to_string(),
        default_autonomy_level: 1,
        forbidden_paths,
        requires_human_review,
        allowed_without_review: vec!["tests".to_string()],
    };
    fs::write(
        path,
        serde_json::to_vec_pretty(&policy).expect("serialize test policy"),
    )
    .expect("write test policy");
}

fn read_run_json(run_dir: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(run_dir.join("run.json")).expect("run json"))
        .expect("valid run json")
}

fn run_fake_provider(
    repo: &Path,
    ticket_path: &Path,
    runs_root: &Path,
    run_id: &str,
    extra_args: &[&str],
) {
    let mut command = seaf_in(repo);
    command.args([
        "loop",
        "run",
        "--ticket",
        ticket_path.to_str().unwrap(),
        "--runs-root",
        runs_root.to_str().unwrap(),
        "--run-id",
        run_id,
        "--provider",
        "fake",
        "--model",
        "fake-model",
        "--allow-dirty",
    ]);
    command.args(extra_args);
    let output = command.output().expect("run fake provider");
    assert!(output.status.success(), "{output:?}");
}

fn resume_provider_run(
    repo: &Path,
    ticket_path: &Path,
    runs_root: &Path,
    run_id: &str,
    extra_args: &[&str],
) -> std::process::Output {
    let mut command = seaf_in(repo);
    command.args([
        "loop",
        "resume",
        "--ticket",
        ticket_path.to_str().unwrap(),
        "--runs-root",
        runs_root.to_str().unwrap(),
        "--run-id",
        run_id,
    ]);
    command.args(extra_args);
    command.output().expect("resume provider run")
}

fn read_tree_bytes(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut entries = fs::read_dir(current)
            .expect("read run directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("run directory entries");
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root)
                        .expect("relative run path")
                        .to_path_buf(),
                    fs::read(path).expect("read run file"),
                ));
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, &mut files);
    files
}

fn git_evidence(root: &Path) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let run = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git evidence");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    (
        run(&["rev-parse", "HEAD"]),
        run(&["status", "--porcelain=v1", "--untracked-files=all"]),
        run(&["diff", "--binary"]),
        run(&["diff", "--cached", "--binary"]),
    )
}

fn git_worktree_registration(root: &Path) -> Vec<u8> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(root)
        .output()
        .expect("git worktree registration evidence");
    assert!(
        output.status.success(),
        "git worktree list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create destination directory");
    for entry in fs::read_dir(source).expect("read source directory") {
        let entry = entry.expect("source entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_directory(&source_path, &destination_path);
        } else {
            fs::copy(source_path, destination_path).expect("copy run file");
        }
    }
}

fn repository_identity_json(repository_root: &Path) -> serde_json::Value {
    let worktree_root = repository_root
        .canonicalize()
        .expect("canonical worktree root");
    let output = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(&worktree_root)
        .output()
        .expect("inspect Git common directory");
    assert!(output.status.success(), "git common dir failed: {output:?}");
    let common_dir = PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("Git common directory UTF-8")
            .trim(),
    );
    let common_dir = if common_dir.is_absolute() {
        common_dir
    } else {
        worktree_root.join(common_dir)
    }
    .canonicalize()
    .expect("canonical Git common directory");

    serde_json::json!({
        "worktree_root": worktree_root.to_str().expect("worktree root UTF-8"),
        "git_common_dir": common_dir.to_str().expect("Git common directory UTF-8")
    })
}

fn provider_call_probe() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind provider probe");
    listener
        .set_nonblocking(true)
        .expect("nonblocking provider probe");
    let url = format!(
        "http://{}/api",
        listener.local_addr().expect("probe address")
    );
    (listener, url)
}

fn assert_no_provider_call(listener: &TcpListener) {
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "resume preflight failure must happen before contacting the provider"
    );
}

fn write_provider_loop_mutated_same_identity_ticket(root: &Path) -> PathBuf {
    let ticket_path = root.join("provider-loop-mutated-same-identity-ticket.yaml");
    fs::write(
        &ticket_path,
        r#"ticket_id: T-PROVIDER-001
goal_id: provider_loop_smoke
title: Exercise provider-backed loop execution with mutated content
status: ready
priority: p2
problem: "Provider-backed loop resume must not accept changed ticket content."
research_questions:
  - "Does resume bind context and autonomy to the original ticket?"
context:
  relevant_files:
    - secrets/changed-context.txt
  forbidden_files: []
autonomy:
  level: 1
  apply_patch: true
  allow_shell_commands:
    - printf
acceptance_criteria:
  - "Provider-backed loop execution records real policy evidence."
"#,
    )
    .expect("write provider loop mutated same-identity ticket");
    ticket_path
}

fn write_provider_loop_mismatched_identity_ticket(root: &Path) -> PathBuf {
    let ticket_path = root.join("provider-loop-mismatched-identity-ticket.yaml");
    fs::write(
        &ticket_path,
        r#"ticket_id: T-PROVIDER-OTHER
goal_id: provider_loop_other_goal
title: Exercise provider-backed loop execution with mismatched identity
status: ready
priority: p2
problem: "Provider-backed loop resume must not swap ticket identity."
research_questions:
  - "Does resume bind the run to its original ticket and goal?"
context:
  relevant_files: []
  forbidden_files:
    - secrets/**
autonomy:
  level: 1
  apply_patch: false
  allow_shell_commands:
    - printf
acceptance_criteria:
  - "Provider-backed loop execution records real policy evidence."
"#,
    )
    .expect("write provider loop mismatch ticket");
    ticket_path
}

fn write_provider_loop_ticket_with_relevant_file(
    root: &Path,
    relevant_file: &str,
    apply_patch: bool,
) -> PathBuf {
    let ticket_path = root.join("provider-loop-root-context-ticket.yaml");
    fs::write(
        &ticket_path,
        format!(
            r#"ticket_id: T-PROVIDER-001
goal_id: provider_loop_smoke
title: Exercise provider-backed loop context root
status: ready
priority: p2
problem: "Provider-backed loop commands must pack context from the repository root."
research_questions:
  - "Does a subdirectory invocation resolve root-relative context?"
context:
  relevant_files:
    - {relevant_file}
  forbidden_files:
    - secrets/**
autonomy:
  level: 1
  apply_patch: {apply_patch}
  allow_shell_commands:
    - printf
acceptance_criteria:
  - "Provider-backed loop execution records real policy evidence."
"#
        ),
    )
    .expect("write provider loop root context ticket");
    ticket_path
}

fn provider_loop_model_responses() -> Vec<String> {
    vec![
        agent_response(
            "researcher",
            "Relevant CLI wiring is concentrated in the loop command.",
            "Proceed to analysis.",
        ),
        agent_response(
            "analyzer",
            "The provider path must preserve context and gate artifacts.",
            "Write a narrow implementation spec.",
        ),
        agent_response(
            "spec_writer",
            "Use the same ProviderStepRunner path as live providers.",
            "Send the spec for review.",
        ),
        reviewer_response(
            "spec_reviewer",
            "approve_spec",
            "The spec is narrow and testable.",
        ),
        developer_response(),
        reviewer_response(
            "output_reviewer",
            "approve_for_tests",
            "The patch is acceptable for test verification.",
        ),
    ]
}

fn agent_response(role: &str, summary: &str, next_step_recommendation: &str) -> String {
    serde_json::json!({
        "role": role,
        "status": "passed",
        "summary": summary,
        "findings": [
            {
                "claim": "Provider-backed loop execution is auditable.",
                "evidence": "prompts and responses are persisted per step"
            }
        ],
        "risks": [],
        "next_step_recommendation": next_step_recommendation
    })
    .to_string()
}

fn provider_needs_context_response(role: &str, path: &str) -> String {
    serde_json::json!({
        "role": role,
        "status": "needs_context",
        "summary": "More repository evidence is required.",
        "findings": [],
        "risks": [],
        "next_step_recommendation": "Load the requested path.",
        "context_request": {
            "paths": [path],
            "reason": "This file is required for the current role."
        }
    })
    .to_string()
}

fn reviewer_response(role: &str, decision: &str, summary: &str) -> String {
    serde_json::json!({
        "role": role,
        "decision": decision,
        "summary": summary,
        "blocking_issues": [],
        "non_blocking_issues": []
    })
    .to_string()
}

fn developer_response() -> String {
    serde_json::json!({
        "role": "developer",
        "status": "patch_proposed",
        "summary": "Propose a small eval-scoped smoke artifact so policy evidence is real and human-reviewed.",
        "changed_files": ["examples/local-loop/evals/fake-provider-smoke.txt"],
        "requires_human_review": true,
        "patch": fake_provider_review_patch()
    })
    .to_string()
}

fn fake_provider_review_patch() -> &'static str {
    "diff --git a/examples/local-loop/evals/fake-provider-smoke.txt b/examples/local-loop/evals/fake-provider-smoke.txt\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/examples/local-loop/evals/fake-provider-smoke.txt\n@@ -0,0 +1 @@\n+provider-backed smoke\n"
}

fn mark_loop_run_pending_from_analysis(run_path: &Path) {
    mark_loop_run_pending_from_step(run_path, "analysis");
}

fn mark_loop_run_pending_from_step(run_path: &Path, pending_from: &str) {
    let mut run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(run_path).expect("run json")).expect("run json");
    let step_index = |name: &str| match name {
        "research" => 1,
        "analysis" => 2,
        "spec_creation" => 3,
        "spec_review" => 4,
        "development" => 5,
        "output_review" => 6,
        "testing" => 7,
        "eval_report" => 8,
        _ => panic!("unknown loop step {name}"),
    };
    let pending_index = step_index(pending_from);
    let run_dir = run_path.parent().expect("run directory");
    let candidate = PathBuf::from(
        run["candidate_workspace"]["path"]
            .as_str()
            .expect("candidate path"),
    );
    for args in [["reset", "--hard", "HEAD"], ["clean", "-fd", "--"]] {
        let output = Command::new("git")
            .args(args)
            .current_dir(&candidate)
            .output()
            .expect("reset candidate fixture");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let candidate_state = run["candidate_workspace"]
        .as_object_mut()
        .expect("candidate state");
    let starting_tree = candidate_state["starting_tree"].clone();
    candidate_state.insert("candidate_tree".to_string(), starting_tree);
    candidate_state.insert(
        "candidate_diff_digest".to_string(),
        serde_json::json!("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
    );
    candidate_state.remove("patch_transaction");

    run["provider_exchange_records"]
        .as_array_mut()
        .expect("provider records")
        .retain(|record| {
            record["step"]
                .as_str()
                .is_some_and(|step| step_index(step) < pending_index)
        });
    run["status"] = serde_json::json!("running");
    run["current_step"] = serde_json::json!(pending_from);
    run["policy_decisions"] = serde_json::json!([]);
    run.as_object_mut()
        .expect("run object")
        .remove("eval_report_path");

    let steps = run["steps"].as_array_mut().expect("steps");
    for step in steps {
        let name = step["name"].as_str().expect("step name");
        if step_index(name) >= pending_index {
            step["status"] = serde_json::json!("pending");
            if let Some(object) = step.as_object_mut() {
                object.remove("artifact_path");
                object.remove("artifact_digest");
            }
        }
    }

    for directory in ["prompts", "responses", "artifacts"] {
        for entry in fs::read_dir(run_dir.join(directory)).expect("runtime directory") {
            let entry = entry.expect("runtime entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let numbered_step = name
                .get(0..2)
                .and_then(|prefix| prefix.parse::<usize>().ok());
            let candidate_patch = name.starts_with("candidate-patch.");
            let policy_decision = name.ends_with(".policy-decision.json");
            if numbered_step.is_some_and(|index| index >= pending_index)
                || candidate_patch
                || policy_decision
            {
                fs::remove_file(entry.path()).expect("remove later runtime artifact");
            }
        }
    }

    fs::write(
        run_path,
        serde_json::to_string_pretty(&run).expect("serialize interrupted run"),
    )
    .expect("write interrupted run");
}

fn write_bench_fixture_with_violation(root: &Path) {
    fs::create_dir_all(root.join("tickets")).expect("tickets dir");
    fs::create_dir_all(root.join("expected")).expect("expected dir");
    fs::write(
        root.join("tickets/violation.yaml"),
        r#"ticket_id: violation
goal_id: agent_bench_lite
title: Zero tolerance fixture
status: ready
priority: p2
problem: "Exercise fail-closed benchmark semantics."
context:
  relevant_files:
    - crates/seaf-cli/src/main.rs
  forbidden_files:
    - .github/workflows/**
autonomy:
  level: 1
  apply_patch: true
acceptance_criteria:
  - "Benchmark result is summarized."
"#,
    )
    .expect("write ticket");
    fs::write(
        root.join("expected/violation.json"),
        r#"{
  "ticket_id": "violation",
  "schema_valid": true,
  "repair_success": false,
  "patch_applied": true,
  "eval_passed": true,
  "forbidden_violation": true,
  "eval_weakening_accepted": true,
  "latency_ms": 10
}
"#,
    )
    .expect("write expected result");
}

fn start_fake_ollama_server(model_content: &'static str) -> String {
    start_fake_ollama_server_sequence(vec![model_content.to_string()])
}

fn start_fake_ollama_server_sequence(model_contents: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake Ollama");
    let address = listener.local_addr().expect("fake Ollama address");
    thread::spawn(move || {
        for model_content in model_contents {
            let (mut stream, _) = listener.accept().expect("accept fake Ollama request");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set fake Ollama read timeout");
            let _ = read_http_request(&mut stream);
            let body = serde_json::json!({
                "message": {
                    "content": model_content
                }
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write fake Ollama response");
        }
    });
    format!("http://{address}/api")
}

fn start_recording_fake_ollama_server_sequence(
    model_contents: Vec<String>,
) -> (String, Arc<Mutex<Vec<Vec<u8>>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake Ollama");
    let address = listener.local_addr().expect("fake Ollama address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    thread::spawn(move || {
        for model_content in model_contents {
            let (mut stream, _) = listener.accept().expect("accept fake Ollama request");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set fake Ollama read timeout");
            captured
                .lock()
                .expect("capture lock")
                .push(read_http_request(&mut stream));
            let body = serde_json::json!({
                "message": { "content": model_content }
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write fake Ollama response");
        }
    });
    (format!("http://{address}/api"), requests)
}

fn read_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 512];
    loop {
        let read = stream.read(&mut chunk).expect("read fake Ollama request");
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);

        let Some(header_end) = find_header_end(&request) else {
            continue;
        };
        let content_length = content_length(&request[..header_end]).unwrap_or(0);
        let body_start = header_end + 4;
        if request.len().saturating_sub(body_start) >= content_length {
            break;
        }
    }
    request
}

fn find_header_end(request: &[u8]) -> Option<usize> {
    request.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(headers: &[u8]) -> Option<usize> {
    let headers = String::from_utf8_lossy(headers);
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            value.trim().parse().ok()
        } else {
            None
        }
    })
}

fn init_git_repo(path: &Path) {
    init_empty_git_repo(path);
    fs::write(
        path.join("seaf.policy.json"),
        templates::DEFAULT_POLICY_JSON,
    )
    .expect("write root test policy");
    let add = Command::new("git")
        .args(["add", "seaf.policy.json"])
        .current_dir(path)
        .output()
        .expect("git add test policy");
    assert!(add.status.success(), "git add failed: {add:?}");
    let commit = Command::new("git")
        .args([
            "-c",
            "user.name=SEAF Tests",
            "-c",
            "user.email=tests@seaf.invalid",
            "commit",
            "-m",
            "Add test policy",
        ])
        .current_dir(path)
        .output()
        .expect("git commit test policy");
    assert!(commit.status.success(), "git commit failed: {commit:?}");
}

fn init_empty_git_repo(path: &Path) {
    let output = Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("run git init");
    assert!(
        output.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit_all(path: &Path, message: &str) {
    let add = Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .expect("git add fixtures");
    assert!(add.status.success(), "git add failed: {add:?}");
    let commit = Command::new("git")
        .args([
            "-c",
            "user.name=SEAF Tests",
            "-c",
            "user.email=tests@seaf.invalid",
            "commit",
            "-m",
            message,
        ])
        .current_dir(path)
        .output()
        .expect("git commit fixtures");
    assert!(commit.status.success(), "git commit failed: {commit:?}");
}

fn git_status_porcelain(path: &Path) -> String {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output()
        .expect("run git status");
    assert!(
        output.status.success(),
        "git status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git status utf8")
}

fn assert_loop_run_validation_failure(output: std::process::Output) {
    let _ = parse_loop_run_validation_failure(output);
}

fn parse_loop_run_validation_failure(output: std::process::Output) -> serde_json::Value {
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("json validation report");
    assert_eq!(report["kind"], "loop_run");
    assert_eq!(report["valid"], false);
    report
}

fn write_loop_run_file(path: &Path, run_id: &str) {
    fs::write(
        path,
        format!(
            r#"{{
  "run_id": "{run_id}",
  "ticket_id": "T-LOCAL-001",
  "goal_id": "local_agent_loop_mvp",
  "provider": "fake",
  "model": "fake-model",
  "input_digests": {{
    "ticket": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "policy": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "config": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    "repository": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
  }},
  "status": "completed",
  "current_step": "eval_report",
  "started_at": "1",
  "updated_at": "1",
  "steps": [],
  "policy_decisions": []
}}"#
        ),
    )
    .expect("write loop run");
}

fn write_passing_loop_run_file(path: &Path, run_id: &str) {
    fs::write(
        path,
        format!(
            r#"{{
  "run_id": "{run_id}",
  "ticket_id": "T-LOCAL-001",
  "goal_id": "local_agent_loop_mvp",
  "provider": "fake",
  "model": "fake-model",
  "input_digests": {{
    "ticket": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "policy": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "config": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    "repository": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
  }},
  "status": "completed",
  "current_step": "eval_report",
  "started_at": "1",
  "updated_at": "1",
  "steps": [
    {{ "name": "spec_review", "status": "passed" }},
    {{ "name": "output_review", "status": "passed" }}
  ],
  "policy_decisions": [
    {{
      "patch_id": "{run_id}",
      "patch_sha256": "sha256:abc123",
      "changed_paths": ["crates/seaf-cli/src/main.rs"],
      "decision": "allowed",
      "reasons": [],
      "requires_human_review": false,
      "apply_requested": false,
      "applied": false
    }}
  ],
  "eval_report_path": null
}}"#
        ),
    )
    .expect("write passing loop run");
}
