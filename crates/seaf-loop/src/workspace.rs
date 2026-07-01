use std::{
    error::Error,
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
};

use crate::context::{ContextManifest, UNTRUSTED_CONTEXT_MARKER};

pub const RUN_FILE: &str = "run.json";
pub const CONTEXT_MANIFEST_PLACEHOLDER_FILE: &str = "context-manifest.json";
pub const PROMPTS_DIR: &str = "prompts";
pub const RESPONSES_DIR: &str = "responses";
pub const ARTIFACTS_DIR: &str = "artifacts";
pub const LOG_FILE: &str = "log.md";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopWorkspace {
    run_directory: PathBuf,
}

impl LoopWorkspace {
    pub fn create(runs_root: &Path, run_id: &str) -> Result<Self, WorkspaceError> {
        fs::create_dir_all(runs_root)?;
        let workspace = Self {
            run_directory: runs_root.join(run_id),
        };
        match fs::create_dir(&workspace.run_directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(WorkspaceError::RunDirectoryAlreadyExists(
                    workspace.run_directory,
                ));
            }
            Err(error) => return Err(error.into()),
        }
        workspace.ensure_layout()?;
        Ok(workspace)
    }

    pub fn open(runs_root: &Path, run_id: &str) -> Result<Self, WorkspaceError> {
        let workspace = Self {
            run_directory: runs_root.join(run_id),
        };
        if !workspace.run_directory.is_dir() {
            return Err(WorkspaceError::MissingRunDirectory(workspace.run_directory));
        }
        workspace.ensure_layout()?;
        Ok(workspace)
    }

    pub fn run_directory(&self) -> &Path {
        &self.run_directory
    }

    pub fn run_file(&self) -> PathBuf {
        self.run_directory.join(RUN_FILE)
    }

    pub fn append_log(&self, line: &str) -> Result<(), WorkspaceError> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.run_directory.join(LOG_FILE))?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        Ok(())
    }

    fn ensure_layout(&self) -> Result<(), WorkspaceError> {
        fs::create_dir_all(self.run_directory.join(PROMPTS_DIR))?;
        fs::create_dir_all(self.run_directory.join(RESPONSES_DIR))?;
        fs::create_dir_all(self.run_directory.join(ARTIFACTS_DIR))?;

        let manifest_path = self.run_directory.join(CONTEXT_MANIFEST_PLACEHOLDER_FILE);
        if !manifest_path.exists() {
            write_empty_context_manifest(&manifest_path)?;
        }

        let log_path = self.run_directory.join(LOG_FILE);
        if !log_path.exists() {
            fs::write(log_path, b"# Loop run log\n")?;
        }

        Ok(())
    }
}

fn write_empty_context_manifest(path: &Path) -> Result<(), WorkspaceError> {
    let manifest = ContextManifest {
        untrusted_context_marker: UNTRUSTED_CONTEXT_MARKER.to_string(),
        total_context_bytes: 0,
        max_bytes_per_file: 0,
        max_total_bytes: 0,
        default_exclude_globs: Vec::new(),
        ticket_forbidden_files: Vec::new(),
        policy_forbidden_paths: Vec::new(),
        files: Vec::new(),
        warnings: Vec::new(),
    };
    let mut json = serde_json::to_vec_pretty(&manifest)?;
    json.push(b'\n');
    fs::write(path, json)?;
    Ok(())
}

pub fn write_artifact(
    run_directory: &Path,
    file_name: &str,
    bytes: &[u8],
) -> std::io::Result<PathBuf> {
    fs::create_dir_all(run_directory)?;
    let path = run_directory.join(file_name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, bytes)?;
    Ok(path)
}

#[derive(Debug)]
pub enum WorkspaceError {
    MissingRunDirectory(PathBuf),
    RunDirectoryAlreadyExists(PathBuf),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRunDirectory(path) => {
                write!(
                    formatter,
                    "run directory does not exist: {}",
                    path.display()
                )
            }
            Self::RunDirectoryAlreadyExists(path) => {
                write!(
                    formatter,
                    "run directory already exists: {}",
                    path.display()
                )
            }
            Self::Io(error) => write!(formatter, "workspace I/O error: {error}"),
            Self::Json(error) => write!(formatter, "workspace JSON error: {error}"),
        }
    }
}

impl Error for WorkspaceError {}

impl From<std::io::Error> for WorkspaceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for WorkspaceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
