use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use seaf_core::{EvalCommandConfig, EvalConfig, EvalGroup};
use seaf_loop::run_eval_checks;

fn config(allow_commands: Vec<String>, required: Vec<EvalCommandConfig>) -> EvalConfig {
    EvalConfig {
        evals: EvalGroup {
            allow_commands,
            required,
        },
        thresholds: None,
    }
}

fn check(name: &str, command: String) -> EvalCommandConfig {
    EvalCommandConfig {
        name: name.to_string(),
        command,
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: None,
        max_output_bytes: None,
    }
}

#[test]
fn eval_and_ticket_allowlists_are_independently_required() {
    let root = tempfile::tempdir().expect("execution root");
    let required = vec![check("version", "printf ok".to_string())];

    let eval_denied = run_eval_checks(
        &config(Vec::new(), required.clone()),
        Some(&["printf".to_string()]),
        root.path(),
    )
    .expect_err("empty eval allowlist must deny the command");
    assert!(eval_denied.to_string().contains("eval allow_commands"));

    let ticket_denied = run_eval_checks(
        &config(vec!["printf".to_string()], required),
        Some(&[]),
        root.path(),
    )
    .expect_err("empty ticket allowlist must deny the command");
    assert!(ticket_denied.to_string().contains("ticket autonomy"));
}

#[test]
fn invalid_later_check_prevents_earlier_command_execution() {
    let root = tempfile::tempdir().expect("execution root");
    let marker = root.path().join("marker");
    let config = config(
        vec!["touch".to_string(), "printf".to_string()],
        vec![
            check("first", format!("touch {}", marker.display())),
            check("invalid", "printf blocked > marker".to_string()),
        ],
    );

    let error = run_eval_checks(&config, None, root.path())
        .expect_err("all checks must be planned before the first command runs");

    assert!(error.to_string().contains("shell metacharacter"));
    assert!(!marker.exists(), "the earlier marker command must not run");
}

#[cfg(unix)]
#[test]
fn returned_output_is_redacted_before_it_is_capped() {
    let root = tempfile::tempdir().expect("execution root");
    let script = root.path().join("emit-secret");
    write_executable(&script, "#!/bin/sh\nprintf '%s\\n' \"$SECRET_TOKEN\"\n");
    let mut configured_check = check("secret", script.display().to_string());
    configured_check.env.insert(
        "SECRET_TOKEN".to_string(),
        "plain-configured-secret-value-1234567890".to_string(),
    );
    configured_check.max_output_bytes = Some(8);
    let config = config(vec![script.display().to_string()], vec![configured_check]);

    let executions =
        run_eval_checks(&config, None, root.path()).expect("controlled command should run");

    assert_eq!(executions.len(), 1);
    assert!(executions[0].stdout.starts_with("[REDACT"));
    assert!(executions[0].stdout.len() <= 8);
    assert!(!executions[0].stdout.contains("plain"));
}

#[cfg(unix)]
#[test]
fn obvious_secret_starting_before_output_cap_is_classified_before_truncation() {
    let root = tempfile::tempdir().expect("execution root");
    let script = root.path().join("emit-obvious-secret");
    write_executable(
        &script,
        "#!/bin/sh\nprintf 'ok sk-proj-exampleSensitiveToken1234567890\\n'\n",
    );
    let mut configured_check = check("secret", script.display().to_string());
    configured_check.max_output_bytes = Some(12);
    let config = config(vec![script.display().to_string()], vec![configured_check]);

    let executions =
        run_eval_checks(&config, None, root.path()).expect("controlled command should run");

    assert_eq!(executions.len(), 1);
    assert!(executions[0].stdout.contains("[REDA"));
    assert!(executions[0].stdout.len() <= 12);
    assert!(!executions[0].stdout.contains("sk-proj-"));
}

#[cfg(unix)]
#[test]
fn escaping_relative_cwd_and_executable_are_rejected() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path().join("candidate");
    let outside = temp.path().join("outside");
    fs::create_dir_all(root.join("nested")).expect("candidate directories");
    fs::create_dir_all(&outside).expect("outside directory");
    let outside_script = outside.join("outside-script");
    write_executable(&outside_script, "#!/bin/sh\nprintf escaped\n");

    let mut cwd_escape = check("cwd_escape", "printf ok".to_string());
    cwd_escape.cwd = Some(PathBuf::from("../outside"));
    let cwd_error = run_eval_checks(
        &config(vec!["printf".to_string()], vec![cwd_escape]),
        None,
        &root,
    )
    .expect_err("relative cwd must stay inside the execution root");
    assert!(cwd_error.to_string().contains("escapes invocation root"));

    let mut executable_escape = check(
        "executable_escape",
        "../../outside/outside-script".to_string(),
    );
    executable_escape.cwd = Some(PathBuf::from("nested"));
    let executable_error = run_eval_checks(
        &config(
            vec!["../../outside/outside-script".to_string()],
            vec![executable_escape],
        ),
        None,
        &root,
    )
    .expect_err("relative executable must stay inside the execution root");
    assert!(executable_error
        .to_string()
        .contains("escapes invocation root"));
}

#[cfg(unix)]
fn write_executable(path: &Path, content: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, content).expect("write executable");
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod executable");
}
