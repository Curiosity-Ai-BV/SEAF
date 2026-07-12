use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use seaf_core::{canonical_json_bytes, canonical_sha256_digest, templates, Policy, ProjectConfig};
use sha2::{Digest, Sha256};

#[cfg(unix)]
use std::os::unix::fs::{symlink, DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

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
    assert!(stdout.contains("\"status\": \"awaiting_human_review\""));
    assert!(stdout.contains("human approval is required"));
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

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"run_id\": \"stable_123-run\""));
    let run_dir = runs_root.join("stable_123-run");
    let persisted = read_run_json(&run_dir);
    assert_eq!(persisted["policy_decisions"].as_array().unwrap().len(), 1);
    assert_eq!(
        persisted["policy_decisions"][0]["patch_id"],
        "stable_123-run"
    );
    assert!(
        run_dir.join("provider-exchange.lock").is_file(),
        "deterministic policy evidence must use the shared durable run-state lock"
    );
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
    assert!(stdout.contains("\"status\": \"awaiting_human_review\""));
    assert!(stdout.contains("human approval is required"));
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
    assert_eq!(persisted_run["status"], "awaiting_human_review");
    assert_eq!(persisted_run["current_step"], "testing");
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
    for absent in [
        "prompts/07-testing.prompt.md",
        "responses/07-testing.raw.txt",
        "artifacts/07-testing.md",
        "prompts/08-eval-report.prompt.md",
        "responses/08-eval-report.raw.txt",
        "artifacts/08-eval-report.md",
    ] {
        assert!(!run_dir.join(absent).exists(), "{absent} must not exist");
    }

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
    assert!(stdout.contains("\"status\": \"awaiting_human_review\""));
    assert!(stdout.contains("human approval is required"));

    let human_status = seaf()
        .args([
            "loop",
            "status",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
        ])
        .output()
        .expect("run human loop status");
    assert!(human_status.status.success());
    let human_stdout = String::from_utf8(human_status.stdout).unwrap();
    assert!(human_stdout.contains("status awaiting_human_review"));
    assert!(human_stdout.contains("human approval is required; Testing has not run"));

    let before_resume = read_tree_bytes(&run_dir);
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
    assert!(resume.status.success(), "{resume:?}");
    let stdout = String::from_utf8(resume.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"command\": \"resume\""));
    assert!(stdout.contains("\"status\": \"awaiting_human_review\""));
    assert!(stdout.contains("human approval is required"));
    assert_eq!(read_tree_bytes(&run_dir), before_resume);

    let rerun = seaf()
        .args([
            "loop",
            "resume",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--rerun-from",
            "output-review",
            "--json",
        ])
        .output()
        .expect("reject unaudited rerun");
    assert!(!rerun.status.success());
    let stderr = String::from_utf8(rerun.stderr).unwrap();
    assert!(stderr.contains("--rerun-from is retired"), "{stderr}");
    assert_eq!(read_tree_bytes(&run_dir), before_resume);
}

#[test]
fn loop_approve_requires_exact_confirmation_and_is_a_byte_identical_retry() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = temp_dir.path().join("runs");
    let run_id = "cli-exact-approval";
    let intent_path = runs_root
        .join(run_id)
        .join("artifacts/07-testing.attempt-001.execution-intent.json");
    let lock_probe = repo.join("provider-lock-check.pl");
    fs::write(
        &lock_probe,
        "#!/usr/bin/perl\nuse Fcntl qw(:flock);\nopen(my $lock, '+<', $ARGV[0]) or die $!;\nflock($lock, LOCK_EX | LOCK_NB) or exit 42;\n",
    )
    .expect("provider lock probe");
    let mut lock_probe_permissions = fs::metadata(&lock_probe).unwrap().permissions();
    lock_probe_permissions.set_mode(0o755);
    fs::set_permissions(&lock_probe, lock_probe_permissions).unwrap();
    fs::write(
        repo.join("seaf.evals.yaml"),
        format!(
            "evals:\n  allow_commands: [test, ./provider-lock-check.pl]\n  required:\n    - name: intent_precedes_execution\n      command: test -f {}\n    - name: provider_lock_is_not_held_during_execution\n      command: ./provider-lock-check.pl {}\n",
            intent_path.display(),
            runs_root.join(run_id).join("provider-exchange.lock").display()
        ),
    )
    .expect("write approved eval config");
    commit_all(&repo, "Use bounded approved eval");
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let ticket = fs::read_to_string(&ticket_path)
        .expect("ticket")
        .replace("    - printf", "    - test\n    - ./provider-lock-check.pl");
    fs::write(&ticket_path, ticket).expect("ticket allowlist");
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
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let run_dir = runs_root.join(run_id);
    let public_run: serde_json::Value =
        serde_json::from_slice(&run.stdout).expect("public run report JSON");
    let diff = public_run["candidate_diff_digest"]
        .as_str()
        .unwrap()
        .to_string();
    let head = public_run["target_head"].as_str().unwrap().to_string();
    let public_status = seaf_in(&repo)
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
        .expect("public approval confirmation status");
    assert!(public_status.status.success());
    let public_status: serde_json::Value =
        serde_json::from_slice(&public_status.stdout).expect("public status JSON");
    assert_eq!(public_status["candidate_diff_digest"], diff);
    assert_eq!(public_status["target_head"], head);
    let human_status = seaf_in(&repo)
        .args([
            "loop",
            "status",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
        ])
        .output()
        .expect("human approval confirmation status");
    let human_status = String::from_utf8(human_status.stdout).unwrap();
    assert!(human_status.contains(&format!("--confirm-candidate-diff: {diff}")));
    assert!(human_status.contains(&format!("--confirm-target-head: {head}")));

    let before_rejected = read_tree_bytes(&run_dir);
    let rejected = seaf_in(&repo)
        .args([
            "loop",
            "approve",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "reviewer@example.invalid",
            "--confirm-candidate-diff",
            &"f".repeat(64),
            "--confirm-target-head",
            &head,
            "--json",
        ])
        .output()
        .expect("reject wrong confirmation");
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("diff digest"));
    assert_eq!(read_tree_bytes(&run_dir), before_rejected);
    let wrong_head = seaf_in(&repo)
        .args([
            "loop",
            "approve",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "reviewer@example.invalid",
            "--confirm-candidate-diff",
            &diff,
            "--confirm-target-head",
            "0000000000000000000000000000000000000000",
        ])
        .output()
        .expect("reject wrong target HEAD");
    assert!(!wrong_head.status.success());
    assert!(String::from_utf8_lossy(&wrong_head.stderr).contains("target HEAD"));
    assert_eq!(read_tree_bytes(&run_dir), before_rejected);
    let unsafe_reviewer = seaf_in(&repo)
        .args([
            "loop",
            "approve",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "unsafe\nreviewer",
            "--confirm-candidate-diff",
            &diff,
            "--confirm-target-head",
            &head,
        ])
        .output()
        .expect("reject unsafe reviewer identity");
    assert!(!unsafe_reviewer.status.success());
    assert!(String::from_utf8_lossy(&unsafe_reviewer.stderr).contains("control characters"));
    assert_eq!(read_tree_bytes(&run_dir), before_rejected);

    fs::write(repo.join("tracked.txt"), "dirty but preserved\n").expect("dirty tracked file");
    fs::write(repo.join("untracked.txt"), "also preserved\n").expect("dirty untracked file");
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [false]\n  required:\n    - name: live_mutation_must_be_ignored\n      command: false\n",
    )
    .expect("mutate live eval after snapshot");
    fs::write(
        &ticket_path,
        "live ticket bytes are no longer authoritative\n",
    )
    .expect("mutate live ticket after snapshot");
    let source_before = git_evidence(&repo);
    let approved = seaf_in(&repo)
        .args([
            "loop",
            "approve",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "reviewer@example.invalid",
            "--confirm-candidate-diff",
            &diff,
            "--confirm-target-head",
            &head,
            "--json",
        ])
        .output()
        .expect("approve exact candidate");
    assert!(
        approved.status.success(),
        "{}",
        String::from_utf8_lossy(&approved.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&approved.stdout).expect("approval JSON");
    assert_eq!(report["status"], "approved");
    assert_eq!(report["current_step"], "testing");
    assert_eq!(report["testing_ran"], false);
    assert_eq!(report["evidence"]["candidate_diff"]["digest"], diff);
    assert_eq!(report["evidence"]["starting_head"], head);
    assert_eq!(git_evidence(&repo), source_before);
    for absent in [
        "prompts/07-testing.prompt.md",
        "responses/07-testing.raw.txt",
        "artifacts/07-testing.md",
        "artifacts/08-eval-report.md",
    ] {
        assert!(
            !run_dir.join(absent).exists(),
            "{absent} must remain absent"
        );
    }

    let approved_bytes = read_tree_bytes(&run_dir);
    let approved_provider_records = read_run_json(&run_dir)["provider_exchange_records"].clone();
    let retry = seaf_in(&repo)
        .args([
            "loop",
            "approve",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "reviewer@example.invalid",
            "--confirm-candidate-diff",
            &diff,
            "--confirm-target-head",
            &head,
            "--json",
        ])
        .output()
        .expect("retry approval");
    assert!(retry.status.success());
    assert_eq!(read_tree_bytes(&run_dir), approved_bytes);

    let changed_reviewer = seaf_in(&repo)
        .args([
            "loop",
            "approve",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "someone-else@example.invalid",
            "--confirm-candidate-diff",
            &diff,
            "--confirm-target-head",
            &head,
        ])
        .output()
        .expect("reject changed reviewer");
    assert!(!changed_reviewer.status.success());
    assert_eq!(read_tree_bytes(&run_dir), approved_bytes);

    let resume = seaf_in(&repo)
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
        .expect("approved resume executes canonical evaluation");
    assert!(resume.status.success(), "{resume:?}");
    assert!(
        String::from_utf8_lossy(&resume.stdout).contains("eval_passed"),
        "{}",
        String::from_utf8_lossy(&resume.stdout)
    );
    let evaluated = read_run_json(&run_dir);
    assert_eq!(evaluated["status"], "eval_passed");
    assert_eq!(evaluated["current_step"], "eval_report");
    assert_eq!(
        evaluated["provider_exchange_records"], approved_provider_records,
        "Approved evaluation must not make or append provider calls"
    );
    let intent_path = "artifacts/07-testing.attempt-001.execution-intent.json";
    let testing_path = "artifacts/07-testing.attempt-001.json";
    let report_path = "artifacts/08-eval-report.attempt-001.json";
    assert!(run_dir.join(intent_path).is_file());
    let intent: serde_json::Value =
        serde_json::from_slice(&fs::read(run_dir.join(intent_path)).expect("execution intent"))
            .expect("canonical intent JSON");
    let approved_run: serde_json::Value = serde_json::from_slice(
        approved_bytes
            .iter()
            .find(|(path, _)| path == Path::new("run.json"))
            .expect("approved run")
            .1
            .as_slice(),
    )
    .expect("approved run JSON");
    assert_eq!(intent["schema_version"], 2);
    assert_eq!(intent["evaluation_attempt"], 1);
    assert_eq!(intent["recovery"], serde_json::Value::Null);
    assert_eq!(intent["input_digests"], approved_run["input_digests"]);
    assert_eq!(intent["planned_checks"].as_array().unwrap().len(), 2);
    assert_eq!(
        intent["approved_run_digest"],
        canonical_sha256_digest(&approved_run).expect("Approved digest")
    );
    assert!(run_dir
        .join("artifacts/07-testing.attempt-001.check-001.stdout.log")
        .is_file());
    assert!(run_dir.join(testing_path).is_file());
    assert!(run_dir.join(report_path).is_file());
    let testing: serde_json::Value =
        serde_json::from_slice(&fs::read(run_dir.join(testing_path)).expect("Testing evidence"))
            .expect("canonical Testing JSON");
    assert_eq!(testing["schema_version"], 2);
    assert_eq!(testing["evaluation_attempt"], 1);
    assert_eq!(testing["recovery"], serde_json::Value::Null);
    assert_eq!(testing["execution_intent"]["path"], intent_path);
    assert!(!run_dir
        .join("artifacts/07-testing.execution-intent.json")
        .exists());
    assert_ne!(read_tree_bytes(&run_dir), approved_bytes);
    assert_eq!(git_evidence(&repo), source_before);

    let terminal_bytes = read_tree_bytes(&run_dir);
    let terminal_retry = seaf_in(&repo)
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
        .expect("terminal resume retry is inert");
    assert!(terminal_retry.status.success());
    assert_eq!(read_tree_bytes(&run_dir), terminal_bytes);
    assert_eq!(git_evidence(&repo), source_before);

    let mut historical = serde_json::from_slice::<serde_json::Value>(
        approved_bytes
            .iter()
            .find(|(path, _)| path == Path::new("run.json"))
            .expect("approved run bytes")
            .1
            .as_slice(),
    )
    .expect("approved run JSON");
    historical["input_digests"]
        .as_object_mut()
        .unwrap()
        .remove("eval_config");
    fs::write(
        run_dir.join("run.json"),
        serde_json::to_vec_pretty(&historical).unwrap(),
    )
    .unwrap();
    for (relative, _) in read_tree_bytes(&run_dir) {
        let path = run_dir.join(relative);
        if path.is_file() && path != run_dir.join("run.json") {
            // Restore the pre-evaluation fixture by removing only artifacts created by resume.
            if path.file_name().is_some_and(|name| {
                name.to_string_lossy().starts_with("07-testing")
                    || name.to_string_lossy().starts_with("08-eval-report")
            }) {
                fs::remove_file(path).unwrap();
            }
        }
    }
    let historical_bytes = read_tree_bytes(&run_dir);
    let historical_resume = seaf_in(&repo)
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
        .expect("historical approved resume is rejected inertly");
    assert!(!historical_resume.status.success());
    let stderr = String::from_utf8(historical_resume.stderr).unwrap();
    assert!(stderr.contains("start a new run"), "{stderr}");
    assert_eq!(read_tree_bytes(&run_dir), historical_bytes);
    assert_eq!(git_evidence(&repo), source_before);
}

#[test]
fn final_evaluation_rejects_mixed_fixed_and_indexed_attempt_one_inventory() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [printf]\n  required:\n    - name: mixed_inventory\n      command: printf verified\n",
    )
    .expect("eval config");
    commit_all(&repo, "Configure mixed inventory evaluation");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "mixed-evaluation-attempt-one";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    run_approve_and_evaluate_provider_loop(&repo, &ticket_path, &runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    let fixed = run_dir.join("artifacts/07-testing.json");
    let indexed = run_dir.join("artifacts/07-testing.attempt-001.json");
    if fixed.exists() {
        fs::copy(&fixed, &indexed).expect("inject indexed alias");
    } else {
        fs::copy(&indexed, &fixed).expect("inject fixed alias");
    }
    let before = read_tree_bytes(&run_dir);

    let resume = seaf_in(&repo)
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
        .expect("reject mixed attempt-one inventory");

    assert!(!resume.status.success(), "{resume:?}");
    assert!(
        String::from_utf8_lossy(&resume.stderr).contains("mixed fixed and indexed"),
        "{}",
        String::from_utf8_lossy(&resume.stderr)
    );
    assert_eq!(read_tree_bytes(&run_dir), before);
}

#[test]
fn loop_promote_applies_only_the_frozen_eval_passed_patch_and_exact_retry_is_inert() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [printf]\n  required:\n    - name: promotion_gate\n      command: printf promotion-ready\n",
    )
    .expect("passing eval config");
    commit_all(&repo, "Configure promotion eval");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "cli-exact-promotion";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let evaluated = run_approve_and_evaluate_provider_loop(&repo, &ticket_path, &runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    let candidate_path = PathBuf::from(
        evaluated["candidate_workspace"]["path"]
            .as_str()
            .expect("candidate path"),
    );
    let provider_records = evaluated["provider_exchange_records"].clone();
    let status = seaf_in(&repo)
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
        .expect("promotion status");
    assert!(status.status.success(), "{status:?}");
    let status: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("promotion status JSON");
    let diff = status["candidate_diff_digest"].as_str().unwrap();
    let eval_report = status["eval_report_digest"].as_str().unwrap();
    let head = status["target_head"].as_str().unwrap();
    assert_eq!(status["status"], "eval_passed");
    assert_eq!(
        status["candidate_diff_path"],
        evaluated["human_approval"]["candidate_diff"]["path"]
    );
    assert_eq!(
        status["eval_report_path"],
        "artifacts/08-eval-report.attempt-001.json"
    );
    assert_eq!(
        status["testing_evidence_path"],
        "artifacts/07-testing.attempt-001.json"
    );
    assert_eq!(
        status["policy_decision_digest"],
        evaluated["human_approval"]["policy_decision_digest"]
    );
    assert_eq!(
        status["eval_passed_run_digest"],
        canonical_sha256_digest(&evaluated).unwrap()
    );

    let source_before = git_evidence(&repo);
    for (flag, value, expected) in [
        (
            "--confirm-candidate-diff",
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "candidate diff",
        ),
        (
            "--confirm-eval-report",
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "EvalReport",
        ),
        (
            "--confirm-target-head",
            "0000000000000000000000000000000000000000",
            "target HEAD",
        ),
    ] {
        let candidate_confirmation = if flag == "--confirm-candidate-diff" {
            value
        } else {
            diff
        };
        let eval_confirmation = if flag == "--confirm-eval-report" {
            value
        } else {
            eval_report
        };
        let head_confirmation = if flag == "--confirm-target-head" {
            value
        } else {
            head
        };
        let mut command = seaf_in(&repo);
        command.args([
            "loop",
            "promote",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "promotion-reviewer@example.invalid",
            "--confirm-candidate-diff",
            candidate_confirmation,
            "--confirm-eval-report",
            eval_confirmation,
            "--confirm-target-head",
            head_confirmation,
        ]);
        let rejected = command.output().expect("reject stale confirmation");
        assert!(!rejected.status.success(), "{flag}");
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains(expected),
            "{flag}: {}",
            String::from_utf8_lossy(&rejected.stderr)
        );
        assert_eq!(git_evidence(&repo), source_before, "{flag}");
        assert_eq!(read_run_json(&run_dir)["status"], "eval_passed", "{flag}");
    }

    let promoted = seaf_in(&repo)
        .args([
            "loop",
            "promote",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "promotion-reviewer@example.invalid",
            "--confirm-candidate-diff",
            diff,
            "--confirm-eval-report",
            eval_report,
            "--confirm-target-head",
            head,
            "--json",
        ])
        .output()
        .expect("promote exact candidate");
    assert!(
        promoted.status.success(),
        "{}",
        String::from_utf8_lossy(&promoted.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&promoted.stdout).expect("promotion report JSON");
    assert_eq!(report["status"], "promoted");
    assert_eq!(report["evidence"]["candidate_diff"]["digest"], diff);
    assert_eq!(report["evidence"]["eval_report"]["digest"], eval_report);
    assert_eq!(report["evidence"]["target_head"], head);
    assert!(
        candidate_path.is_dir(),
        "promotion must retain the frozen candidate"
    );
    let promoted_run = read_run_json(&run_dir);
    assert_eq!(promoted_run["status"], "promoted");
    assert_eq!(promoted_run["provider_exchange_records"], provider_records);
    let _expected_diff = fs::read(
        run_dir.join(
            promoted_run["promotion"]["candidate_diff"]["path"]
                .as_str()
                .unwrap(),
        ),
    )
    .expect("approved diff bytes");
    assert_eq!(
        fs::read(repo.join("examples/local-loop/evals/fake-provider-smoke.txt")).unwrap(),
        fs::read(candidate_path.join("examples/local-loop/evals/fake-provider-smoke.txt")).unwrap(),
    );
    assert!(
        git_cached_diff_binary(&repo).is_empty(),
        "promotion stays unstaged"
    );

    let run_bytes = read_tree_bytes(&run_dir);
    let source_after = git_evidence(&repo);
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
        .expect("Promoted cleanup remains forbidden");
    assert!(!cleanup.status.success());
    let rerun = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--rerun-from",
            "output-review",
            "--json",
        ])
        .output()
        .expect("Promoted provider rerun remains forbidden");
    assert!(!rerun.status.success());
    assert_eq!(read_tree_bytes(&run_dir), run_bytes);
    assert!(candidate_path.is_dir());
    let retry = seaf_in(&repo)
        .args([
            "loop",
            "promote",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "promotion-reviewer@example.invalid",
            "--confirm-candidate-diff",
            diff,
            "--confirm-eval-report",
            eval_report,
            "--confirm-target-head",
            head,
            "--json",
        ])
        .output()
        .expect("retry exact promotion");
    assert!(retry.status.success(), "{retry:?}");
    assert_eq!(read_tree_bytes(&run_dir), run_bytes);
    assert_eq!(git_evidence(&repo), source_after);
}

#[test]
fn loop_promote_rejects_dirty_stale_wrong_repo_and_tampered_authority_without_source_mutation() {
    for case in [
        "tracked",
        "staged",
        "untracked",
        "ignored",
        "stale-head",
        "wrong-repo",
        "eval-config",
        "command-log",
        "candidate-diff",
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let repo = temp_dir.path().join("repo");
        fs::create_dir_all(&repo).expect("repo dir");
        init_git_repo(&repo);
        fs::write(repo.join(".gitignore"), "ignored-promotion-marker\n").unwrap();
        fs::write(
            repo.join("seaf.evals.yaml"),
            "evals:\n  allow_commands: [printf]\n  required:\n    - name: promotion_gate\n      command: printf promotion-ready\n",
        )
        .unwrap();
        commit_all(&repo, "Configure promotion denial fixture");
        let runs_root = temp_dir.path().join("runs");
        let run_id = format!("promotion-{case}");
        let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
        let evaluated =
            run_approve_and_evaluate_provider_loop(&repo, &ticket_path, &runs_root, &run_id);
        let run_dir = runs_root.join(&run_id);
        let diff = evaluated["human_approval"]["candidate_diff"]["digest"]
            .as_str()
            .unwrap()
            .to_string();
        let eval_report = evaluated["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["name"] == "eval_report")
            .unwrap()["artifact_digest"]
            .as_str()
            .unwrap()
            .to_string();
        let head = evaluated["human_approval"]["starting_head"]
            .as_str()
            .unwrap()
            .to_string();
        let invocation_repo = if case == "wrong-repo" {
            let other = temp_dir.path().join("other-repo");
            fs::create_dir_all(&other).unwrap();
            init_git_repo(&other);
            other
        } else {
            repo.clone()
        };
        match case {
            "tracked" => fs::write(repo.join("seaf.policy.json"), b"dirty tracked\n").unwrap(),
            "staged" => {
                fs::write(repo.join("staged-promotion-marker"), b"dirty\n").unwrap();
                let add = Command::new("git")
                    .args(["add", "staged-promotion-marker"])
                    .current_dir(&repo)
                    .output()
                    .unwrap();
                assert!(add.status.success());
            }
            "untracked" => fs::write(repo.join("untracked-promotion-marker"), b"dirty\n").unwrap(),
            "ignored" => fs::write(repo.join("ignored-promotion-marker"), b"dirty\n").unwrap(),
            "stale-head" => {
                fs::write(repo.join("stale-head"), b"new head\n").unwrap();
                commit_all(&repo, "Move promotion target");
            }
            "eval-config" => fs::write(run_dir.join("inputs/eval-config.json"), b"{}\n").unwrap(),
            "command-log" => {
                let testing: serde_json::Value = serde_json::from_slice(
                    &fs::read(run_dir.join("artifacts/07-testing.attempt-001.json")).unwrap(),
                )
                .unwrap();
                let path = testing["checks"][0]["stdout_path"].as_str().unwrap();
                fs::write(run_dir.join(path), b"tampered log\n").unwrap();
            }
            "candidate-diff" => {
                let path = evaluated["human_approval"]["candidate_diff"]["path"]
                    .as_str()
                    .unwrap();
                fs::write(run_dir.join(path), b"tampered diff\n").unwrap();
            }
            "wrong-repo" => {}
            _ => unreachable!(),
        }
        let run_before = read_tree_bytes(&run_dir);
        let source_before = git_evidence(&repo);
        let rejected = seaf_in(&invocation_repo)
            .args([
                "loop",
                "promote",
                "--run-id",
                &run_id,
                "--runs-root",
                runs_root.to_str().unwrap(),
                "--reviewer",
                "promotion-reviewer@example.invalid",
                "--confirm-candidate-diff",
                &diff,
                "--confirm-eval-report",
                &eval_report,
                "--confirm-target-head",
                &head,
                "--json",
            ])
            .output()
            .expect("reject unsafe promotion");
        assert!(!rejected.status.success(), "{case}: {rejected:?}");
        assert_eq!(git_evidence(&repo), source_before, "{case}");
        assert_eq!(read_tree_bytes(&run_dir), run_before, "{case}");
    }
}

#[cfg(unix)]
#[test]
fn loop_promote_never_executes_unrelated_filters_or_accepts_normalized_extra_bytes() {
    let temp_dir = tempfile::tempdir().unwrap();
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    fs::write(repo.join("unrelated.txt"), b"canonical\n").unwrap();
    fs::write(repo.join(".gitattributes"), b"unrelated.txt filter=evil\n").unwrap();
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [printf]\n  required:\n    - name: filter_free_promotion\n      command: printf ready\n",
    )
    .unwrap();
    commit_all(&repo, "Configure unrelated filtered path");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "promotion-filter-free";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let evaluated = run_approve_and_evaluate_provider_loop(&repo, &ticket_path, &runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    let reviewer = "filter-reviewer@example.invalid";
    fs::write(
        run_dir.join("artifacts/09-promotion.intent.json"),
        canonical_json_bytes(&promotion_intent_json(&evaluated, reviewer)).unwrap(),
    )
    .unwrap();
    let patch_path = run_dir.join(
        evaluated["human_approval"]["candidate_diff"]["path"]
            .as_str()
            .unwrap(),
    );
    let apply = Command::new("git")
        .args(["apply", "--whitespace=nowarn", patch_path.to_str().unwrap()])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(apply.status.success(), "{apply:?}");
    let marker = temp_dir.path().join("filter-executed");
    let filter = temp_dir.path().join("evil-filter.sh");
    write_executable_script(
        &filter,
        &format!(
            "#!/bin/sh\nprintf executed > '{}'\nprintf 'canonical\\n'\n",
            marker.display()
        ),
    );
    for (key, value) in [
        ("filter.evil.clean", filter.to_str().unwrap()),
        ("filter.evil.smudge", "cat"),
        ("filter.evil.required", "true"),
    ] {
        let configured = Command::new("git")
            .args(["config", key, value])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(configured.status.success());
    }
    fs::write(
        repo.join("unrelated.txt"),
        b"extra physical bytes hidden by filter\n",
    )
    .unwrap();
    let diff = evaluated["human_approval"]["candidate_diff"]["digest"]
        .as_str()
        .unwrap();
    let eval_report = evaluated["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "eval_report")
        .unwrap()["artifact_digest"]
        .as_str()
        .unwrap();
    let head = evaluated["human_approval"]["starting_head"]
        .as_str()
        .unwrap();
    let before = read_tree_bytes(&run_dir);
    let rejected = seaf_in(&repo)
        .args([
            "loop",
            "promote",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            reviewer,
            "--confirm-candidate-diff",
            diff,
            "--confirm-eval-report",
            eval_report,
            "--confirm-target-head",
            head,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success(), "{rejected:?}");
    assert!(!marker.exists(), "repository filter helper executed");
    assert_eq!(
        fs::read(repo.join("unrelated.txt")).unwrap(),
        b"extra physical bytes hidden by filter\n"
    );
    assert!(repo
        .join("examples/local-loop/evals/fake-provider-smoke.txt")
        .exists());
    assert_eq!(read_run_json(&run_dir)["status"], "eval_passed");
    assert_eq!(read_tree_bytes(&run_dir), before);
}

#[test]
fn loop_promote_persists_intent_before_apply_and_adopts_exact_patch_after_process_crash() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [printf]\n  required:\n    - name: crash_boundary\n      command: printf ready\n",
    )
    .unwrap();
    commit_all(&repo, "Configure crash-boundary eval");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "promotion-crash-adoption";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let evaluated = run_approve_and_evaluate_provider_loop(&repo, &ticket_path, &runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    let diff = evaluated["human_approval"]["candidate_diff"]["digest"]
        .as_str()
        .unwrap();
    let eval_report = evaluated["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "eval_report")
        .unwrap()["artifact_digest"]
        .as_str()
        .unwrap();
    let head = evaluated["human_approval"]["starting_head"]
        .as_str()
        .unwrap();
    let repository_lock = open_repository_operation_lock(&evaluated);
    repository_lock
        .lock()
        .expect("hold repository operation lock");
    let provider_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(run_dir.join("provider-exchange.lock"))
        .expect("provider lock");

    let mut command = seaf_in(&repo);
    command.args([
        "loop",
        "promote",
        "--run-id",
        run_id,
        "--runs-root",
        runs_root.to_str().unwrap(),
        "--reviewer",
        "crash-reviewer@example.invalid",
        "--confirm-candidate-diff",
        diff,
        "--confirm-eval-report",
        eval_report,
        "--confirm-target-head",
        head,
        "--json",
    ]);
    let mut child = command
        .spawn()
        .expect("start promotion held before state publication");
    let intent = run_dir.join("artifacts/09-promotion.intent.json");
    for _ in 0..500 {
        if intent.is_file() {
            break;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "promotion exited before intent"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(intent.is_file(), "promotion did not durably publish intent");
    provider_lock.lock().expect("hold final publication lock");
    repository_lock
        .unlock()
        .expect("release repository operation lock");
    let applied = repo.join("examples/local-loop/evals/fake-provider-smoke.txt");
    for _ in 0..500 {
        if applied.is_file() {
            break;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "promotion exited before apply"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        applied.is_file(),
        "promotion did not reach the source apply boundary"
    );
    assert!(
        intent.is_file(),
        "durable intent must precede source mutation"
    );
    assert_eq!(read_run_json(&run_dir)["status"], "eval_passed");
    child.kill().expect("simulate crash after apply");
    child.wait().expect("reap crashed promotion");
    provider_lock
        .unlock()
        .expect("release final publication lock");

    let interrupted_bytes = fs::read(&applied).unwrap();
    let retry = seaf_in(&repo)
        .args([
            "loop",
            "promote",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "crash-reviewer@example.invalid",
            "--confirm-candidate-diff",
            diff,
            "--confirm-eval-report",
            eval_report,
            "--confirm-target-head",
            head,
            "--json",
        ])
        .output()
        .expect("adopt exact applied patch");
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(fs::read(&applied).unwrap(), interrupted_bytes);
    assert_eq!(read_run_json(&run_dir)["status"], "promoted");
}

#[test]
fn loop_promote_rechecks_intent_while_waiting_for_final_state_publication() {
    let temp_dir = tempfile::tempdir().unwrap();
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [printf]\n  required:\n    - name: final_intent_recheck\n      command: printf ready\n",
    )
    .unwrap();
    commit_all(&repo, "Configure final intent recheck");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "promotion-final-intent-recheck";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let evaluated = run_approve_and_evaluate_provider_loop(&repo, &ticket_path, &runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    let repository_lock = open_repository_operation_lock(&evaluated);
    repository_lock.lock().unwrap();
    let provider_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(run_dir.join("provider-exchange.lock"))
        .unwrap();
    let mut command = seaf_in(&repo);
    command
        .args([
            "loop",
            "promote",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "final-intent-reviewer@example.invalid",
            "--confirm-candidate-diff",
            evaluated["human_approval"]["candidate_diff"]["digest"]
                .as_str()
                .unwrap(),
            "--confirm-eval-report",
            evaluated["steps"]
                .as_array()
                .unwrap()
                .iter()
                .find(|step| step["name"] == "eval_report")
                .unwrap()["artifact_digest"]
                .as_str()
                .unwrap(),
            "--confirm-target-head",
            evaluated["human_approval"]["starting_head"]
                .as_str()
                .unwrap(),
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    let intent = run_dir.join("artifacts/09-promotion.intent.json");
    for _ in 0..500 {
        if intent.is_file() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(intent.is_file());
    provider_lock.lock().unwrap();
    repository_lock.unlock().unwrap();
    let applied = repo.join("examples/local-loop/evals/fake-provider-smoke.txt");
    for _ in 0..500 {
        if applied.is_file() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(applied.is_file());
    let mut substituted: serde_json::Value =
        serde_json::from_slice(&fs::read(&intent).unwrap()).unwrap();
    substituted["run_id"] = serde_json::json!("substituted-final-run");
    let replacement = intent.with_extension("replacement");
    fs::write(&replacement, canonical_json_bytes(&substituted).unwrap()).unwrap();
    fs::rename(&replacement, &intent).unwrap();
    provider_lock.unlock().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(read_run_json(&run_dir)["status"], "eval_passed");
    assert!(
        applied.is_file(),
        "exact crash-adoptable source patch remains"
    );
}

#[test]
fn loop_promote_rejects_wrong_run_and_noncanonical_intent_before_source_mutation() {
    for case in ["noncanonical-time", "wrong-run"] {
        let temp_dir = tempfile::tempdir().unwrap();
        let repo = temp_dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);
        fs::write(
            repo.join("seaf.evals.yaml"),
            "evals:\n  allow_commands: [printf]\n  required:\n    - name: intent_validation\n      command: printf ready\n",
        )
        .unwrap();
        commit_all(&repo, "Configure intent validation");
        let runs_root = temp_dir.path().join("runs");
        let run_id = format!("promotion-intent-{case}");
        let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
        let evaluated =
            run_approve_and_evaluate_provider_loop(&repo, &ticket_path, &runs_root, &run_id);
        let run_dir = runs_root.join(&run_id);
        let reviewer = "intent-reviewer@example.invalid";
        let mut intent = promotion_intent_json(&evaluated, reviewer);
        match case {
            "wrong-run" => intent["run_id"] = serde_json::json!("another-run"),
            "noncanonical-time" => {
                intent["started_at"] =
                    serde_json::json!(format!("0{}", evaluated["updated_at"].as_str().unwrap()));
            }
            _ => unreachable!(),
        }
        fs::write(
            run_dir.join("artifacts/09-promotion.intent.json"),
            canonical_json_bytes(&intent).unwrap(),
        )
        .unwrap();
        let source_before = git_evidence(&repo);
        let run_before = read_tree_bytes(&run_dir);
        let rejected = seaf_in(&repo)
            .args([
                "loop",
                "promote",
                "--run-id",
                &run_id,
                "--runs-root",
                runs_root.to_str().unwrap(),
                "--reviewer",
                reviewer,
                "--confirm-candidate-diff",
                evaluated["human_approval"]["candidate_diff"]["digest"]
                    .as_str()
                    .unwrap(),
                "--confirm-eval-report",
                evaluated["steps"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|step| step["name"] == "eval_report")
                    .unwrap()["artifact_digest"]
                    .as_str()
                    .unwrap(),
                "--confirm-target-head",
                evaluated["human_approval"]["starting_head"]
                    .as_str()
                    .unwrap(),
                "--json",
            ])
            .output()
            .unwrap();
        assert!(!rejected.status.success(), "{case}: {rejected:?}");
        assert_eq!(git_evidence(&repo), source_before, "{case}");
        assert_eq!(read_tree_bytes(&run_dir), run_before, "{case}");
    }
}

#[test]
fn loop_promote_excludes_only_its_bound_runtime_inside_the_clean_target() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    fs::write(repo.join(".gitignore"), ".seaf/loops/runs/\n").unwrap();
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [printf]\n  required:\n    - name: in_repo_runtime\n      command: printf ready\n",
    )
    .unwrap();
    commit_all(&repo, "Configure in-repository runtime");
    let runs_root = repo.join(".seaf/loops/runs");
    let run_id = "promotion-in-repository-runtime";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let evaluated = run_approve_and_evaluate_provider_loop(&repo, &ticket_path, &runs_root, run_id);
    let diff = evaluated["human_approval"]["candidate_diff"]["digest"]
        .as_str()
        .unwrap();
    let eval_report = evaluated["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "eval_report")
        .unwrap()["artifact_digest"]
        .as_str()
        .unwrap();
    let head = evaluated["human_approval"]["starting_head"]
        .as_str()
        .unwrap();
    let sibling = runs_root.join("unrelated-sibling-run/ignored.txt");
    fs::create_dir_all(sibling.parent().unwrap()).unwrap();
    fs::write(&sibling, b"unrelated ignored runtime bytes\n").unwrap();
    let rejected = seaf_in(&repo)
        .args([
            "loop",
            "promote",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "runtime-reviewer@example.invalid",
            "--confirm-candidate-diff",
            diff,
            "--confirm-eval-report",
            eval_report,
            "--confirm-target-head",
            head,
            "--json",
        ])
        .output()
        .expect("reject unrelated ignored runtime sibling");
    assert!(!rejected.status.success());
    assert_eq!(
        read_run_json(&runs_root.join(run_id))["status"],
        "eval_passed"
    );
    fs::remove_dir_all(sibling.parent().unwrap()).unwrap();
    let promoted = seaf_in(&repo)
        .args([
            "loop",
            "promote",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "runtime-reviewer@example.invalid",
            "--confirm-candidate-diff",
            diff,
            "--confirm-eval-report",
            eval_report,
            "--confirm-target-head",
            head,
            "--json",
        ])
        .output()
        .expect("promote with bound runtime inside source");
    assert!(
        promoted.status.success(),
        "{}",
        String::from_utf8_lossy(&promoted.stderr)
    );
    assert_eq!(read_run_json(&runs_root.join(run_id))["status"], "promoted");
}

#[test]
fn loop_promote_rejects_staged_rename_pair_from_non_runtime_into_bound_runtime() {
    let temp_dir = tempfile::tempdir().unwrap();
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let run_id = "promotion-staged-rename";
    let deceptive_source = repo.join(format!("xxx.seaf/loops/runs/{run_id}/outside.txt"));
    fs::create_dir_all(deceptive_source.parent().unwrap()).unwrap();
    fs::write(&deceptive_source, b"tracked outside runtime\n").unwrap();
    fs::write(repo.join(".gitignore"), ".seaf/loops/runs/\n").unwrap();
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [printf]\n  required:\n    - name: staged_rename_guard\n      command: printf ready\n",
    )
    .unwrap();
    commit_all(&repo, "Configure staged rename promotion guard");
    let runs_root = repo.join(".seaf/loops/runs");
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let evaluated = run_approve_and_evaluate_provider_loop(&repo, &ticket_path, &runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    let deceptive_destination = run_dir.join("outside.txt");
    let rename = Command::new("git")
        .args([
            "mv",
            deceptive_source
                .strip_prefix(&repo)
                .unwrap()
                .to_str()
                .unwrap(),
            deceptive_destination
                .strip_prefix(&repo)
                .unwrap()
                .to_str()
                .unwrap(),
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(rename.status.success(), "{rename:?}");
    let diff = evaluated["human_approval"]["candidate_diff"]["digest"]
        .as_str()
        .unwrap();
    let eval_report = evaluated["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "eval_report")
        .unwrap()["artifact_digest"]
        .as_str()
        .unwrap();
    let head = evaluated["human_approval"]["starting_head"]
        .as_str()
        .unwrap();
    let run_before = read_tree_bytes(&run_dir);
    let rejected = seaf_in(&repo)
        .args([
            "loop",
            "promote",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "rename-reviewer@example.invalid",
            "--confirm-candidate-diff",
            diff,
            "--confirm-eval-report",
            eval_report,
            "--confirm-target-head",
            head,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success(), "{rejected:?}");
    assert!(!repo
        .join("examples/local-loop/evals/fake-provider-smoke.txt")
        .exists());
    assert_eq!(read_tree_bytes(&run_dir), run_before);
}

#[test]
fn loop_promote_revalidates_conflict_and_run_cas_inside_repository_lock_before_apply() {
    for case in ["patch-conflict", "run-change", "intent-replacement"] {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let repo = temp_dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);
        fs::write(
            repo.join("seaf.evals.yaml"),
            "evals:\n  allow_commands: [printf]\n  required:\n    - name: locked_revalidation\n      command: printf ready\n",
        )
        .unwrap();
        commit_all(&repo, "Configure locked promotion revalidation");
        let runs_root = temp_dir.path().join("runs");
        let run_id = format!("promotion-locked-{case}");
        let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
        let evaluated =
            run_approve_and_evaluate_provider_loop(&repo, &ticket_path, &runs_root, &run_id);
        let run_dir = runs_root.join(&run_id);
        let diff = evaluated["human_approval"]["candidate_diff"]["digest"]
            .as_str()
            .unwrap();
        let eval_report = evaluated["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["name"] == "eval_report")
            .unwrap()["artifact_digest"]
            .as_str()
            .unwrap();
        let head = evaluated["human_approval"]["starting_head"]
            .as_str()
            .unwrap();
        let repository_lock_path = find_named_file(
            &repo.parent().unwrap().join("seaf-test-tmp"),
            ".repository-operation.lock",
        );
        let repository_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(repository_lock_path)
            .unwrap();
        repository_lock.lock().unwrap();
        let mut command = seaf_in(&repo);
        command
            .args([
                "loop",
                "promote",
                "--run-id",
                &run_id,
                "--runs-root",
                runs_root.to_str().unwrap(),
                "--reviewer",
                "locked-reviewer@example.invalid",
                "--confirm-candidate-diff",
                diff,
                "--confirm-eval-report",
                eval_report,
                "--confirm-target-head",
                head,
                "--json",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().expect("start blocked promotion");
        let intent = run_dir.join("artifacts/09-promotion.intent.json");
        for _ in 0..500 {
            if intent.is_file() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            intent.is_file(),
            "{case}: intent was not persisted before lock"
        );
        let target = repo.join("examples/local-loop/evals/fake-provider-smoke.txt");
        match case {
            "patch-conflict" => {
                fs::create_dir_all(target.parent().unwrap()).unwrap();
                fs::write(&target, b"concurrent conflicting bytes\n").unwrap();
            }
            "run-change" => {
                let mut changed = read_run_json(&run_dir);
                changed["updated_at"] = serde_json::json!("9999999999");
                fs::write(
                    run_dir.join("run.json"),
                    format!("{}\n", serde_json::to_string_pretty(&changed).unwrap()),
                )
                .unwrap();
            }
            "intent-replacement" => {
                let mut substituted: serde_json::Value =
                    serde_json::from_slice(&fs::read(&intent).unwrap()).unwrap();
                substituted["run_id"] = serde_json::json!("substituted-run");
                let replacement = intent.with_extension("replacement");
                fs::write(&replacement, canonical_json_bytes(&substituted).unwrap()).unwrap();
                fs::rename(&replacement, &intent).unwrap();
            }
            _ => unreachable!(),
        }
        repository_lock.unlock().unwrap();
        let output = child.wait_with_output().expect("finish rejected promotion");
        assert!(!output.status.success(), "{case}");
        assert_eq!(read_run_json(&run_dir)["status"], "eval_passed", "{case}");
        if case == "patch-conflict" {
            assert_eq!(
                fs::read(&target).unwrap(),
                b"concurrent conflicting bytes\n"
            );
        } else {
            assert!(
                !target.exists(),
                "concurrent run or intent change must precede apply"
            );
        }
    }
}

#[test]
fn loop_promote_rejects_historical_failed_and_cleaned_runs_byte_identically() {
    for case in ["historical", "failed", "cleaned"] {
        let temp_dir = tempfile::tempdir().unwrap();
        let repo = temp_dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);
        let command = if case == "failed" { "false" } else { "printf" };
        fs::write(
            repo.join("seaf.evals.yaml"),
            format!(
                "evals:\n  allow_commands: [{command}]\n  required:\n    - name: terminal_denial\n      command: {command}\n"
            ),
        )
        .unwrap();
        commit_all(&repo, "Configure terminal promotion denial");
        let runs_root = temp_dir.path().join("runs");
        let run_id = format!("promotion-terminal-{case}");
        let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
        if case == "failed" {
            let ticket = fs::read_to_string(&ticket_path)
                .unwrap()
                .replace("    - printf", "    - false");
            fs::write(&ticket_path, ticket).unwrap();
        }
        let approved = run_and_approve_provider_loop(&repo, &ticket_path, &runs_root, &run_id);
        let run_dir = runs_root.join(&run_id);
        match case {
            "historical" => {
                let mut historical = approved;
                historical["input_digests"]
                    .as_object_mut()
                    .unwrap()
                    .remove("eval_config");
                fs::write(
                    run_dir.join("run.json"),
                    format!("{}\n", serde_json::to_string_pretty(&historical).unwrap()),
                )
                .unwrap();
            }
            "failed" => {
                let resume = seaf_in(&repo)
                    .args([
                        "loop",
                        "resume",
                        "--run-id",
                        &run_id,
                        "--runs-root",
                        runs_root.to_str().unwrap(),
                        "--json",
                    ])
                    .output()
                    .unwrap();
                assert!(resume.status.success());
                assert_eq!(read_run_json(&run_dir)["status"], "failed");
            }
            "cleaned" => {
                mark_run_completed_for_cleanup_compatibility(&run_dir);
                let mut completed = read_run_json(&run_dir);
                completed.as_object_mut().unwrap().remove("human_approval");
                fs::write(
                    run_dir.join("run.json"),
                    format!("{}\n", serde_json::to_string_pretty(&completed).unwrap()),
                )
                .unwrap();
                let cleanup = seaf_in(&repo)
                    .args([
                        "loop",
                        "cleanup",
                        "--run-id",
                        &run_id,
                        "--runs-root",
                        runs_root.to_str().unwrap(),
                        "--json",
                    ])
                    .output()
                    .unwrap();
                assert!(cleanup.status.success(), "{cleanup:?}");
                assert_eq!(
                    read_run_json(&run_dir)["candidate_workspace"]["lifecycle"],
                    "cleaned"
                );
            }
            _ => unreachable!(),
        }
        let run_before = read_tree_bytes(&run_dir);
        let source_before = git_evidence(&repo);
        let rejected = seaf_in(&repo)
            .args([
                "loop",
                "promote",
                "--run-id",
                &run_id,
                "--runs-root",
                runs_root.to_str().unwrap(),
                "--reviewer",
                "terminal-reviewer@example.invalid",
                "--confirm-candidate-diff",
                &"f".repeat(64),
                "--confirm-eval-report",
                &"e".repeat(64),
                "--confirm-target-head",
                "0000000000000000000000000000000000000000",
                "--json",
            ])
            .output()
            .unwrap();
        assert!(!rejected.status.success(), "{case}");
        assert_eq!(read_tree_bytes(&run_dir), run_before, "{case}");
        assert_eq!(git_evidence(&repo), source_before, "{case}");
    }
}

#[test]
fn approved_eval_prevalidation_denials_and_partial_intent_execute_zero_commands() {
    for (case, eval_yaml, ticket_command, pre_mutation, expected_error) in [
        (
            "ticket-denial",
            "evals:\n  allow_commands: [touch]\n  required:\n    - name: marker\n      command: touch eval-marker\n",
            "printf",
            "none",
            "ticket autonomy",
        ),
        (
            "eval-denial",
            "evals:\n  allow_commands: [printf]\n  required:\n    - name: marker\n      command: touch eval-marker\n",
            "touch",
            "none",
            "eval allow_commands",
        ),
        (
            "partial-intent",
            "evals:\n  allow_commands: [touch]\n  required:\n    - name: marker\n      command: touch eval-marker\n",
            "touch",
            "partial",
            "audited recovery",
        ),
        (
            "duplicate-check-identity",
            "evals:\n  allow_commands: [touch]\n  required:\n    - name: duplicated\n      command: touch eval-marker\n    - name: duplicated\n      command: touch eval-marker-two\n",
            "touch",
            "none",
            "duplicated check names",
        ),
        (
            "approval-artifact-substitution",
            "evals:\n  allow_commands: [touch]\n  required:\n    - name: marker\n      command: touch eval-marker\n",
            "touch",
            "approval",
            "diff artifact digest mismatch",
        ),
        (
            "source-substitution",
            "evals:\n  allow_commands: [touch]\n  required:\n    - name: marker\n      command: touch eval-marker\n",
            "touch",
            "source",
            "source HEAD",
        ),
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let repo = temp_dir.path().join("repo");
        fs::create_dir_all(&repo).expect("repo dir");
        init_git_repo(&repo);
        fs::write(repo.join("seaf.evals.yaml"), eval_yaml).expect("eval config");
        commit_all(&repo, "Configure denied eval");
        let runs_root = temp_dir.path().join("runs");
        let run_id = format!("approved-{case}");
        let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
        let ticket = fs::read_to_string(&ticket_path)
            .expect("ticket")
            .replace("    - printf", &format!("    - {ticket_command}"));
        fs::write(&ticket_path, ticket).expect("ticket allowlist");
        let approved = run_and_approve_provider_loop(
            &repo,
            &ticket_path,
            &runs_root,
            &run_id,
        );
        let run_dir = runs_root.join(&run_id);
        let candidate = PathBuf::from(
            approved["candidate_workspace"]["path"]
                .as_str()
                .expect("candidate path"),
        );
        match pre_mutation {
            "partial" => write_private_run_fixture(
                run_dir.join("artifacts/07-testing.attempt-001.execution-intent.json"),
                b"partial-attempt-bytes",
            ),
            "approval" => {
                let relative = approved["human_approval"]["candidate_diff"]["path"]
                    .as_str()
                    .expect("approved diff path");
                fs::write(run_dir.join(relative), b"substituted approved diff")
                    .expect("substitute approved diff");
            }
            "source" => {
                fs::write(repo.join("source-substitution.txt"), b"new source HEAD\n")
                    .expect("source substitution");
                commit_all(&repo, "Substitute source HEAD");
            }
            "none" => {}
            _ => unreachable!(),
        }
        let before = read_tree_bytes(&run_dir);
        let resume = seaf_in(&repo)
            .args([
                "loop",
                "resume",
                "--run-id",
                &run_id,
                "--runs-root",
                runs_root.to_str().unwrap(),
                "--json",
            ])
            .output()
            .expect("resume denied evaluation");

        assert!(!resume.status.success(), "{case}: {resume:?}");
        let stderr = String::from_utf8_lossy(&resume.stderr);
        assert!(stderr.contains(expected_error), "{case}: {stderr}");
        assert!(!candidate.join("eval-marker").exists(), "{case}");
        assert_eq!(read_tree_bytes(&run_dir), before, "{case}");
        assert_eq!(read_run_json(&run_dir)["status"], "approved", "{case}");
        if pre_mutation != "partial" {
            assert!(
                !run_dir
                    .join("artifacts/07-testing.attempt-001.execution-intent.json")
                    .exists(),
                "{case}: prevalidation must not claim an execution attempt"
            );
        }
    }
}

#[test]
fn approved_eval_failed_command_publishes_rejecting_bound_terminal_report() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [false]\n  required:\n    - name: required_failure\n      command: false\n",
    )
    .expect("failing eval config");
    commit_all(&repo, "Configure failing eval");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "approved-failed-check";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let ticket = fs::read_to_string(&ticket_path)
        .expect("ticket")
        .replace("    - printf", "    - false");
    fs::write(&ticket_path, ticket).expect("ticket allowlist");
    let approved = run_and_approve_provider_loop(&repo, &ticket_path, &runs_root, run_id);
    let source_before = git_evidence(&repo);
    let provider_records = approved["provider_exchange_records"].clone();

    let resume = seaf_in(&repo)
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
        .expect("resume failing evaluation");

    assert!(resume.status.success(), "{resume:?}");
    let run_dir = runs_root.join(run_id);
    let failed = read_run_json(&run_dir);
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["provider_exchange_records"], provider_records);
    let report: serde_json::Value = serde_json::from_slice(
        &fs::read(run_dir.join("artifacts/08-eval-report.attempt-001.json")).expect("EvalReport"),
    )
    .expect("EvalReport JSON");
    assert_eq!(report["passed"], false);
    assert_eq!(report["decision"], "reject");
    assert_eq!(report["checks"][0]["status"], "failed");
    assert_eq!(git_evidence(&repo), source_before);
}

#[test]
fn approved_eval_timeout_is_rejecting_evidence_not_eval_success() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [sleep]\n  required:\n    - name: bounded_timeout\n      command: sleep 1\n      timeout_ms: 10\n",
    )
    .expect("timeout eval config");
    commit_all(&repo, "Configure timeout eval");
    let runs_root = temp_dir.path().join("runs");
    let run_id = "approved-timeout";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let ticket = fs::read_to_string(&ticket_path)
        .expect("ticket")
        .replace("    - printf", "    - sleep");
    fs::write(&ticket_path, ticket).expect("ticket allowlist");
    run_and_approve_provider_loop(&repo, &ticket_path, &runs_root, run_id);

    let resume = seaf_in(&repo)
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
        .expect("resume timeout evaluation");

    assert!(resume.status.success(), "{resume:?}");
    let run_dir = runs_root.join(run_id);
    assert_eq!(read_run_json(&run_dir)["status"], "failed");
    let report: serde_json::Value = serde_json::from_slice(
        &fs::read(run_dir.join("artifacts/08-eval-report.attempt-001.json")).expect("EvalReport"),
    )
    .expect("EvalReport JSON");
    assert_eq!(report["passed"], false);
    assert_eq!(report["decision"], "reject");
    assert!(report["checks"][0]["summary"]
        .as_str()
        .expect("timeout summary")
        .contains("timed out"));
}

#[cfg(unix)]
#[test]
fn approved_eval_physical_tamper_or_publication_collision_never_claims_terminal_success() {
    for (case, script_body, expected_error) in [
        (
            "config-tamper",
            "printf tampered > \"$1/inputs/eval-config.json\"\n",
            "eval-config.json",
        ),
        (
            "intent-tamper",
            "printf tampered > \"$1/artifacts/07-testing.attempt-001.execution-intent.json\"\n",
            "execution-intent.json",
        ),
        (
            "candidate-tamper",
            "printf tampered >> seaf.policy.json\ngit add seaf.policy.json\n",
            "candidate",
        ),
        (
            "candidate-nonignored-output",
            "touch generated-not-ignored\n",
            "candidate",
        ),
        (
            "concurrent-authority",
            "perl -0pi -e 's/safety-reviewer/concurrent-reviewer/' \"$1/run.json\"\n",
            "Approved authority changed",
        ),
        (
            "publication-collision",
            "printf collision > \"$1/artifacts/07-testing.attempt-001.json\"\nchmod 600 \"$1/artifacts/07-testing.attempt-001.json\"\n",
            "different bytes",
        ),
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let repo = temp_dir.path().join("repo");
        fs::create_dir_all(&repo).expect("repo dir");
        init_git_repo(&repo);
        let runs_root = temp_dir.path().join("runs");
        let run_id = format!("approved-{case}");
        let run_dir = runs_root.join(&run_id);
        let script = repo.join("eval-tamper.sh");
        fs::write(&script, format!("#!/bin/sh\n{script_body}")).expect("tamper script");
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
        fs::write(
            repo.join("seaf.evals.yaml"),
            format!(
                "evals:\n  allow_commands: [./eval-tamper.sh]\n  required:\n    - name: {case}\n      command: ./eval-tamper.sh {}\n",
                run_dir.display()
            ),
        )
        .expect("tamper eval config");
        commit_all(&repo, "Configure tamper eval");
        let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
        let ticket = fs::read_to_string(&ticket_path)
            .expect("ticket")
            .replace("    - printf", "    - ./eval-tamper.sh");
        fs::write(&ticket_path, ticket).expect("ticket allowlist");
        run_and_approve_provider_loop(&repo, &ticket_path, &runs_root, &run_id);
        let source_before = git_evidence(&repo);

        let resume = seaf_in(&repo)
            .args([
                "loop",
                "resume",
                "--run-id",
                &run_id,
                "--runs-root",
                runs_root.to_str().unwrap(),
                "--json",
            ])
            .output()
            .expect("resume tampering evaluation");

        assert!(!resume.status.success(), "{case}: {resume:?}");
        let stderr = String::from_utf8_lossy(&resume.stderr);
        assert!(stderr.contains(expected_error), "{case}: {stderr}");
        assert_eq!(read_run_json(&run_dir)["status"], "approved", "{case}");
        assert!(
            !run_dir
                .join("artifacts/08-eval-report.attempt-001.json")
                .exists(),
            "{case}: no terminal report may be claimed"
        );
        assert_eq!(git_evidence(&repo), source_before, "{case}");

        let interrupted = read_tree_bytes(&run_dir);
        let retry = seaf_in(&repo)
            .args([
                "loop",
                "resume",
                "--run-id",
                &run_id,
                "--runs-root",
                runs_root.to_str().unwrap(),
                "--json",
            ])
            .output()
            .expect("retry interrupted evaluation");
        assert!(!retry.status.success(), "{case}: {retry:?}");
        assert_eq!(read_tree_bytes(&run_dir), interrupted, "{case}");
    }
}

#[cfg(unix)]
#[test]
fn approved_eval_rechecks_each_persisted_log_before_terminal_publication() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = temp_dir.path().join("runs");
    let run_id = "approved-log-tamper";
    let run_dir = runs_root.join(run_id);
    let script = repo.join("log-tamper.sh");
    fs::write(
        &script,
        "#!/bin/sh\nprintf substituted > \"$1/artifacts/07-testing.attempt-001.check-001.stdout.log\"\n",
    )
    .expect("log tamper script");
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).unwrap();
    fs::write(
        repo.join("seaf.evals.yaml"),
        format!(
            "evals:\n  allow_commands: [printf, ./log-tamper.sh]\n  required:\n    - name: first_log\n      command: printf original\n    - name: tamper_first_log\n      command: ./log-tamper.sh {}\n",
            run_dir.display()
        ),
    )
    .expect("log tamper eval config");
    commit_all(&repo, "Configure log tamper eval");
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let ticket = fs::read_to_string(&ticket_path)
        .expect("ticket")
        .replace("    - printf", "    - printf\n    - ./log-tamper.sh");
    fs::write(&ticket_path, ticket).expect("ticket allowlist");
    run_and_approve_provider_loop(&repo, &ticket_path, &runs_root, run_id);

    let resume = seaf_in(&repo)
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
        .expect("resume log tamper evaluation");

    assert!(!resume.status.success(), "{resume:?}");
    assert!(
        String::from_utf8_lossy(&resume.stderr).contains("log digest mismatch"),
        "{}",
        String::from_utf8_lossy(&resume.stderr)
    );
    assert_eq!(read_run_json(&run_dir)["status"], "approved");
    assert!(!run_dir
        .join("artifacts/08-eval-report.attempt-001.json")
        .exists());
}

#[test]
fn approved_eval_real_candidate_cargo_build_allows_only_ignored_generated_output() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(repo.join("src")).expect("repo src");
    init_git_repo(&repo);
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"approved-eval-build\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("Cargo manifest");
    fs::write(repo.join("src/lib.rs"), "pub fn ready() -> bool { true }\n").expect("Cargo source");
    fs::write(repo.join(".gitignore"), "/target\n").expect("Cargo ignore");
    let lock = Command::new("cargo")
        .arg("generate-lockfile")
        .current_dir(&repo)
        .output()
        .expect("generate Cargo lockfile");
    assert!(lock.status.success(), "{lock:?}");
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [cargo check]\n  required:\n    - name: candidate_cargo_check\n      command: cargo check --quiet\n",
    )
    .expect("Cargo eval config");
    commit_all(&repo, "Add buildable candidate");
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn ready() -> bool { true } // dirty\n",
    )
    .expect("pre-existing dirty tracked source");
    fs::write(repo.join("pre-existing.txt"), "preserved dirty bytes\n")
        .expect("pre-existing dirty untracked source");
    let source_before = git_evidence(&repo);
    let runs_root = temp_dir.path().join("runs");
    let run_id = "approved-real-cargo-build";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let ticket = fs::read_to_string(&ticket_path)
        .expect("ticket")
        .replace("    - printf", "    - cargo check");
    fs::write(&ticket_path, ticket).expect("ticket allowlist");
    let approved = run_and_approve_provider_loop(&repo, &ticket_path, &runs_root, run_id);
    let approved_diff = approved["human_approval"]["candidate_diff"]["digest"].clone();
    let candidate = PathBuf::from(
        approved["candidate_workspace"]["path"]
            .as_str()
            .expect("candidate path"),
    );
    let candidate_before = git_evidence(&candidate);

    let resume = seaf_in(&repo)
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
        .expect("resume Cargo evaluation");

    assert!(
        resume.status.success(),
        "{}",
        String::from_utf8_lossy(&resume.stderr)
    );
    let evaluated = read_run_json(&runs_root.join(run_id));
    assert_eq!(evaluated["status"], "eval_passed");
    assert_eq!(
        evaluated["human_approval"]["candidate_diff"]["digest"], approved_diff,
        "the approved staged diff must remain exact"
    );
    assert!(
        candidate.join("target").is_dir(),
        "build output belongs to candidate"
    );
    assert_eq!(
        git_evidence(&candidate),
        candidate_before,
        "ignored build output must not change candidate HEAD/index/staged or tracked worktree diff"
    );
    assert!(
        !repo.join("target").exists(),
        "source must not receive build output"
    );
    assert_eq!(git_evidence(&repo), source_before);
    assert_eq!(
        fs::read(repo.join("pre-existing.txt")).unwrap(),
        b"preserved dirty bytes\n"
    );
}

#[test]
fn approved_eval_detects_lasting_source_worktree_drift_with_preexisting_dirty_state() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    fs::write(repo.join(".gitignore"), "/ignored-output/\n").expect("source ignore rule");
    fs::create_dir_all(repo.join("ignored-output")).expect("ignored source directory");
    let injected = repo.join("ignored-output/eval-mutated");
    fs::write(
        repo.join("seaf.evals.yaml"),
        format!(
            "evals:\n  allow_commands: [touch]\n  required:\n    - name: lasting_source_mutation\n      command: touch {}\n",
            injected.display()
        ),
    )
    .expect("source mutation eval config");
    commit_all(&repo, "Configure ignored source mutation eval");
    let policy = fs::read_to_string(repo.join("seaf.policy.json")).expect("policy");
    fs::write(repo.join("seaf.policy.json"), format!("{policy}\n "))
        .expect("valid dirty tracked source");
    fs::write(
        repo.join("pre-existing-untracked.txt"),
        "original untracked bytes\n",
    )
    .expect("dirty untracked source");
    let original_tracked = fs::read(repo.join("seaf.policy.json")).unwrap();
    let original_untracked = fs::read(repo.join("pre-existing-untracked.txt")).unwrap();
    let runs_root = temp_dir.path().join("runs");
    let run_id = "approved-source-drift";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), true);
    let ticket = fs::read_to_string(&ticket_path)
        .expect("ticket")
        .replace("    - printf", "    - touch");
    fs::write(&ticket_path, ticket).expect("ticket allowlist");
    run_and_approve_provider_loop(&repo, &ticket_path, &runs_root, run_id);

    let resume = seaf_in(&repo)
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
        .expect("resume source mutation evaluation");

    assert!(
        !resume.status.success(),
        "lasting source drift must block publication"
    );
    assert!(String::from_utf8_lossy(&resume.stderr).contains("source worktree authority"));
    let run_dir = runs_root.join(run_id);
    assert_eq!(read_run_json(&run_dir)["status"], "approved");
    assert!(injected.exists(), "the allowed command did execute");
    assert_eq!(
        fs::read(repo.join("seaf.policy.json")).unwrap(),
        original_tracked
    );
    assert_eq!(
        fs::read(repo.join("pre-existing-untracked.txt")).unwrap(),
        original_untracked
    );
    assert!(!run_dir
        .join("artifacts/08-eval-report.attempt-001.json")
        .exists());
    let interrupted = read_tree_bytes(&run_dir);
    let retry = seaf_in(&repo)
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
        .expect("retry source-drift interruption");
    assert!(!retry.status.success());
    assert_eq!(read_tree_bytes(&run_dir), interrupted);
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
    mark_run_completed_for_cleanup_compatibility(&run_dir);
    let before = read_run_json(&run_dir);
    assert_eq!(before["status"], "completed");
    assert_eq!(before["current_step"], "eval_report");
    let legacy = seaf_core::load_loop_run_file(&run_dir.join("run.json"))
        .expect("exact pre-06 Completed run remains loadable");
    for step in [
        seaf_core::LoopStepName::Testing,
        seaf_core::LoopStepName::EvalReport,
    ] {
        let record = legacy
            .steps
            .iter()
            .find(|record| record.name == step)
            .unwrap();
        assert_eq!(record.status, seaf_core::LoopStepStatus::Completed);
        assert!(record.artifact_path.is_none() && record.artifact_digest.is_none());
    }
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
    mark_run_completed_for_cleanup_compatibility(&run_dir);
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
    mark_run_completed_for_cleanup_compatibility(&run_dir);
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
    mark_run_completed_for_cleanup_compatibility(&run_dir);
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
fn provider_loop_requires_and_binds_canonical_repository_eval_config_before_workspace_creation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = repo.join("runs");

    let missing_eval_ticket = write_provider_loop_ticket_without_eval(temp_dir.path(), false);
    let missing = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            missing_eval_ticket.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            "missing-eval-authority",
            "--allow-dirty",
            "--json",
        ])
        .output()
        .expect("run without eval config");
    assert!(!missing.status.success(), "{missing:?}");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("ticket.eval.config"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    assert!(!runs_root.join("missing-eval-authority").exists());

    fs::write(
        repo.join("seaf.evals.yaml"),
        r#"evals:
  required:
    - command: cargo test
      name: tests
  allow_commands:
    - cargo
"#,
    )
    .expect("write eval config");
    let ticket_path =
        write_provider_loop_ticket_with_eval_config(temp_dir.path(), "seaf.evals.yaml", false);
    let output = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            "bound-eval-authority",
            "--allow-dirty",
            "--json",
        ])
        .output()
        .expect("run with eval config");
    assert!(output.status.success(), "{output:?}");

    let run_dir = runs_root.join("bound-eval-authority");
    let snapshot = fs::read(run_dir.join("inputs/eval-config.json")).expect("eval snapshot");
    let parsed = seaf_core::parse_eval_config(
        &fs::read_to_string(repo.join("seaf.evals.yaml")).expect("eval yaml"),
    )
    .expect("parse eval config");
    let expected = canonical_json_bytes(&parsed).expect("canonical eval config");
    assert_eq!(snapshot, expected);
    let run = read_run_json(&run_dir);
    assert_eq!(
        run["input_digests"]["eval_config"],
        canonical_sha256_digest(&parsed).expect("eval config digest")
    );

    fs::write(
        repo.join("seaf.evals.yaml"),
        r#"evals:
  allow_commands:
    - cargo
  required:
    - name: tests
      command: cargo test
"#,
    )
    .expect("rewrite equivalent eval config");
    let reordered = seaf_in(&repo)
        .args([
            "loop",
            "run",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            "reordered-eval-authority",
            "--allow-dirty",
            "--json",
        ])
        .output()
        .expect("run with reordered eval config");
    assert!(reordered.status.success(), "{reordered:?}");
    let reordered_dir = runs_root.join("reordered-eval-authority");
    assert_eq!(
        fs::read(reordered_dir.join("inputs/eval-config.json")).unwrap(),
        snapshot
    );
    assert_eq!(
        read_run_json(&reordered_dir)["input_digests"]["eval_config"],
        run["input_digests"]["eval_config"]
    );
}

#[cfg(unix)]
#[test]
fn provider_loop_rejects_unsafe_or_invalid_eval_authority_without_side_effects() {
    use std::os::unix::fs::symlink;

    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    fs::write(repo.join("malformed.yaml"), "evals: [").expect("malformed eval config");
    fs::create_dir(repo.join("eval-dir")).expect("eval directory");
    fs::create_dir(repo.join("sub")).expect("subdirectory");
    fs::copy(repo.join("seaf.evals.yaml"), repo.join("sub/eval.yaml")).expect("sub eval config");
    fs::create_dir(repo.join("C:")).expect("drive-like directory");
    fs::copy(repo.join("seaf.evals.yaml"), repo.join("C:/eval.yaml"))
        .expect("drive-like eval config");
    fs::copy(repo.join("seaf.evals.yaml"), repo.join("bad\u{1}path.yaml"))
        .expect("control-character eval config");
    let outside = temp_dir.path().join("outside.yaml");
    fs::write(
        &outside,
        "evals:\n  required:\n    - name: tests\n      command: cargo test\n",
    )
    .expect("outside eval config");
    symlink(&outside, repo.join("eval-link.yaml")).expect("eval symlink");
    let absolute = outside.to_str().unwrap().to_string();
    let cases = [
        ("empty", r#""""#),
        ("absolute", absolute.as_str()),
        ("traversal", "../outside.yaml"),
        ("backslash", "sub\\eval.yaml"),
        ("symlink", "eval-link.yaml"),
        ("missing", "missing.yaml"),
        ("directory", "eval-dir"),
        ("malformed", "malformed.yaml"),
        ("dot-segment", "sub/./eval.yaml"),
        ("repeated-slash", "sub//eval.yaml"),
        ("trailing-slash", "seaf.evals.yaml/"),
        ("control", r#""bad\x01path.yaml""#),
        ("drive-prefix", "C:/eval.yaml"),
    ];
    let runs_root = repo.join("runs");
    for (case, configured) in cases {
        fs::create_dir(temp_dir.path().join(case)).expect("ticket fixture directory");
        let ticket = write_provider_loop_ticket_with_eval_config(
            &temp_dir.path().join(case),
            configured,
            false,
        );
        let output = seaf_in(&repo)
            .args([
                "loop",
                "run",
                "--ticket",
                ticket.to_str().unwrap(),
                "--runs-root",
                runs_root.to_str().unwrap(),
                "--run-id",
                case,
                "--allow-dirty",
                "--json",
            ])
            .output()
            .expect("run with invalid eval authority");
        assert!(!output.status.success(), "{case}: {output:?}");
        let diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(diagnostic.contains("eval.config"), "{case}: {diagnostic}");
        assert!(!runs_root.join(case).exists(), "{case} created workspace");
    }
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
    assert_eq!(persisted_run["status"], "awaiting_human_review");
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
        stderr.contains("--rerun-from is retired"),
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
    assert!(stderr.contains("--rerun-from is retired"), "{stderr}");
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
    assert!(stderr.contains("--rerun-from is retired"), "{stderr}");
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
    let revise = seaf_in(&repo)
        .args([
            "loop",
            "revise",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--from-step",
            "research",
            "--actor",
            "operator@example.invalid",
            "--reason",
            "Retry within the durable context cap.",
        ])
        .output()
        .expect("revise at cap");
    assert!(revise.status.success(), "{revise:?}");
    let rerun = seaf_in(&repo)
        .args([
            "loop",
            "rerun",
            "--ticket",
            ticket_path.to_str().unwrap(),
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--base-url",
            &rerun_url,
            "--recovery",
            "1",
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
fn loop_resume_rejects_output_review_rerun_while_awaiting_without_mutation() {
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
    let before_tree = read_tree_bytes(&run_dir);
    let source_before = git_evidence(&repo);
    let candidate = PathBuf::from(before["candidate_workspace"]["path"].as_str().unwrap());
    let candidate_before = git_evidence(&candidate);
    let attempt_one = before["provider_exchange_records"]
        .as_array()
        .expect("provider records")
        .iter()
        .filter(|record| record["step"] == "output_review")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(attempt_one.len(), 2);
    assert!(attempt_one.iter().all(|record| record["step_attempt"] == 1));

    let rerun = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--rerun-from",
            "output-review",
            "--json",
        ])
        .output()
        .expect("reject OutputReview rerun");

    assert!(!rerun.status.success(), "{rerun:?}");
    let stderr = String::from_utf8(rerun.stderr).unwrap();
    assert!(stderr.contains("--rerun-from is retired"), "{stderr}");
    let after = read_run_json(&run_dir);
    assert_eq!(after, before);
    let output_review_records = after["provider_exchange_records"]
        .as_array()
        .expect("provider records")
        .iter()
        .filter(|record| record["step"] == "output_review")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(&output_review_records[..attempt_one.len()], attempt_one);
    assert_eq!(output_review_records.len(), 2);
    assert!(run_dir
        .join("prompts/06-output-review.attempt-001.exchange-001.initial.request.md")
        .exists());
    assert!(!run_dir
        .join("prompts/06-output-review.attempt-002.exchange-001.initial.request.md")
        .exists());
    assert_eq!(read_tree_bytes(&run_dir), before_tree);
    assert_eq!(git_evidence(&repo), source_before);
    assert_eq!(git_evidence(&candidate), candidate_before);
}

#[test]
fn loop_resume_rejects_historical_unapproved_testing_before_ticket_or_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let run_id = "cli-historical-unapproved-testing";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    run_fake_provider(&repo, &ticket_path, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    let mut historical = read_run_json(&run_dir);
    historical["status"] = serde_json::json!("running");
    fs::write(
        run_dir.join("run.json"),
        serde_json::to_vec_pretty(&historical).unwrap(),
    )
    .unwrap();
    let candidate = PathBuf::from(historical["candidate_workspace"]["path"].as_str().unwrap());
    let run_before = read_tree_bytes(&run_dir);
    let source_before = git_evidence(&repo);
    let candidate_before = git_evidence(&candidate);

    let resume = seaf_in(&repo)
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
        .expect("reject historical prefix without ticket");

    assert!(!resume.status.success());
    let stderr = String::from_utf8(resume.stderr).unwrap();
    assert!(stderr.contains("human approval"), "{stderr}");
    assert!(stderr.contains("start a new run"), "{stderr}");
    assert!(!stderr.contains("--ticket is required"), "{stderr}");
    assert_eq!(read_tree_bytes(&run_dir), run_before);
    assert_eq!(git_evidence(&repo), source_before);
    assert_eq!(git_evidence(&candidate), candidate_before);
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
    write_raw_canonical_run_fixture(&run_dir.join("run.json"), &interrupted);

    let exact_output_review = provider_loop_model_responses()
        .pop()
        .expect("OutputReview response");
    let (resume_url, captured) =
        start_recording_fake_ollama_server_sequence(vec![exact_output_review]);
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
    assert_eq!(read_run_json(&run_dir)["status"], "awaiting_human_review");

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
        let (probe, _probe_url) = provider_call_probe();
        let rejected = seaf_in(&repo)
            .args([
                "loop",
                "revise",
                "--runs-root",
                runs_root.to_str().unwrap(),
                "--run-id",
                run_id,
                "--from-step",
                "research",
                "--actor",
                "operator@example.invalid",
                "--reason",
                "Tamper probe.",
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
    write_raw_canonical_run_fixture(&run_dir.join("run.json"), &interrupted);
    fs::write(&context_path, "changed live repository bytes\n").expect("mutate repository");
    let mut resume_responses = vec![agent_response(
        "researcher",
        "Research complete.",
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
fn loop_resume_rejects_mutated_eval_config_before_candidate_or_provider_side_effects() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let run_id = "resume-mutated-eval-config";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let original_eval = fs::read_to_string(repo.join("seaf.evals.yaml")).expect("original eval");
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
            "--allow-dirty",
        ])
        .output()
        .expect("start eval-bound run");
    assert!(run.status.success(), "{run:?}");

    let run_dir = runs_root.join(run_id);
    mark_loop_run_pending_from_analysis(&run_dir.join("run.json"));
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [cargo]\n  required:\n    - name: changed\n      command: cargo check\n",
    )
    .expect("mutate eval config");
    let before = read_tree_bytes(&run_dir);
    let candidate = PathBuf::from(
        read_run_json(&run_dir)["candidate_workspace"]["path"]
            .as_str()
            .unwrap(),
    );
    let candidate_before = git_evidence(&candidate);
    let (probe, probe_url) = provider_call_probe();
    let resume = resume_provider_run(
        &repo,
        &ticket_path,
        &runs_root,
        run_id,
        &["--base-url", &probe_url],
    );

    assert!(!resume.status.success(), "{resume:?}");
    let stderr = String::from_utf8(resume.stderr).expect("stderr");
    assert!(
        stderr.contains("eval config") && stderr.contains("start a new run"),
        "{stderr}"
    );
    assert_eq!(read_tree_bytes(&run_dir), before);
    assert_eq!(git_evidence(&candidate), candidate_before);
    assert_no_provider_call(&probe);

    fs::write(repo.join("seaf.evals.yaml"), &original_eval).expect("restore eval config");
    fs::write(repo.join("alternate.evals.yaml"), &original_eval).expect("alternate eval config");
    let substituted_ticket = temp_dir.path().join("substituted-eval-path-ticket.yaml");
    fs::write(
        &substituted_ticket,
        fs::read_to_string(&ticket_path)
            .unwrap()
            .replace("config: seaf.evals.yaml", "config: alternate.evals.yaml"),
    )
    .expect("substitute eval config path");
    let before_path_substitution = read_tree_bytes(&run_dir);
    let candidate_before_path_substitution = git_evidence(&candidate);
    let (path_probe, path_probe_url) = provider_call_probe();
    let path_resume = resume_provider_run(
        &repo,
        &substituted_ticket,
        &runs_root,
        run_id,
        &["--base-url", &path_probe_url],
    );
    assert!(!path_resume.status.success(), "{path_resume:?}");
    let stderr = String::from_utf8(path_resume.stderr).expect("stderr");
    assert!(
        stderr.contains("ticket") && stderr.contains("start a new run"),
        "{stderr}"
    );
    assert_eq!(read_tree_bytes(&run_dir), before_path_substitution);
    assert_eq!(git_evidence(&candidate), candidate_before_path_substitution);
    assert_no_provider_call(&path_probe);
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
fn loop_resume_rejects_snapshot_holes_and_noncanonical_collisions() {
    for case in [
        "missing-policy",
        "noncanonical-config",
        "missing-eval",
        "noncanonical-eval",
        "substituted-eval",
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let repo = temp_dir.path().join("repo");
        fs::create_dir_all(&repo).expect("repo dir");
        init_git_repo(&repo);
        let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
        let runs_root = repo.join("runs");

        run_fake_provider(&repo, &ticket_path, &runs_root, case, &[]);
        let run_dir = runs_root.join(case);
        mark_loop_run_pending_from_analysis(&run_dir.join("run.json"));
        match case {
            "missing-policy" => {
                let path = run_dir.join("inputs/policy.json");
                fs::remove_file(path).expect("remove policy snapshot");
            }
            "noncanonical-config" => {
                let path = run_dir.join("inputs/config.json");
                let mut bytes = fs::read(&path).expect("config snapshot");
                bytes.push(b'\n');
                fs::write(path, bytes).expect("tamper config snapshot");
            }
            "missing-eval" => {
                let path = run_dir.join("inputs/eval-config.json");
                fs::remove_file(path).expect("remove eval config snapshot");
            }
            "noncanonical-eval" => {
                let path = run_dir.join("inputs/eval-config.json");
                let mut bytes = fs::read(&path).expect("eval config snapshot");
                bytes.push(b'\n');
                fs::write(path, bytes).expect("tamper eval config snapshot");
                fs::remove_file(run_dir.join("context-manifest.json"))
                    .expect("remove scaffold suffix marker");
            }
            "substituted-eval" => {
                let path = run_dir.join("inputs/eval-config.json");
                let bytes = canonical_json_bytes(&serde_json::json!({
                    "evals": {
                        "allow_commands": ["cargo"],
                        "required": [{"name": "substituted", "command": "cargo check"}]
                    }
                }))
                .unwrap();
                fs::write(path, bytes).expect("substitute eval config snapshot");
            }
            _ => unreachable!(),
        }
        let before = read_tree_bytes(&run_dir);

        let resume = resume_provider_run(&repo, &ticket_path, &runs_root, case, &[]);

        assert!(!resume.status.success(), "{resume:?}");
        let stderr = String::from_utf8(resume.stderr).expect("stderr");
        assert!(
            stderr.contains("collision")
                || stderr.contains("exact prefix")
                || stderr.contains("required regular file"),
            "noncanonical snapshot must collide: {stderr}"
        );
        assert_eq!(read_tree_bytes(&run_dir), before);
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
fn loop_resume_rejects_eval_snapshot_digest_mismatch_without_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    let runs_root = repo.join("runs");
    let run_id = "resume-eval-digest-mismatch";
    run_fake_provider(&repo, &ticket_path, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    mark_loop_run_pending_from_analysis(&run_dir.join("run.json"));
    let mut run = read_run_json(&run_dir);
    run["input_digests"]["eval_config"] = serde_json::json!("0".repeat(64));
    fs::write(
        run_dir.join("run.json"),
        serde_json::to_vec_pretty(&run).expect("serialize run"),
    )
    .expect("write mismatched eval digest");
    let before = read_tree_bytes(&run_dir);

    let resume = resume_provider_run(&repo, &ticket_path, &runs_root, run_id, &[]);

    assert!(!resume.status.success());
    let stderr = String::from_utf8(resume.stderr).expect("stderr");
    assert!(
        stderr.contains("eval config digest") && stderr.contains("start a new run"),
        "{stderr}"
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
        &["--policy", policy_path.to_str().unwrap()],
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
fn loop_resume_rerun_from_returns_migration_guidance_before_run_lookup_or_mutation() {
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
            "--rerun-from",
            "research",
        ])
        .output()
        .expect("retired rerun flag");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr");
    assert!(stderr.contains("loop revise"), "{stderr}");
    assert!(stderr.contains("loop rerun --recovery"), "{stderr}");
    assert!(
        !runs_root.exists(),
        "migration guidance must precede run lookup"
    );
}

#[test]
fn loop_revise_and_rerun_publish_the_audited_recovery_cli_contract() {
    for (command, required) in [
        (
            "revise",
            ["--from-step", "--eval-recovery", "--actor", "--reason"].as_slice(),
        ),
        ("rerun", ["--recovery"].as_slice()),
    ] {
        let output = seaf()
            .args(["loop", command, "--help"])
            .output()
            .expect("recovery command help");
        assert!(output.status.success(), "{command}: {output:?}");
        let stdout = String::from_utf8(output.stdout).expect("help stdout");
        for flag in required {
            assert!(
                stdout.contains(flag),
                "{command} help is missing {flag}: {stdout}"
            );
        }
    }
}

#[test]
fn loop_revise_rejects_invalid_evaluation_recovery_grammar_before_workspace_lookup() {
    let temp = tempfile::tempdir().unwrap();
    let runs_root = temp.path().join("runs");
    for args in [
        vec!["--from-step", "testing"],
        vec!["--from-step", "testing", "--eval-recovery", "discard"],
        vec!["--from-step", "output-review", "--eval-recovery", "adopt"],
    ] {
        let mut command = seaf();
        command.args([
            "loop",
            "revise",
            "--run-id",
            "missing-run",
            "--runs-root",
            runs_root.to_str().unwrap(),
        ]);
        command.args(args);
        command.args([
            "--actor",
            "operator@example.invalid",
            "--reason",
            "must reject before mutation",
        ]);
        let output = command.output().unwrap();
        assert!(!output.status.success(), "{output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("eval-recovery"), "{stderr}");
        assert!(!runs_root.exists(), "grammar rejection must precede lookup");
    }
}

#[test]
fn loop_revise_testing_adopt_finalizes_an_interrupted_complete_prefix_without_rerunning() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [printf]\n  required:\n    - name: adoption_probe\n      command: printf adoption-prefix\n",
    )
    .unwrap();
    commit_all(&repo, "Configure adoption probe");
    let runs_root = temp.path().join("runs");
    let run_id = "cli-evaluation-adoption";
    let ticket = write_provider_loop_ticket(temp.path(), true);
    let approved = run_and_approve_provider_loop(&repo, &ticket, &runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    let approved_bytes = fs::read(run_dir.join("run.json")).unwrap();
    let provider_records = approved["provider_exchange_records"].clone();

    let evaluation = seaf_in(&repo)
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
        .unwrap();
    assert!(evaluation.status.success(), "{evaluation:?}");
    let log_path = run_dir.join("artifacts/07-testing.attempt-001.check-001.stdout.log");
    let log_before = fs::read(&log_path).unwrap();
    assert_eq!(log_before, b"adoption-prefix");

    // Simulate interruption after complete prefix publication but before final LoopRun CAS.
    fs::write(run_dir.join("run.json"), approved_bytes).unwrap();
    let adopted = seaf_in(&repo)
        .args([
            "loop",
            "revise",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--from-step",
            "testing",
            "--eval-recovery",
            "adopt",
            "--actor",
            "operator@example.invalid",
            "--reason",
            "adopt complete CLI evaluation prefix",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(adopted.status.success(), "{adopted:?}");
    let output: serde_json::Value = serde_json::from_slice(&adopted.stdout).unwrap();
    assert_eq!(output["command"], "revise");
    assert_eq!(output["status"], "eval_passed");
    assert_eq!(output["current_step"], "eval_report");
    assert_eq!(output["evaluation_attempt"], 1);
    assert_eq!(output["report_disposition"], "verify_existing");
    let final_run = read_run_json(&run_dir);
    assert_eq!(final_run["status"], "eval_passed");
    assert_eq!(final_run["latest_recovery"]["recovery_id"], 1);
    assert_eq!(final_run["provider_exchange_records"], provider_records);
    assert_eq!(fs::read(log_path).unwrap(), log_before);
    assert!(run_dir
        .join("artifacts/recovery-001.source-run.json")
        .is_file());
    assert!(run_dir.join("artifacts/recovery-001.json").is_file());
}

#[test]
fn loop_revise_testing_invalidate_and_rerun_use_no_provider_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    fs::write(
        repo.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [printf]\n  required:\n    - name: retry_probe\n      command: printf recovered\n",
    )
    .unwrap();
    commit_all(&repo, "Configure recovery probe");
    let runs_root = temp.path().join("runs");
    let run_id = "cli-evaluation-invalidation";
    let ticket = write_provider_loop_ticket(temp.path(), true);
    run_and_approve_provider_loop(&repo, &ticket, &runs_root, run_id);
    let run_dir = runs_root.join(run_id);
    let approved_bytes = fs::read(run_dir.join("run.json")).unwrap();

    let evaluation = seaf_in(&repo)
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
        .unwrap();
    assert!(evaluation.status.success(), "{evaluation:?}");
    fs::write(run_dir.join("run.json"), approved_bytes).unwrap();
    for path in [
        "artifacts/07-testing.attempt-001.check-001.stderr.log",
        "artifacts/07-testing.attempt-001.json",
        "artifacts/08-eval-report.attempt-001.json",
    ] {
        fs::remove_file(run_dir.join(path)).unwrap();
    }

    let invalidated = seaf_in(&repo)
        .args([
            "loop",
            "revise",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--from-step",
            "testing",
            "--eval-recovery",
            "invalidate",
            "--actor",
            "operator@example.invalid",
            "--reason",
            "invalidate trailing stdout crash cut",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(invalidated.status.success(), "{invalidated:?}");
    let invalidated_output: serde_json::Value =
        serde_json::from_slice(&invalidated.stdout).unwrap();
    assert_eq!(invalidated_output["invalidated_attempt"], 1);
    assert_eq!(invalidated_output["next_evaluation_attempt"], 2);

    let rerun = seaf_in(&repo)
        .args([
            "loop",
            "rerun",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--recovery",
            "1",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(rerun.status.success(), "{rerun:?}");
    let final_run = read_run_json(&run_dir);
    assert_eq!(final_run["status"], "eval_passed");
    assert_eq!(
        final_run["steps"][6]["artifact_path"],
        "artifacts/07-testing.attempt-002.json"
    );
    assert_eq!(
        fs::read(run_dir.join("artifacts/07-testing.attempt-002.check-001.stdout.log")).unwrap(),
        b"recovered"
    );
}

#[test]
fn loop_revise_is_provider_free_and_exact_rerun_consumes_one_recovery_request() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let ticket = write_provider_loop_ticket(temp.path(), false);
    let run_id = "audited-provider-recovery";
    run_fake_provider(&repo, &ticket, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    let candidate_root = PathBuf::from(
        read_run_json(&run_dir)["candidate_workspace"]["path"]
            .as_str()
            .unwrap(),
    );
    let source_before = git_evidence(&candidate_root);
    let source_repo_before = git_evidence(&repo);
    let source_tracked_before = (
        source_repo_before.0.clone(),
        source_repo_before.2.clone(),
        source_repo_before.3.clone(),
    );
    let fixed_review = fs::read(run_dir.join("artifacts/06-output-review.json")).unwrap();
    let ledger_len = read_run_json(&run_dir)["provider_exchange_records"]
        .as_array()
        .unwrap()
        .len();

    let revised = seaf_in(&repo)
        .args([
            "loop",
            "revise",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--from-step",
            "output-review",
            "--actor",
            "operator@example.invalid",
            "--reason",
            "Reviewer requested one correction.",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(revised.status.success(), "{revised:?}");
    let revised_report: serde_json::Value = serde_json::from_slice(&revised.stdout).unwrap();
    assert_eq!(revised_report["recovery_id"], 1);
    assert_eq!(git_evidence(&candidate_root), source_before);
    assert_eq!(
        fs::read(run_dir.join("artifacts/06-output-review.json")).unwrap(),
        fixed_review
    );
    let reset = read_run_json(&run_dir);
    assert_eq!(reset["status"], "pending");
    assert_eq!(reset["current_step"], "output_review");
    assert_eq!(
        reset["provider_exchange_records"].as_array().unwrap().len(),
        ledger_len
    );
    assert_eq!(reset["latest_recovery"]["recovery_id"], 1);
    assert_eq!(reset["human_approval"], serde_json::Value::Null);
    let reset_bytes = fs::read(run_dir.join("run.json")).unwrap();
    let recovery_bytes = fs::read(run_dir.join("artifacts/recovery-001.json")).unwrap();
    let source_bytes = fs::read(run_dir.join("artifacts/recovery-001.source-run.json")).unwrap();

    let exact_retry = seaf_in(&repo)
        .args([
            "loop",
            "revise",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--from-step",
            "output-review",
            "--actor",
            "operator@example.invalid",
            "--reason",
            "Reviewer requested one correction.",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(exact_retry.status.success(), "{exact_retry:?}");
    assert_eq!(fs::read(run_dir.join("run.json")).unwrap(), reset_bytes);
    assert_eq!(
        fs::read(run_dir.join("artifacts/recovery-001.json")).unwrap(),
        recovery_bytes
    );
    assert_eq!(
        fs::read(run_dir.join("artifacts/recovery-001.source-run.json")).unwrap(),
        source_bytes
    );

    let substituted_retry = seaf_in(&repo)
        .args([
            "loop",
            "revise",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--from-step",
            "output-review",
            "--actor",
            "operator@example.invalid",
            "--reason",
            "Different reason.",
        ])
        .output()
        .unwrap();
    assert!(!substituted_retry.status.success());
    assert_eq!(fs::read(run_dir.join("run.json")).unwrap(), reset_bytes);

    for mutation_target in [
        candidate_root.join("examples/local-loop/evals/fake-provider-smoke.txt"),
        repo.join("seaf.policy.json"),
    ] {
        let original = fs::read(&mutation_target).unwrap();
        let mut mutated = original.clone();
        mutated.extend_from_slice(b"\nrecovery mutation probe\n");
        fs::write(&mutation_target, mutated).unwrap();
        let mutated_retry = seaf_in(&repo)
            .args([
                "loop",
                "revise",
                "--run-id",
                run_id,
                "--runs-root",
                runs_root.to_str().unwrap(),
                "--from-step",
                "output-review",
                "--actor",
                "operator@example.invalid",
                "--reason",
                "Reviewer requested one correction.",
            ])
            .output()
            .unwrap();
        assert!(!mutated_retry.status.success(), "{mutated_retry:?}");
        assert_eq!(fs::read(run_dir.join("run.json")).unwrap(), reset_bytes);
        let mutated_rerun = seaf_in(&repo)
            .args([
                "loop",
                "rerun",
                "--run-id",
                run_id,
                "--runs-root",
                runs_root.to_str().unwrap(),
                "--recovery",
                "1",
                "--ticket",
                ticket.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(!mutated_rerun.status.success(), "{mutated_rerun:?}");
        assert_eq!(fs::read(run_dir.join("run.json")).unwrap(), reset_bytes);
        assert!(!run_dir
            .join("artifacts/06-output-review.attempt-002.exchange-001.initial.request.record.json")
            .exists());
        fs::write(mutation_target, original).unwrap();
    }

    let mut substituted_input = reset.clone();
    substituted_input["input_digests"]["ticket"] = serde_json::json!("0".repeat(64));
    fs::write(
        run_dir.join("run.json"),
        serde_json::to_vec_pretty(&substituted_input).unwrap(),
    )
    .unwrap();
    let input_rerun = seaf_in(&repo)
        .args([
            "loop",
            "rerun",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--recovery",
            "1",
            "--ticket",
            ticket.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!input_rerun.status.success(), "{input_rerun:?}");
    fs::write(run_dir.join("run.json"), &reset_bytes).unwrap();

    let ordinary = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!ordinary.status.success());
    let stderr = String::from_utf8(ordinary.stderr).unwrap();
    assert!(stderr.contains("pending recovery"), "{stderr}");
    assert!(!stderr.contains("--ticket is required"), "{stderr}");

    let attempt_two_prompt = run_dir.join("prompts/06-output-review.attempt-002.prompt.md");
    write_private_run_fixture(&attempt_two_prompt, b"substituted prompt");
    let substituted_prompt = seaf_in(&repo)
        .args([
            "loop",
            "rerun",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--recovery",
            "1",
            "--ticket",
            ticket.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        !substituted_prompt.status.success(),
        "{substituted_prompt:?}"
    );
    assert_eq!(
        read_run_json(&run_dir)["provider_exchange_records"]
            .as_array()
            .unwrap()
            .len(),
        ledger_len
    );
    assert!(!run_dir
        .join("artifacts/06-output-review.attempt-002.exchange-001.initial.request.record.json")
        .exists());
    let pre_request_run_bytes = fs::read(run_dir.join("run.json")).unwrap();
    fs::write(
        &attempt_two_prompt,
        fs::read(run_dir.join("prompts/06-output-review.prompt.md")).unwrap(),
    )
    .unwrap();

    let rerun = seaf_in(&repo)
        .args([
            "loop",
            "rerun",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--recovery",
            "1",
            "--ticket",
            ticket.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(rerun.status.success(), "{rerun:?}");
    let completed = read_run_json(&run_dir);
    assert_eq!(completed["status"], "awaiting_human_review");
    assert_eq!(completed["latest_recovery"]["recovery_id"], 1);
    assert_eq!(git_evidence(&candidate_root), source_before);
    let source_repo_after = git_evidence(&repo);
    assert_eq!(
        (
            source_repo_after.0,
            source_repo_after.2,
            source_repo_after.3,
        ),
        source_tracked_before
    );
    assert_eq!(
        completed["provider_exchange_records"]
            .as_array()
            .unwrap()
            .len(),
        ledger_len + 2
    );
    assert!(!run_dir
        .join("artifacts/06-output-review.attempt-002.rerun-authorization.json")
        .exists());

    fs::write(run_dir.join("run.json"), &pre_request_run_bytes).unwrap();
    for relative in [
        "artifacts/06-output-review.attempt-002.exchange-001.initial.response.record.json",
        "artifacts/06-output-review.attempt-002.json",
        "responses/06-output-review.attempt-002.exchange-001.initial.response.json",
        "responses/06-output-review.attempt-002.raw.txt",
    ] {
        let path = run_dir.join(relative);
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    let first_request_cut_retry = seaf_in(&repo)
        .args([
            "loop",
            "resume",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--ticket",
            ticket.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        first_request_cut_retry.status.success(),
        "{first_request_cut_retry:?}"
    );
    assert_eq!(
        read_run_json(&run_dir)["provider_exchange_records"]
            .as_array()
            .unwrap()
            .len(),
        ledger_len + 2
    );

    let ordinary_after_consumption = seaf_in(&repo)
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
        .unwrap();
    assert!(
        ordinary_after_consumption.status.success(),
        "{ordinary_after_consumption:?}"
    );

    let awaiting_after_recovery = read_run_json(&run_dir);
    let candidate_diff = awaiting_after_recovery["candidate_workspace"]["candidate_diff_digest"]
        .as_str()
        .unwrap();
    let starting_head = awaiting_after_recovery["candidate_workspace"]["starting_head"]
        .as_str()
        .unwrap();
    let approval = seaf_in(&repo)
        .args([
            "loop",
            "approve",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "recovery-reviewer@example.invalid",
            "--confirm-candidate-diff",
            candidate_diff,
            "--confirm-target-head",
            starting_head,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(approval.status.success(), "{approval:?}");
    let approved = read_run_json(&run_dir);
    assert_eq!(approved["status"], "approved");
    assert_eq!(approved["latest_recovery"]["recovery_id"], 1);

    let second = seaf_in(&repo)
        .args([
            "loop",
            "revise",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--from-step",
            "output-review",
            "--actor",
            "operator@example.invalid",
            "--reason",
            "Reviewer requested a second correction.",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(second.status.success(), "{second:?}");
    let second_report: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second_report["recovery_id"], 2);
    let second_recovery: serde_json::Value =
        serde_json::from_slice(&fs::read(run_dir.join("artifacts/recovery-002.json")).unwrap())
            .unwrap();
    assert_eq!(second_recovery["previous_recovery"]["recovery_id"], 1);

    let pending_second_bytes = fs::read(run_dir.join("run.json")).unwrap();
    fs::write(
        run_dir.join("artifacts/recovery-001.json"),
        b"{\"tampered\":true}",
    )
    .unwrap();
    let tampered_history_retry = seaf_in(&repo)
        .args([
            "loop",
            "revise",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--from-step",
            "output-review",
            "--actor",
            "operator@example.invalid",
            "--reason",
            "Reviewer requested a second correction.",
        ])
        .output()
        .unwrap();
    assert!(!tampered_history_retry.status.success());
    assert_eq!(
        fs::read(run_dir.join("run.json")).unwrap(),
        pending_second_bytes
    );
}

#[test]
fn loop_revise_adopts_exact_source_and_recovery_publication_crash_cuts() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let ticket = write_provider_loop_ticket(temp.path(), false);
    let run_id = "audited-recovery-publication-cuts";
    run_fake_provider(&repo, &ticket, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    let source_run_bytes = fs::read(run_dir.join("run.json")).unwrap();
    let revise = || {
        seaf_in(&repo)
            .args([
                "loop",
                "revise",
                "--run-id",
                run_id,
                "--runs-root",
                runs_root.to_str().unwrap(),
                "--from-step",
                "output-review",
                "--actor",
                "operator@example.invalid",
                "--reason",
                "Crash-cut retry.",
                "--json",
            ])
            .output()
            .unwrap()
    };

    let initial = revise();
    assert!(initial.status.success(), "{initial:?}");
    let source_snapshot = fs::read(run_dir.join("artifacts/recovery-001.source-run.json")).unwrap();

    fs::write(run_dir.join("run.json"), &source_run_bytes).unwrap();
    fs::remove_file(run_dir.join("artifacts/recovery-001.json")).unwrap();
    let after_source_publish = revise();
    assert!(
        after_source_publish.status.success(),
        "{after_source_publish:?}"
    );
    assert_eq!(
        fs::read(run_dir.join("artifacts/recovery-001.source-run.json")).unwrap(),
        source_snapshot
    );
    let recovery = fs::read(run_dir.join("artifacts/recovery-001.json")).unwrap();

    fs::write(run_dir.join("run.json"), &source_run_bytes).unwrap();
    let after_recovery_publish = revise();
    assert!(
        after_recovery_publish.status.success(),
        "{after_recovery_publish:?}"
    );
    assert_eq!(
        fs::read(run_dir.join("artifacts/recovery-001.json")).unwrap(),
        recovery
    );
    assert_eq!(read_run_json(&run_dir)["latest_recovery"]["recovery_id"], 1);
}

#[test]
fn loop_revise_rejects_applied_earlier_steps_and_any_factual_evaluation_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let ticket = write_provider_loop_ticket(temp.path(), false);
    let run_id = "audited-recovery-eligibility";
    run_fake_provider(&repo, &ticket, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    let before = read_tree_bytes(&run_dir);

    let applied_earlier_step = seaf_in(&repo)
        .args([
            "loop",
            "revise",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--from-step",
            "development",
            "--actor",
            "operator@example.invalid",
            "--reason",
            "Invalid earlier-step retry.",
        ])
        .output()
        .unwrap();
    assert!(!applied_earlier_step.status.success());
    assert_eq!(read_tree_bytes(&run_dir), before);

    write_private_run_fixture(
        run_dir.join("artifacts/07-testing.orphan.json"),
        b"factual evaluation prefix",
    );
    let run_before_prefix_rejection = fs::read(run_dir.join("run.json")).unwrap();
    let evaluation_prefix = seaf_in(&repo)
        .args([
            "loop",
            "revise",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--from-step",
            "output-review",
            "--actor",
            "operator@example.invalid",
            "--reason",
            "Evaluation prefix must reject.",
        ])
        .output()
        .unwrap();
    assert!(!evaluation_prefix.status.success());
    let stderr = String::from_utf8(evaluation_prefix.stderr).unwrap();
    assert!(stderr.contains("evaluation prefix"), "{stderr}");
    assert_eq!(
        fs::read(run_dir.join("run.json")).unwrap(),
        run_before_prefix_rejection
    );
}

#[test]
fn loop_revise_failed_development_clears_only_current_policy_and_preserves_history() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let policy = repo.join("reject-development.json");
    write_policy(
        &policy,
        &["examples/local-loop/evals/fake-provider-smoke.txt"],
        &[],
    );
    let ticket = write_provider_loop_ticket(temp.path(), true);
    let runs_root = repo.join("runs");
    let run_id = "failed-development-recovery";
    run_fake_provider(
        &repo,
        &ticket,
        &runs_root,
        run_id,
        &["--policy", policy.to_str().unwrap()],
    );
    let run_dir = runs_root.join(run_id);
    let failed = read_run_json(&run_dir);
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["current_step"], "development");
    assert!(failed["candidate_workspace"]["patch_transaction"].is_null());
    assert_eq!(failed["policy_decisions"].as_array().unwrap().len(), 1);
    let ledger = failed["provider_exchange_records"].clone();
    let historical_artifacts = read_tree_bytes(&run_dir.join("artifacts"));

    let revised = seaf_in(&repo)
        .args([
            "loop",
            "revise",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--from-step",
            "development",
            "--actor",
            "operator@example.invalid",
            "--reason",
            "Retry rejected Development with revised instructions.",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(revised.status.success(), "{revised:?}");
    let reset = read_run_json(&run_dir);
    assert_eq!(reset["status"], "pending");
    assert_eq!(reset["current_step"], "development");
    assert!(reset["policy_decisions"].as_array().unwrap().is_empty());
    assert_eq!(reset["provider_exchange_records"], ledger);
    let after_artifacts = read_tree_bytes(&run_dir.join("artifacts"));
    for (path, bytes) in historical_artifacts {
        let preserved = after_artifacts
            .iter()
            .find(|(candidate, _)| candidate == &path)
            .map(|(_, bytes)| bytes);
        assert_eq!(preserved, Some(&bytes), "{path:?}");
    }
}

#[test]
fn loop_revise_research_replays_failed_development_downstream_at_exact_attempt_two() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let policy = repo.join("reject-development-replay.json");
    write_policy(
        &policy,
        &["examples/local-loop/evals/fake-provider-smoke.txt"],
        &[],
    );
    let ticket = write_provider_loop_ticket(temp.path(), true);
    let runs_root = repo.join("runs");
    let run_id = "failed-development-research-replay";
    run_fake_provider(
        &repo,
        &ticket,
        &runs_root,
        run_id,
        &["--policy", policy.to_str().unwrap()],
    );
    let run_dir = runs_root.join(run_id);
    let failed = read_run_json(&run_dir);
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["current_step"], "development");
    let old_ledger = failed["provider_exchange_records"]
        .as_array()
        .unwrap()
        .clone();
    let old_artifacts = read_tree_bytes(&run_dir.join("artifacts"));
    let source_before = git_evidence(&repo);
    let source_tracked_before = (
        source_before.0.clone(),
        source_before.2.clone(),
        source_before.3.clone(),
    );

    let revised = seaf_in(&repo)
        .args([
            "loop",
            "revise",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--from-step",
            "research",
            "--actor",
            "operator@example.invalid",
            "--reason",
            "Replay the provider chain from Research.",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(revised.status.success(), "{revised:?}");

    let rerun = seaf_in(&repo)
        .args([
            "loop",
            "rerun",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--recovery",
            "1",
            "--ticket",
            ticket.to_str().unwrap(),
            "--policy",
            policy.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(rerun.status.success(), "{rerun:?}");

    let replayed = read_run_json(&run_dir);
    assert_eq!(replayed["status"], "failed");
    assert_eq!(replayed["current_step"], "development");
    let ledger = replayed["provider_exchange_records"].as_array().unwrap();
    assert_eq!(&ledger[..old_ledger.len()], old_ledger.as_slice());
    for step in [
        "research",
        "analysis",
        "spec_creation",
        "spec_review",
        "development",
    ] {
        let attempt_two = ledger
            .iter()
            .filter(|record| record["step"] == step && record["step_attempt"] == 2)
            .collect::<Vec<_>>();
        assert_eq!(
            attempt_two.len(),
            2,
            "{step} must have request and response attempt 2"
        );
    }
    assert!(!ledger
        .iter()
        .any(|record| record["step"] == "output_review" && record["step_attempt"] == 2));

    let artifacts = read_tree_bytes(&run_dir.join("artifacts"));
    for (path, bytes) in old_artifacts {
        let preserved = artifacts
            .iter()
            .find(|(candidate, _)| candidate == &path)
            .map(|(_, bytes)| bytes);
        assert_eq!(
            preserved,
            Some(&bytes),
            "historical artifact changed: {path:?}"
        );
    }
    assert!(!artifacts.iter().any(|(path, _)| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".rerun-authorization.json"))
    }));
    let source_after = git_evidence(&repo);
    assert_eq!(
        (source_after.0, source_after.2, source_after.3),
        source_tracked_before
    );
}

#[test]
fn competing_revise_commands_publish_exactly_one_recovery_cas_winner() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let ticket = write_provider_loop_ticket(temp.path(), false);
    let runs_root = repo.join("runs");
    let run_id = "competing-revise-cas";
    run_fake_provider(&repo, &ticket, &runs_root, run_id, &[]);

    let spawn_revise = |reason: &'static str| {
        let mut command = seaf_in(&repo);
        command.args([
            "loop",
            "revise",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--from-step",
            "output-review",
            "--actor",
            "operator@example.invalid",
            "--reason",
            reason,
            "--json",
        ]);
        command.spawn().unwrap()
    };
    let first = spawn_revise("Competing reason A.");
    let second = spawn_revise("Competing reason B.");
    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();
    assert_ne!(first.status.success(), second.status.success());

    let run_dir = runs_root.join(run_id);
    let run = read_run_json(&run_dir);
    assert_eq!(run["latest_recovery"]["recovery_id"], 1);
    assert!(!run_dir.join("artifacts/recovery-002.json").exists());
    let recovery: serde_json::Value =
        serde_json::from_slice(&fs::read(run_dir.join("artifacts/recovery-001.json")).unwrap())
            .unwrap();
    let expected_reason = if first.status.success() {
        "Competing reason A."
    } else {
        "Competing reason B."
    };
    assert_eq!(recovery["reason"], expected_reason);
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

    let human = seaf()
        .args([
            "loop",
            "status",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
        ])
        .output()
        .expect("human legacy status");
    assert!(human.status.success());
    let stdout = String::from_utf8(human.stdout).unwrap();
    assert!(stdout.contains("status Completed"), "{stdout}");
}

#[test]
fn loop_inspect_is_factual_and_byte_identical_in_json_and_human_modes() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let run_id = "inspect-provider-run";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    run_fake_provider(&repo, &ticket_path, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    fs::write(
        run_dir.join("artifacts/07-testing.attempt-002.json"),
        b"historical testing evidence",
    )
    .expect("historical testing prefix");
    fs::write(
        run_dir.join("artifacts/08-eval-report.json"),
        b"historical eval prefix",
    )
    .expect("historical eval prefix");
    let before = read_tree_bytes(&run_dir);
    let persisted = read_run_json(&run_dir);
    let candidate = PathBuf::from(persisted["candidate_workspace"]["path"].as_str().unwrap());
    let candidate_before = read_tree_bytes(&candidate);
    let source_before = git_evidence(&repo);

    let json = seaf_in(&repo)
        .args([
            "loop",
            "inspect",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("inspect JSON");
    assert!(json.status.success(), "{json:?}");
    assert_eq!(read_tree_bytes(&run_dir), before);
    assert_eq!(read_tree_bytes(&candidate), candidate_before);
    assert_eq!(git_evidence(&repo), source_before);
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).expect("inspect report");
    assert_eq!(report["command"], "inspect");
    assert_eq!(report["run_id"], run_id);
    assert_eq!(report["integrity"], "verified");
    assert!(report["run_digest"]
        .as_str()
        .is_some_and(|value| value.len() == 64));
    assert_eq!(report["candidate"]["lifecycle"], "active");
    assert!(report["input_digests"]["ticket"]["verification"] == "verified");
    assert!(report["provider_attempts"]
        .as_array()
        .is_some_and(|items| !items.is_empty()));
    assert_eq!(report["evaluation_prefix"].as_array().unwrap().len(), 2);
    assert_eq!(report["steps"][0]["artifact_history"][0]["attempt"], 1);
    assert_eq!(
        report["steps"][0]["artifact_history"][0]["classification"],
        "current"
    );
    let rendered = String::from_utf8(json.stdout).unwrap();
    for secret in [
        "Research complete.",
        "Analyze the repository",
        "raw_response",
    ] {
        assert!(
            !rendered.contains(secret),
            "inspect leaked provider body: {secret}"
        );
    }

    let human = seaf_in(&repo)
        .args([
            "loop",
            "inspect",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
        ])
        .output()
        .expect("inspect human");
    assert!(human.status.success(), "{human:?}");
    assert_eq!(read_tree_bytes(&run_dir), before);
    assert_eq!(read_tree_bytes(&candidate), candidate_before);
    assert_eq!(git_evidence(&repo), source_before);
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("canonical run digest"), "{human}");
    assert!(human.contains("provider attempts"), "{human}");
    assert!(human.contains("evaluation prefix"), "{human}");
    assert!(!human.contains("Research complete."), "{human}");
}

#[test]
fn loop_inspect_reports_bounded_tamper_classifications_without_repair_or_mutation() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let run_id = "inspect-tampered-run";
    let ticket_path = write_provider_loop_ticket(temp_dir.path(), false);
    run_fake_provider(&repo, &ticket_path, &runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    fs::write(run_dir.join("inputs/ticket.json"), b"tampered input").unwrap();
    let run = read_run_json(&run_dir);
    let ledger_path = run["provider_exchange_records"][0]["path"]
        .as_str()
        .unwrap();
    fs::write(run_dir.join(ledger_path), b"tampered ledger").unwrap();
    let artifact_path = run["steps"][0]["artifact_path"].as_str().unwrap();
    fs::write(run_dir.join(artifact_path), b"tampered artifact").unwrap();
    let candidate = PathBuf::from(run["candidate_workspace"]["path"].as_str().unwrap());
    fs::write(candidate.join("README.md"), "candidate tamper\n").unwrap();
    let add = Command::new("git")
        .args(["add", "README.md"])
        .current_dir(&candidate)
        .output()
        .expect("stage candidate tamper");
    assert!(add.status.success(), "{add:?}");
    let before = read_tree_bytes(&run_dir);

    let output = seaf_in(&repo)
        .args([
            "loop",
            "inspect",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("inspect tamper");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(read_tree_bytes(&run_dir), before);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["integrity"], "degraded");
    assert_eq!(
        report["input_digests"]["ticket"]["verification"],
        "tampered"
    );
    assert_eq!(report["candidate"]["verification"], "tampered");
    assert!(report["provider_attempts"].to_string().contains("tampered"));
    assert!(report["steps"].to_string().contains("tampered"));
    assert!(report["integrity_messages"]
        .as_array()
        .is_some_and(|items| items.len() >= 4));
}

#[test]
fn loop_inspect_rejects_unsafe_run_id_and_run_directory_without_writing() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let runs_root = temp_dir.path().join("runs");
    fs::create_dir_all(&runs_root).unwrap();
    let before = read_tree_bytes(&runs_root);

    let unsafe_id = seaf()
        .args([
            "loop",
            "inspect",
            "--run-id",
            "../escape",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("unsafe id");
    assert!(!unsafe_id.status.success());
    assert_eq!(read_tree_bytes(&runs_root), before);

    #[cfg(unix)]
    {
        let outside = temp_dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, runs_root.join("linked")).unwrap();
        let before = read_tree_bytes(&outside);
        let unsafe_directory = seaf()
            .args([
                "loop",
                "inspect",
                "--run-id",
                "linked",
                "--runs-root",
                runs_root.to_str().unwrap(),
            ])
            .output()
            .expect("unsafe directory");
        assert!(!unsafe_directory.status.success());
        assert_eq!(read_tree_bytes(&outside), before);
    }
}

#[cfg(unix)]
#[test]
fn loop_inspect_never_executes_candidate_filters_fsmonitor_or_inherited_git_config() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let ticket = write_provider_loop_ticket(temp.path(), false);
    run_fake_provider(&repo, &ticket, &runs_root, "inspect-hostile-git", &[]);
    let run_dir = runs_root.join("inspect-hostile-git");
    let run = read_run_json(&run_dir);
    let candidate = PathBuf::from(run["candidate_workspace"]["path"].as_str().unwrap());
    let marker = temp.path().join("git-side-effect");
    let script = temp.path().join("hostile-git.sh");
    fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(candidate.join(".gitattributes"), "*.md filter=hostile\n").unwrap();
    let config = Command::new("git")
        .args(["config", "filter.hostile.clean", script.to_str().unwrap()])
        .current_dir(&candidate)
        .output()
        .unwrap();
    assert!(config.status.success());
    let config = Command::new("git")
        .args(["config", "filter.hostile.process", script.to_str().unwrap()])
        .current_dir(&candidate)
        .output()
        .unwrap();
    assert!(config.status.success());
    let config = Command::new("git")
        .args(["config", "core.fsmonitor", script.to_str().unwrap()])
        .current_dir(&candidate)
        .output()
        .unwrap();
    assert!(config.status.success());
    fs::write(candidate.join("README.md"), "unstaged hostile diff\n").unwrap();
    let _ = fs::remove_file(&marker);
    let run_before = read_tree_bytes(&run_dir);
    let candidate_before = read_tree_bytes(&candidate);
    let source_policy = fs::read(repo.join("seaf.policy.json")).unwrap();

    let output = seaf_in(&repo)
        .args([
            "loop",
            "inspect",
            "--run-id",
            "inspect-hostile-git",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.fsmonitor")
        .env("GIT_CONFIG_VALUE_0", script.to_str().unwrap())
        .env("GIT_DIR", temp.path().join("redirected-git-dir"))
        .output()
        .expect("inspect hostile Git");

    assert!(output.status.success(), "{output:?}");
    assert!(
        !marker.exists(),
        "inspection executed hostile Git configuration"
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["candidate"]["verification"], "tampered");
    assert_eq!(read_tree_bytes(&run_dir), run_before);
    assert_eq!(read_tree_bytes(&candidate), candidate_before);
    assert_eq!(
        fs::read(repo.join("seaf.policy.json")).unwrap(),
        source_policy
    );
}

#[test]
fn loop_inspect_degrades_a_canonical_provider_record_with_a_broken_global_chain() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    let runs_root = repo.join("runs");
    let ticket = write_provider_loop_ticket(temp.path(), false);
    run_fake_provider(&repo, &ticket, &runs_root, "inspect-chain", &[]);
    let run_dir = runs_root.join("inspect-chain");
    let mut run = read_run_json(&run_dir);
    let path = run["provider_exchange_records"][1]["path"]
        .as_str()
        .unwrap()
        .to_string();
    let mut record: serde_json::Value =
        serde_json::from_slice(&fs::read(run_dir.join(&path)).unwrap()).unwrap();
    record["previous_record_digest"] = serde_json::json!("f".repeat(64));
    let bytes = canonical_json_bytes(&record).unwrap();
    fs::write(run_dir.join(&path), &bytes).unwrap();
    run["provider_exchange_records"][1]["digest"] =
        serde_json::json!(canonical_sha256_digest(&record).unwrap());
    fs::write(
        run_dir.join("run.json"),
        serde_json::to_vec_pretty(&run).unwrap(),
    )
    .unwrap();
    let before = read_tree_bytes(&run_dir);

    let output = seaf_in(&repo)
        .args([
            "loop",
            "inspect",
            "--run-id",
            "inspect-chain",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(read_tree_bytes(&run_dir), before);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["integrity"], "degraded");
    assert!(report["provider_attempts"].to_string().contains("tampered"));
    assert!(!String::from_utf8(output.stdout)
        .unwrap()
        .contains("Research complete."));
}

#[test]
fn loop_inspect_caps_large_inventories_deterministically_in_both_modes() {
    let temp = tempfile::tempdir().expect("temp dir");
    let runs_root = temp.path().join("runs");
    let smoke = seaf()
        .args([
            "loop",
            "smoke",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(smoke.status.success());
    let smoke: serde_json::Value = serde_json::from_slice(&smoke.stdout).unwrap();
    let run_id = smoke["run_id"].as_str().unwrap();
    let run_dir = runs_root.join(run_id);
    for index in 0..100 {
        fs::write(
            run_dir.join(format!("artifacts/07-testing.extra-{index:03}.json")),
            format!("item {index}"),
        )
        .unwrap();
    }
    let before = read_tree_bytes(&run_dir);
    let invoke = |json: bool| {
        let mut command = seaf();
        command.args([
            "loop",
            "inspect",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
        ]);
        if json {
            command.arg("--json");
        }
        command.output().unwrap()
    };
    let json = invoke(true);
    assert!(json.status.success(), "{json:?}");
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(report["evaluation_prefix"].as_array().unwrap().len(), 64);
    assert_eq!(
        report["evaluation_prefix"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["classification"] == "current")
            .count(),
        2,
        "current Testing and EvalReport authority must displace historical prefix entries"
    );
    assert_eq!(report["bounds"]["evaluation_prefix_total"], 102);
    assert_eq!(report["bounds"]["evaluation_prefix_truncated"], 38);
    let human = invoke(false);
    assert!(human.status.success(), "{human:?}");
    assert!(String::from_utf8(human.stdout)
        .unwrap()
        .contains("evaluation prefix: showing 64 of 102"));
    let human = String::from_utf8(invoke(false).stdout).unwrap();
    assert!(human.contains("bounds: provider attempts"));
    assert!(human.contains("provider exchanges"));
    assert!(human.contains("artifact history"));
    assert!(human.contains("integrity messages"));
    assert!(human.contains("ambiguity messages"));
    assert_eq!(read_tree_bytes(&run_dir), before);

    let persisted = read_run_json(&run_dir);
    for step in persisted["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|step| matches!(step["name"].as_str(), Some("testing" | "eval_report")))
    {
        fs::remove_file(run_dir.join(step["artifact_path"].as_str().unwrap())).unwrap();
    }
    let missing_before = read_tree_bytes(&run_dir);
    let missing = invoke(true);
    assert!(missing.status.success(), "{missing:?}");
    let missing: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(missing["bounds"]["evaluation_prefix_total"], 102);
    assert_eq!(
        missing["evaluation_prefix"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["classification"] == "missing")
            .count(),
        2
    );
    assert_eq!(read_tree_bytes(&run_dir), missing_before);
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
fn eval_run_classifies_obvious_secret_before_persisted_log_cap() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let config_path = temp_dir.path().join("seaf.evals.yaml");
    let report_path = temp_dir.path().join("eval-report.json");
    let script_path = temp_dir.path().join("emit-capped-secret");
    write_executable_script(
        &script_path,
        "#!/bin/sh\nprintf 'ok sk-proj-exampleSensitiveToken1234567890\\n'\n",
    );
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - {command}
  required:
    - name: capped_secret
      command: "{command}"
      max_output_bytes: 12
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
    let stdout_log = fs::read_to_string(temp_dir.path().join("logs/capped_secret.stdout.log"))
        .expect("stdout log");
    assert!(stdout_log.contains("[REDA"), "{stdout_log}");
    assert!(stdout_log.len() <= 12, "{stdout_log}");
    assert!(!stdout_log.contains("sk-proj-"), "{stdout_log}");
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

#[test]
fn eval_run_prevalidates_sanitized_log_name_collisions_before_executing_commands() {
    for (case, first_name, second_name, expected_error) in [
        (
            "exact duplicate",
            "same",
            "same",
            "duplicate eval check name",
        ),
        (
            "sanitization collision",
            "same/name",
            "same?name",
            "log name collision",
        ),
        (
            "case-folded collision",
            "Same",
            "same",
            "log name collision",
        ),
    ] {
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
    - name: "{first_name}"
      command: "touch {marker}"
    - name: "{second_name}"
      command: "printf ok"
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

        assert!(!output.status.success(), "{case}: {output:?}");
        let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
        assert!(stderr.contains(expected_error), "{case}: {stderr}");
        assert!(
            !marker_path.exists(),
            "{case}: no eval command should execute"
        );
        assert!(
            !report_path.exists(),
            "{case}: colliding eval should not write report"
        );
        assert!(
            !temp_dir.path().join("logs").exists(),
            "{case}: colliding eval should not create a log directory"
        );
    }
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
    write_provider_loop_ticket_with_eval_config(root, "seaf.evals.yaml", apply_patch)
}

fn write_provider_loop_ticket_without_eval(root: &Path, apply_patch: bool) -> PathBuf {
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

fn write_provider_loop_ticket_with_eval_config(
    root: &Path,
    eval_config: &str,
    apply_patch: bool,
) -> PathBuf {
    let ticket_path = write_provider_loop_ticket_without_eval(root, apply_patch);
    let mut ticket = fs::read_to_string(&ticket_path).expect("read provider loop ticket");
    ticket.push_str(&format!("eval:\n  config: {eval_config}\n"));
    fs::write(&ticket_path, ticket).expect("write provider loop ticket eval config");
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

fn mark_run_completed_for_cleanup_compatibility(run_dir: &Path) {
    let mut run = read_run_json(run_dir);
    run["status"] = serde_json::json!("completed");
    run["current_step"] = serde_json::json!("eval_report");
    for record in run["steps"].as_array_mut().unwrap() {
        if matches!(record["name"].as_str(), Some("testing" | "eval_report")) {
            record["status"] = serde_json::json!("completed");
            record.as_object_mut().unwrap().remove("artifact_path");
            record.as_object_mut().unwrap().remove("artifact_digest");
        }
    }
    fs::write(
        run_dir.join("run.json"),
        serde_json::to_vec_pretty(&run).unwrap(),
    )
    .unwrap();
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

fn run_and_approve_provider_loop(
    repo: &Path,
    ticket_path: &Path,
    runs_root: &Path,
    run_id: &str,
) -> serde_json::Value {
    run_fake_provider(repo, ticket_path, runs_root, run_id, &[]);
    let run_dir = runs_root.join(run_id);
    let awaiting = read_run_json(&run_dir);
    let diff = awaiting["candidate_workspace"]["candidate_diff_digest"]
        .as_str()
        .expect("candidate diff")
        .to_string();
    let head = awaiting["candidate_workspace"]["starting_head"]
        .as_str()
        .expect("starting HEAD")
        .to_string();
    let approval = seaf_in(repo)
        .args([
            "loop",
            "approve",
            "--run-id",
            run_id,
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--reviewer",
            "safety-reviewer@example.invalid",
            "--confirm-candidate-diff",
            &diff,
            "--confirm-target-head",
            &head,
            "--json",
        ])
        .output()
        .expect("approve provider loop");
    assert!(
        approval.status.success(),
        "{}",
        String::from_utf8_lossy(&approval.stderr)
    );
    let approved = read_run_json(&run_dir);
    assert_eq!(approved["status"], "approved");
    approved
}

fn run_approve_and_evaluate_provider_loop(
    repo: &Path,
    ticket_path: &Path,
    runs_root: &Path,
    run_id: &str,
) -> serde_json::Value {
    run_and_approve_provider_loop(repo, ticket_path, runs_root, run_id);
    let evaluated = seaf_in(repo)
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
        .expect("evaluate approved provider loop");
    assert!(
        evaluated.status.success(),
        "{}",
        String::from_utf8_lossy(&evaluated.stderr)
    );
    let run = read_run_json(&runs_root.join(run_id));
    assert_eq!(run["status"], "eval_passed");
    run
}

fn promotion_intent_json(evaluated: &serde_json::Value, reviewer: &str) -> serde_json::Value {
    let step_reference = |name: &str| {
        let step = evaluated["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["name"] == name)
            .unwrap();
        serde_json::json!({
            "path": step["artifact_path"],
            "digest": step["artifact_digest"],
        })
    };
    serde_json::json!({
        "schema_version": 1,
        "run_id": evaluated["run_id"],
        "reviewer": reviewer,
        "started_at": evaluated["updated_at"],
        "candidate_diff": evaluated["human_approval"]["candidate_diff"],
        "testing_evidence": step_reference("testing"),
        "eval_report": step_reference("eval_report"),
        "policy_decision_digest": evaluated["human_approval"]["policy_decision_digest"],
        "target_head": evaluated["human_approval"]["starting_head"],
        "eval_passed_run_digest": canonical_sha256_digest(evaluated).unwrap(),
    })
}

fn open_repository_operation_lock(evaluated: &serde_json::Value) -> fs::File {
    let git_common_dir = PathBuf::from(
        evaluated["candidate_workspace"]["git_common_dir"]
            .as_str()
            .expect("candidate Git common directory"),
    );
    let canonical = git_common_dir
        .canonicalize()
        .expect("canonical Git common directory");
    assert_eq!(canonical, git_common_dir);
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let path = std::env::temp_dir()
        .join("seaf-candidates")
        .join(".repository-operation-locks")
        .join(digest)
        .join(".repository-operation.lock");
    open_repository_operation_lock_path(&path).expect("repository operation lock")
}

fn open_repository_operation_lock_path(path: &Path) -> std::io::Result<fs::File> {
    for directory in path
        .ancestors()
        .skip(1)
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        ensure_private_test_directory(directory)?;
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    match options.create_new(true).open(path) {
        Ok(file) => validate_private_test_lock(path, file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut existing = fs::OpenOptions::new();
            existing.read(true).write(true);
            #[cfg(unix)]
            existing.custom_flags(libc::O_NOFOLLOW);
            validate_private_test_lock(path, existing.open(path)?)
        }
        Err(error) => Err(error),
    }
}

fn ensure_private_test_directory(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_test_directory(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            builder.mode(0o700);
            match builder.create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            let metadata = fs::symlink_metadata(path)?;
            validate_private_test_directory(path, &metadata)
        }
        Err(error) => Err(error),
    }
}

fn validate_private_test_directory(path: &Path, metadata: &fs::Metadata) -> std::io::Result<()> {
    #[cfg(unix)]
    if metadata.file_type().is_symlink() || !metadata.is_dir() || metadata.mode() & 0o7777 != 0o700
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "test repository lock directory is not a real 0700 directory: {}",
                path.display()
            ),
        ));
    }
    #[cfg(not(unix))]
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "test repository lock directory is unsafe: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn validate_private_test_lock(path: &Path, file: fs::File) -> std::io::Result<fs::File> {
    let opened = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    #[cfg(unix)]
    if !opened.is_file()
        || current.file_type().is_symlink()
        || !current.is_file()
        || opened.mode() & 0o7777 != 0o600
        || current.mode() & 0o7777 != 0o600
        || opened.nlink() != 1
        || current.nlink() != 1
        || opened.dev() != current.dev()
        || opened.ino() != current.ino()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "test repository lock is not the same real single-link 0600 file: {}",
                path.display()
            ),
        ));
    }
    #[cfg(not(unix))]
    if !opened.is_file() || current.file_type().is_symlink() || !current.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("test repository lock is unsafe: {}", path.display()),
        ));
    }
    Ok(file)
}

#[cfg(unix)]
#[test]
fn repository_operation_lock_test_helper_rejects_unsafe_existing_layout_without_mutation() {
    for case in ["broad-dir", "symlink-dir", "broad-lock", "symlink-lock"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("authority");
        let namespace = root.join("locks");
        let digest = namespace.join("digest");
        let lock = digest.join("lock");
        match case {
            "broad-dir" => {
                fs::create_dir(&root).unwrap();
                fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
            }
            "symlink-dir" => {
                let outside = temp.path().join("outside");
                fs::create_dir(&outside).unwrap();
                fs::write(outside.join("sentinel"), b"outside").unwrap();
                symlink(&outside, &root).unwrap();
            }
            "broad-lock" | "symlink-lock" => {
                for directory in [&root, &namespace, &digest] {
                    let mut builder = fs::DirBuilder::new();
                    builder.mode(0o700).create(directory).unwrap();
                }
                if case == "broad-lock" {
                    fs::write(&lock, b"broad").unwrap();
                    fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
                } else {
                    let outside = temp.path().join("outside-lock");
                    fs::write(&outside, b"outside").unwrap();
                    symlink(&outside, &lock).unwrap();
                }
            }
            _ => unreachable!(),
        }
        let before_root_mode = fs::symlink_metadata(&root).unwrap().mode() & 0o7777;

        let error = open_repository_operation_lock_path(&lock)
            .expect_err("unsafe helper layout must fail closed");

        assert!(
            error.kind() == std::io::ErrorKind::InvalidInput
                || error.raw_os_error() == Some(libc::ELOOP),
            "{case}: {error}"
        );
        assert_eq!(
            fs::symlink_metadata(&root).unwrap().mode() & 0o7777,
            before_root_mode,
            "{case}"
        );
        if case == "broad-lock" {
            assert_eq!(fs::read(&lock).unwrap(), b"broad");
            assert_eq!(fs::symlink_metadata(&lock).unwrap().mode() & 0o7777, 0o644);
        }
        if case == "symlink-dir" {
            assert_eq!(
                fs::read(temp.path().join("outside/sentinel")).unwrap(),
                b"outside"
            );
        }
        if case == "symlink-lock" {
            assert_eq!(
                fs::read(temp.path().join("outside-lock")).unwrap(),
                b"outside"
            );
        }
    }
}

fn git_cached_diff_binary(root: &Path) -> Vec<u8> {
    let output = Command::new("git")
        .args([
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
            "--",
        ])
        .current_dir(root)
        .output()
        .expect("index diff");
    assert!(output.status.success(), "{output:?}");
    output.stdout
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

fn find_named_file(root: &Path, name: &str) -> PathBuf {
    let mut matches = Vec::new();
    fn visit(root: &Path, name: &str, matches: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(root).expect("read lock search directory") {
            let entry = entry.expect("lock search entry");
            let path = entry.path();
            if path.is_dir() {
                visit(&path, name, matches);
            } else if path.file_name().and_then(|value| value.to_str()) == Some(name) {
                matches.push(path);
            }
        }
    }
    visit(root, name, &mut matches);
    assert_eq!(matches.len(), 1, "expected one {name}: {matches:?}");
    matches.pop().unwrap()
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
    fs::set_permissions(
        destination,
        fs::symlink_metadata(source)
            .expect("source directory mode")
            .permissions(),
    )
    .expect("copy directory mode");
    for entry in fs::read_dir(source).expect("read source directory") {
        let entry = entry.expect("source entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_directory(&source_path, &destination_path);
        } else {
            fs::copy(&source_path, &destination_path).expect("copy run file");
            fs::set_permissions(
                &destination_path,
                fs::symlink_metadata(&source_path)
                    .expect("source file mode")
                    .permissions(),
            )
            .expect("copy file mode");
        }
    }
}

#[cfg(unix)]
fn write_private_run_fixture(path: impl AsRef<Path>, bytes: &[u8]) {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(path).expect("create private run fixture");
    file.write_all(bytes).expect("write private run fixture");
}

#[cfg(not(unix))]
fn write_private_run_fixture(_path: impl AsRef<Path>, _bytes: &[u8]) {
    panic!("private loop workspace tests require Unix")
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
eval:
  config: seaf.evals.yaml
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
eval:
  config: seaf.evals.yaml
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
eval:
  config: seaf.evals.yaml
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

    write_raw_canonical_run_fixture(run_path, &run);
}

fn write_raw_canonical_run_fixture(run_path: &Path, run: &serde_json::Value) {
    let typed: seaf_core::LoopRun =
        serde_json::from_value(run.clone()).expect("decode typed raw run fixture");
    let mut bytes = serde_json::to_vec_pretty(&typed).expect("serialize raw run fixture");
    bytes.push(b'\n');
    fs::write(run_path, bytes).expect("write raw canonical run fixture");
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
    fs::write(
        path.join("seaf.evals.yaml"),
        "evals:\n  allow_commands: [cargo]\n  required:\n    - name: tests\n      command: cargo test\n",
    )
    .expect("write root test eval config");
    fs::create_dir_all(path.join("examples/local-loop")).expect("local loop eval directory");
    fs::write(
        path.join("examples/local-loop/seaf.evals.yaml"),
        "evals:\n  allow_commands: [cargo test]\n  required:\n    - name: local_loop_smoke\n      command: cargo test -p seaf-core validate_eval_report --quiet\n",
    )
    .expect("write local loop test eval config");
    let add = Command::new("git")
        .args([
            "add",
            "seaf.policy.json",
            "seaf.evals.yaml",
            "examples/local-loop/seaf.evals.yaml",
        ])
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
