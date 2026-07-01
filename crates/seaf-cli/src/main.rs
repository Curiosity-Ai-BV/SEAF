use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use clap::{Args, Parser, Subcommand};
use seaf_core::{
    sha256_digest_file, templates, AgentTaskBrief, AgentTaskConstraints, CheckStatus, EvalCheck,
    EvalDecision, EvalReport, FieldError, LoopRun, LoopStatus, LoopStepName, ReleaseCapsule,
    RiskLevel, RolloutChannel, RolloutPolicy, TicketAutonomy, TicketContext, TicketPriority,
    TicketSpec, TicketStatus, ValidationReport,
};
use seaf_loop::{
    build_loop_eval_report, ArtifactContent, LoopRunner, LoopRunnerConfig, PatchDecisionKind,
    PolicyDecision, RunnerError, StepOutput, StepRunner,
};
use seaf_models::{
    ModelMessage, ModelMessageRole, ModelProvider, ModelRequest, OllamaConfig, OllamaProvider,
    DEFAULT_OLLAMA_BASE_URL,
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
    /// Run and inspect deterministic local-loop executions.
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
    /// Start a deterministic local-loop run for a ticket.
    Run(LoopRunArgs),
    /// Print persisted loop run status.
    Status(LoopStatusArgs),
    /// Resume a deterministic local-loop run.
    Resume(LoopStatusArgs),
    /// Run a deterministic smoke loop without contacting a model provider.
    Smoke(LoopSmokeArgs),
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
    /// Directory where loop run workspaces are written.
    #[arg(long, default_value = ".seaf/loops/runs")]
    runs_root: PathBuf,
    /// Stable run ID. Generated when omitted.
    #[arg(long)]
    run_id: Option<String>,
    /// Provider metadata recorded in run.json. Execution remains deterministic.
    #[arg(long, default_value = "fake")]
    provider: String,
    /// Model metadata recorded in run.json. Execution remains deterministic.
    #[arg(long, default_value = "fake-local")]
    model: String,
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
struct LoopSmokeArgs {
    /// Directory where loop run workspaces are written.
    #[arg(long, default_value = ".seaf/loops/runs")]
    runs_root: PathBuf,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvalConfig {
    evals: EvalGroup,
    #[serde(default, rename = "thresholds")]
    _thresholds: Option<serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvalGroup {
    required: Vec<EvalCommandConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvalCommandConfig {
    name: String,
    command: String,
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
            command: LoopCommand::Smoke(args),
        } => smoke_loop(args),
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
    ensure_clean_git_worktree(args.allow_dirty)?;
    let run_id = match args.run_id {
        Some(run_id) => {
            validate_run_id(&run_id)?;
            run_id
        }
        None => generated_run_id("run"),
    };
    let run = start_loop_to_completion(
        &args.runs_root,
        &run_id,
        &ticket,
        &args.provider,
        &args.model,
    )?;
    finish_loop_command("run", &args.runs_root, &run, args.json)
}

fn loop_status(args: LoopStatusArgs) -> Result<(), CliFailure> {
    validate_run_id(&args.run_id)?;
    let run = load_persisted_loop_run(&args.runs_root, &args.run_id, args.json)?;
    finish_loop_command("status", &args.runs_root, &run, args.json)
}

fn resume_loop(args: LoopStatusArgs) -> Result<(), CliFailure> {
    validate_run_id(&args.run_id)?;
    load_persisted_loop_run(&args.runs_root, &args.run_id, args.json)?;
    let mut step_runner = DeterministicStepRunner;
    let mut runner = LoopRunner::resume(args.runs_root.clone(), &args.run_id, &mut step_runner)
        .map_err(loop_runner_failure)?;
    let run = runner
        .run_to_completion()
        .map_err(loop_runner_failure)?
        .clone();
    finish_loop_command("resume", &args.runs_root, &run, args.json)
}

fn smoke_loop(args: LoopSmokeArgs) -> Result<(), CliFailure> {
    let ticket = smoke_ticket();
    let run_id = generated_run_id("smoke");
    let run = start_loop_to_completion(
        &args.runs_root,
        &run_id,
        &ticket,
        "fake",
        "deterministic-smoke",
    )?;
    finish_loop_command("smoke", &args.runs_root, &run, args.json)
}

fn start_loop_to_completion(
    runs_root: &Path,
    run_id: &str,
    ticket: &TicketSpec,
    provider: &str,
    model: &str,
) -> Result<LoopRun, CliFailure> {
    let mut step_runner = DeterministicStepRunner;
    let config = LoopRunnerConfig::for_ticket(
        runs_root,
        run_id,
        ticket,
        provider.to_string(),
        model.to_string(),
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
            "loop {} {}: status {:?}, current step {:?}",
            report.command, report.run_id, report.status, report.current_step
        );
        println!("next action: {}", report.next_action);
        println!("run file: {}", report.run_file);
    }
    Ok(())
}

fn loop_command_report(command: &str, runs_root: &Path, run: &LoopRun) -> LoopCommandReport {
    let run_directory = runs_root.join(&run.run_id);
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
    }
}

fn next_loop_action(run: &LoopRun) -> String {
    match run.status {
        LoopStatus::Pending | LoopStatus::Running => {
            "resume the run to continue pending loop steps".to_string()
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

fn ensure_clean_git_worktree(allow_dirty: bool) -> Result<(), CliFailure> {
    if allow_dirty {
        return Ok(());
    }

    let output = ProcessCommand::new("git")
        .args(["status", "--porcelain"])
        .output()
        .map_err(|err| {
            CliFailure::message(format!(
                "could not inspect git working tree: {err}; rerun with --allow-dirty to skip this guard"
            ))
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
            "could not inspect git working tree{suffix}; rerun from a git repository or pass --allow-dirty"
        )));
    }

    if !output.stdout.is_empty() {
        return Err(CliFailure::message(
            "refusing to start loop with a dirty git working tree; commit or stash changes, or rerun with --allow-dirty"
                .to_string(),
        ));
    }

    Ok(())
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
    let config: EvalConfig = serde_yaml::from_str(&config_text).map_err(|err| {
        CliFailure::message(format!("could not parse {}: {err}", args.config.display()))
    })?;

    if config.evals.required.is_empty() {
        return Err(CliFailure::message(
            "eval config must include at least one required check".to_string(),
        ));
    }

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
    for check in &config.evals.required {
        checks.push(run_eval_check(check, &log_dir)?);
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

fn run_eval_check(check: &EvalCommandConfig, log_dir: &Path) -> Result<EvalCheck, CliFailure> {
    if check.name.trim().is_empty() {
        return Err(CliFailure::message(
            "eval check name must not be empty".to_string(),
        ));
    }
    if check.command.trim().is_empty() {
        return Err(CliFailure::message(format!(
            "eval check {} command must not be empty",
            check.name
        )));
    }

    let started = Instant::now();
    let output = ProcessCommand::new("sh")
        .arg("-c")
        .arg(&check.command)
        .output()
        .map_err(|err| {
            CliFailure::message(format!("could not run eval check {}: {err}", check.name))
        })?;
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let safe_name = sanitize_id(&check.name);
    let stdout_path = log_dir.join(format!("{safe_name}.stdout.log"));
    let stderr_path = log_dir.join(format!("{safe_name}.stderr.log"));
    fs::write(&stdout_path, &output.stdout).map_err(|err| {
        CliFailure::message(format!("could not write {}: {err}", stdout_path.display()))
    })?;
    fs::write(&stderr_path, &output.stderr).map_err(|err| {
        CliFailure::message(format!("could not write {}: {err}", stderr_path.display()))
    })?;

    Ok(EvalCheck {
        name: check.name.clone(),
        status: if output.status.success() {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
        duration_ms: Some(duration_ms),
        stdout_path: Some(stdout_path.display().to_string()),
        stderr_path: Some(stderr_path.display().to_string()),
        summary: Some(match output.status.code() {
            Some(code) => format!("command exited with code {code}"),
            None => "command terminated by signal".to_string(),
        }),
    })
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
