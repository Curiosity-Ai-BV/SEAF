use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use seaf_core::{validate_eval_config, CheckStatus, EvalCommandConfig, EvalConfig};
use sha2::{Digest, Sha256};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

const DEFAULT_EVAL_TIMEOUT_MS: u64 = 120_000;
const MAX_EVAL_TIMEOUT_MS: u64 = 3_600_000;
const DEFAULT_EVAL_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_EVAL_OUTPUT_BYTES: usize = 1024 * 1024;
const EVAL_OUTPUT_DRAIN_GRACE_MS: u64 = 250;
const MIN_OBVIOUS_SECRET_SUFFIX_BYTES: usize = 16;
const MAX_OBVIOUS_SECRET_PREFIX_BYTES: usize = 11;
// A token beginning at the final persisted byte needs 26 more ASCII bytes to
// reach the longest recognized prefix plus the minimum classified suffix.
const EVAL_REDACTION_LOOKAHEAD_BYTES: usize =
    MAX_OBVIOUS_SECRET_PREFIX_BYTES + MIN_OBVIOUS_SECRET_SUFFIX_BYTES - 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalCheckExecution {
    pub name: String,
    pub status: CheckStatus,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub summary: String,
}

#[derive(Debug)]
pub struct EvalPlan {
    checks: Vec<EvalCheckPlan>,
}

pub fn run_eval_checks(
    config: &EvalConfig,
    ticket_allow_commands: Option<&[String]>,
    execution_root: &Path,
) -> Result<Vec<EvalCheckExecution>, EvalEngineError> {
    let plan = plan_eval_checks(config, ticket_allow_commands, execution_root)?;
    execute_eval_checks(&plan).collect()
}

pub fn plan_eval_checks(
    config: &EvalConfig,
    ticket_allow_commands: Option<&[String]>,
    execution_root: &Path,
) -> Result<EvalPlan, EvalEngineError> {
    validate_eval_config(config).map_err(|error| EvalEngineError::message(error.to_string()))?;
    let execution_root = execution_root.canonicalize().map_err(|error| {
        EvalEngineError::io(
            format!(
                "could not canonicalize execution root {}",
                execution_root.display()
            ),
            error,
        )
    })?;

    let checks = config
        .evals
        .required
        .iter()
        .map(|check| {
            plan_eval_check(
                check,
                &config.evals.allow_commands,
                ticket_allow_commands,
                &execution_root,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(EvalPlan { checks })
}

pub fn execute_eval_checks(
    plan: &EvalPlan,
) -> impl Iterator<Item = Result<EvalCheckExecution, EvalEngineError>> + '_ {
    plan.checks.iter().map(run_eval_check)
}

pub(crate) fn execute_eval_checks_with_pre_spawn<'a, F>(
    plan: &'a EvalPlan,
    mut validate_authority: F,
) -> impl Iterator<Item = Result<EvalCheckExecution, EvalEngineError>> + 'a
where
    F: FnMut(usize) -> Result<(), String> + 'a,
{
    plan.checks.iter().enumerate().map(move |(index, check)| {
        validate_authority(index).map_err(|error| {
            EvalEngineError::authority(format!(
                "eval check {} pre-spawn authority rejected: {error}",
                check.name
            ))
        })?;
        run_eval_check(check)
    })
}

#[derive(Debug)]
pub struct EvalEngineError {
    message: String,
    source: Option<io::Error>,
    authority_rejection: bool,
}

impl EvalEngineError {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
            authority_rejection: false,
        }
    }

    pub(crate) fn authority(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
            authority_rejection: true,
        }
    }

    fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self {
            message: format!("{}: {source}", context.into()),
            source: Some(source),
            authority_rejection: false,
        }
    }

    pub(crate) fn is_authority_rejection(&self) -> bool {
        self.authority_rejection
    }
}

impl fmt::Display for EvalEngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for EvalEngineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|error| error as &(dyn Error + 'static))
    }
}

#[derive(Debug)]
struct EvalCommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

#[derive(Debug)]
struct OutputDrain {
    state: Arc<Mutex<OutputDrainState>>,
    handle: JoinHandle<()>,
}

#[derive(Debug)]
struct OutputDrainState {
    retained: Vec<u8>,
    completed: bool,
    error: Option<OutputDrainError>,
}

#[derive(Debug, Clone)]
struct OutputDrainError {
    kind: io::ErrorKind,
    message: String,
}

impl OutputDrainError {
    fn into_io_error(self) -> io::Error {
        io::Error::new(self.kind, self.message)
    }
}

#[derive(Debug)]
struct EvalCheckPlan {
    name: String,
    argv: Vec<String>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    timeout_ms: u64,
    max_output_bytes: usize,
    cwd_identity: PlannedDirectoryIdentity,
    executable_identity: PlannedExecutableIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedDirectoryIdentity {
    canonical_path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedExecutableIdentity {
    canonical_path: PathBuf,
    digest: String,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn plan_eval_check(
    check: &EvalCommandConfig,
    eval_allow_commands: &[String],
    ticket_allow_commands: Option<&[String]>,
    execution_root: &Path,
) -> Result<EvalCheckPlan, EvalEngineError> {
    if check.name.trim().is_empty() {
        return Err(EvalEngineError::message(
            "eval check name must not be empty",
        ));
    }
    if check.command.trim().is_empty() {
        return Err(EvalEngineError::message(format!(
            "eval check {} command must not be empty",
            check.name
        )));
    }

    let argv = parse_eval_command(&check.command).map_err(|error| {
        EvalEngineError::message(format!(
            "eval check {} command rejected: {error}",
            check.name
        ))
    })?;
    ensure_command_allowed(&argv, eval_allow_commands).map_err(|error| {
        EvalEngineError::message(format!(
            "eval check {} command rejected by eval allow_commands: {error}",
            check.name
        ))
    })?;
    if let Some(ticket_allow_commands) = ticket_allow_commands {
        ensure_command_allowed(&argv, ticket_allow_commands).map_err(|error| {
            EvalEngineError::message(format!(
                "eval check {} command rejected by ticket autonomy: {error}",
                check.name
            ))
        })?;
    }

    let cwd = resolve_eval_cwd(check, execution_root)?;
    validate_eval_env(check)?;
    let mut argv = argv;
    argv[0] = resolve_eval_executable(&argv[0], &cwd, execution_root, &check.name)?;
    let cwd_identity = capture_directory_identity(&cwd, &check.name)?;
    let executable_identity = capture_executable_identity(Path::new(&argv[0]), &check.name)?;
    let timeout_ms = check.timeout_ms.unwrap_or(DEFAULT_EVAL_TIMEOUT_MS);
    if timeout_ms == 0 || timeout_ms > MAX_EVAL_TIMEOUT_MS {
        return Err(EvalEngineError::message(format!(
            "eval check {} timeout_ms must be between 1 and {MAX_EVAL_TIMEOUT_MS}",
            check.name
        )));
    }
    let max_output_bytes = check.max_output_bytes.unwrap_or(DEFAULT_EVAL_OUTPUT_BYTES);
    if max_output_bytes == 0 || max_output_bytes > MAX_EVAL_OUTPUT_BYTES {
        return Err(EvalEngineError::message(format!(
            "eval check {} max_output_bytes must be between 1 and {MAX_EVAL_OUTPUT_BYTES}",
            check.name
        )));
    }

    Ok(EvalCheckPlan {
        name: check.name.clone(),
        argv,
        cwd,
        env: check.env.clone(),
        timeout_ms,
        max_output_bytes,
        cwd_identity,
        executable_identity,
    })
}

fn run_eval_check(plan: &EvalCheckPlan) -> Result<EvalCheckExecution, EvalEngineError> {
    validate_planned_spawn_identity(plan)?;
    let started = Instant::now();
    let output = run_controlled_command(
        &plan.argv,
        &plan.cwd,
        &plan.env,
        plan.timeout_ms,
        plan.max_output_bytes,
    )
    .map_err(|error| {
        EvalEngineError::io(format!("could not run eval check {}", plan.name), error)
    })?;
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let stdout = sanitize_eval_log(&output.stdout, &plan.env, plan.max_output_bytes);
    let stderr = sanitize_eval_log(&output.stderr, &plan.env, plan.max_output_bytes);
    let summary = if output.timed_out {
        format!("command timed out after {}ms", plan.timeout_ms)
    } else {
        match output.status.code() {
            Some(code) => format!("command exited with code {code}"),
            None => "command terminated by signal".to_string(),
        }
    };

    Ok(EvalCheckExecution {
        name: plan.name.clone(),
        status: if !output.timed_out && output.status.success() {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
        duration_ms,
        stdout,
        stderr,
        summary,
    })
}

fn validate_planned_spawn_identity(plan: &EvalCheckPlan) -> Result<(), EvalEngineError> {
    let cwd = capture_directory_identity(&plan.cwd, &plan.name)?;
    if cwd != plan.cwd_identity {
        return Err(EvalEngineError::message(format!(
            "eval check {} cwd identity changed after planning",
            plan.name
        )));
    }
    let executable = capture_executable_identity(Path::new(&plan.argv[0]), &plan.name)?;
    if executable != plan.executable_identity {
        return Err(EvalEngineError::message(format!(
            "eval check {} executable identity changed after planning",
            plan.name
        )));
    }
    Ok(())
}

fn capture_directory_identity(
    path: &Path,
    check_name: &str,
) -> Result<PlannedDirectoryIdentity, EvalEngineError> {
    let canonical_path = path.canonicalize().map_err(|error| {
        EvalEngineError::io(
            format!("eval check {check_name} cwd identity could not be resolved"),
            error,
        )
    })?;
    let metadata = fs::symlink_metadata(&canonical_path).map_err(|error| {
        EvalEngineError::io(
            format!("eval check {check_name} cwd identity could not be inspected"),
            error,
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(EvalEngineError::message(format!(
            "eval check {check_name} cwd identity is not a real directory"
        )));
    }
    Ok(PlannedDirectoryIdentity {
        canonical_path,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    })
}

#[cfg(unix)]
fn capture_executable_identity(
    path: &Path,
    check_name: &str,
) -> Result<PlannedExecutableIdentity, EvalEngineError> {
    let canonical_path = path.canonicalize().map_err(|error| {
        EvalEngineError::io(
            format!("eval check {check_name} executable identity could not be resolved"),
            error,
        )
    })?;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&canonical_path)
        .map_err(|error| {
            EvalEngineError::io(
                format!("eval check {check_name} executable identity could not be opened"),
                error,
            )
        })?;
    let opened = file.metadata().map_err(|error| {
        EvalEngineError::io(
            format!("eval check {check_name} executable identity could not be inspected"),
            error,
        )
    })?;
    let current = fs::symlink_metadata(&canonical_path).map_err(|error| {
        EvalEngineError::io(
            format!("eval check {check_name} executable path could not be inspected"),
            error,
        )
    })?;
    if !opened.is_file()
        || current.file_type().is_symlink()
        || !current.is_file()
        || opened.dev() != current.dev()
        || opened.ino() != current.ino()
    {
        return Err(EvalEngineError::message(format!(
            "eval check {check_name} executable identity is not a stable regular file"
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|error| {
        EvalEngineError::io(
            format!("eval check {check_name} executable identity could not be read"),
            error,
        )
    })?;
    let after = fs::symlink_metadata(&canonical_path).map_err(|error| {
        EvalEngineError::io(
            format!("eval check {check_name} executable path could not be revalidated"),
            error,
        )
    })?;
    if after.file_type().is_symlink()
        || !after.is_file()
        || opened.dev() != after.dev()
        || opened.ino() != after.ino()
    {
        return Err(EvalEngineError::message(format!(
            "eval check {check_name} executable identity changed while reading"
        )));
    }
    Ok(PlannedExecutableIdentity {
        canonical_path,
        digest: format!("{:x}", Sha256::digest(&bytes)),
        device: opened.dev(),
        inode: opened.ino(),
    })
}

#[cfg(not(unix))]
fn capture_executable_identity(
    path: &Path,
    check_name: &str,
) -> Result<PlannedExecutableIdentity, EvalEngineError> {
    let canonical_path = path.canonicalize().map_err(|error| {
        EvalEngineError::io(
            format!("eval check {check_name} executable identity could not be resolved"),
            error,
        )
    })?;
    let bytes = fs::read(&canonical_path).map_err(|error| {
        EvalEngineError::io(
            format!("eval check {check_name} executable identity could not be read"),
            error,
        )
    })?;
    Ok(PlannedExecutableIdentity {
        canonical_path,
        digest: format!("{:x}", Sha256::digest(&bytes)),
    })
}

fn parse_eval_command(command: &str) -> Result<Vec<String>, String> {
    if command.contains('\0') {
        return Err("command must not contain NUL bytes".to_string());
    }
    if command.contains("$(") {
        return Err("shell metacharacter '$(' is not supported".to_string());
    }
    for metacharacter in [';', '&', '|', '<', '>', '`', '\n', '\r'] {
        if command.contains(metacharacter) {
            return Err(format!(
                "shell metacharacter '{metacharacter}' is not supported"
            ));
        }
    }
    if command.contains('"') || command.contains('\'') {
        return Err("quoted shell syntax is not supported".to_string());
    }
    let argv: Vec<String> = command
        .split_whitespace()
        .map(ToString::to_string)
        .collect();
    if argv.is_empty() {
        return Err("command must not be empty".to_string());
    }
    Ok(argv)
}

fn ensure_command_allowed(argv: &[String], allow_commands: &[String]) -> Result<(), String> {
    for allow_command in allow_commands {
        let allow_argv = parse_eval_command(allow_command)
            .map_err(|error| format!("invalid allowlist entry {allow_command:?}: {error}"))?;
        if allow_argv.len() <= argv.len() && argv[..allow_argv.len()] == allow_argv {
            return Ok(());
        }
    }
    Err(format!("{} is not allowed", argv.join(" ")))
}

fn resolve_eval_cwd(
    check: &EvalCommandConfig,
    execution_root: &Path,
) -> Result<PathBuf, EvalEngineError> {
    let cwd = match &check.cwd {
        Some(cwd) if cwd.is_absolute() => cwd.clone(),
        Some(cwd) => execution_root.join(cwd),
        None => execution_root.to_path_buf(),
    };
    let cwd = cwd.canonicalize().map_err(|error| {
        EvalEngineError::io(
            format!("eval check {} cwd {} is invalid", check.name, cwd.display()),
            error,
        )
    })?;
    if !cwd.is_dir() {
        return Err(EvalEngineError::message(format!(
            "eval check {} cwd {} is not a directory",
            check.name,
            cwd.display()
        )));
    }
    if !cwd.starts_with(execution_root) {
        return Err(EvalEngineError::message(format!(
            "eval check {} cwd {} escapes invocation root {}",
            check.name,
            cwd.display(),
            execution_root.display()
        )));
    }
    Ok(cwd)
}

fn validate_eval_env(check: &EvalCommandConfig) -> Result<(), EvalEngineError> {
    for (name, value) in &check.env {
        if name.eq_ignore_ascii_case("PATH") {
            return Err(EvalEngineError::message(format!(
                "eval check {} env var {name:?} is not allowed",
                check.name
            )));
        }
        if !is_safe_env_name(name) {
            return Err(EvalEngineError::message(format!(
                "eval check {} env var {name:?} is invalid",
                check.name
            )));
        }
        if value.contains('\0') {
            return Err(EvalEngineError::message(format!(
                "eval check {} env var {name:?} value must not contain NUL bytes",
                check.name
            )));
        }
    }
    Ok(())
}

fn is_safe_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn resolve_eval_executable(
    program: &str,
    cwd: &Path,
    execution_root: &Path,
    check_name: &str,
) -> Result<String, EvalEngineError> {
    let program_path = Path::new(program);
    if program_path.is_absolute() {
        if is_executable_file(program_path) {
            validate_eval_executable_shape(program_path, program, check_name)?;
            return Ok(program_path.display().to_string());
        }
        return Err(executable_not_found(check_name, program));
    }
    if program_path.components().count() > 1 {
        let candidate = cwd.join(program_path);
        let Ok(candidate) = candidate.canonicalize() else {
            return Err(executable_not_found(check_name, program));
        };
        if !candidate.starts_with(execution_root) {
            return Err(EvalEngineError::message(format!(
                "eval check {check_name} executable {program:?} escapes invocation root {}",
                execution_root.display()
            )));
        }
        if is_executable_file(&candidate) {
            validate_eval_executable_shape(&candidate, program, check_name)?;
            return Ok(candidate.display().to_string());
        }
        return Err(executable_not_found(check_name, program));
    }

    for directory in trusted_eval_search_paths() {
        let candidate = directory.join(program);
        if is_executable_file(&candidate) {
            validate_eval_executable_shape(&candidate, program, check_name)?;
            return Ok(candidate.display().to_string());
        }
    }
    Err(EvalEngineError::message(format!(
        "eval check {check_name} executable {program:?} was not found on trusted PATH"
    )))
}

fn executable_not_found(check_name: &str, program: &str) -> EvalEngineError {
    EvalEngineError::message(format!(
        "eval check {check_name} executable {program:?} was not found or is not executable"
    ))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && is_executable_metadata(&metadata)
}

fn validate_eval_executable_shape(
    path: &Path,
    program: &str,
    check_name: &str,
) -> Result<(), EvalEngineError> {
    validate_platform_executable_shape(path).map_err(|error| {
        EvalEngineError::message(format!(
            "eval check {check_name} executable {program:?} cannot spawn: {error}"
        ))
    })
}

#[cfg(unix)]
fn validate_platform_executable_shape(path: &Path) -> Result<(), String> {
    let bytes =
        fs::read(path).map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    if bytes.starts_with(b"#!") {
        let shebang_end = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(bytes.len());
        let shebang = String::from_utf8_lossy(&bytes[2..shebang_end]);
        let mut shebang_parts = shebang.split_whitespace();
        let interpreter = shebang_parts
            .next()
            .ok_or_else(|| "shebang is missing an interpreter".to_string())?;
        let interpreter_path = Path::new(interpreter);
        if !interpreter_path.is_absolute() {
            return Err(format!(
                "shebang interpreter {interpreter:?} is not absolute"
            ));
        }
        if !is_executable_file(interpreter_path) {
            return Err(format!(
                "shebang interpreter {} was not found or is not executable",
                interpreter_path.display()
            ));
        }
        if interpreter_path
            .file_name()
            .is_some_and(|name| name == "env")
        {
            let env_interpreter = shebang_parts
                .next()
                .ok_or_else(|| "env shebang is missing an interpreter".to_string())?;
            validate_env_shebang_interpreter(env_interpreter)?;
        }
        return Ok(());
    }

    if looks_like_text_without_shebang(&bytes) {
        return Err("text executable is missing a shebang interpreter".to_string());
    }
    Ok(())
}

#[cfg(unix)]
fn validate_env_shebang_interpreter(interpreter: &str) -> Result<(), String> {
    if interpreter.starts_with('-') {
        return Err(format!(
            "env shebang option {interpreter:?} is not supported"
        ));
    }
    let interpreter_path = Path::new(interpreter);
    if interpreter_path.components().count() > 1 {
        if interpreter_path.is_absolute() && is_executable_file(interpreter_path) {
            return Ok(());
        }
        return Err(format!(
            "env shebang interpreter {interpreter:?} was not found or is not executable"
        ));
    }
    for directory in trusted_eval_search_paths() {
        if is_executable_file(&directory.join(interpreter)) {
            return Ok(());
        }
    }
    Err(format!(
        "env shebang interpreter {interpreter:?} was not found on trusted PATH"
    ))
}

#[cfg(not(unix))]
fn validate_platform_executable_shape(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn looks_like_text_without_shebang(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    let sample_len = bytes.len().min(512);
    std::str::from_utf8(&bytes[..sample_len]).is_ok_and(|text| {
        text.chars().all(|character| {
            character == '\n' || character == '\r' || character == '\t' || !character.is_control()
        })
    })
}

#[cfg(unix)]
fn is_executable_metadata(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_metadata(_metadata: &fs::Metadata) -> bool {
    true
}

fn trusted_eval_path() -> String {
    std::env::join_paths(trusted_eval_search_paths())
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn trusted_eval_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(cargo_home) = option_env!("CARGO_HOME") {
        paths.push(PathBuf::from(cargo_home).join("bin"));
    }
    if let Some(home) = option_env!("HOME") {
        let home = PathBuf::from(home);
        paths.push(home.join(".cargo/bin"));
        paths.push(home.join("Library/pnpm"));
        paths.push(home.join(".local/bin"));
    }
    for path in [
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        paths.push(PathBuf::from(path));
    }
    paths
}

fn run_controlled_command(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    timeout_ms: u64,
    max_output_bytes: usize,
) -> io::Result<EvalCommandOutput> {
    let retained_output_bytes = max_output_bytes.saturating_add(EVAL_REDACTION_LOOKAHEAD_BYTES);
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_eval_child(&mut command);
    command.env_clear();
    inherit_safe_eval_env(&mut command);
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .map(|stdout| spawn_capped_output_drain(stdout, retained_output_bytes));
    let stderr = child
        .stderr
        .take()
        .map(|stderr| spawn_capped_output_drain(stderr, retained_output_bytes));
    let timeout = Duration::from_millis(timeout_ms);
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            terminate_eval_child(&mut child)?;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait()?;
    if !timed_out {
        terminate_eval_child(&mut child)?;
    }
    let drain_deadline = Instant::now() + Duration::from_millis(EVAL_OUTPUT_DRAIN_GRACE_MS);
    let stdout = match stdout {
        Some(drain) => finish_output_drain(drain, drain_deadline)?,
        None => Vec::new(),
    };
    let stderr = match stderr {
        Some(drain) => finish_output_drain(drain, drain_deadline)?,
        None => Vec::new(),
    };
    Ok(EvalCommandOutput {
        status,
        stdout,
        stderr,
        timed_out,
    })
}

fn inherit_safe_eval_env(command: &mut Command) {
    command.env("PATH", trusted_eval_path());
    for name in [
        "HOME",
        "TMPDIR",
        "TEMP",
        "TMP",
        "USER",
        "LOGNAME",
        "SHELL",
        "CARGO_HOME",
        "RUSTUP_HOME",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

fn spawn_capped_output_drain<R>(mut reader: R, max_output_bytes: usize) -> OutputDrain
where
    R: Read + Send + 'static,
{
    let state = Arc::new(Mutex::new(OutputDrainState {
        retained: Vec::with_capacity(max_output_bytes.min(8192)),
        completed: false,
        error: None,
    }));
    let thread_state = Arc::clone(&state);
    let handle = thread::spawn(move || {
        let mut chunk = [0_u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    if let Ok(mut state) = thread_state.lock() {
                        state.completed = true;
                    }
                    break;
                }
                Ok(read) => {
                    let Ok(mut state) = thread_state.lock() else {
                        break;
                    };
                    let remaining = max_output_bytes.saturating_sub(state.retained.len());
                    if remaining > 0 {
                        state
                            .retained
                            .extend_from_slice(&chunk[..read.min(remaining)]);
                    }
                }
                Err(error) => {
                    if let Ok(mut state) = thread_state.lock() {
                        state.error = Some(OutputDrainError {
                            kind: error.kind(),
                            message: error.to_string(),
                        });
                    }
                    break;
                }
            }
        }
    });
    OutputDrain { state, handle }
}

#[cfg(unix)]
fn configure_eval_child(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_eval_child(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_eval_child(child: &mut std::process::Child) -> io::Result<()> {
    let process_group = format!("-{}", child.id());
    let kill_group = Command::new("/bin/kill")
        .args(["-KILL", &process_group])
        .status();
    if let Err(error) = kill_group {
        if error.kind() != io::ErrorKind::NotFound {
            return Err(error);
        }
    }
    if let Err(error) = child.kill() {
        if error.kind() != io::ErrorKind::InvalidInput {
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn terminate_eval_child(child: &mut std::process::Child) -> io::Result<()> {
    if let Err(error) = child.kill() {
        if error.kind() != io::ErrorKind::InvalidInput {
            return Err(error);
        }
    }
    Ok(())
}

fn finish_output_drain(drain: OutputDrain, deadline: Instant) -> io::Result<Vec<u8>> {
    loop {
        let snapshot = output_drain_snapshot(&drain.state)?;
        if snapshot.completed || snapshot.error.is_some() {
            drain
                .handle
                .join()
                .map_err(|_| io::Error::other("output drain thread panicked"))?;
            if let Some(error) = snapshot.error {
                return Err(error.into_io_error());
            }
            return Ok(snapshot.retained);
        }
        if drain.handle.is_finished() {
            drain
                .handle
                .join()
                .map_err(|_| io::Error::other("output drain thread panicked"))?;
            let snapshot = output_drain_snapshot(&drain.state)?;
            if let Some(error) = snapshot.error {
                return Err(error.into_io_error());
            }
            return Ok(snapshot.retained);
        }
        if Instant::now() >= deadline {
            return Ok(snapshot.retained);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn output_drain_snapshot(state: &Arc<Mutex<OutputDrainState>>) -> io::Result<OutputDrainState> {
    let state = state
        .lock()
        .map_err(|_| io::Error::other("output drain state lock poisoned"))?;
    Ok(OutputDrainState {
        retained: state.retained.clone(),
        completed: state.completed,
        error: state.error.clone(),
    })
}

fn sanitize_eval_log(
    output: &[u8],
    env: &BTreeMap<String, String>,
    max_output_bytes: usize,
) -> String {
    let mut text = String::from_utf8_lossy(output).into_owned();
    for (name, value) in env {
        if is_sensitive_name(name) && !value.is_empty() {
            text = text.replace(value, "[REDACTED]");
        }
    }
    text = redact_configured_secret_prefixes(&text, env);
    text = redact_sensitive_assignments(&text);
    text = redact_obvious_standalone_secrets(&text);
    truncate_to_bytes(&text, max_output_bytes)
}

fn redact_configured_secret_prefixes(text: &str, env: &BTreeMap<String, String>) -> String {
    redact_tokens(text, |token| {
        if is_configured_secret_prefix(token, env) {
            "[REDACTED]".to_string()
        } else {
            token.to_string()
        }
    })
}

fn is_configured_secret_prefix(token: &str, env: &BTreeMap<String, String>) -> bool {
    let candidate = trim_secret_token(token);
    if candidate.is_empty() {
        return false;
    }
    env.iter().any(|(name, value)| {
        is_sensitive_name(name)
            && ((value.len() > candidate.len() && value.starts_with(candidate))
                || is_labeled_configured_secret_prefix(candidate, name, value))
    })
}

fn is_labeled_configured_secret_prefix(candidate: &str, name: &str, value: &str) -> bool {
    for separator in [':', '='] {
        let Some((label, prefix)) = candidate.split_once(separator) else {
            continue;
        };
        if !prefix.is_empty()
            && (label == name || is_sensitive_name(label))
            && value.len() > prefix.len()
            && value.starts_with(prefix)
        {
            return true;
        }
    }
    false
}

fn redact_sensitive_assignments(text: &str) -> String {
    redact_tokens(text, |token| {
        let Some((name, _value)) = token.split_once('=') else {
            return token.to_string();
        };
        if is_sensitive_name(name) {
            format!("{name}=[REDACTED]")
        } else {
            token.to_string()
        }
    })
}

fn is_sensitive_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD"]
        .iter()
        .any(|needle| name.contains(needle))
}

fn redact_obvious_standalone_secrets(text: &str) -> String {
    redact_tokens(text, |token| {
        if is_obvious_standalone_secret(token) {
            "[REDACTED]".to_string()
        } else if let Some((label, value)) = token.split_once(':') {
            if !label.is_empty() && is_obvious_standalone_secret(value) {
                format!("{label}:[REDACTED]")
            } else {
                token.to_string()
            }
        } else {
            token.to_string()
        }
    })
}

fn redact_tokens(text: &str, redact: impl Fn(&str) -> String) -> String {
    let mut redacted = String::new();
    let mut token = String::new();
    for character in text.chars() {
        if character.is_whitespace() {
            redacted.push_str(&redact(&token));
            token.clear();
            redacted.push(character);
        } else {
            token.push(character);
        }
    }
    redacted.push_str(&redact(&token));
    redacted
}

fn trim_secret_token(token: &str) -> &str {
    token.trim_matches(|character: char| {
        matches!(
            character,
            '"' | '\'' | ',' | '.' | ':' | ';' | '(' | ')' | '[' | ']' | '{' | '}'
        )
    })
}

fn is_obvious_standalone_secret(token: &str) -> bool {
    let candidate = trim_secret_token(token);
    for prefix in [
        "sk-proj-",
        "sk-",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "xoxa-",
    ] {
        let Some(rest) = candidate.strip_prefix(prefix) else {
            continue;
        };
        if rest.len() >= MIN_OBVIOUS_SECRET_SUFFIX_BYTES
            && rest.chars().all(|character| {
                character == '_' || character == '-' || character.is_ascii_alphanumeric()
            })
        {
            return true;
        }
    }
    false
}

fn truncate_to_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}
