use std::{
    error::Error,
    fmt, fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
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
    pub(crate) fn create_minimal(runs_root: &Path, run_id: &str) -> Result<Self, WorkspaceError> {
        fs::create_dir_all(runs_root)?;
        let workspace = Self {
            run_directory: runs_root.join(run_id),
        };
        match fs::create_dir(&workspace.run_directory) {
            Ok(()) => {
                fs::File::open(runs_root)?.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(WorkspaceError::RunDirectoryAlreadyExists(
                    workspace.run_directory,
                ));
            }
            Err(error) => return Err(error.into()),
        }
        Ok(workspace)
    }

    pub fn create(runs_root: &Path, run_id: &str) -> Result<Self, WorkspaceError> {
        let workspace = Self::create_minimal(runs_root, run_id)?;
        workspace.ensure_layout()?;
        Ok(workspace)
    }

    pub fn open(runs_root: &Path, run_id: &str) -> Result<Self, WorkspaceError> {
        let workspace = Self::open_minimal(runs_root, run_id)?;
        workspace.validate_existing_layout()?;
        Ok(workspace)
    }

    pub fn open_minimal(runs_root: &Path, run_id: &str) -> Result<Self, WorkspaceError> {
        let run_directory = runs_root.join(run_id);
        let metadata = match fs::symlink_metadata(&run_directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(WorkspaceError::MissingRunDirectory(run_directory));
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return Err(WorkspaceError::UnsafeExistingLayout(
                run_directory,
                "run directory is a symlink".to_string(),
            ));
        }
        if !metadata.is_dir() {
            return Err(WorkspaceError::UnsafeExistingLayout(
                run_directory,
                "run path is not a directory".to_string(),
            ));
        }

        let canonical_runs_root = runs_root.canonicalize()?;
        if !canonical_runs_root.is_dir() {
            return Err(WorkspaceError::UnsafeExistingLayout(
                runs_root.to_path_buf(),
                "runs root is not a directory".to_string(),
            ));
        }
        let canonical_run_directory = run_directory.canonicalize()?;
        if !canonical_run_directory.starts_with(&canonical_runs_root) {
            return Err(WorkspaceError::UnsafeExistingLayout(
                run_directory,
                format!(
                    "canonical run directory resolves outside runs root {}",
                    canonical_runs_root.display()
                ),
            ));
        }

        let workspace = Self {
            run_directory: canonical_run_directory,
        };
        validate_regular_file(&workspace.run_file())?;
        Ok(workspace)
    }

    pub fn run_directory(&self) -> &Path {
        &self.run_directory
    }

    pub fn run_file(&self) -> PathBuf {
        self.run_directory.join(RUN_FILE)
    }

    pub(crate) fn scaffold_runtime(&self) -> Result<(), WorkspaceError> {
        self.ensure_layout()
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
        let manifest = empty_context_manifest_bytes()?;
        let files = [
            (CONTEXT_MANIFEST_PLACEHOLDER_FILE, manifest.as_slice()),
            (LOG_FILE, b"# Loop run log\n".as_slice()),
        ];
        for directory_name in [PROMPTS_DIR, RESPONSES_DIR, ARTIFACTS_DIR] {
            let directory = self.run_directory.join(directory_name);
            match fs::symlink_metadata(&directory) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(WorkspaceError::UnsafeExistingLayout(
                        directory,
                        "runtime scaffold directory is not a real directory".to_string(),
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        for (name, _) in files {
            let path = self.run_directory.join(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                    return Err(WorkspaceError::UnsafeExistingLayout(
                        path,
                        "runtime scaffold file is not a regular file".to_string(),
                    ));
                }
                Ok(_) if name == LOG_FILE => {
                    if !fs::read(&path)?.starts_with(b"# Loop run log\n") {
                        return Err(WorkspaceError::UnsafeExistingLayout(
                            path,
                            "runtime log does not begin with the canonical header".to_string(),
                        ));
                    }
                }
                Ok(_) => {
                    let parsed: ContextManifest = serde_json::from_slice(&fs::read(&path)?)
                        .map_err(|error| {
                            WorkspaceError::UnsafeExistingLayout(
                                path.clone(),
                                format!("context manifest is invalid: {error}"),
                            )
                        })?;
                    if parsed.untrusted_context_marker != UNTRUSTED_CONTEXT_MARKER {
                        return Err(WorkspaceError::UnsafeExistingLayout(
                            path,
                            "context manifest has the wrong trust marker".to_string(),
                        ));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        for directory_name in [PROMPTS_DIR, RESPONSES_DIR, ARTIFACTS_DIR] {
            let directory = self.run_directory.join(directory_name);
            if !directory.exists() {
                fs::create_dir(&directory)?;
                fs::File::open(&self.run_directory)?.sync_all()?;
            }
        }
        fs::File::open(&self.run_directory)?.sync_all()?;
        for (name, expected) in files {
            if !self.run_directory.join(name).exists() {
                crate::immutable_artifact::publish_create_only(&self.run_directory, name, expected)
                    .map_err(|error| {
                        WorkspaceError::UnsafeExistingLayout(
                            self.run_directory.join(name),
                            format!("runtime scaffold publication failed: {error}"),
                        )
                    })?;
            }
        }
        Ok(())
    }

    fn validate_existing_layout(&self) -> Result<(), WorkspaceError> {
        for file_name in [RUN_FILE, LOG_FILE, CONTEXT_MANIFEST_PLACEHOLDER_FILE] {
            validate_regular_file(&self.run_directory.join(file_name))?;
        }
        for directory_name in [PROMPTS_DIR, RESPONSES_DIR, ARTIFACTS_DIR] {
            let directory = self.run_directory.join(directory_name);
            validate_real_directory(&directory)?;
            validate_regular_child_files(&self.run_directory, &directory)?;
        }
        Ok(())
    }
}

fn validate_regular_file(path: &Path) -> Result<(), WorkspaceError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        WorkspaceError::UnsafeExistingLayout(
            path.to_path_buf(),
            format!("required regular file could not be inspected: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(WorkspaceError::UnsafeExistingLayout(
            path.to_path_buf(),
            "required regular file is a symlink".to_string(),
        ));
    }
    if !metadata.is_file() {
        return Err(WorkspaceError::UnsafeExistingLayout(
            path.to_path_buf(),
            "required path is not a regular file".to_string(),
        ));
    }
    Ok(())
}

fn validate_real_directory(path: &Path) -> Result<(), WorkspaceError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        WorkspaceError::UnsafeExistingLayout(
            path.to_path_buf(),
            format!("required directory could not be inspected: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(WorkspaceError::UnsafeExistingLayout(
            path.to_path_buf(),
            "required directory is a symlink".to_string(),
        ));
    }
    if !metadata.is_dir() {
        return Err(WorkspaceError::UnsafeExistingLayout(
            path.to_path_buf(),
            "required path is not a directory".to_string(),
        ));
    }
    Ok(())
}

fn validate_regular_child_files(
    run_directory: &Path,
    directory: &Path,
) -> Result<(), WorkspaceError> {
    let canonical_directory = directory.canonicalize()?;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(WorkspaceError::UnsafeExistingLayout(
                path,
                "workspace child is a symlink".to_string(),
            ));
        }
        if !metadata.is_file() {
            return Err(WorkspaceError::UnsafeExistingLayout(
                path,
                "workspace child is not a regular file".to_string(),
            ));
        }
        let canonical_path = path.canonicalize()?;
        if !canonical_path.starts_with(&canonical_directory)
            || !canonical_path.starts_with(run_directory)
        {
            return Err(WorkspaceError::UnsafeExistingLayout(
                path,
                "workspace child resolves outside its run layout directory".to_string(),
            ));
        }
    }
    Ok(())
}

fn empty_context_manifest_bytes() -> Result<Vec<u8>, WorkspaceError> {
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
    Ok(json)
}

pub fn write_artifact(
    run_directory: &Path,
    file_name: &str,
    bytes: &[u8],
) -> std::io::Result<PathBuf> {
    let relative_path = Path::new(file_name);
    if relative_path.as_os_str().is_empty()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("artifact path must be a safe relative path: {file_name}"),
        ));
    }

    fs::create_dir_all(run_directory)?;
    let run_metadata = fs::symlink_metadata(run_directory)?;
    if run_metadata.file_type().is_symlink() || !run_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "artifact run directory must be a real directory, not a symlink: {}",
                run_directory.display()
            ),
        ));
    }
    let canonical_run_directory = run_directory.canonicalize()?;
    let path = run_directory.join(relative_path);
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("artifact path has no parent: {}", path.display()),
        )
    })?;
    let canonical_parent = parent.canonicalize()?;
    if !canonical_parent.starts_with(&canonical_run_directory) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "artifact parent {} resolves outside run directory {}",
                parent.display(),
                canonical_run_directory.display()
            ),
        ));
    }
    let file_name = relative_path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("artifact path has no file name: {}", path.display()),
        )
    })?;
    let safe_target = canonical_parent.join(file_name);
    let mut file = match fs::symlink_metadata(&safe_target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("artifact target is a symlink: {}", path.display()),
            ));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("artifact target is not a regular file: {}", path.display()),
            ));
        }
        Ok(_) => fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&safe_target)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&safe_target)?,
        Err(error) => return Err(error),
    };
    file.write_all(bytes)?;
    Ok(path)
}

#[derive(Debug)]
pub enum WorkspaceError {
    MissingRunDirectory(PathBuf),
    RunDirectoryAlreadyExists(PathBuf),
    UnsafeExistingLayout(PathBuf, String),
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
            Self::UnsafeExistingLayout(path, reason) => {
                write!(
                    formatter,
                    "unsafe existing loop workspace path {}: {reason}",
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
