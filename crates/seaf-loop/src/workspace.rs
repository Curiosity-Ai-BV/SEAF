use std::{
    error::Error,
    ffi::OsStr,
    fmt, fs, io,
    path::{Component, Path, PathBuf},
};

use crate::artifact_safety;
use crate::context::{ContextManifest, UNTRUSTED_CONTEXT_MARKER};
use crate::run_persistence::RunMutationGuard;

pub const RUN_FILE: &str = "run.json";
pub const CONTEXT_MANIFEST_PLACEHOLDER_FILE: &str = "context-manifest.json";
pub const PROMPTS_DIR: &str = "prompts";
pub const RESPONSES_DIR: &str = "responses";
pub const ARTIFACTS_DIR: &str = "artifacts";
pub const LOG_FILE: &str = "log.md";
pub(crate) const CANDIDATE_LOCK_FILE: &str = ".candidate-workspace.lock";
pub(crate) const LOG_HEADER: &[u8] = b"# Loop run log\n";

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
        match artifact_safety::create_private_directory(&workspace.run_directory) {
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
        artifact_safety::validate_private_directory(&run_directory)?;

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

    pub(crate) fn scaffold_runtime_with_validator<F>(
        &self,
        validator: F,
    ) -> Result<(), WorkspaceError>
    where
        F: FnOnce(&[(&'static str, Vec<u8>)]) -> Result<(), String>,
    {
        self.ensure_layout_with_hooks(
            |payloads| {
                validator(payloads).map_err(|message| {
                    WorkspaceError::UnsafeExistingLayout(self.run_directory.clone(), message)
                })
            },
            || Ok(()),
        )
    }

    #[cfg(test)]
    fn scaffold_runtime(&self) -> Result<(), WorkspaceError> {
        self.scaffold_runtime_with_validator(|_| Ok(()))
    }

    pub fn append_log(&self, line: &str) -> Result<(), WorkspaceError> {
        let guard = RunMutationGuard::acquire(&self.run_directory).map_err(io::Error::other)?;
        let directory = artifact_safety::PinnedPrivateDirectory::open(&self.run_directory)?;
        let mut file = directory.open_existing_file(OsStr::new(LOG_FILE), true, false)?;
        let identity = file.metadata()?;
        directory.validate_identity()?;
        directory.validate_single_link_file(OsStr::new(LOG_FILE), &identity)?;
        let mut bytes = read_bounded_run_artifact(&mut file, LOG_FILE)?;
        directory.validate_single_link_file(OsStr::new(LOG_FILE), &identity)?;
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        crate::immutable_artifact::publish_mutable_with_guard_expected(
            &guard,
            LOG_FILE,
            &bytes,
            Some(&identity),
        )
        .map_err(io::Error::other)?;
        Ok(())
    }

    fn ensure_layout(&self) -> Result<(), WorkspaceError> {
        self.ensure_layout_with_hooks(|_| Ok(()), || Ok(()))
    }

    #[cfg(test)]
    fn ensure_layout_with_hook<F>(&self, after_inspection: F) -> Result<(), WorkspaceError>
    where
        F: FnOnce() -> Result<(), WorkspaceError>,
    {
        self.ensure_layout_with_hooks(|_| after_inspection(), || Ok(()))
    }

    fn ensure_layout_with_hooks<AfterInspection, AfterDirectories>(
        &self,
        after_inspection: AfterInspection,
        after_directories: AfterDirectories,
    ) -> Result<(), WorkspaceError>
    where
        AfterInspection: FnOnce(&[(&'static str, Vec<u8>)]) -> Result<(), WorkspaceError>,
        AfterDirectories: FnOnce() -> Result<(), WorkspaceError>,
    {
        let guard = RunMutationGuard::acquire(&self.run_directory).map_err(io::Error::other)?;
        let directory = artifact_safety::PinnedPrivateDirectory::open(&self.run_directory)?;
        let files = runtime_scaffold_default_payloads()?;
        let mut existing_files = Vec::new();
        let mut missing_files = Vec::new();
        let mut prospective_payloads = Vec::new();
        for directory_name in [PROMPTS_DIR, RESPONSES_DIR, ARTIFACTS_DIR] {
            match directory.open_child_directory(OsStr::new(directory_name)) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(WorkspaceError::UnsafeExistingLayout(
                        self.run_directory.join(directory_name),
                        format!("runtime scaffold directory is unsafe: {error}"),
                    ));
                }
            }
        }
        for (name, expected) in files {
            let path = self.run_directory.join(name);
            match directory.open_existing_file(OsStr::new(name), true, false) {
                Ok(mut file) if name == LOG_FILE => {
                    let identity = file.metadata()?;
                    let bytes = read_bounded_run_artifact(&mut file, name)?;
                    if !bytes.starts_with(LOG_HEADER) {
                        return Err(WorkspaceError::UnsafeExistingLayout(
                            path,
                            "runtime log does not begin with the canonical header".to_string(),
                        ));
                    }
                    directory.validate_file(OsStr::new(name), &identity)?;
                    prospective_payloads.push((name, bytes));
                    existing_files.push((name, file, identity));
                }
                Ok(mut file) if name == CANDIDATE_LOCK_FILE => {
                    let identity = file.metadata()?;
                    let bytes = read_bounded_run_artifact(&mut file, name)?;
                    if !bytes.is_empty() {
                        return Err(WorkspaceError::UnsafeExistingLayout(
                            path,
                            "candidate workspace lock is not empty".to_string(),
                        ));
                    }
                    directory.validate_file(OsStr::new(name), &identity)?;
                    prospective_payloads.push((name, bytes));
                    existing_files.push((name, file, identity));
                }
                Ok(mut file) => {
                    let identity = file.metadata()?;
                    let bytes = read_bounded_run_artifact(&mut file, name)?;
                    let parsed: ContextManifest =
                        serde_json::from_slice(&bytes).map_err(|error| {
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
                    directory.validate_file(OsStr::new(name), &identity)?;
                    prospective_payloads.push((name, bytes));
                    existing_files.push((name, file, identity));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    prospective_payloads.push((name, expected.clone()));
                    missing_files.push((name, expected));
                }
                Err(error) => {
                    return Err(WorkspaceError::UnsafeExistingLayout(
                        path,
                        format!("runtime scaffold file is unsafe: {error}"),
                    ));
                }
            }
        }
        after_inspection(&prospective_payloads)?;
        directory.validate_identity()?;
        for (name, _file, identity) in &existing_files {
            directory.validate_file(OsStr::new(name), identity)?;
        }

        let mut retained_directories = Vec::new();
        for directory_name in [PROMPTS_DIR, RESPONSES_DIR, ARTIFACTS_DIR] {
            match directory.open_child_directory(OsStr::new(directory_name)) {
                Ok(child) => retained_directories.push((directory_name, child)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    guard
                        .ensure_child_directory(OsStr::new(directory_name))
                        .map_err(io::Error::other)?;
                    let child = directory.open_child_directory(OsStr::new(directory_name))?;
                    retained_directories.push((directory_name, child));
                }
                Err(error) => return Err(error.into()),
            }
        }
        directory.sync_all()?;
        after_directories()?;
        directory.validate_identity()?;
        for (_name, child) in &retained_directories {
            child.validate_identity()?;
        }
        for (name, expected) in missing_files {
            crate::immutable_artifact::publish_create_only_with_guard(&guard, name, &expected)
                .map_err(|error| {
                    WorkspaceError::UnsafeExistingLayout(
                        self.run_directory.join(name),
                        format!("runtime scaffold publication failed: {error}"),
                    )
                })?;
        }
        directory.validate_identity()?;
        for (name, _file, identity) in existing_files {
            directory.validate_file(OsStr::new(name), &identity)?;
        }
        for (_name, child) in retained_directories {
            child.validate_identity()?;
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
    artifact_safety::validate_private_regular_file(path)?;
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
    artifact_safety::validate_private_directory(path)?;
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
        artifact_safety::validate_private_regular_file(&path)?;
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

pub(crate) fn runtime_scaffold_default_payloads(
) -> Result<Vec<(&'static str, Vec<u8>)>, WorkspaceError> {
    Ok(vec![
        (
            CONTEXT_MANIFEST_PLACEHOLDER_FILE,
            empty_context_manifest_bytes()?,
        ),
        (LOG_FILE, LOG_HEADER.to_vec()),
        (CANDIDATE_LOCK_FILE, Vec::new()),
    ])
}

fn read_bounded_run_artifact(file: &mut fs::File, relative_path: &str) -> io::Result<Vec<u8>> {
    let opened = file.metadata()?;
    crate::artifact_storage::validate_artifact_size_u64(relative_path, opened.len())?;
    let cap = crate::artifact_storage::artifact_byte_cap(relative_path);
    let mut bytes = Vec::new();
    {
        let mut limited = io::Read::take(&mut *file, cap + 1);
        io::Read::read_to_end(&mut limited, &mut bytes)?;
    }
    crate::artifact_storage::validate_artifact_size(relative_path, bytes.len())?;
    if file.metadata()?.len() != bytes.len() as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "run artifact changed while being read",
        ));
    }
    Ok(bytes)
}

pub fn write_artifact(
    run_directory: &Path,
    file_name: &str,
    bytes: &[u8],
) -> std::io::Result<PathBuf> {
    write_artifact_with_hooks(run_directory, file_name, bytes, || Ok(()), || Ok(()))
}

#[cfg(test)]
fn write_artifact_with_hook<F>(
    run_directory: &Path,
    file_name: &str,
    bytes: &[u8],
    after_open: F,
) -> std::io::Result<PathBuf>
where
    F: FnOnce() -> std::io::Result<()>,
{
    write_artifact_with_hooks(run_directory, file_name, bytes, || Ok(()), after_open)
}

fn write_artifact_with_hooks<BeforeOpen, AfterOpen>(
    run_directory: &Path,
    file_name: &str,
    bytes: &[u8],
    before_open: BeforeOpen,
    after_open: AfterOpen,
) -> std::io::Result<PathBuf>
where
    BeforeOpen: FnOnce() -> std::io::Result<()>,
    AfterOpen: FnOnce() -> std::io::Result<()>,
{
    let relative_name = file_name;
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

    let path = run_directory.join(relative_path);
    let guard = RunMutationGuard::acquire(run_directory).map_err(io::Error::other)?;
    let parent = artifact_safety::open_private_descendant_parent(run_directory, relative_path)?;
    let file_name = relative_path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("artifact path has no file name: {}", path.display()),
        )
    })?;
    before_open()?;
    parent.validate_identity()?;
    let file = match parent.open_existing_file(file_name, true, false) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let opened = file.as_ref().map(fs::File::metadata).transpose()?;
    if let Some(opened) = &opened {
        parent.validate_single_link_file(file_name, opened)?;
    }
    after_open()?;
    parent.validate_identity()?;
    if let Some(opened) = &opened {
        parent.validate_single_link_file(file_name, opened)?;
    }
    crate::immutable_artifact::publish_mutable_with_guard_expected(
        &guard,
        relative_name,
        bytes,
        opened.as_ref(),
    )
    .map_err(io::Error::other)?;
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

#[cfg(all(test, unix))]
mod tests {
    use std::{
        io::Write,
        os::unix::fs::{symlink, PermissionsExt},
    };

    use super::*;

    fn create_zero_byte_entries(root: &Path, count: usize, prefix: &str) {
        let directory = artifact_safety::PinnedPrivateDirectory::open(root).unwrap();
        for index in 0..count {
            directory
                .create_file(OsStr::new(&format!("{prefix}-{index:04}")))
                .unwrap();
        }
    }

    fn create_existing_runtime_scaffold_without_candidate_lock(workspace: &LoopWorkspace) {
        for directory in [PROMPTS_DIR, RESPONSES_DIR, ARTIFACTS_DIR] {
            artifact_safety::create_private_directory(&workspace.run_directory().join(directory))
                .unwrap();
        }
        artifact_safety::write_private_fixture(
            workspace
                .run_directory()
                .join(CONTEXT_MANIFEST_PLACEHOLDER_FILE),
            empty_context_manifest_bytes().unwrap(),
        )
        .unwrap();
        artifact_safety::write_private_fixture(
            workspace.run_directory().join(LOG_FILE),
            b"# Loop run log\n",
        )
        .unwrap();
    }

    #[test]
    fn runtime_scaffold_respects_projected_entry_peaks_before_temp_or_directory_mutation() {
        let exact_temp = tempfile::tempdir().unwrap();
        let exact =
            LoopWorkspace::create_minimal(&exact_temp.path().join("runs"), "exact").unwrap();
        drop(RunMutationGuard::acquire(exact.run_directory()).unwrap());
        create_zero_byte_entries(
            exact.run_directory(),
            crate::artifact_storage::RUN_TREE_ENTRY_CAP - 8,
            "scaffold-exact",
        );
        exact
            .scaffold_runtime()
            .expect("three directories and three files may reach the exact peak cap");
        assert!(exact
            .run_directory()
            .join(".candidate-workspace.lock")
            .exists());

        let rejected_temp = tempfile::tempdir().unwrap();
        let rejected =
            LoopWorkspace::create_minimal(&rejected_temp.path().join("runs"), "rejected").unwrap();
        drop(RunMutationGuard::acquire(rejected.run_directory()).unwrap());
        create_zero_byte_entries(
            rejected.run_directory(),
            crate::artifact_storage::RUN_TREE_ENTRY_CAP - 1,
            "scaffold-rejected",
        );
        let error = rejected
            .scaffold_runtime()
            .expect_err("the first projected directory must fail before scaffold mutation");
        assert!(error.to_string().contains("entry cap"), "{error}");
        for path in [
            PROMPTS_DIR,
            RESPONSES_DIR,
            ARTIFACTS_DIR,
            CONTEXT_MANIFEST_PLACEHOLDER_FILE,
            LOG_FILE,
        ] {
            assert!(!rejected.run_directory().join(path).exists(), "{path}");
        }
    }

    #[test]
    fn missing_candidate_lock_migration_respects_exact_and_plus_one_entry_peaks() {
        let exact_temp = tempfile::tempdir().unwrap();
        let exact =
            LoopWorkspace::create_minimal(&exact_temp.path().join("runs"), "exact-lock").unwrap();
        drop(RunMutationGuard::acquire(exact.run_directory()).unwrap());
        create_existing_runtime_scaffold_without_candidate_lock(&exact);
        create_zero_byte_entries(
            exact.run_directory(),
            crate::artifact_storage::RUN_TREE_ENTRY_CAP - 8,
            "candidate-lock-exact",
        );
        exact
            .scaffold_runtime()
            .expect("candidate temp and final names may reach the exact cap");
        assert!(exact
            .run_directory()
            .join(".candidate-workspace.lock")
            .exists());

        let rejected_temp = tempfile::tempdir().unwrap();
        let rejected =
            LoopWorkspace::create_minimal(&rejected_temp.path().join("runs"), "rejected-lock")
                .unwrap();
        drop(RunMutationGuard::acquire(rejected.run_directory()).unwrap());
        create_existing_runtime_scaffold_without_candidate_lock(&rejected);
        create_zero_byte_entries(
            rejected.run_directory(),
            crate::artifact_storage::RUN_TREE_ENTRY_CAP - 7,
            "candidate-lock-rejected",
        );
        let error = rejected
            .scaffold_runtime()
            .expect_err("candidate-lock peak cap plus one must reject before temp creation");
        assert!(error.to_string().contains("entry cap"), "{error}");
        assert!(!rejected
            .run_directory()
            .join(".candidate-workspace.lock")
            .exists());
        assert!(!fs::read_dir(rejected.run_directory())
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains(".tmp-")));
    }

    #[test]
    fn mutable_writer_revalidates_identity_before_truncating_existing_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), "identity-race").unwrap();
        write_artifact(
            workspace.run_directory(),
            "prompts/race.md",
            b"authoritative",
        )
        .unwrap();
        let target = workspace.run_directory().join("prompts/race.md");
        let parked = workspace.run_directory().join("prompts/parked.md");
        let error = write_artifact_with_hook(
            workspace.run_directory(),
            "prompts/race.md",
            b"replacement",
            || {
                fs::rename(&target, &parked)?;
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                artifact_safety::configure_private_file(&mut options);
                let mut substitute = options.open(&target)?;
                substitute.write_all(b"substitute")?;
                Ok(())
            },
        )
        .expect_err("replacement race must fail before truncation");
        assert!(error.to_string().contains("identity"), "{error}");
        assert_eq!(fs::read(parked).unwrap(), b"authoritative");
        assert_eq!(fs::read(target).unwrap(), b"substitute");
    }

    #[test]
    fn mutable_writer_rejects_a_target_created_after_absence_was_authenticated() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), "create-race").unwrap();
        let target = workspace.run_directory().join("prompts/race.md");
        let error = write_artifact_with_hook(
            workspace.run_directory(),
            "prompts/race.md",
            b"intended",
            || {
                crate::artifact_safety::write_private_fixture(&target, b"external")?;
                Ok(())
            },
        )
        .expect_err("late target creation must not be replaced");
        assert!(error.to_string().contains("identity"), "{error}");
        assert_eq!(fs::read(target).unwrap(), b"external");
    }

    #[test]
    fn append_and_scaffold_reject_sparse_oversized_log_without_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), "oversized-log").unwrap();
        let log = workspace.run_directory().join(LOG_FILE);
        fs::File::options()
            .write(true)
            .open(&log)
            .unwrap()
            .set_len(1024 * 1024 + 1)
            .unwrap();

        let append = workspace
            .append_log("must not append")
            .expect_err("append cap");
        assert!(append.to_string().contains("byte cap"), "{append}");
        let scaffold = workspace.scaffold_runtime().expect_err("scaffold cap");
        assert!(scaffold.to_string().contains("byte cap"), "{scaffold}");
        assert_eq!(fs::metadata(log).unwrap().len(), 1024 * 1024 + 1);
    }

    #[test]
    fn mutable_writer_rejects_symlinked_or_broad_parent_without_writing() {
        for kind in ["symlink", "broad"] {
            let temp = tempfile::tempdir().unwrap();
            let workspace = LoopWorkspace::create(&temp.path().join("runs"), kind).unwrap();
            let prompts = workspace.run_directory().join("prompts");
            if kind == "symlink" {
                fs::remove_dir(&prompts).unwrap();
                symlink(workspace.run_directory().join("responses"), &prompts).unwrap();
            } else {
                fs::set_permissions(&prompts, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let error = write_artifact(
                workspace.run_directory(),
                "prompts/unsafe.md",
                b"must not write",
            )
            .expect_err("unsafe parent must fail closed");
            assert!(
                error.to_string().contains("real directory")
                    || error.to_string().contains("chmod 700")
                    || error.to_string().contains("Not a directory"),
                "{kind}: {error}"
            );
            assert!(!workspace
                .run_directory()
                .join("responses/unsafe.md")
                .exists());
        }
    }

    #[test]
    fn mutable_writer_parent_substitution_before_open_cannot_create_external_target() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), "parent-race").unwrap();
        let prompts = workspace.run_directory().join("prompts");
        let parked = workspace.run_directory().join("parked-prompts");
        let outside = temp.path().join("outside");
        artifact_safety::create_private_directory(&outside).unwrap();
        let error = write_artifact_with_hooks(
            workspace.run_directory(),
            "prompts/escaped.md",
            b"must not escape",
            || {
                fs::rename(&prompts, &parked)?;
                symlink(&outside, &prompts)?;
                Ok(())
            },
            || Ok(()),
        )
        .expect_err("parent substitution must fail before openat");
        assert!(
            error.to_string().contains("directory") || error.to_string().contains("identity"),
            "{error}"
        );
        assert!(!outside.join("escaped.md").exists());
        assert!(!parked.join("escaped.md").exists());
    }

    #[test]
    fn scaffold_accepts_an_existing_nonempty_trusted_context_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let workspace =
            LoopWorkspace::create(&temp.path().join("runs"), "captured-context").unwrap();
        let manifest = ContextManifest {
            untrusted_context_marker: UNTRUSTED_CONTEXT_MARKER.to_string(),
            total_context_bytes: 7,
            max_bytes_per_file: 16,
            max_total_bytes: 32,
            default_exclude_globs: vec![".env*".to_string()],
            ticket_forbidden_files: Vec::new(),
            policy_forbidden_paths: Vec::new(),
            files: Vec::new(),
            warnings: vec!["captured".to_string()],
        };
        let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        bytes.push(b'\n');
        write_artifact(
            workspace.run_directory(),
            CONTEXT_MANIFEST_PLACEHOLDER_FILE,
            &bytes,
        )
        .unwrap();

        workspace
            .scaffold_runtime()
            .expect("established trusted context is not an empty placeholder retry");
        assert_eq!(
            fs::read(
                workspace
                    .run_directory()
                    .join(CONTEXT_MANIFEST_PLACEHOLDER_FILE)
            )
            .unwrap(),
            bytes
        );
    }

    #[test]
    fn scaffold_revalidates_existing_file_identity_after_inspection() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), "scaffold-race").unwrap();
        let context = workspace
            .run_directory()
            .join(CONTEXT_MANIFEST_PLACEHOLDER_FILE);
        let parked = workspace.run_directory().join("parked-context.json");
        let outside = temp.path().join("outside-context.json");
        artifact_safety::write_private_fixture(&outside, b"outside unchanged").unwrap();
        let error = workspace
            .ensure_layout_with_hook(|| {
                fs::rename(&context, &parked)?;
                symlink(&outside, &context)?;
                Ok(())
            })
            .expect_err("existing scaffold file substitution must fail closed");
        assert!(error.to_string().contains("artifact"), "{error}");
        assert_eq!(fs::read(outside).unwrap(), b"outside unchanged");
        assert!(parked.is_file());
    }

    #[test]
    fn scaffold_detects_existing_file_substitution_before_creating_missing_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), "partial-race").unwrap();
        let log = workspace.run_directory().join(LOG_FILE);
        fs::remove_file(&log).unwrap();
        let context = workspace
            .run_directory()
            .join(CONTEXT_MANIFEST_PLACEHOLDER_FILE);
        let parked = workspace.run_directory().join("parked-context.json");
        let outside = temp.path().join("outside-context.json");
        artifact_safety::write_private_fixture(&outside, b"outside unchanged").unwrap();

        let error = workspace
            .ensure_layout_with_hook(|| {
                fs::rename(&context, &parked)?;
                symlink(&outside, &context)?;
                Ok(())
            })
            .expect_err("existing substitution must fail before missing log publication");
        assert!(error.to_string().contains("artifact"), "{error}");
        assert!(
            !log.exists(),
            "failed inspection must not create the missing log"
        );
        assert_eq!(fs::read(outside).unwrap(), b"outside unchanged");
        assert!(parked.is_file());
    }

    #[test]
    fn scaffold_retains_and_revalidates_existing_and_created_directories() {
        for kind in ["existing", "created"] {
            let temp = tempfile::tempdir().unwrap();
            let workspace = LoopWorkspace::create(&temp.path().join("runs"), kind).unwrap();
            let prompts = workspace.run_directory().join(PROMPTS_DIR);
            let log = workspace.run_directory().join(LOG_FILE);
            fs::remove_file(&log).unwrap();
            if kind == "created" {
                fs::remove_dir(&prompts).unwrap();
            }
            let parked = workspace.run_directory().join("parked-prompts");
            let outside = temp.path().join("outside");
            artifact_safety::create_private_directory(&outside).unwrap();

            let error = workspace
                .ensure_layout_with_hooks(
                    |_| Ok(()),
                    || {
                        fs::rename(&prompts, &parked)?;
                        symlink(&outside, &prompts)?;
                        Ok(())
                    },
                )
                .expect_err("substituted scaffold directory must fail final binding check");
            assert!(error.to_string().contains("directory"), "{kind}: {error}");
            assert!(fs::read_dir(&outside).unwrap().next().is_none(), "{kind}");
            assert!(parked.is_dir(), "{kind}");
            assert!(
                !log.exists(),
                "{kind}: directory drift must precede log creation"
            );
        }
    }
}
