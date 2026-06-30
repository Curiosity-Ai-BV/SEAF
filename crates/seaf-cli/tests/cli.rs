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
fn eval_placeholder_fails_closed() {
    let output = seaf()
        .args([
            "eval",
            "run",
            example_path("seaf.evals.yaml").to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run eval");

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"passed\": false"));
    assert!(stdout.contains("\"decision\": \"reject\""));
}

#[test]
fn release_verify_accepts_valid_capsule() {
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
