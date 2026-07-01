use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

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
  required:
    - name: fail
      command: "exit 7"
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
