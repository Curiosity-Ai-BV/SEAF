use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::os::unix::fs::{MetadataExt as UnixMetadataExt, OpenOptionsExt as UnixOpenOptionsExt};
#[cfg(target_os = "windows")]
use std::os::windows::fs::{
    MetadataExt as WindowsMetadataExt, OpenOptionsExt as WindowsOpenOptionsExt,
};

use clap::{Args, Parser, Subcommand};
use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, sha256_digest_file, templates, AgentTaskBrief,
    AgentTaskConstraints, CandidateWorkspaceLifecycle, CheckStatus, EvalCheck, EvalConfigError,
    EvalDecision, EvalReport, FieldError, HumanApprovalEvidence, LoopInputDigests, LoopRun,
    LoopStatus, LoopStepName, LoopStepStatus, Policy, ProjectConfig, ReleaseCapsule, RiskLevel,
    RolloutChannel, RolloutPolicy, TicketAutonomy, TicketContext, TicketPriority, TicketSpec,
    TicketStatus, ValidationReport,
};
use seaf_loop::{
    approve_candidate_for_testing, build_loop_eval_report, cleanup_candidate_workspace_outcome,
    evaluate_zero_tolerance, execute_eval_checks, load_agent_bench_fixture, plan_eval_checks,
    preflight_authoritative_run_inputs, validate_human_review_execution_barrier,
    validate_rerun_eligibility, AgentBenchSummary, ArtifactContent, AuthoritativeRunInputSnapshots,
    CandidateCleanupOutcome, ContextLimits, ContextPackRequest, EvalCheckExecution,
    GitCommandRunner, InitializedLoopRun, LoopRunner, LoopRunnerConfig, LoopWorkspace,
    PatchDecisionKind, PolicyDecision, PreparedLoopRun, ProviderPatchGateConfig,
    ProviderStepRunner, RunnerError, StepOutput, StepRunner,
};
use seaf_models::{
    FakeProvider, ModelMessage, ModelMessageRole, ModelProvider, ModelRequest, ModelResponse,
    OllamaConfig, OllamaProvider, DEFAULT_OLLAMA_BASE_URL,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(name = "seaf")]
#[command(about = "Self-Evolving Application Framework developer CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print basic framework information.
    Info,
    /// Initialize SEAF config files in a project.
    Init(InitArgs),
    /// Work with goal specs.
    Goal {
        #[command(subcommand)]
        command: GoalCommand,
    },
    /// Work with agent policies.
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
    /// Generate manual agent task briefs.
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Run configured evals.
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },
    /// Work with local model providers.
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    /// Work with local-loop tickets.
    Ticket {
        #[command(subcommand)]
        command: TicketCommand,
    },
    /// Run and inspect local-loop executions.
    Loop {
        #[command(subcommand)]
        command: LoopCommand,
    },
    /// Work with release capsules.
    Release {
        #[command(subcommand)]
        command: ReleaseCommand,
    },
}

#[derive(Debug, Args)]
struct InitArgs {
    /// Directory to initialize.
    #[arg(long, default_value = ".")]
    path: PathBuf,
    /// Starter template to use.
    #[arg(long, default_value = "adaptive-notes")]
    template: String,
    /// Overwrite existing template files.
    #[arg(long)]
    force: bool,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum GoalCommand {
    /// Validate a GoalSpec YAML or JSON file.
    Validate(ValidateArgs),
}

#[derive(Debug, Subcommand)]
enum PolicyCommand {
    /// Validate a policy JSON or YAML file.
    Validate(ValidateArgs),
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    /// Generate a manual coding-agent task brief from a goal and policy.
    Brief(TaskBriefArgs),
}

#[derive(Debug, Subcommand)]
enum EvalCommand {
    /// Run configured eval commands and emit an EvalReport.
    Run(EvalRunArgs),
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    /// Check that a local model provider can answer a structured request.
    Check(ModelCheckArgs),
}

#[derive(Debug, Subcommand)]
enum TicketCommand {
    /// Validate a local-loop ticket YAML or JSON file.
    Validate(ValidateArgs),
}

#[derive(Debug, Subcommand)]
enum LoopCommand {
    /// Start a provider-backed local-loop run for a ticket.
    Run(LoopRunArgs),
    /// Print persisted loop run status.
    Status(LoopStatusArgs),
    /// Resume a local-loop run.
    Resume(LoopResumeArgs),
    /// Approve the exact reviewed candidate for future Testing.
    Approve(LoopApproveArgs),
    /// Explicitly remove a terminal run's verified candidate worktree.
    Cleanup(LoopCleanupArgs),
    /// Run a deterministic smoke loop without contacting a model provider.
    Smoke(LoopSmokeArgs),
    /// Run AgentBench-lite against a deterministic fixture.
    Bench(LoopBenchArgs),
}

#[derive(Debug, Subcommand)]
enum ReleaseCommand {
    /// Prepare a release capsule from an artifact and passing EvalReport.
    Prepare(ReleasePrepareArgs),
    /// Verify release capsule structure and optional artifact/eval digests.
    Verify(ReleaseVerifyArgs),
}

#[derive(Debug, Args)]
struct ValidateArgs {
    /// File to validate.
    path: PathBuf,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct TaskBriefArgs {
    /// GoalSpec file.
    #[arg(long)]
    goal: PathBuf,
    /// Policy file.
    #[arg(long)]
    policy: PathBuf,
    /// Directory where task JSON and Markdown files are written.
    #[arg(long, default_value = ".seaf/tasks")]
    output_dir: PathBuf,
    /// Relevant file to include in the task brief. Repeatable.
    #[arg(long = "relevant-file")]
    relevant_files: Vec<String>,
    /// Acceptance criterion to include in the task brief. Repeatable.
    #[arg(long = "acceptance")]
    acceptance_criteria: Vec<String>,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct EvalRunArgs {
    /// Eval config path.
    #[arg(default_value = "seaf.evals.yaml")]
    config: PathBuf,
    /// EvalReport output path.
    #[arg(long, default_value = ".seaf/evals/eval-report.json")]
    output: PathBuf,
    /// Patch ID to bind into the EvalReport.
    #[arg(long, default_value = "patch_local")]
    patch_id: String,
    /// Goal ID to bind into the EvalReport.
    #[arg(long, default_value = "unknown")]
    goal_id: String,
    /// LoopRun artifact to integrate into the EvalReport.
    #[arg(long)]
    loop_run: Option<PathBuf>,
    /// TicketSpec artifact to integrate into the EvalReport.
    #[arg(long)]
    ticket: Option<PathBuf>,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ModelCheckArgs {
    /// Provider to check. Supported: ollama.
    #[arg(long)]
    provider: String,
    /// Model name to check.
    #[arg(long)]
    model: String,
    /// Ollama API base URL.
    #[arg(long, default_value = DEFAULT_OLLAMA_BASE_URL)]
    base_url: String,
    /// Request timeout in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    timeout_ms: u64,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct LoopRunArgs {
    /// Ticket file to validate and run.
    #[arg(long)]
    ticket: PathBuf,
    /// Project configuration file. When omitted, seaf.config.json is discovered at the Git root.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Policy file that overrides project configuration and root policy discovery.
    #[arg(long)]
    policy: Option<PathBuf>,
    /// Directory where loop run workspaces are written.
    #[arg(long, default_value = ".seaf/loops/runs")]
    runs_root: PathBuf,
    /// Stable run ID. Generated when omitted.
    #[arg(long)]
    run_id: Option<String>,
    /// Provider to execute. Supported: fake, ollama.
    #[arg(long, default_value = "fake")]
    provider: String,
    /// Model name for provider-backed execution. Defaults to fake-local for --provider fake.
    #[arg(long)]
    model: Option<String>,
    /// Ollama API base URL.
    #[arg(long, default_value = DEFAULT_OLLAMA_BASE_URL)]
    base_url: String,
    /// Provider request timeout in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    timeout_ms: u64,
    /// Allow starting a loop when the git working tree is dirty.
    #[arg(long)]
    allow_dirty: bool,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct LoopStatusArgs {
    /// Run ID under --runs-root.
    #[arg(long)]
    run_id: String,
    /// Directory containing loop run workspaces.
    #[arg(long, default_value = ".seaf/loops/runs")]
    runs_root: PathBuf,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct LoopResumeArgs {
    /// Run ID under --runs-root.
    #[arg(long)]
    run_id: String,
    /// Directory containing loop run workspaces.
    #[arg(long, default_value = ".seaf/loops/runs")]
    runs_root: PathBuf,
    /// Ticket file required when resuming incomplete provider-backed runs.
    #[arg(long)]
    ticket: Option<PathBuf>,
    /// Project configuration file. Must resolve to the inputs used when the run was created.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Policy file that overrides project configuration and must match the original run input.
    #[arg(long)]
    policy: Option<PathBuf>,
    /// Ollama API base URL.
    #[arg(long, default_value = DEFAULT_OLLAMA_BASE_URL)]
    base_url: String,
    /// Provider request timeout in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    timeout_ms: u64,
    /// Explicitly rerun from this provider step using a new audited attempt.
    #[arg(long)]
    rerun_from: Option<String>,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct LoopCleanupArgs {
    /// Run ID under --runs-root.
    #[arg(long)]
    run_id: String,
    /// Directory containing loop run workspaces.
    #[arg(long, default_value = ".seaf/loops/runs")]
    runs_root: PathBuf,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct LoopApproveArgs {
    /// Run ID under --runs-root.
    #[arg(long)]
    run_id: String,
    /// Directory containing loop run workspaces.
    #[arg(long, default_value = ".seaf/loops/runs")]
    runs_root: PathBuf,
    /// Stable identity of the human reviewer granting approval.
    #[arg(long)]
    reviewer: String,
    /// Exact candidate staged-diff SHA-256 shown to and confirmed by the reviewer.
    #[arg(long)]
    confirm_candidate_diff: String,
    /// Exact target HEAD shown to and confirmed by the reviewer.
    #[arg(long)]
    confirm_target_head: String,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct LoopSmokeArgs {
    /// Directory where loop run workspaces are written.
    #[arg(long, default_value = ".seaf/loops/runs")]
    runs_root: PathBuf,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct LoopBenchArgs {
    /// Provider to benchmark. Supported: fake, ollama.
    #[arg(long, default_value = "fake")]
    provider: String,
    /// Model name for live local smoke execution.
    #[arg(long)]
    model: Option<String>,
    /// Ollama API base URL.
    #[arg(long, default_value = DEFAULT_OLLAMA_BASE_URL)]
    base_url: String,
    /// Ollama smoke request timeout in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    timeout_ms: u64,
    /// AgentBench-lite fixture directory.
    #[arg(long)]
    fixture: PathBuf,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ReleasePrepareArgs {
    #[arg(long)]
    app_id: String,
    #[arg(long)]
    version: String,
    #[arg(long)]
    source_commit: String,
    #[arg(long)]
    artifact: PathBuf,
    #[arg(long)]
    eval_report: PathBuf,
    #[arg(long)]
    goal_id: Option<String>,
    #[arg(long)]
    agent_task_id: Option<String>,
    #[arg(long)]
    rollback_plan: String,
    #[arg(long, default_value = "canary")]
    channel: String,
    #[arg(long, default_value_t = 5)]
    initial_percentage: u8,
    #[arg(long, default_value = ".seaf/releases/release-capsule.json")]
    output: PathBuf,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ReleaseVerifyArgs {
    /// Release capsule path.
    path: PathBuf,
    /// Artifact path to verify against artifact_digest.
    #[arg(long)]
    artifact: Option<PathBuf>,
    /// EvalReport path to verify against eval_report_digest.
    #[arg(long)]
    eval_report: Option<PathBuf>,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct InitReport {
    path: String,
    template: String,
    created: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ModelCheckReport {
    provider: String,
    model: String,
    base_url: String,
    ok: bool,
    status: String,
    message: String,
    latency_ms: Option<u64>,
    error_kind: Option<String>,
}

#[derive(Debug, Serialize)]
struct LoopCommandReport {
    command: String,
    run_id: String,
    ticket_id: String,
    goal_id: String,
    provider: String,
    model: String,
    status: LoopStatus,
    current_step: LoopStepName,
    run_directory: String,
    run_file: String,
    next_action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    candidate_diff_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_head: Option<String>,
}

#[derive(Debug, Serialize)]
struct LoopCleanupReport {
    command: String,
    run_id: String,
    status: LoopStatus,
    candidate_lifecycle: CandidateWorkspaceLifecycle,
    candidate_path: String,
    run_directory: String,
    run_file: String,
}

#[derive(Debug, Serialize)]
struct LoopApprovalReport {
    command: String,
    run_id: String,
    status: LoopStatus,
    current_step: LoopStepName,
    testing_ran: bool,
    run_directory: String,
    run_file: String,
    evidence: HumanApprovalEvidence,
}

#[derive(Debug, Serialize)]
struct LoopBenchReport<'a> {
    provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model_latency_ms: Option<u64>,
    #[serde(flatten)]
    summary: &'a AgentBenchSummary,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            failure.print();
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<(), CliFailure> {
    match cli.command {
        Command::Info => {
            println!("{}", seaf_core::framework_name());
            Ok(())
        }
        Command::Init(args) => init_project(args),
        Command::Goal {
            command: GoalCommand::Validate(args),
        } => validate_file(args, "goal", seaf_core::load_goal_file),
        Command::Policy {
            command: PolicyCommand::Validate(args),
        } => validate_file(args, "policy", seaf_core::load_policy_file),
        Command::Task {
            command: TaskCommand::Brief(args),
        } => generate_task_brief(args),
        Command::Eval {
            command: EvalCommand::Run(args),
        } => run_eval(args),
        Command::Model {
            command: ModelCommand::Check(args),
        } => check_model(args),
        Command::Ticket {
            command: TicketCommand::Validate(args),
        } => validate_file(args, "ticket", seaf_core::load_ticket_file),
        Command::Loop {
            command: LoopCommand::Run(args),
        } => run_loop(args),
        Command::Loop {
            command: LoopCommand::Status(args),
        } => loop_status(args),
        Command::Loop {
            command: LoopCommand::Resume(args),
        } => resume_loop(args),
        Command::Loop {
            command: LoopCommand::Approve(args),
        } => approve_loop(args),
        Command::Loop {
            command: LoopCommand::Cleanup(args),
        } => cleanup_loop(args),
        Command::Loop {
            command: LoopCommand::Smoke(args),
        } => smoke_loop(args),
        Command::Loop {
            command: LoopCommand::Bench(args),
        } => bench_loop(args),
        Command::Release {
            command: ReleaseCommand::Prepare(args),
        } => prepare_release(args),
        Command::Release {
            command: ReleaseCommand::Verify(args),
        } => verify_release(args),
    }
}

fn init_project(args: InitArgs) -> Result<(), CliFailure> {
    if args.template != "adaptive-notes" {
        return Err(CliFailure::message(format!(
            "unsupported template '{}'; supported templates: adaptive-notes",
            args.template
        )));
    }

    let root = args.path;
    let targets = [
        ("adaptive.yaml", templates::ADAPTIVE_GOAL_YAML),
        ("seaf.policy.json", templates::DEFAULT_POLICY_JSON),
        ("seaf.evals.yaml", templates::DEFAULT_EVALS_YAML),
        (".seaf/loops/current/contract.md", templates::LOOP_CONTRACT),
        (".seaf/loops/current/progress.md", templates::LOOP_PROGRESS),
        (".seaf/loops/current/log.md", templates::LOOP_LOG),
    ];

    for (relative_path, _) in targets {
        let target = root.join(relative_path);
        if target.exists() && !args.force {
            return Err(CliFailure::message(format!(
                "{} already exists; rerun with --force to overwrite template files",
                target.display()
            )));
        }
    }

    let mut created = Vec::new();
    for (relative_path, contents) in targets {
        let target = root.join(relative_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                CliFailure::message(format!("could not create {}: {err}", parent.display()))
            })?;
        }
        fs::write(&target, contents).map_err(|err| {
            CliFailure::message(format!("could not write {}: {err}", target.display()))
        })?;
        created.push(relative_path.to_string());
    }

    let report = InitReport {
        path: root.display().to_string(),
        template: args.template,
        created,
    };

    if args.json {
        print_json(&report)?;
    } else {
        println!("initialized SEAF project at {}", report.path);
        for path in &report.created {
            println!("- created {path}");
        }
    }

    Ok(())
}

fn validate_file<T, F>(args: ValidateArgs, kind: &'static str, loader: F) -> Result<(), CliFailure>
where
    F: FnOnce(&Path) -> seaf_core::ValidationResult<T>,
{
    match loader(&args.path) {
        Ok(_) => {
            let report = ValidationReport::valid(kind, Some(&args.path));
            print_validation_report(&report, args.json)?;
            Ok(())
        }
        Err(report) => {
            print_validation_report(&report, args.json)?;
            Err(CliFailure::already_printed())
        }
    }
}

fn generate_task_brief(args: TaskBriefArgs) -> Result<(), CliFailure> {
    let goal = seaf_core::load_goal_file(&args.goal)
        .map_err(|report| CliFailure::validation(report, args.json))?;
    let policy = seaf_core::load_policy_file(&args.policy)
        .map_err(|report| CliFailure::validation(report, args.json))?;
    let task_id = format!("task_{}", sanitize_id(&goal.goal_id));
    let task_dir = args.output_dir.join(&task_id);

    let acceptance_criteria = if args.acceptance_criteria.is_empty() {
        vec![
            "Patch remains traceable to the stated goal.".to_string(),
            "Forbidden paths are not modified.".to_string(),
            "Configured evals pass before commit or merge.".to_string(),
        ]
    } else {
        args.acceptance_criteria
    };

    let brief = AgentTaskBrief {
        task_id: task_id.clone(),
        goal_id: goal.goal_id.clone(),
        objective: goal.objective.metric.clone(),
        signal: None,
        constraints: AgentTaskConstraints {
            default_autonomy_level: policy.default_autonomy_level,
            forbidden_paths: policy.forbidden_paths,
            requires_human_review: policy.requires_human_review,
            allowed_without_review: policy.allowed_without_review,
        },
        relevant_files: args.relevant_files,
        acceptance_criteria,
        generated_at: current_timestamp(),
    };

    fs::create_dir_all(&task_dir)
        .map_err(|err| CliFailure::message(format!("could not create task dir: {err}")))?;
    let json_path = task_dir.join("agent-task.json");
    let markdown_path = task_dir.join("agent-task.md");
    write_json_file(&json_path, &brief)?;
    fs::write(&markdown_path, render_task_markdown(&brief)).map_err(|err| {
        CliFailure::message(format!(
            "could not write {}: {err}",
            markdown_path.display()
        ))
    })?;

    if args.json {
        print_json(&brief)?;
    } else {
        println!("created task brief {}", json_path.display());
        println!("created task brief {}", markdown_path.display());
    }

    Ok(())
}

fn check_model(args: ModelCheckArgs) -> Result<(), CliFailure> {
    if args.provider != "ollama" {
        return finish_model_check(
            ModelCheckReport {
                provider: args.provider,
                model: args.model,
                base_url: args.base_url,
                ok: false,
                status: "failed".to_string(),
                message: "unsupported model provider; supported providers: ollama".to_string(),
                latency_ms: None,
                error_kind: Some("unsupported_provider".to_string()),
            },
            args.json,
        );
    }

    if args.timeout_ms == 0 {
        return finish_model_check(
            ModelCheckReport {
                provider: args.provider,
                model: args.model,
                base_url: args.base_url,
                ok: false,
                status: "failed".to_string(),
                message: "--timeout-ms must be greater than 0".to_string(),
                latency_ms: None,
                error_kind: Some("invalid_timeout".to_string()),
            },
            args.json,
        );
    }

    let provider = OllamaProvider::new(OllamaConfig {
        base_url: args.base_url.clone(),
        ..OllamaConfig::default()
    });
    let request = model_check_request(&args.model, args.timeout_ms);
    let report = match provider.complete(request) {
        Ok(response) => ModelCheckReport {
            provider: args.provider,
            model: args.model,
            base_url: args.base_url,
            ok: true,
            status: "passed".to_string(),
            message: "Ollama model check passed".to_string(),
            latency_ms: Some(response.latency_ms),
            error_kind: None,
        },
        Err(error) => ModelCheckReport {
            provider: args.provider,
            model: args.model,
            base_url: args.base_url,
            ok: false,
            status: "failed".to_string(),
            message: error.message,
            latency_ms: None,
            error_kind: Some(error.kind.to_string()),
        },
    };

    finish_model_check(report, args.json)
}

fn finish_model_check(report: ModelCheckReport, as_json: bool) -> Result<(), CliFailure> {
    let ok = report.ok;
    if as_json {
        print_json(&report)?;
    } else if ok {
        println!(
            "model check passed for {} model {}",
            report.provider, report.model
        );
    } else {
        eprintln!(
            "model check failed for {} model {}: {}",
            report.provider, report.model, report.message
        );
    }

    if ok {
        Ok(())
    } else {
        Err(CliFailure::already_printed())
    }
}

fn model_check_request(model: &str, timeout_ms: u64) -> ModelRequest {
    ModelRequest {
        model: model.to_string(),
        system: "Return JSON only.".to_string(),
        messages: vec![ModelMessage {
            role: ModelMessageRole::User,
            content: "Return exactly a JSON object with boolean field ok set to true.".to_string(),
        }],
        response_schema: Some(serde_json::json!({
            "type": "object",
            "required": ["ok"],
            "properties": {
                "ok": { "type": "boolean" }
            }
        })),
        temperature: 0.0,
        timeout_ms,
    }
}

fn run_loop(args: LoopRunArgs) -> Result<(), CliFailure> {
    let ticket = seaf_core::load_ticket_file(&args.ticket)
        .map_err(|report| CliFailure::validation(report, args.json))?;
    validate_provider_timeout(args.timeout_ms)?;
    let model = loop_model(&args.provider, args.model)?;
    let run_id = match args.run_id {
        Some(run_id) => {
            validate_run_id(&run_id)?;
            run_id
        }
        None => generated_run_id("run"),
    };
    let repository_root = current_repository_root()?;
    let effective_inputs = resolve_effective_project_inputs(
        &repository_root,
        args.config.as_deref(),
        args.policy.as_deref(),
        args.json,
    )?;
    let repository_identity = current_repository_identity(&repository_root)?;
    let eval_config = load_authoritative_eval_config(&repository_root, &ticket)?;
    ensure_clean_git_worktree(args.allow_dirty)?;
    let run = match args.provider.as_str() {
        "fake" => {
            if args.base_url != DEFAULT_OLLAMA_BASE_URL {
                return Err(CliFailure::message(
                    "--base-url is only used with --provider ollama".to_string(),
                ));
            }
            let provider = FakeProvider::new(fake_provider_script_from(LoopStepName::Research));
            start_provider_loop_to_completion(
                ProviderLoopConfig {
                    runs_root: &args.runs_root,
                    run_id: &run_id,
                    ticket: &ticket,
                    model: &model,
                    timeout_ms: args.timeout_ms,
                    repository_root: &repository_root,
                    policy: &effective_inputs.policy,
                    project_config: &effective_inputs.config,
                    repository_identity: &repository_identity,
                    eval_config: &eval_config,
                },
                &args.provider,
                &provider,
            )?
        }
        "ollama" => {
            let provider = OllamaProvider::new(OllamaConfig {
                base_url: args.base_url,
                ..OllamaConfig::default()
            });
            start_provider_loop_to_completion(
                ProviderLoopConfig {
                    runs_root: &args.runs_root,
                    run_id: &run_id,
                    ticket: &ticket,
                    model: &model,
                    timeout_ms: args.timeout_ms,
                    repository_root: &repository_root,
                    policy: &effective_inputs.policy,
                    project_config: &effective_inputs.config,
                    repository_identity: &repository_identity,
                    eval_config: &eval_config,
                },
                &args.provider,
                &provider,
            )?
        }
        _ => {
            return Err(CliFailure::message(format!(
                "unsupported loop provider '{}'; supported providers: fake, ollama",
                args.provider
            )));
        }
    };
    finish_loop_command("run", &args.runs_root, &run, args.json)
}

fn loop_status(args: LoopStatusArgs) -> Result<(), CliFailure> {
    validate_run_id(&args.run_id)?;
    let run = load_persisted_loop_run(&args.runs_root, &args.run_id, args.json)?;
    finish_loop_command("status", &args.runs_root, &run, args.json)
}

fn cleanup_loop(args: LoopCleanupArgs) -> Result<(), CliFailure> {
    validate_run_id(&args.run_id)?;
    let workspace = LoopWorkspace::open_minimal(&args.runs_root, &args.run_id)
        .map_err(|error| CliFailure::message(format!("could not open loop run: {error}")))?;
    let repository_root = current_cleanup_repository_root()?;
    let outcome = cleanup_candidate_workspace_outcome(&workspace, &repository_root)
        .map_err(|error| CliFailure::message(format!("candidate cleanup failed: {error}")))?;
    let CandidateCleanupOutcome {
        run_id,
        status,
        candidate,
    } = outcome;
    let report = LoopCleanupReport {
        command: "cleanup".to_string(),
        run_id,
        status,
        candidate_lifecycle: candidate.lifecycle,
        candidate_path: candidate.path,
        run_directory: workspace.run_directory().display().to_string(),
        run_file: workspace.run_file().display().to_string(),
    };

    if args.json {
        print_json(&report)
    } else {
        println!(
            "cleaned candidate for loop {}: {}",
            report.run_id, report.candidate_path
        );
        println!("run file: {}", report.run_file);
        Ok(())
    }
}

fn approve_loop(args: LoopApproveArgs) -> Result<(), CliFailure> {
    validate_run_id(&args.run_id)?;
    let workspace = LoopWorkspace::open(&args.runs_root, &args.run_id)
        .map_err(|error| CliFailure::message(format!("could not open loop run: {error}")))?;
    let repository_root = current_cleanup_repository_root()?;
    let outcome = approve_candidate_for_testing(
        &workspace,
        &repository_root,
        &args.reviewer,
        &args.confirm_candidate_diff,
        &args.confirm_target_head,
    )
    .map_err(|error| CliFailure::message(format!("candidate approval failed: {error}")))?;
    let report = LoopApprovalReport {
        command: "approve".to_string(),
        run_id: outcome.run.run_id,
        status: outcome.run.status,
        current_step: outcome.run.current_step,
        testing_ran: false,
        run_directory: workspace.run_directory().display().to_string(),
        run_file: workspace.run_file().display().to_string(),
        evidence: outcome.evidence,
    };
    if args.json {
        print_json(&report)
    } else {
        println!(
            "approved loop {} for future Testing; Testing has not run",
            report.run_id
        );
        println!("reviewer: {}", report.evidence.reviewer);
        println!("candidate diff: {}", report.evidence.candidate_diff.digest);
        println!("target HEAD: {}", report.evidence.starting_head);
        println!("run file: {}", report.run_file);
        Ok(())
    }
}

fn resume_loop(args: LoopResumeArgs) -> Result<(), CliFailure> {
    validate_run_id(&args.run_id)?;
    validate_provider_timeout(args.timeout_ms)?;
    let existing = load_persisted_loop_run(&args.runs_root, &args.run_id, args.json)?;
    validate_human_review_execution_barrier(&existing).map_err(loop_runner_failure)?;
    if existing.status == LoopStatus::Approved && existing.input_digests.eval_config.is_none() {
        return Err(CliFailure::message(
            "approved historical run has no authoritative eval config; start a new run".to_string(),
        ));
    }
    let rerun_from = args
        .rerun_from
        .as_deref()
        .map(parse_provider_rerun_step)
        .transpose()?;
    if let Some(step) = rerun_from {
        validate_rerun_eligibility(&existing, step).map_err(loop_runner_failure)?;
    }
    let run = if loop_run_needs_provider_resume(&existing) || rerun_from.is_some() {
        let Some(ticket_path) = args.ticket.as_ref() else {
            return Err(CliFailure::message(
                "--ticket is required to resume an incomplete provider-backed run".to_string(),
            ));
        };
        let ticket = seaf_core::load_ticket_file(ticket_path)
            .map_err(|report| CliFailure::validation(report, args.json))?;
        let repository_root = current_repository_root()?;
        let effective_inputs = resolve_effective_project_inputs(
            &repository_root,
            args.config.as_deref(),
            args.policy.as_deref(),
            args.json,
        )?;
        let repository_identity = current_repository_identity(&repository_root)?;
        let eval_config = load_authoritative_eval_config(&repository_root, &ticket)?;
        validate_resume_ticket_identity(&existing, &ticket)?;
        verify_resume_current_digests(
            &existing,
            &ticket,
            &effective_inputs.policy,
            &effective_inputs.config,
            &repository_identity,
            &eval_config,
        )?;
        let snapshots = authoritative_input_snapshots(
            &ticket,
            &effective_inputs.policy,
            &effective_inputs.config,
            &repository_identity,
            &eval_config,
        )?;
        preflight_authoritative_run_inputs(&args.runs_root, &existing, &snapshots)
            .map_err(loop_runner_failure)?;
        let initialized = match rerun_from {
            Some(step) => {
                InitializedLoopRun::resume_isolated_for_rerun(&args.runs_root, existing, step)
            }
            None => InitializedLoopRun::resume_isolated(&args.runs_root, existing),
        }
        .map_err(loop_runner_failure)?;
        let scaffolded = initialized.scaffold().map_err(loop_runner_failure)?;
        let prepared = scaffolded
            .publish_authoritative_inputs(snapshots)
            .map_err(loop_runner_failure)?;
        let provider_name = prepared.run().provider.clone();
        let ticket = ticket.clone();
        match provider_name.as_str() {
            "fake" => {
                if args.base_url != DEFAULT_OLLAMA_BASE_URL {
                    return Err(CliFailure::message(
                        "--base-url is only used with --provider ollama".to_string(),
                    ));
                }
                let next_step = rerun_from
                    .or_else(|| next_pending_model_step(prepared.run()))
                    .unwrap_or(LoopStepName::Research);
                let provider = FakeProvider::new(fake_provider_script_from(next_step));
                let model = prepared.run().model.clone();
                resume_provider_loop_to_completion(
                    ProviderLoopConfig {
                        runs_root: &args.runs_root,
                        run_id: &args.run_id,
                        ticket: &ticket,
                        model: &model,
                        timeout_ms: args.timeout_ms,
                        repository_root: &repository_root,
                        policy: &effective_inputs.policy,
                        project_config: &effective_inputs.config,
                        repository_identity: &repository_identity,
                        eval_config: &eval_config,
                    },
                    prepared,
                    &provider,
                    rerun_from,
                )?
            }
            "ollama" => {
                let provider = OllamaProvider::new(OllamaConfig {
                    base_url: args.base_url,
                    ..OllamaConfig::default()
                });
                let model = prepared.run().model.clone();
                resume_provider_loop_to_completion(
                    ProviderLoopConfig {
                        runs_root: &args.runs_root,
                        run_id: &args.run_id,
                        ticket: &ticket,
                        model: &model,
                        timeout_ms: args.timeout_ms,
                        repository_root: &repository_root,
                        policy: &effective_inputs.policy,
                        project_config: &effective_inputs.config,
                        repository_identity: &repository_identity,
                        eval_config: &eval_config,
                    },
                    prepared,
                    &provider,
                    rerun_from,
                )?
            }
            provider => {
                return Err(CliFailure::message(format!(
                    "unsupported loop provider '{provider}'; supported providers: fake, ollama"
                )));
            }
        }
    } else {
        existing
    };
    finish_loop_command("resume", &args.runs_root, &run, args.json)
}

fn smoke_loop(args: LoopSmokeArgs) -> Result<(), CliFailure> {
    let ticket = smoke_ticket();
    let run_id = generated_run_id("smoke");
    let run = start_deterministic_loop_to_completion(
        &args.runs_root,
        &run_id,
        &ticket,
        "fake",
        "deterministic-smoke",
    )?;
    finish_loop_command("smoke", &args.runs_root, &run, args.json)
}

fn bench_loop(args: LoopBenchArgs) -> Result<(), CliFailure> {
    match args.provider.as_str() {
        "fake" => bench_loop_fake(args),
        "ollama" => bench_loop_ollama(args),
        _ => Err(CliFailure::message(format!(
            "unsupported benchmark provider '{}'; supported providers: fake, ollama",
            args.provider
        ))),
    }
}

fn bench_loop_fake(args: LoopBenchArgs) -> Result<(), CliFailure> {
    if args.model.is_some() {
        return Err(CliFailure::message(
            "--model is only used with --provider ollama".to_string(),
        ));
    }
    if args.base_url != DEFAULT_OLLAMA_BASE_URL {
        return Err(CliFailure::message(
            "--base-url is only used with --provider ollama".to_string(),
        ));
    }

    let fixture = load_agent_bench_fixture(&args.fixture).map_err(|err| {
        CliFailure::message(format!("could not load AgentBench-lite fixture: {err}"))
    })?;
    let summary = fixture.summary();
    finish_bench_summary(
        LoopBenchReport {
            provider: args.provider,
            model: None,
            base_url: None,
            model_latency_ms: None,
            summary: &summary,
        },
        args.json,
    )
}

fn bench_loop_ollama(args: LoopBenchArgs) -> Result<(), CliFailure> {
    if args.timeout_ms == 0 {
        return Err(CliFailure::message(
            "--timeout-ms must be greater than 0".to_string(),
        ));
    }
    let Some(model) = args.model.clone() else {
        return Err(CliFailure::message(
            "--model is required with --provider ollama".to_string(),
        ));
    };

    let fixture = load_agent_bench_fixture(&args.fixture).map_err(|err| {
        CliFailure::message(format!("could not load AgentBench-lite fixture: {err}"))
    })?;
    let provider = OllamaProvider::new(OllamaConfig {
        base_url: args.base_url.clone(),
        ..OllamaConfig::default()
    });
    let response = provider
        .complete(agent_bench_ollama_smoke_request(&model, args.timeout_ms))
        .map_err(|error| {
            CliFailure::message(format!(
                "Ollama AgentBench-lite smoke failed: {}",
                error.message
            ))
        })?;
    validate_agent_bench_ollama_smoke_content(&response.content)?;
    let summary = fixture.summary();
    finish_bench_summary(
        LoopBenchReport {
            provider: args.provider,
            model: Some(model),
            base_url: Some(args.base_url),
            model_latency_ms: Some(response.latency_ms),
            summary: &summary,
        },
        args.json,
    )
}

fn finish_bench_summary(report: LoopBenchReport<'_>, as_json: bool) -> Result<(), CliFailure> {
    if as_json {
        print_json(&report)?;
    } else {
        println!("AgentBench-lite {}-provider summary", report.provider);
        if let Some(model) = &report.model {
            println!("model: {model}");
        }
        if let Some(base_url) = &report.base_url {
            println!("base_url: {base_url}");
        }
        if let Some(latency_ms) = report.model_latency_ms {
            println!("model_latency_ms: {latency_ms}");
        }
        println!("tickets: {}", report.summary.ticket_count);
        println!("schema_valid_rate: {:.3}", report.summary.schema_valid_rate);
        println!(
            "repair_success_rate: {:.3}",
            report.summary.repair_success_rate
        );
        println!("patch_apply_rate: {:.3}", report.summary.patch_apply_rate);
        println!("eval_pass_rate: {:.3}", report.summary.eval_pass_rate);
        println!(
            "forbidden_violation_count: {}",
            report.summary.forbidden_violation_count
        );
        println!(
            "eval_weakening_accepted_count: {}",
            report.summary.eval_weakening_accepted_count
        );
        println!("median_latency_ms: {}", report.summary.median_latency_ms);
    }

    match evaluate_zero_tolerance(report.summary) {
        Ok(()) => Ok(()),
        Err(error) => Err(CliFailure::message(error.to_string())),
    }
}

fn agent_bench_ollama_smoke_request(model: &str, timeout_ms: u64) -> ModelRequest {
    ModelRequest {
        model: model.to_string(),
        system: "Return JSON only.".to_string(),
        messages: vec![ModelMessage {
            role: ModelMessageRole::User,
            content: "Return exactly a JSON object with boolean field ok set to true for an AgentBench-lite smoke check.".to_string(),
        }],
        response_schema: Some(serde_json::json!({
            "type": "object",
            "required": ["ok"],
            "properties": {
                "ok": { "type": "boolean" }
            }
        })),
        temperature: 0.0,
        timeout_ms,
    }
}

fn validate_agent_bench_ollama_smoke_content(content: &str) -> Result<(), CliFailure> {
    let value: serde_json::Value = serde_json::from_str(content).map_err(|err| {
        CliFailure::message(format!(
            "Ollama AgentBench-lite smoke response.content must be a JSON object with ok == true: {err}"
        ))
    })?;
    let Some(object) = value.as_object() else {
        return Err(CliFailure::message(
            "Ollama AgentBench-lite smoke response.content must be a JSON object with ok == true"
                .to_string(),
        ));
    };

    match object.get("ok").and_then(serde_json::Value::as_bool) {
        Some(true) => Ok(()),
        Some(false) => Err(CliFailure::message(
            "Ollama AgentBench-lite smoke response.content must have ok == true; got false"
                .to_string(),
        )),
        None => Err(CliFailure::message(
            "Ollama AgentBench-lite smoke response.content must include boolean field ok == true"
                .to_string(),
        )),
    }
}

struct ProviderLoopConfig<'a> {
    runs_root: &'a Path,
    run_id: &'a str,
    ticket: &'a TicketSpec,
    model: &'a str,
    timeout_ms: u64,
    repository_root: &'a Path,
    policy: &'a Policy,
    project_config: &'a ProjectConfig,
    repository_identity: &'a RepositoryIdentity,
    eval_config: &'a AuthoritativeEvalConfig,
}

fn start_provider_loop_to_completion<P: ModelProvider + ?Sized>(
    config: ProviderLoopConfig<'_>,
    provider_name: &str,
    provider: &P,
) -> Result<LoopRun, CliFailure> {
    let policy = config.policy;
    let project_config = config.project_config;
    let input_digests = current_input_digests(
        config.ticket,
        policy,
        project_config,
        config.repository_identity,
        Some(config.eval_config),
    )?;
    let runner_config = LoopRunnerConfig::for_ticket(
        config.runs_root,
        config.run_id,
        config.ticket,
        provider_name.to_string(),
        config.model.to_string(),
        input_digests,
    );
    let initialized = InitializedLoopRun::create_isolated(runner_config, config.repository_root)
        .map_err(loop_runner_failure)?;
    let candidate_root = PathBuf::from(
        &initialized
            .run()
            .candidate_workspace
            .as_ref()
            .expect("isolated initializer guarantees candidate authority")
            .path,
    );
    let scaffolded = initialized.scaffold().map_err(loop_runner_failure)?;
    let prepared = scaffolded
        .publish_authoritative_inputs(authoritative_input_snapshots(
            config.ticket,
            policy,
            project_config,
            config.repository_identity,
            config.eval_config,
        )?)
        .map_err(loop_runner_failure)?;
    let context_request = provider_context_request(&candidate_root, config.ticket, policy);
    let patch_gate_config =
        ProviderPatchGateConfig::for_ticket(&candidate_root, config.ticket, policy.clone(), true);
    let mut patch_runner = GitCommandRunner;
    let mut step_runner = ProviderStepRunner::new(provider, config.model, config.timeout_ms)
        .with_ticket(config.ticket.clone())
        .with_context_pack_request(context_request)
        .with_patch_gate(patch_gate_config, &mut patch_runner);
    let mut runner =
        LoopRunner::start_initialized(prepared, &mut step_runner).map_err(loop_runner_failure)?;
    runner
        .run_to_completion()
        .map_err(loop_runner_failure)
        .cloned()
}

fn resume_provider_loop_to_completion<P: ModelProvider + ?Sized>(
    config: ProviderLoopConfig<'_>,
    prepared: PreparedLoopRun,
    provider: &P,
    rerun_from: Option<LoopStepName>,
) -> Result<LoopRun, CliFailure> {
    let policy = config.policy;
    let candidate_root = PathBuf::from(
        &prepared
            .run()
            .candidate_workspace
            .as_ref()
            .expect("prepared isolated run guarantees candidate authority")
            .path,
    );
    let context_request = provider_context_request(&candidate_root, config.ticket, policy);
    let mut patch_gate_config =
        ProviderPatchGateConfig::for_ticket(&candidate_root, config.ticket, policy.clone(), true);
    patch_gate_config.apply_patch &= persisted_apply_authority(prepared.run());
    let mut patch_runner = GitCommandRunner;
    let mut step_runner = ProviderStepRunner::new(provider, config.model, config.timeout_ms)
        .with_ticket(config.ticket.clone())
        .with_context_pack_request(context_request)
        .with_patch_gate(patch_gate_config, &mut patch_runner);
    let runner =
        LoopRunner::resume_initialized(prepared, &mut step_runner).map_err(loop_runner_failure)?;
    let mut runner = match rerun_from {
        Some(step) => runner.rerun_from(step).map_err(loop_runner_failure)?,
        None => runner,
    };
    runner
        .run_to_completion()
        .map_err(loop_runner_failure)
        .cloned()
}

fn parse_provider_rerun_step(value: &str) -> Result<LoopStepName, CliFailure> {
    match value {
        "research" => Ok(LoopStepName::Research),
        "analysis" => Ok(LoopStepName::Analysis),
        "spec" | "spec-creation" => Ok(LoopStepName::SpecCreation),
        "spec-review" => Ok(LoopStepName::SpecReview),
        "development" => Ok(LoopStepName::Development),
        "output-review" => Ok(LoopStepName::OutputReview),
        _ => Err(CliFailure::message(format!(
            "unsupported --rerun-from step '{value}'; expected research, analysis, spec, spec-review, development, or output-review"
        ))),
    }
}

fn provider_context_request(
    repository_root: &Path,
    ticket: &TicketSpec,
    policy: &Policy,
) -> ContextPackRequest {
    ContextPackRequest::for_ticket(
        repository_root,
        Path::new("provider-runner-will-set-run-directory"),
        ticket,
        &policy.forbidden_paths,
        ContextLimits {
            max_bytes_per_file: 32 * 1024,
            max_total_bytes: 128 * 1024,
        },
    )
}

fn default_policy() -> Result<Policy, CliFailure> {
    serde_json::from_str(templates::DEFAULT_POLICY_JSON)
        .map_err(|err| CliFailure::message(format!("could not parse default policy: {err}")))
}

struct EffectiveProjectInputs {
    policy: Policy,
    config: ProjectConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoryIdentity {
    worktree_root: String,
    git_common_dir: String,
}

fn resolve_effective_project_inputs(
    repository_root: &Path,
    explicit_config: Option<&Path>,
    explicit_policy: Option<&Path>,
    as_json: bool,
) -> Result<EffectiveProjectInputs, CliFailure> {
    let config_authority = match explicit_config {
        Some(path) => Some(load_repository_project_config(
            repository_root,
            path,
            as_json,
        )?),
        None if explicit_policy.is_some() => None,
        None => {
            let discovered = repository_root.join("seaf.config.json");
            if authority_path_exists(&discovered, "project config")? {
                Some(load_repository_project_config(
                    repository_root,
                    &discovered,
                    as_json,
                )?)
            } else {
                None
            }
        }
    };

    if let Some(path) = explicit_policy {
        let policy_path = canonical_repository_file(repository_root, path, "policy")?;
        let policy = seaf_core::load_policy_file(&policy_path)
            .map_err(|report| CliFailure::validation(report, as_json))?;
        return Ok(EffectiveProjectInputs {
            policy,
            config: ProjectConfig {
                policy_path: repository_relative_path(repository_root, &policy_path, "policy")?,
            },
        });
    }

    if let Some((config, config_path)) = config_authority {
        let config_dir = config_path.parent().ok_or_else(|| {
            CliFailure::message(format!(
                "project config {} has no parent directory",
                config_path.display()
            ))
        })?;
        let policy_path = canonical_repository_file(
            repository_root,
            &config_dir.join(&config.policy_path),
            "policy named by project config",
        )?;
        let policy = seaf_core::load_policy_file(&policy_path)
            .map_err(|report| CliFailure::validation(report, as_json))?;
        return Ok(EffectiveProjectInputs { policy, config });
    }

    let root_policy = repository_root.join("seaf.policy.json");
    if authority_path_exists(&root_policy, "root policy")? {
        let policy_path = canonical_repository_file(repository_root, &root_policy, "root policy")?;
        let policy = seaf_core::load_policy_file(&policy_path)
            .map_err(|report| CliFailure::validation(report, as_json))?;
        return Ok(EffectiveProjectInputs {
            policy,
            config: ProjectConfig {
                policy_path: "seaf.policy.json".to_string(),
            },
        });
    }

    Err(CliFailure::message(
        "no authoritative loop policy found; pass --policy, pass --config, add seaf.config.json at the Git root, or add seaf.policy.json at the Git root"
            .to_string(),
    ))
}

fn authority_path_exists(path: &Path, kind: &str) -> Result<bool, CliFailure> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(CliFailure::message(format!(
            "could not inspect {kind} {}: {error}",
            path.display()
        ))),
    }
}

fn load_repository_project_config(
    repository_root: &Path,
    path: &Path,
    as_json: bool,
) -> Result<(ProjectConfig, PathBuf), CliFailure> {
    let config_path = canonical_repository_file(repository_root, path, "project config")?;
    let config = seaf_core::load_project_config_file(&config_path)
        .map_err(|report| CliFailure::validation(report, as_json))?;
    Ok((config, config_path))
}

fn canonical_repository_file(
    repository_root: &Path,
    path: &Path,
    kind: &str,
) -> Result<PathBuf, CliFailure> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| {
                CliFailure::message(format!("could not inspect current directory: {error}"))
            })?
            .join(path)
    };
    let canonical = candidate.canonicalize().map_err(|error| {
        CliFailure::message(format!(
            "could not resolve {kind} {}: {error}",
            candidate.display()
        ))
    })?;
    if !canonical.starts_with(repository_root) {
        return Err(CliFailure::message(format!(
            "{kind} {} resolves outside the git repository {}",
            candidate.display(),
            repository_root.display()
        )));
    }
    if !canonical.is_file() {
        return Err(CliFailure::message(format!(
            "{kind} {} is not a file",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn repository_relative_path(
    repository_root: &Path,
    path: &Path,
    kind: &str,
) -> Result<String, CliFailure> {
    let relative = path.strip_prefix(repository_root).map_err(|_| {
        CliFailure::message(format!(
            "{kind} {} is outside the git repository {}",
            path.display(),
            repository_root.display()
        ))
    })?;
    let relative = relative.to_str().ok_or_else(|| {
        CliFailure::message(format!("{kind} path {} is not UTF-8", path.display()))
    })?;
    Ok(relative.replace(std::path::MAIN_SEPARATOR, "/"))
}

#[derive(Debug, Clone)]
struct AuthoritativeEvalConfig {
    bytes: Vec<u8>,
    digest: String,
}

fn load_authoritative_eval_config(
    repository_root: &Path,
    ticket: &TicketSpec,
) -> Result<AuthoritativeEvalConfig, CliFailure> {
    load_authoritative_eval_config_with_hook(repository_root, ticket, || Ok(()))
}

fn load_authoritative_eval_config_with_hook<F>(
    repository_root: &Path,
    ticket: &TicketSpec,
    before_open: F,
) -> Result<AuthoritativeEvalConfig, CliFailure>
where
    F: FnOnce() -> io::Result<()>,
{
    let configured = ticket.eval.as_ref().ok_or_else(|| {
        CliFailure::message(
            "ticket.eval.config is required for provider-backed loop runs".to_string(),
        )
    })?;
    let raw = configured.config.as_str();
    if raw.is_empty()
        || raw.starts_with('/')
        || raw.ends_with('/')
        || raw.contains("//")
        || raw.contains('\\')
        || raw.contains(':')
        || raw.chars().any(char::is_control)
        || raw
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(CliFailure::message(
            "ticket.eval.config must be a normalized portable repository-relative '/' path"
                .to_string(),
        ));
    }
    let relative = Path::new(raw);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CliFailure::message(
            "ticket.eval.config must be a normalized repository-relative path without traversal"
                .to_string(),
        ));
    }

    let candidate = repository_root.join(relative);
    let prevalidated = inspect_eval_config_path(repository_root, relative)?;
    let canonical = candidate.canonicalize().map_err(|error| {
        CliFailure::message(format!(
            "could not canonicalize ticket.eval.config {}: {error}",
            candidate.display()
        ))
    })?;
    if !canonical.starts_with(repository_root) || canonical != candidate {
        return Err(CliFailure::message(format!(
            "ticket.eval.config {} does not name its exact repository file",
            candidate.display()
        )));
    }
    before_open().map_err(|error| {
        CliFailure::message(format!(
            "ticket.eval.config pre-open check failed for {}: {error}",
            canonical.display()
        ))
    })?;
    let mut file = open_eval_config_no_follow(&canonical).map_err(|error| {
        CliFailure::message(format!(
            "could not safely open ticket.eval.config {}: {error}",
            canonical.display()
        ))
    })?;
    let opened = file.metadata().map_err(|error| {
        CliFailure::message(format!(
            "could not inspect opened ticket.eval.config {}: {error}",
            canonical.display()
        ))
    })?;
    if !opened.is_file() || !same_eval_config_file_identity(&prevalidated, &opened) {
        return Err(CliFailure::message(format!(
            "ticket.eval.config {} changed before its authoritative handle was opened",
            canonical.display()
        )));
    }
    let revalidated = inspect_eval_config_path(repository_root, relative)?;
    if !same_eval_config_file_identity(&opened, &revalidated) {
        return Err(CliFailure::message(format!(
            "ticket.eval.config {} changed while its authoritative handle was opened",
            canonical.display()
        )));
    }
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|error| {
        CliFailure::message(format!(
            "could not read ticket.eval.config {} as UTF-8: {error}",
            canonical.display()
        ))
    })?;
    let parsed = seaf_core::parse_eval_config(&text)
        .map_err(|error| CliFailure::message(format!("invalid ticket.eval.config: {error}")))?;
    let bytes = canonical_json_bytes(&parsed).map_err(|error| {
        CliFailure::message(format!(
            "could not canonicalize ticket.eval.config: {error}"
        ))
    })?;
    let digest = canonical_sha256_digest(&parsed).map_err(|error| {
        CliFailure::message(format!("could not digest ticket.eval.config: {error}"))
    })?;
    Ok(AuthoritativeEvalConfig { bytes, digest })
}

fn inspect_eval_config_path(
    repository_root: &Path,
    relative: &Path,
) -> Result<fs::Metadata, CliFailure> {
    let components = relative.components().collect::<Vec<_>>();
    let mut candidate = repository_root.to_path_buf();
    let mut final_metadata = None;
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            return Err(CliFailure::message(
                "ticket.eval.config must be a normalized repository-relative path without traversal"
                    .to_string(),
            ));
        };
        candidate.push(component);
        let metadata = fs::symlink_metadata(&candidate).map_err(|error| {
            CliFailure::message(format!(
                "could not resolve ticket.eval.config {}: {error}",
                candidate.display()
            ))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(CliFailure::message(format!(
                "ticket.eval.config {} must not use a symlink alias",
                candidate.display()
            )));
        }
        if index + 1 == components.len() {
            if !metadata.is_file() {
                return Err(CliFailure::message(format!(
                    "ticket.eval.config {} is not a real regular file",
                    candidate.display()
                )));
            }
            final_metadata = Some(metadata);
        } else if !metadata.is_dir() {
            return Err(CliFailure::message(format!(
                "ticket.eval.config parent {} is not a real directory",
                candidate.display()
            )));
        }
    }
    final_metadata
        .ok_or_else(|| CliFailure::message("ticket.eval.config must not be empty".to_string()))
}

#[cfg(target_os = "macos")]
fn open_eval_config_no_follow(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).custom_flags(0x100);
    options.open(path)
}

#[cfg(target_os = "linux")]
fn open_eval_config_no_follow(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).custom_flags(0x20_000);
    options.open(path)
}

#[cfg(target_os = "windows")]
fn open_eval_config_no_follow(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).custom_flags(0x0020_0000);
    options.open(path)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn open_eval_config_no_follow(_path: &Path) -> io::Result<fs::File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure eval config opening is unsupported on this platform",
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn same_eval_config_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(target_os = "windows")]
fn same_eval_config_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.volume_serial_number() == right.volume_serial_number()
        && left.file_index() == right.file_index()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn same_eval_config_file_identity(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    false
}

fn current_input_digests<T: Serialize, R: Serialize>(
    ticket: &TicketSpec,
    policy: &Policy,
    project_config: &T,
    repository_identity: &R,
    eval_config: Option<&AuthoritativeEvalConfig>,
) -> Result<LoopInputDigests, CliFailure> {
    Ok(LoopInputDigests {
        ticket: canonical_sha256_digest(ticket).map_err(canonical_digest_failure("ticket"))?,
        policy: canonical_sha256_digest(policy).map_err(canonical_digest_failure("policy"))?,
        config: canonical_sha256_digest(project_config)
            .map_err(canonical_digest_failure("config"))?,
        repository: canonical_sha256_digest(repository_identity)
            .map_err(canonical_digest_failure("repository identity"))?,
        eval_config: eval_config.map(|authority| authority.digest.clone()),
    })
}

fn verify_resume_current_digests(
    run: &LoopRun,
    ticket: &TicketSpec,
    policy: &Policy,
    project_config: &ProjectConfig,
    repository_identity: &RepositoryIdentity,
    eval_config: &AuthoritativeEvalConfig,
) -> Result<(), CliFailure> {
    let current_digests = current_input_digests(
        ticket,
        policy,
        project_config,
        repository_identity,
        Some(eval_config),
    )?;
    verify_current_digest("ticket", &run.input_digests.ticket, &current_digests.ticket)?;
    verify_current_digest("policy", &run.input_digests.policy, &current_digests.policy)?;
    verify_current_digest("config", &run.input_digests.config, &current_digests.config)?;
    verify_current_digest(
        "repository",
        &run.input_digests.repository,
        &current_digests.repository,
    )?;
    let persisted_eval = run.input_digests.eval_config.as_deref().ok_or_else(|| {
        CliFailure::message(
            "resume run has no authoritative eval config digest; start a new run".to_string(),
        )
    })?;
    verify_current_digest(
        "eval config",
        persisted_eval,
        current_digests
            .eval_config
            .as_deref()
            .expect("current provider inputs include eval config"),
    )?;

    Ok(())
}

fn authoritative_input_snapshots(
    ticket: &TicketSpec,
    policy: &Policy,
    project_config: &ProjectConfig,
    repository_identity: &RepositoryIdentity,
    eval_config: &AuthoritativeEvalConfig,
) -> Result<AuthoritativeRunInputSnapshots, CliFailure> {
    let ticket = canonical_json_bytes(ticket).map_err(|error| {
        CliFailure::message(format!("could not serialize effective ticket: {error}"))
    })?;
    Ok(AuthoritativeRunInputSnapshots {
        provider_ticket: ticket.clone(),
        ticket,
        policy: canonical_json_bytes(policy).map_err(|error| {
            CliFailure::message(format!("could not serialize effective policy: {error}"))
        })?,
        config: canonical_json_bytes(project_config).map_err(|error| {
            CliFailure::message(format!("could not serialize effective config: {error}"))
        })?,
        repository: canonical_json_bytes(repository_identity).map_err(|error| {
            CliFailure::message(format!("could not serialize repository identity: {error}"))
        })?,
        eval_config: eval_config.bytes.clone(),
    })
}

fn verify_current_digest(
    kind: &str,
    persisted_digest: &str,
    current_digest: &str,
) -> Result<(), CliFailure> {
    if persisted_digest == current_digest {
        return Ok(());
    }

    let detail = match kind {
        "ticket" => {
            "does not match the original provider run ticket snapshot; use the matching --ticket input or start a new run"
        }
        "repository" => {
            "does not match the persisted run; resume from the original repository or start a new run"
        }
        _ => {
            "does not match the persisted run; use matching --config/--policy inputs or start a new run"
        }
    };
    let kind = if kind == "repository" {
        "repository identity"
    } else {
        kind
    };
    Err(CliFailure::message(format!(
        "resume {kind} digest {detail}"
    )))
}

fn canonical_digest_failure(kind: &'static str) -> impl FnOnce(serde_json::Error) -> CliFailure {
    move |error| CliFailure::message(format!("could not digest effective {kind}: {error}"))
}

fn loop_model(provider: &str, model: Option<String>) -> Result<String, CliFailure> {
    match (provider, model) {
        ("fake", Some(model)) => Ok(model),
        ("fake", None) => Ok("fake-local".to_string()),
        ("ollama", Some(model)) => Ok(model),
        ("ollama", None) => Err(CliFailure::message(
            "--model is required with --provider ollama".to_string(),
        )),
        (_, Some(model)) => Ok(model),
        (_, None) => Ok("fake-local".to_string()),
    }
}

fn validate_provider_timeout(timeout_ms: u64) -> Result<(), CliFailure> {
    if timeout_ms == 0 {
        return Err(CliFailure::message(
            "--timeout-ms must be greater than 0".to_string(),
        ));
    }
    Ok(())
}

fn current_repository_root() -> Result<PathBuf, CliFailure> {
    resolve_current_repository_root(ProcessCommand::new("git"))
}

fn current_cleanup_repository_root() -> Result<PathBuf, CliFailure> {
    let mut command = ProcessCommand::new("git");
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
        "GIT_ATTR_NOSYSTEM",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_EXTERNAL_DIFF",
        "GIT_DIFF_OPTS",
        "GIT_PAGER",
        "GIT_EDITOR",
        "GIT_SEQUENCE_EDITOR",
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
    let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", null_device)
        .env("GIT_CONFIG_GLOBAL", null_device)
        .env("GIT_ATTR_NOSYSTEM", "1");
    resolve_current_repository_root(command)
}

fn resolve_current_repository_root(mut command: ProcessCommand) -> Result<PathBuf, CliFailure> {
    let output = command
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|err| {
            CliFailure::message(format!("could not inspect git repository root: {err}"))
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        return Err(CliFailure::message(format!(
            "could not inspect git repository root{suffix}; rerun from a git repository"
        )));
    }

    let root = String::from_utf8(output.stdout)
        .map_err(|err| CliFailure::message(format!("git repository root was not UTF-8: {err}")))?;
    let root = PathBuf::from(root.trim());
    root.canonicalize().map_err(|err| {
        CliFailure::message(format!(
            "could not canonicalize git repository root {}: {err}",
            root.display()
        ))
    })
}

fn current_repository_identity(repository_root: &Path) -> Result<RepositoryIdentity, CliFailure> {
    let output = ProcessCommand::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(repository_root)
        .output()
        .map_err(|error| {
            CliFailure::message(format!("could not inspect Git common directory: {error}"))
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(CliFailure::message(format!(
            "could not inspect Git common directory: {}",
            detail.trim()
        )));
    }
    let common_dir = String::from_utf8(output.stdout).map_err(|error| {
        CliFailure::message(format!("Git common directory was not UTF-8: {error}"))
    })?;
    let common_dir = PathBuf::from(common_dir.trim());
    let common_dir = if common_dir.is_absolute() {
        common_dir
    } else {
        repository_root.join(common_dir)
    };
    let common_dir = common_dir.canonicalize().map_err(|error| {
        CliFailure::message(format!(
            "could not canonicalize Git common directory {}: {error}",
            common_dir.display()
        ))
    })?;

    Ok(RepositoryIdentity {
        worktree_root: utf8_repository_identity_path(repository_root, "worktree root")?,
        git_common_dir: utf8_repository_identity_path(&common_dir, "Git common directory")?,
    })
}

fn utf8_repository_identity_path(path: &Path, kind: &str) -> Result<String, CliFailure> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| CliFailure::message(format!("{kind} {} is not UTF-8", path.display())))
}

fn loop_run_needs_provider_resume(run: &LoopRun) -> bool {
    matches!(run.status, LoopStatus::Pending | LoopStatus::Running)
}

fn validate_resume_ticket_identity(run: &LoopRun, ticket: &TicketSpec) -> Result<(), CliFailure> {
    let mut mismatches = Vec::new();
    if run.ticket_id != ticket.ticket_id {
        mismatches.push(format!(
            "ticket_id mismatch (run has {}, ticket has {})",
            run.ticket_id, ticket.ticket_id
        ));
    }
    if run.goal_id != ticket.goal_id {
        mismatches.push(format!(
            "goal_id mismatch (run has {}, ticket has {})",
            run.goal_id, ticket.goal_id
        ));
    }
    if !mismatches.is_empty() {
        return Err(CliFailure::message(format!(
            "resume ticket does not match persisted run: {}; use the matching --ticket input or start a new run",
            mismatches.join("; ")
        )));
    }
    Ok(())
}

fn persisted_apply_authority(run: &LoopRun) -> bool {
    run.policy_decisions.iter().any(|entry| {
        serde_json::to_value(entry)
            .ok()
            .and_then(|value| serde_json::from_value::<PolicyDecision>(value).ok())
            .is_some_and(|decision| decision.patch_id == run.run_id && decision.apply_requested)
    })
}

fn next_pending_model_step(run: &LoopRun) -> Option<LoopStepName> {
    run.steps
        .iter()
        .find(|step| {
            matches!(
                step.status,
                LoopStepStatus::Pending | LoopStepStatus::Running
            ) && is_model_step(step.name)
        })
        .map(|step| step.name)
}

fn is_model_step(step: LoopStepName) -> bool {
    matches!(
        step,
        LoopStepName::Research
            | LoopStepName::Analysis
            | LoopStepName::SpecCreation
            | LoopStepName::SpecReview
            | LoopStepName::Development
            | LoopStepName::OutputReview
    )
}

fn fake_provider_script_from(
    start_step: LoopStepName,
) -> Vec<Result<ModelResponse, seaf_models::ModelError>> {
    fake_provider_script()
        .into_iter()
        .filter(|(step, _)| step_index(*step) >= step_index(start_step))
        .map(|(_, response)| Ok(fake_model_response(response)))
        .collect()
}

fn fake_provider_script() -> Vec<(LoopStepName, String)> {
    vec![
        (
            LoopStepName::Research,
            fake_agent_response(
                "researcher",
                "Relevant CLI wiring is concentrated in the loop command.",
                "Proceed to analysis.",
            ),
        ),
        (
            LoopStepName::Analysis,
            fake_agent_response(
                "analyzer",
                "The provider path must preserve context and gate artifacts.",
                "Write a narrow implementation spec.",
            ),
        ),
        (
            LoopStepName::SpecCreation,
            fake_agent_response(
                "spec_writer",
                "Use the same ProviderStepRunner path as live providers.",
                "Send the spec for review.",
            ),
        ),
        (
            LoopStepName::SpecReview,
            fake_reviewer_response(
                "spec_reviewer",
                "approve_spec",
                "The spec is narrow and testable.",
            ),
        ),
        (LoopStepName::Development, fake_developer_response()),
        (
            LoopStepName::OutputReview,
            fake_reviewer_response(
                "output_reviewer",
                "approve_for_tests",
                "The patch is acceptable for test verification.",
            ),
        ),
    ]
}

fn fake_agent_response(role: &str, summary: &str, next_step_recommendation: &str) -> String {
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

fn fake_reviewer_response(role: &str, decision: &str, summary: &str) -> String {
    serde_json::json!({
        "role": role,
        "decision": decision,
        "summary": summary,
        "blocking_issues": [],
        "non_blocking_issues": []
    })
    .to_string()
}

fn fake_developer_response() -> String {
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

fn fake_model_response(content: String) -> ModelResponse {
    ModelResponse {
        content,
        latency_ms: 0,
        raw_provider_metadata: serde_json::json!({ "provider": "fake" }),
    }
}

fn step_index(step: LoopStepName) -> usize {
    match step {
        LoopStepName::Research => 0,
        LoopStepName::Analysis => 1,
        LoopStepName::SpecCreation => 2,
        LoopStepName::SpecReview => 3,
        LoopStepName::Development => 4,
        LoopStepName::OutputReview => 5,
        LoopStepName::Testing => 6,
        LoopStepName::EvalReport => 7,
    }
}

fn start_deterministic_loop_to_completion(
    runs_root: &Path,
    run_id: &str,
    ticket: &TicketSpec,
    provider: &str,
    model: &str,
) -> Result<LoopRun, CliFailure> {
    let mut step_runner = DeterministicStepRunner;
    let policy = default_policy()?;
    let no_project_config = Option::<ProjectConfig>::None;
    let repository_root = current_repository_root()?;
    let repository_identity = current_repository_identity(&repository_root)?;
    let config = LoopRunnerConfig::for_ticket(
        runs_root,
        run_id,
        ticket,
        provider.to_string(),
        model.to_string(),
        current_input_digests(
            ticket,
            &policy,
            &no_project_config,
            &repository_identity,
            None,
        )?,
    );
    let mut runner = LoopRunner::start(config, &mut step_runner).map_err(loop_runner_failure)?;
    let run = runner
        .run_to_completion()
        .map_err(loop_runner_failure)?
        .clone();
    let mut run = run;
    persist_deterministic_policy_evidence(runs_root, &mut run)?;
    Ok(run)
}

fn persist_deterministic_policy_evidence(
    runs_root: &Path,
    run: &mut LoopRun,
) -> Result<(), CliFailure> {
    if !run.policy_decisions.is_empty() {
        return Ok(());
    }

    let decision = PolicyDecision {
        patch_id: run.run_id.clone(),
        patch_sha256: "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            .to_string(),
        changed_paths: Vec::new(),
        decision: PatchDecisionKind::Allowed,
        reasons: Vec::new(),
        requires_human_review: false,
        apply_requested: false,
        applied: false,
    };
    let value = serde_json::to_value(decision).map_err(|err| {
        CliFailure::message(format!("could not serialize policy decision: {err}"))
    })?;
    let entry = serde_json::from_value(value)
        .map_err(|err| CliFailure::message(format!("could not encode policy decision: {err}")))?;
    run.policy_decisions.push(entry);

    seaf_loop::state::write_run_file(&runs_root.join(&run.run_id).join("run.json"), run)
        .map_err(|err| CliFailure::message(format!("could not persist loop run: {err}")))
}

fn load_persisted_loop_run(
    runs_root: &Path,
    run_id: &str,
    as_json: bool,
) -> Result<LoopRun, CliFailure> {
    let run_file = runs_root.join(run_id).join("run.json");
    let run = seaf_core::load_loop_run_file(&run_file)
        .map_err(|report| CliFailure::validation(report, as_json))?;

    if !is_valid_run_id(&run.run_id) {
        return Err(loop_run_validation_failure(
            &run_file,
            "run_id",
            "must use only ASCII letters, numbers, '-' or '_'",
            as_json,
        ));
    }

    if run.run_id != run_id {
        return Err(loop_run_validation_failure(
            &run_file,
            "run_id",
            "must match requested --run-id",
            as_json,
        ));
    }

    Ok(run)
}

fn validate_run_id(run_id: &str) -> Result<(), CliFailure> {
    if is_valid_run_id(run_id) {
        Ok(())
    } else {
        Err(CliFailure::message(
            "invalid run ID; use only ASCII letters, numbers, '-' or '_'".to_string(),
        ))
    }
}

fn is_valid_run_id(run_id: &str) -> bool {
    !run_id.is_empty()
        && run_id.trim() == run_id
        && run_id != "."
        && run_id != ".."
        && !Path::new(run_id).is_absolute()
        && !run_id.contains('/')
        && !run_id.contains('\\')
        && run_id
            .chars()
            .all(|item| item.is_ascii_alphanumeric() || item == '-' || item == '_')
}

fn loop_run_validation_failure(
    path: &Path,
    field: &str,
    message: &str,
    as_json: bool,
) -> CliFailure {
    CliFailure::validation(
        ValidationReport::invalid(
            "loop_run",
            Some(path),
            vec![FieldError::new(field, message)],
        ),
        as_json,
    )
}

fn finish_loop_command(
    command: &str,
    runs_root: &Path,
    run: &LoopRun,
    as_json: bool,
) -> Result<(), CliFailure> {
    let report = loop_command_report(command, runs_root, run);
    if as_json {
        print_json(&report)?;
    } else {
        println!(
            "loop {} {}: status {}, current step {:?}",
            report.command,
            report.run_id,
            loop_status_label(report.status),
            report.current_step
        );
        println!("next action: {}", report.next_action);
        if let Some(candidate_diff_digest) = &report.candidate_diff_digest {
            println!("--confirm-candidate-diff: {candidate_diff_digest}");
        }
        if let Some(target_head) = &report.target_head {
            println!("--confirm-target-head: {target_head}");
        }
        println!("run file: {}", report.run_file);
    }
    Ok(())
}

fn loop_status_label(status: LoopStatus) -> String {
    match status {
        LoopStatus::AwaitingHumanReview => "awaiting_human_review".to_string(),
        LoopStatus::Approved => "approved".to_string(),
        legacy => format!("{legacy:?}"),
    }
}

fn loop_command_report(command: &str, runs_root: &Path, run: &LoopRun) -> LoopCommandReport {
    let run_directory = runs_root.join(&run.run_id);
    let confirmation_candidate = matches!(
        run.status,
        LoopStatus::AwaitingHumanReview | LoopStatus::Approved
    )
    .then(|| run.candidate_workspace.as_ref())
    .flatten();
    LoopCommandReport {
        command: command.to_string(),
        run_id: run.run_id.clone(),
        ticket_id: run.ticket_id.clone(),
        goal_id: run.goal_id.clone(),
        provider: run.provider.clone(),
        model: run.model.clone(),
        status: run.status,
        current_step: run.current_step,
        run_file: run_directory.join("run.json").display().to_string(),
        run_directory: run_directory.display().to_string(),
        next_action: next_loop_action(run),
        candidate_diff_digest: confirmation_candidate
            .map(|candidate| candidate.candidate_diff_digest.clone()),
        target_head: confirmation_candidate.map(|candidate| candidate.starting_head.clone()),
    }
}

fn next_loop_action(run: &LoopRun) -> String {
    match run.status {
        LoopStatus::Pending | LoopStatus::Running => {
            "resume the run to continue pending loop steps".to_string()
        }
        LoopStatus::AwaitingHumanReview => {
            "human approval is required; Testing has not run".to_string()
        }
        LoopStatus::Approved => {
            "candidate is approved; Testing has not run in this release slice".to_string()
        }
        LoopStatus::Blocked => {
            "inspect the blocked step artifact, resolve the blocker, then resume".to_string()
        }
        LoopStatus::Failed => {
            "inspect log.md and the failed step response before retrying".to_string()
        }
        LoopStatus::Passed | LoopStatus::Completed => {
            "review run artifacts before applying or committing any changes".to_string()
        }
    }
}

fn ensure_clean_git_worktree(allow_dirty: bool) -> Result<bool, CliFailure> {
    let output = match ProcessCommand::new("git")
        .args(["status", "--porcelain"])
        .output()
    {
        Ok(output) => output,
        Err(_) if allow_dirty => return Ok(false),
        Err(err) => {
            return Err(CliFailure::message(format!(
                "could not inspect git working tree: {err}; rerun with --allow-dirty to skip this guard"
            )));
        }
    };

    if !output.status.success() {
        if allow_dirty {
            return Ok(false);
        }
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        return Err(CliFailure::message(format!(
            "could not inspect git working tree{suffix}; rerun from a git repository or pass --allow-dirty"
        )));
    }

    if !output.stdout.is_empty() {
        if allow_dirty {
            return Ok(false);
        }
        return Err(CliFailure::message(
            "refusing to start loop with a dirty git working tree; commit or stash changes, or rerun with --allow-dirty"
                .to_string(),
        ));
    }

    Ok(true)
}

fn smoke_ticket() -> TicketSpec {
    TicketSpec {
        ticket_id: "T-SMOKE-LOCAL".to_string(),
        goal_id: "local_agent_loop_smoke".to_string(),
        title: "Deterministic local loop smoke".to_string(),
        status: TicketStatus::Ready,
        priority: TicketPriority::P2,
        problem: "Verify the loop workspace and state machine without contacting a model provider."
            .to_string(),
        research_questions: vec!["Can the deterministic runner write all loop artifacts?".to_string()],
        context: TicketContext {
            relevant_files: vec!["crates/seaf-cli/src/main.rs".to_string()],
            forbidden_files: vec!["secrets/**".to_string()],
        },
        autonomy: TicketAutonomy {
            level: 1,
            apply_patch: false,
            allow_shell_commands: vec!["cargo test -p seaf-cli".to_string()],
        },
        acceptance_criteria: vec![
            "Loop run infrastructure writes run.json, prompts, responses, artifacts, and log output."
                .to_string(),
        ],
        eval: None,
    }
}

#[derive(Debug, Default)]
struct DeterministicStepRunner;

impl StepRunner for DeterministicStepRunner {
    fn step_request(&mut self, step: LoopStepName) -> Result<String, RunnerError> {
        Ok(format!(
            "# {:?}\n\nDeterministic local-loop request for CI smoke execution.\n",
            step
        ))
    }

    fn run_step(&mut self, step: LoopStepName, _request: &str) -> Result<StepOutput, RunnerError> {
        Ok(
            StepOutput::completed(format!("deterministic local-loop response for {:?}", step))
                .with_artifact(ArtifactContent::markdown(format!(
                    "# {:?}\n\nDeterministic artifact generated by seaf-cli fake runner.\n",
                    step
                ))),
        )
    }
}

fn loop_runner_failure(error: RunnerError) -> CliFailure {
    CliFailure::message(format!("loop runner failed: {error}"))
}

fn run_eval(args: EvalRunArgs) -> Result<(), CliFailure> {
    let config_text = fs::read_to_string(&args.config).map_err(|err| {
        CliFailure::message(format!("could not read {}: {err}", args.config.display()))
    })?;
    let config = seaf_core::parse_eval_config(&config_text).map_err(|error| match error {
        EvalConfigError::Parse(error) => CliFailure::message(format!(
            "could not parse {}: {error}",
            args.config.display()
        )),
        EvalConfigError::MissingRequiredChecks => CliFailure::message(error.to_string()),
    })?;

    if args.loop_run.is_some() != args.ticket.is_some() {
        return Err(CliFailure::message(
            "--loop-run and --ticket must be provided together".to_string(),
        ));
    }

    let loop_artifacts = match (&args.loop_run, &args.ticket) {
        (Some(loop_run_path), Some(ticket_path)) => {
            let run = seaf_core::load_loop_run_file(loop_run_path)
                .map_err(|report| CliFailure::validation(report, args.json))?;
            let ticket = seaf_core::load_ticket_file(ticket_path)
                .map_err(|report| CliFailure::validation(report, args.json))?;
            Some((run, ticket))
        }
        (None, None) => None,
        _ => unreachable!("loop artifact pairing is validated before checks run"),
    };

    let invocation_root = std::env::current_dir()
        .map_err(|err| CliFailure::message(format!("could not determine current dir: {err}")))?;
    let invocation_root = invocation_root.canonicalize().map_err(|err| {
        CliFailure::message(format!(
            "could not canonicalize invocation root {}: {err}",
            invocation_root.display()
        ))
    })?;

    let ticket_allow_commands = loop_artifacts
        .as_ref()
        .map(|(_, ticket)| ticket.autonomy.allow_shell_commands.as_slice());
    let plan = plan_eval_checks(&config, ticket_allow_commands, &invocation_root)
        .map_err(|error| CliFailure::message(error.to_string()))?;
    validate_unique_eval_log_names(&config.evals.required)?;

    let output_path = args.output;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            CliFailure::message(format!("could not create {}: {err}", parent.display()))
        })?;
    }
    let log_dir = output_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("logs");
    fs::create_dir_all(&log_dir)
        .map_err(|err| CliFailure::message(format!("could not create logs dir: {err}")))?;

    let mut checks = Vec::new();
    for execution in execute_eval_checks(&plan) {
        let execution = execution.map_err(|error| CliFailure::message(error.to_string()))?;
        checks.push(persist_eval_check(execution, &log_dir)?);
    }

    let report = match (&args.loop_run, &args.ticket) {
        (Some(_), Some(_)) => {
            let (run, ticket) =
                loop_artifacts.expect("loop artifacts loaded before running checks");
            build_loop_eval_report(&run, &ticket, checks)
        }
        (None, None) => command_eval_report(args.patch_id, args.goal_id, checks),
        _ => unreachable!("loop artifact pairing is validated before checks run"),
    };
    let passed = report.passed;

    let errors = seaf_core::validate_eval_report(&report);
    if !errors.is_empty() {
        return Err(CliFailure::message(format!(
            "generated invalid eval report: {errors:?}"
        )));
    }

    write_json_file(&output_path, &report)?;
    if args.json {
        print_json(&report)?;
    } else {
        println!("wrote eval report {}", output_path.display());
        println!("{}", report.summary);
    }

    if passed {
        Ok(())
    } else {
        Err(CliFailure::already_printed())
    }
}

fn command_eval_report(patch_id: String, goal_id: String, checks: Vec<EvalCheck>) -> EvalReport {
    let passed = checks
        .iter()
        .all(|check| check.status == CheckStatus::Passed);
    EvalReport {
        eval_report_id: format!("eval_{}", sanitize_id(&goal_id)),
        patch_id,
        goal_id,
        passed,
        summary: if passed {
            "All required eval checks passed.".to_string()
        } else {
            "One or more required eval checks failed.".to_string()
        },
        checks,
        score_delta_estimate: None,
        risk_level: if passed {
            RiskLevel::Low
        } else {
            RiskLevel::High
        },
        decision: if passed {
            EvalDecision::ApproveForHumanReview
        } else {
            EvalDecision::Reject
        },
    }
}

fn persist_eval_check(
    execution: EvalCheckExecution,
    log_dir: &Path,
) -> Result<EvalCheck, CliFailure> {
    let safe_name = sanitize_id(&execution.name);
    let stdout_path = log_dir.join(format!("{safe_name}.stdout.log"));
    let stderr_path = log_dir.join(format!("{safe_name}.stderr.log"));
    fs::write(&stdout_path, execution.stdout).map_err(|error| {
        CliFailure::message(format!(
            "could not write {}: {error}",
            stdout_path.display()
        ))
    })?;
    fs::write(&stderr_path, execution.stderr).map_err(|error| {
        CliFailure::message(format!(
            "could not write {}: {error}",
            stderr_path.display()
        ))
    })?;

    Ok(EvalCheck {
        name: execution.name,
        status: execution.status,
        duration_ms: Some(execution.duration_ms),
        stdout_path: Some(stdout_path.display().to_string()),
        stderr_path: Some(stderr_path.display().to_string()),
        summary: Some(execution.summary),
    })
}

fn validate_unique_eval_log_names(
    checks: &[seaf_core::EvalCommandConfig],
) -> Result<(), CliFailure> {
    let mut names_by_identity = BTreeMap::new();
    for check in checks {
        let log_name = sanitize_id(&check.name);
        let identity = log_name.to_ascii_lowercase();
        if let Some((previous_name, previous_log_name)) =
            names_by_identity.insert(identity, (check.name.clone(), log_name.clone()))
        {
            if previous_name == check.name {
                return Err(CliFailure::message(format!(
                    "duplicate eval check name {:?} maps to log name {:?}",
                    check.name, log_name
                )));
            }
            return Err(CliFailure::message(format!(
                "eval check log name collision: {previous_name:?} maps to {previous_log_name:?} and {:?} maps to {log_name:?}; both share one filesystem identity",
                check.name,
            )));
        }
    }
    Ok(())
}

fn prepare_release(args: ReleasePrepareArgs) -> Result<(), CliFailure> {
    let eval_report = seaf_core::load_eval_report_file(&args.eval_report)
        .map_err(|report| CliFailure::validation(report, args.json))?;
    if !eval_report.passed
        || eval_report.decision == EvalDecision::Reject
        || eval_report.risk_level == RiskLevel::High
    {
        return Err(CliFailure::message(
            "refusing to prepare release from a failing, rejected, or high-risk EvalReport"
                .to_string(),
        ));
    }

    let goal_id = args.goal_id.unwrap_or_else(|| eval_report.goal_id.clone());
    let capsule = ReleaseCapsule {
        release_id: format!("rel_{}", args.version),
        app_id: args.app_id,
        version: args.version,
        source_commit: args.source_commit,
        agent_task_id: args.agent_task_id,
        goal_id,
        build_recipe_hash: None,
        artifact_digest: sha256_digest_file(&args.artifact).map_err(|err| {
            CliFailure::message(format!(
                "could not digest {}: {err}",
                args.artifact.display()
            ))
        })?,
        eval_report_digest: sha256_digest_file(&args.eval_report).map_err(|err| {
            CliFailure::message(format!(
                "could not digest {}: {err}",
                args.eval_report.display()
            ))
        })?,
        migration_plan: None,
        rollback_plan: args.rollback_plan,
        signatures: Vec::new(),
        rollout_policy: RolloutPolicy {
            channel: parse_rollout_channel(&args.channel)?,
            initial_percentage: args.initial_percentage,
        },
    };

    let errors = seaf_core::validate_release_capsule(&capsule);
    if !errors.is_empty() {
        return Err(CliFailure::message(format!(
            "generated invalid release capsule: {errors:?}"
        )));
    }

    write_json_file(&args.output, &capsule)?;
    if args.json {
        print_json(&capsule)?;
    } else {
        println!("wrote release capsule {}", args.output.display());
    }

    Ok(())
}

fn verify_release(args: ReleaseVerifyArgs) -> Result<(), CliFailure> {
    let capsule = match seaf_core::load_release_capsule_file(&args.path) {
        Ok(capsule) => capsule,
        Err(report) => {
            print_validation_report(&report, args.json)?;
            return Err(CliFailure::already_printed());
        }
    };

    let mut errors = Vec::new();
    if let Some(artifact) = &args.artifact {
        let digest = sha256_digest_file(artifact).map_err(|err| {
            CliFailure::message(format!("could not digest {}: {err}", artifact.display()))
        })?;
        if digest != capsule.artifact_digest {
            errors.push(FieldError::new(
                "artifact_digest",
                format!("expected {}, got {digest}", capsule.artifact_digest),
            ));
        }
    }
    if let Some(eval_report) = &args.eval_report {
        let digest = sha256_digest_file(eval_report).map_err(|err| {
            CliFailure::message(format!("could not digest {}: {err}", eval_report.display()))
        })?;
        if digest != capsule.eval_report_digest {
            errors.push(FieldError::new(
                "eval_report_digest",
                format!("expected {}, got {digest}", capsule.eval_report_digest),
            ));
        }
    }

    if errors.is_empty() {
        let report = ValidationReport::valid("release_capsule", Some(&args.path));
        print_validation_report(&report, args.json)?;
        Ok(())
    } else {
        let report = ValidationReport::invalid("release_capsule", Some(&args.path), errors);
        print_validation_report(&report, args.json)?;
        Err(CliFailure::already_printed())
    }
}

fn print_validation_report(report: &ValidationReport, as_json: bool) -> Result<(), CliFailure> {
    if as_json {
        return print_json(report);
    }

    let path = report.path.as_deref().unwrap_or("<memory>");
    if report.valid {
        println!("valid {}: {}", report.kind, path);
    } else {
        eprintln!("invalid {}: {}", report.kind, path);
        for error in &report.errors {
            eprintln!("- {}: {}", error.field, error.message);
        }
    }

    Ok(())
}

fn write_json_file<T>(path: &Path, value: &T) -> Result<(), CliFailure>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            CliFailure::message(format!("could not create {}: {err}", parent.display()))
        })?;
    }
    let json = serde_json::to_string_pretty(value)
        .map_err(|err| CliFailure::message(format!("could not serialize JSON: {err}")))?;
    fs::write(path, format!("{json}\n"))
        .map_err(|err| CliFailure::message(format!("could not write {}: {err}", path.display())))
}

fn print_json<T>(value: &T) -> Result<(), CliFailure>
where
    T: Serialize,
{
    let json = serde_json::to_string_pretty(value)
        .map_err(|err| CliFailure::message(format!("could not serialize JSON output: {err}")))?;
    println!("{json}");
    Ok(())
}

fn render_task_markdown(brief: &AgentTaskBrief) -> String {
    let mut markdown = String::new();
    markdown.push_str(&format!("# Agent Task: {}\n\n", brief.task_id));
    markdown.push_str(&format!("Goal: `{}`\n\n", brief.goal_id));
    markdown.push_str(&format!("Objective: `{}`\n\n", brief.objective));
    markdown.push_str("## Constraints\n\n");
    markdown.push_str(&format!(
        "- Default autonomy level: {}\n",
        brief.constraints.default_autonomy_level
    ));
    for path in &brief.constraints.forbidden_paths {
        markdown.push_str(&format!("- Forbidden path: `{path}`\n"));
    }
    for item in &brief.constraints.requires_human_review {
        markdown.push_str(&format!("- Requires human review: `{item}`\n"));
    }
    markdown.push_str("\n## Acceptance Criteria\n\n");
    for item in &brief.acceptance_criteria {
        markdown.push_str(&format!("- {item}\n"));
    }
    markdown
}

fn parse_rollout_channel(value: &str) -> Result<RolloutChannel, CliFailure> {
    match value {
        "dev" => Ok(RolloutChannel::Dev),
        "canary" => Ok(RolloutChannel::Canary),
        "beta" => Ok(RolloutChannel::Beta),
        "stable" => Ok(RolloutChannel::Stable),
        _ => Err(CliFailure::message(format!(
            "unsupported rollout channel '{value}'"
        ))),
    }
}

fn current_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    format!("unix:{seconds}")
}

fn generated_run_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{}-{}-{nanos}", sanitize_id(prefix), std::process::id())
}

fn sanitize_id(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|item| {
            if item.is_ascii_alphanumeric() || item == '-' || item == '_' {
                item
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "local".to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Debug)]
struct CliFailure {
    message: Option<String>,
}

impl CliFailure {
    fn message(message: String) -> Self {
        Self {
            message: Some(message),
        }
    }

    fn validation(report: ValidationReport, as_json: bool) -> Self {
        let _ = print_validation_report(&report, as_json);
        Self::already_printed()
    }

    fn already_printed() -> Self {
        Self { message: None }
    }

    fn print(&self) {
        if let Some(message) = &self.message {
            eprintln!("{message}");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn eval_authority_rejects_a_symlink_replacement_at_the_pre_open_cut() {
        let temp = tempfile::tempdir().expect("temp");
        let repository_root = temp.path().join("repo");
        fs::create_dir(&repository_root).expect("repository root");
        let eval_path = repository_root.join("seaf.evals.yaml");
        let outside = temp.path().join("outside.evals.yaml");
        let config = "evals:\n  allow_commands: [cargo]\n  required:\n    - name: tests\n      command: cargo test\n";
        fs::write(&eval_path, config).expect("eval config");
        fs::write(&outside, config).expect("outside eval config");
        let ticket = TicketSpec {
            ticket_id: "T-EVAL-SWAP".to_string(),
            goal_id: "bind-eval-authority".to_string(),
            title: "Bind eval authority".to_string(),
            status: TicketStatus::Ready,
            priority: TicketPriority::P1,
            problem: "The opened file must be the prevalidated file.".to_string(),
            research_questions: Vec::new(),
            context: TicketContext {
                relevant_files: Vec::new(),
                forbidden_files: Vec::new(),
            },
            autonomy: TicketAutonomy {
                level: 1,
                apply_patch: false,
                allow_shell_commands: vec!["cargo".to_string()],
            },
            acceptance_criteria: vec!["Reject replacement.".to_string()],
            eval: Some(seaf_core::TicketEval {
                config: "seaf.evals.yaml".to_string(),
            }),
        };

        let error = load_authoritative_eval_config_with_hook(
            &repository_root.canonicalize().unwrap(),
            &ticket,
            || {
                fs::remove_file(&eval_path)?;
                symlink(&outside, &eval_path)
            },
        )
        .expect_err("replacement must not become authoritative");

        assert!(
            error
                .message
                .as_deref()
                .is_some_and(|message| message.contains("safely open")),
            "{error:?}"
        );
    }
}
