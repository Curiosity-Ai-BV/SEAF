use std::{
    error::Error,
    ffi::OsStr,
    fmt, fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

use crate::artifact_safety;
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

    pub(crate) fn scaffold_runtime(&self) -> Result<(), WorkspaceError> {
        self.ensure_layout()
    }

    pub fn append_log(&self, line: &str) -> Result<(), WorkspaceError> {
        let directory = artifact_safety::PinnedPrivateDirectory::open(&self.run_directory)?;
        let mut file = directory.open_append_file(OsStr::new(LOG_FILE))?;
        let identity = file.metadata()?;
        directory.validate_identity()?;
        directory.validate_single_link_file(OsStr::new(LOG_FILE), &identity)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        Ok(())
    }

    fn ensure_layout(&self) -> Result<(), WorkspaceError> {
        self.ensure_layout_with_hooks(|| Ok(()), || Ok(()))
    }

    #[cfg(test)]
    fn ensure_layout_with_hook<F>(&self, after_inspection: F) -> Result<(), WorkspaceError>
    where
        F: FnOnce() -> Result<(), WorkspaceError>,
    {
        self.ensure_layout_with_hooks(after_inspection, || Ok(()))
    }

    fn ensure_layout_with_hooks<AfterInspection, AfterDirectories>(
        &self,
        after_inspection: AfterInspection,
        after_directories: AfterDirectories,
    ) -> Result<(), WorkspaceError>
    where
        AfterInspection: FnOnce() -> Result<(), WorkspaceError>,
        AfterDirectories: FnOnce() -> Result<(), WorkspaceError>,
    {
        let directory = artifact_safety::PinnedPrivateDirectory::open(&self.run_directory)?;
        let manifest = empty_context_manifest_bytes()?;
        let files = [
            (CONTEXT_MANIFEST_PLACEHOLDER_FILE, manifest.as_slice()),
            (LOG_FILE, b"# Loop run log\n".as_slice()),
        ];
        let mut existing_files = Vec::new();
        let mut missing_files = Vec::new();
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
                    let mut bytes = Vec::new();
                    io::Read::read_to_end(&mut file, &mut bytes)?;
                    if !bytes.starts_with(b"# Loop run log\n") {
                        return Err(WorkspaceError::UnsafeExistingLayout(
                            path,
                            "runtime log does not begin with the canonical header".to_string(),
                        ));
                    }
                    directory.validate_file(OsStr::new(name), &identity)?;
                    existing_files.push((name, file, identity));
                }
                Ok(mut file) => {
                    let identity = file.metadata()?;
                    let mut bytes = Vec::new();
                    io::Read::read_to_end(&mut file, &mut bytes)?;
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
                    existing_files.push((name, file, identity));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
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
        after_inspection()?;
        directory.validate_identity()?;
        for (name, _file, identity) in &existing_files {
            directory.validate_file(OsStr::new(name), identity)?;
        }

        let mut retained_directories = Vec::new();
        for directory_name in [PROMPTS_DIR, RESPONSES_DIR, ARTIFACTS_DIR] {
            match directory.open_child_directory(OsStr::new(directory_name)) {
                Ok(child) => retained_directories.push((directory_name, child)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let child = directory.create_child_directory(OsStr::new(directory_name))?;
                    retained_directories.push((directory_name, child));
                    directory.sync_all()?;
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
            crate::immutable_artifact::publish_create_only(&self.run_directory, name, expected)
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
    let parent = artifact_safety::open_private_descendant_parent(run_directory, relative_path)?;
    let file_name = relative_path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("artifact path has no file name: {}", path.display()),
        )
    })?;
    before_open()?;
    parent.validate_identity()?;
    let (mut file, existed) = match parent.open_existing_file(file_name, false, true) {
        Ok(file) => (file, true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            (parent.create_file(file_name)?, false)
        }
        Err(error) => return Err(error),
    };
    let opened = file.metadata()?;
    parent.validate_single_link_file(file_name, &opened)?;
    after_open()?;
    parent.validate_identity()?;
    parent.validate_single_link_file(file_name, &opened)?;
    if existed {
        file.set_len(0)?;
    }
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

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};

    use super::*;

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
                    || Ok(()),
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
