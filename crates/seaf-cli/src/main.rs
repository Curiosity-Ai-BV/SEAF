use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand};
use seaf_core::{templates, ValidationReport};
use serde::Serialize;

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
    /// Run configured evals.
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
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
enum EvalCommand {
    /// Emit a fail-closed placeholder EvalReport.
    Run(EvalRunArgs),
}

#[derive(Debug, Subcommand)]
enum ReleaseCommand {
    /// Verify release capsule structure and digest fields.
    Verify(ValidateArgs),
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
struct EvalRunArgs {
    /// Eval config path.
    #[arg(default_value = "seaf.evals.yaml")]
    config: PathBuf,
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
struct PlaceholderEvalReport {
    eval_report_id: String,
    patch_id: String,
    goal_id: String,
    passed: bool,
    summary: String,
    checks: Vec<PlaceholderCheck>,
    risk_level: String,
    decision: String,
}

#[derive(Debug, Serialize)]
struct PlaceholderCheck {
    name: String,
    status: String,
    summary: String,
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
        Command::Eval {
            command: EvalCommand::Run(args),
        } => run_eval_placeholder(args),
        Command::Release {
            command: ReleaseCommand::Verify(args),
        } => validate_file(
            args,
            "release_capsule",
            seaf_core::load_release_capsule_file,
        ),
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
            print_validation_report(&report, args.json);
            Ok(())
        }
        Err(report) => {
            print_validation_report(&report, args.json);
            Err(CliFailure::already_printed())
        }
    }
}

fn run_eval_placeholder(args: EvalRunArgs) -> Result<(), CliFailure> {
    if !args.config.exists() {
        return Err(CliFailure::message(format!(
            "eval config {} does not exist",
            args.config.display()
        )));
    }

    let report = PlaceholderEvalReport {
        eval_report_id: "eval_placeholder".to_string(),
        patch_id: "patch_placeholder".to_string(),
        goal_id: "unknown".to_string(),
        passed: false,
        summary: "Eval runner placeholder: no checks were executed, so this report cannot approve a release.".to_string(),
        checks: vec![PlaceholderCheck {
            name: "eval_runner_placeholder".to_string(),
            status: "skipped".to_string(),
            summary: "Real eval execution lands in a later slice.".to_string(),
        }],
        risk_level: "high".to_string(),
        decision: "reject".to_string(),
    };

    if args.json {
        print_json(&report)?;
    } else {
        println!("{}", report.summary);
    }

    Err(CliFailure::already_printed())
}

fn print_validation_report(report: &ValidationReport, as_json: bool) {
    if as_json {
        let _ = print_json(report);
        return;
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

    fn already_printed() -> Self {
        Self { message: None }
    }

    fn print(&self) {
        if let Some(message) = &self.message {
            eprintln!("{message}");
        }
    }
}
