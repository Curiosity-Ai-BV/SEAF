use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

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
fn loop_run_status_and_resume_emit_json_and_persist_artifacts() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path();
    init_git_repo(repo);
    let runs_root = repo.join("runs");
    let run_id = "cli-loop-json";

    let run = seaf_in(repo)
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

    assert!(run.status.success());
    let stdout = String::from_utf8(run.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"status\": \"completed\""));
    assert!(runs_root.join(run_id).join("run.json").exists());
    assert!(runs_root
        .join(run_id)
        .join("artifacts/08-eval-report.md")
        .exists());

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
    assert_eq!(decision["decision"], "allowed");
    assert_eq!(decision["apply_requested"], false);
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
fn loop_resume_persists_policy_evidence_for_pre_evidence_run() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let repo = temp_dir.path();
    init_git_repo(repo);
    let runs_root = repo.join("runs");
    let run_id = "loop-resume-evidence";
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
    let mut persisted_run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&run_path).expect("run json"))
            .expect("persisted run json");
    *persisted_run
        .get_mut("policy_decisions")
        .expect("policy decisions") = serde_json::json!([]);
    fs::write(
        &run_path,
        serde_json::to_string_pretty(&persisted_run).expect("serialize run"),
    )
    .expect("write pre-evidence run");

    let resume = seaf_in(repo)
        .args([
            "loop",
            "resume",
            "--runs-root",
            runs_root.to_str().unwrap(),
            "--run-id",
            run_id,
            "--json",
        ])
        .output()
        .expect("resume loop");
    assert!(resume.status.success(), "{resume:?}");
    let stdout = String::from_utf8(resume.stdout).expect("utf8 stdout");
    let resume_report: serde_json::Value =
        serde_json::from_str(&stdout).expect("resume report json");
    assert_eq!(resume_report["status"], "completed");

    let resumed_run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&run_path).expect("resumed run json"))
            .expect("resumed run json");
    assert!(
        !resumed_run["policy_decisions"]
            .as_array()
            .expect("policy decisions")
            .is_empty(),
        "resume should backfill deterministic policy evidence so completed runs remain evaluable"
    );
    assert_eq!(resumed_run["policy_decisions"][0]["patch_id"], run_id);

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
    assert_eq!(report["passed"], true);
    assert_eq!(report["decision"], "approve_for_human_review");
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
    io::{self, Write},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

fn main() {
    if std::env::args().any(|arg| arg == "--hold-pipes") {
        thread::sleep(Duration::from_secs(2));
        return;
    }

    println!("direct-child-done");
    io::stdout().flush().unwrap();
    Command::new(std::env::current_exe().unwrap())
        .arg("--hold-pipes")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
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
    fs::write(
        &config_path,
        format!(
            r#"evals:
  allow_commands:
    - {command}
  required:
    - name: descendant_pipe
      command: "{command}"
      timeout_ms: 1000
"#,
            command = helper_path.display()
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

    assert!(output.status.success(), "{output:?}");
    assert!(
        elapsed < Duration::from_secs(1),
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
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake Ollama");
    let address = listener.local_addr().expect("fake Ollama address");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept fake Ollama request");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set fake Ollama read timeout");
        read_http_request(&mut stream);
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
    });
    format!("http://{address}/api")
}

fn read_http_request(stream: &mut std::net::TcpStream) {
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
