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

    assert!(!output.status.success());
    assert!(report_path.exists());
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

fn example_path(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/adaptive-notes")
        .join(file_name)
}
