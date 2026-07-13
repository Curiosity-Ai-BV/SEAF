use std::{path::PathBuf, process::Command as ProcessCommand};

use clap::Args;
use seaf_core::{canonical_sha256_digest, EvalConfig, TicketSpec, ValidationReport};
use seaf_models::{
    ModelProvider, ModelResponse, OllamaConfig, OllamaProvider, DEFAULT_OLLAMA_BASE_URL,
};
use serde::Serialize;

use super::{
    load_authoritative_eval_config, model_check_request, print_json,
    resolve_current_repository_root, resolve_effective_project_inputs_quiet, CliFailure,
    EffectiveProjectInputFailure, RepositoryIdentity,
};

const DOCTOR_SCHEMA_VERSION: u32 = 1;
const DEFAULT_DOCTOR_TIMEOUT_MS: u64 = 5_000;
const DOCTOR_DIAGNOSTIC_ID: &str = "doctor-readiness";

#[derive(Debug, Args)]
pub(super) struct DoctorArgs {
    /// Provider to diagnose.
    #[arg(long, value_parser = ["fake", "ollama"])]
    provider: String,
    /// Model name. Required for Ollama; defaults to fake-local for fake.
    #[arg(long, required_if_eq("provider", "ollama"))]
    model: Option<String>,
    /// Caller-relative ticket file. Defaults to seaf.ticket.yaml at the Git root.
    #[arg(long)]
    ticket: Option<PathBuf>,
    /// Repository-contained project configuration file.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Repository-contained policy overriding project configuration and root discovery.
    #[arg(long)]
    policy: Option<PathBuf>,
    /// Ollama API base URL.
    #[arg(long)]
    base_url: Option<String>,
    /// Authorize one live Ollama structured health request.
    #[arg(long)]
    live_provider: bool,
    /// Live provider timeout in milliseconds.
    #[arg(
        long,
        default_value_t = DEFAULT_DOCTOR_TIMEOUT_MS,
        value_parser = clap::value_parser!(u64).range(1..=30_000)
    )]
    timeout_ms: u64,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DoctorCheckStatus {
    Passed,
    Failed,
    Blocked,
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    id: &'static str,
    status: DoctorCheckStatus,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    remediation: Option<String>,
}

impl DoctorCheck {
    fn passed(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            status: DoctorCheckStatus::Passed,
            message: message.into(),
            remediation: None,
        }
    }

    fn failed(
        id: &'static str,
        message: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            id,
            status: DoctorCheckStatus::Failed,
            message: message.into(),
            remediation: Some(remediation.into()),
        }
    }

    fn blocked(
        id: &'static str,
        message: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            id,
            status: DoctorCheckStatus::Blocked,
            message: message.into(),
            remediation: Some(remediation.into()),
        }
    }
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    schema_version: u32,
    ready: bool,
    repository: String,
    provider: String,
    model: String,
    checks: Vec<DoctorCheck>,
}

struct GitRepositoryReadiness {
    root: PathBuf,
    identity: RepositoryIdentity,
}

pub(super) fn run(args: DoctorArgs) -> Result<(), CliFailure> {
    let fallback_repository = std::env::current_dir()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| "fake-local".to_string());
    let mut checks = Vec::with_capacity(8);

    let repository = match inspect_git_repository() {
        Ok(repository) => {
            checks.push(DoctorCheck::passed(
                "git_repository",
                "Git repository root and common directory are available",
            ));
            Some(repository)
        }
        Err(error) => {
            checks.push(DoctorCheck::failed(
                "git_repository",
                error,
                "rerun seaf doctor from a Git repository with at least one committed revision",
            ));
            None
        }
    };

    let worktree_ready = match &repository {
        Some(repository) => match inspect_clean_committed_worktree(&repository.root) {
            Ok(()) => {
                checks.push(DoctorCheck::passed(
                    "git_worktree",
                    "Git HEAD/tree are committed and the worktree is clean",
                ));
                true
            }
            Err(error) => {
                checks.push(DoctorCheck::failed(
                    "git_worktree",
                    error,
                    "commit or stash all tracked and untracked changes, then rerun seaf doctor",
                ));
                false
            }
        },
        None => {
            checks.push(DoctorCheck::blocked(
                "git_worktree",
                "Git worktree inspection requires a valid repository",
                "fix git_repository, then rerun seaf doctor",
            ));
            false
        }
    };

    match &repository {
        Some(repository) => {
            match resolve_effective_project_inputs_quiet(
                &repository.root,
                args.config.as_deref(),
                args.policy.as_deref(),
            ) {
                Ok(inputs) => {
                    let _ = (&inputs.config, &inputs.policy);
                    checks.push(DoctorCheck::passed(
                        "project_inputs",
                        "project configuration and policy authority are valid",
                    ));
                }
                Err(error) => checks.push(DoctorCheck::failed(
                    "project_inputs",
                    project_input_error(error),
                    "fix --config/--policy or the Git-root project configuration and policy",
                )),
            }
        }
        None => checks.push(DoctorCheck::blocked(
            "project_inputs",
            "project input discovery requires a valid Git repository",
            "fix git_repository, then rerun seaf doctor",
        )),
    }

    let ticket = match &repository {
        Some(repository) => match load_ticket(&args, &repository.root) {
            Ok(ticket) => {
                checks.push(DoctorCheck::passed("ticket", "ticket authority is valid"));
                Some(ticket)
            }
            Err(error) => {
                checks.push(DoctorCheck::failed(
                    "ticket",
                    error,
                    "fix --ticket or the Git-root seaf.ticket.yaml file",
                ));
                None
            }
        },
        None => {
            checks.push(DoctorCheck::blocked(
                "ticket",
                "ticket discovery requires a valid Git repository",
                "fix git_repository, then rerun seaf doctor",
            ));
            None
        }
    };

    match (&repository, worktree_ready) {
        (Some(repository), true) => {
            let digest = canonical_sha256_digest(&repository.identity).map_err(|error| {
                CliFailure::message(format!("could not digest repository identity: {error}"))
            })?;
            match seaf_loop::plan_candidate_workspace_readiness(
                &repository.root,
                &digest,
                DOCTOR_DIAGNOSTIC_ID,
            ) {
                Ok(_) => checks.push(DoctorCheck::passed(
                    "candidate_workspace",
                    "committed Git authority, worktree support, and an external candidate path are available",
                )),
                Err(error) => checks.push(DoctorCheck::failed(
                    "candidate_workspace",
                    format!("candidate workspace planning failed: {error}"),
                    "fix Git worktree/common-directory support or the external candidate path namespace",
                )),
            }
        }
        (Some(_), false) => checks.push(DoctorCheck::blocked(
            "candidate_workspace",
            "candidate planning requires a clean committed worktree",
            "fix git_worktree, then rerun seaf doctor",
        )),
        (None, _) => checks.push(DoctorCheck::blocked(
            "candidate_workspace",
            "candidate planning requires a valid Git repository",
            "fix git_repository, then rerun seaf doctor",
        )),
    }

    let eval_config = match (&repository, &ticket) {
        (Some(repository), Some(ticket)) => {
            match load_authoritative_eval_config(&repository.root, ticket) {
                Ok(authority) => {
                    let config: EvalConfig =
                        serde_json::from_slice(&authority.bytes).map_err(|error| {
                            CliFailure::message(format!(
                                "could not decode canonical eval config: {error}"
                            ))
                        })?;
                    let _ = authority.digest;
                    checks.push(DoctorCheck::passed(
                        "eval_config",
                        "ticket-selected eval configuration is valid",
                    ));
                    Some(config)
                }
                Err(error) => {
                    checks.push(DoctorCheck::failed(
                        "eval_config",
                        cli_failure_message(error),
                        "fix ticket.eval.config and its repository-contained eval file",
                    ));
                    None
                }
            }
        }
        (Some(_), None) => {
            checks.push(DoctorCheck::blocked(
                "eval_config",
                "eval configuration discovery requires a valid ticket",
                "fix ticket, then rerun seaf doctor",
            ));
            None
        }
        (None, _) => {
            checks.push(DoctorCheck::blocked(
                "eval_config",
                "eval configuration discovery requires a valid Git repository",
                "fix git_repository, then rerun seaf doctor",
            ));
            None
        }
    };

    match (&repository, &ticket, &eval_config) {
        (Some(repository), Some(ticket), Some(config)) => match seaf_loop::plan_eval_checks(
            config,
            Some(&ticket.autonomy.allow_shell_commands),
            &repository.root,
        ) {
            Ok(_) => checks.push(DoctorCheck::passed(
                "eval_executables",
                "all eval commands and executable identities plan successfully",
            )),
            Err(error) => checks.push(DoctorCheck::failed(
                "eval_executables",
                format!("eval planning failed: {error}"),
                "install or correct every allowlisted eval executable without running it",
            )),
        },
        _ => checks.push(DoctorCheck::blocked(
            "eval_executables",
            "eval executable planning requires valid ticket and eval configuration authority",
            "fix ticket and eval_config, then rerun seaf doctor",
        )),
    }

    checks.push(provider_check(&args, &model));
    debug_assert_eq!(checks.len(), 8);
    let ready = checks
        .iter()
        .all(|check| check.status == DoctorCheckStatus::Passed);
    let report = DoctorReport {
        schema_version: DOCTOR_SCHEMA_VERSION,
        ready,
        repository: repository
            .as_ref()
            .map(|repository| repository.root.display().to_string())
            .unwrap_or_else(|| fallback_repository.display().to_string()),
        provider: args.provider,
        model,
        checks,
    };
    print_report(&report, args.json)?;
    if report.ready {
        Ok(())
    } else {
        Err(CliFailure::already_printed())
    }
}

fn inspect_git_repository() -> Result<GitRepositoryReadiness, String> {
    let root =
        resolve_current_repository_root(sanitized_git_command()).map_err(cli_failure_message)?;
    let common = git_text(&root, &["rev-parse", "--git-common-dir"])?;
    let common = PathBuf::from(common);
    let common = if common.is_absolute() {
        common
    } else {
        root.join(common)
    }
    .canonicalize()
    .map_err(|error| format!("could not canonicalize Git common directory: {error}"))?;
    let identity = RepositoryIdentity {
        worktree_root: root
            .to_str()
            .ok_or_else(|| "Git worktree root is not UTF-8".to_string())?
            .to_string(),
        git_common_dir: common
            .to_str()
            .ok_or_else(|| "Git common directory is not UTF-8".to_string())?
            .to_string(),
    };
    Ok(GitRepositoryReadiness { root, identity })
}

fn inspect_clean_committed_worktree(root: &std::path::Path) -> Result<(), String> {
    git_text(root, &["rev-parse", "--verify", "HEAD"])?;
    git_text(root, &["rev-parse", "HEAD^{tree}"])?;
    let status = git_bytes(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if status.is_empty() {
        Ok(())
    } else {
        Err("Git worktree has tracked or untracked changes".to_string())
    }
}

fn load_ticket(args: &DoctorArgs, repository_root: &std::path::Path) -> Result<TicketSpec, String> {
    let path = args
        .ticket
        .clone()
        .unwrap_or_else(|| repository_root.join("seaf.ticket.yaml"));
    seaf_core::load_ticket_file(&path).map_err(validation_message)
}

fn provider_check(args: &DoctorArgs, model: &str) -> DoctorCheck {
    match args.provider.as_str() {
        "fake" if args.base_url.is_some() => DoctorCheck::failed(
            "provider",
            "--base-url is only used with --provider ollama",
            "remove --base-url when diagnosing the deterministic fake provider",
        ),
        "fake" if args.live_provider => DoctorCheck::failed(
            "provider",
            "--live-provider is only valid with --provider ollama",
            "remove --live-provider when diagnosing the deterministic fake provider",
        ),
        "fake" => DoctorCheck::passed(
            "provider",
            "fake provider is configured and requires no process or network contact",
        ),
        "ollama" => {
            let base_url = args
                .base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_OLLAMA_BASE_URL.to_string());
            let provider = OllamaProvider::new(OllamaConfig {
                base_url,
                ..OllamaConfig::default()
            });
            let request = model_check_request(model, args.timeout_ms);
            if let Err(error) = provider.build_chat_request(&request) {
                return DoctorCheck::failed(
                    "provider",
                    error.message,
                    "fix --base-url or the selected Ollama model configuration",
                );
            }
            if !args.live_provider {
                return DoctorCheck::blocked(
                    "provider",
                    "Ollama configuration is valid but live readiness was not authorized",
                    "rerun with --live-provider to authorize one bounded structured Ollama request",
                );
            }
            match provider.complete(request) {
                Ok(response) => match validate_doctor_provider_response(&response) {
                    Ok(()) => DoctorCheck::passed(
                        "provider",
                        "Ollama returned the required structured ok=true response",
                    ),
                    Err(error) => DoctorCheck::failed(
                        "provider",
                        error,
                        "verify the selected model supports structured output and returns ok=true",
                    ),
                },
                Err(error) => DoctorCheck::failed(
                    "provider",
                    error.message,
                    "start Ollama, install the selected model, verify --base-url, or adjust the bounded timeout",
                ),
            }
        }
        _ => unreachable!("Clap constrains doctor providers"),
    }
}

fn validate_doctor_provider_response(response: &ModelResponse) -> Result<(), String> {
    let value: serde_json::Value = serde_json::from_str(&response.content).map_err(|error| {
        format!("doctor response.content must be a JSON object with ok == true: {error}")
    })?;
    match value.get("ok").and_then(serde_json::Value::as_bool) {
        Some(true) => Ok(()),
        Some(false) => Err("doctor response.content must have ok == true; got false".to_string()),
        None => Err("doctor response.content must include boolean field ok == true".to_string()),
    }
}

fn sanitized_git_command() -> ProcessCommand {
    let mut command = ProcessCommand::new("git");
    let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        &format!("core.hooksPath={null_device}"),
    ]);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_EXTERNAL_DIFF",
        "GIT_DIFF_OPTS",
        "GIT_PAGER",
        "GIT_EDITOR",
        "GIT_ASKPASS",
        "SSH_ASKPASS",
    ] {
        command.env_remove(name);
    }
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("GIT_CONFIG_KEY_") || name.starts_with("GIT_CONFIG_VALUE_") {
            command.env_remove(name.as_ref());
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", null_device)
        .env("GIT_CONFIG_GLOBAL", null_device)
        .env("GIT_OPTIONAL_LOCKS", "0");
    command
}

fn git_text(root: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let bytes = git_bytes(root, args)?;
    String::from_utf8(bytes)
        .map(|value| value.trim().to_string())
        .map_err(|error| format!("Git output was not UTF-8: {error}"))
}

fn git_bytes(root: &std::path::Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = sanitized_git_command()
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| format!("could not run git {}: {error}", args.join(" ")))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn validation_message(report: ValidationReport) -> String {
    let details = report
        .errors
        .iter()
        .map(|error| format!("{}: {}", error.field, error.message))
        .collect::<Vec<_>>()
        .join("; ");
    format!("invalid {}: {details}", report.kind)
}

fn project_input_error(error: EffectiveProjectInputFailure) -> String {
    match error {
        EffectiveProjectInputFailure::Message(message) => message,
        EffectiveProjectInputFailure::Validation(report) => validation_message(report),
    }
}

fn cli_failure_message(error: CliFailure) -> String {
    error
        .message
        .unwrap_or_else(|| "diagnostic validation failed".to_string())
}

fn print_report(report: &DoctorReport, json: bool) -> Result<(), CliFailure> {
    if json {
        return print_json(report);
    }
    println!(
        "project doctor: {}",
        if report.ready { "ready" } else { "not ready" }
    );
    println!("repository: {}", report.repository);
    println!("provider: {}", report.provider);
    println!("model: {}", report.model);
    for check in &report.checks {
        let status = match check.status {
            DoctorCheckStatus::Passed => "passed",
            DoctorCheckStatus::Failed => "failed",
            DoctorCheckStatus::Blocked => "blocked",
        };
        println!("[{status}] {}: {}", check.id, check.message);
        if let Some(remediation) = &check.remediation {
            println!("  remediation: {remediation}");
        }
    }
    Ok(())
}
