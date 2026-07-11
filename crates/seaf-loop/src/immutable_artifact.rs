use std::{
    error::Error,
    fmt, fs,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn read_verified_regular_file(
    run_directory: &Path,
    relative_path: &str,
    label: &str,
) -> Result<Vec<u8>, ImmutableArtifactError> {
    validate_relative_path(relative_path)?;
    let path = run_directory.join(relative_path);
    validate_real_run_parent(run_directory, Path::new(relative_path))?;
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        ImmutableArtifactError::Safety(format!("{label} could not be inspected: {error}"))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ImmutableArtifactError::Safety(format!(
            "{label} is not a real regular file"
        )));
    }
    Ok(fs::read(path)?)
}

pub(crate) fn publish_create_only(
    run_directory: &Path,
    relative_path: &str,
    bytes: &[u8],
) -> Result<(), ImmutableArtifactError> {
    validate_relative_path(relative_path)?;
    let canonical_parent = validate_real_run_parent(run_directory, Path::new(relative_path))?;
    let file_name = Path::new(relative_path).file_name().ok_or_else(|| {
        ImmutableArtifactError::Safety("artifact has no flat file name".to_string())
    })?;
    let target = canonical_parent.join(file_name);
    let (temp_path, mut temp) = create_unique_temp_file(&canonical_parent, file_name)?;
    let result = (|| {
        temp.write_all(bytes)?;
        temp.sync_all()?;
        drop(temp);

        match fs::hard_link(&temp_path, &target) {
            Ok(()) => {
                sync_parent_directory(&canonical_parent)?;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_existing_winner(&target, bytes)?;
                sync_parent_directory(&canonical_parent)?;
                Ok(())
            }
            Err(error) => Err(ImmutableArtifactError::Io(error)),
        }
    })();
    let cleanup = fs::remove_file(&temp_path);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(error), _) => Err(error),
    }
}

fn create_unique_temp_file(
    parent: &Path,
    final_name: &std::ffi::OsStr,
) -> Result<(PathBuf, fs::File), ImmutableArtifactError> {
    let final_name = final_name.to_string_lossy();
    loop {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{final_name}.tmp-{}-{sequence}",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn verify_existing_winner(target: &Path, bytes: &[u8]) -> Result<(), ImmutableArtifactError> {
    let metadata = fs::symlink_metadata(target)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ImmutableArtifactError::Collision(
            "existing artifact target is not a real regular file".to_string(),
        ));
    }
    if fs::read(target)? == bytes {
        fs::File::open(target)?.sync_all()?;
        Ok(())
    } else {
        Err(ImmutableArtifactError::Collision(
            "existing artifact has different bytes".to_string(),
        ))
    }
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), ImmutableArtifactError> {
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), ImmutableArtifactError> {
    Ok(())
}

fn validate_real_run_parent(
    run_directory: &Path,
    relative_path: &Path,
) -> Result<PathBuf, ImmutableArtifactError> {
    let run_metadata = fs::symlink_metadata(run_directory)?;
    if run_metadata.file_type().is_symlink() || !run_metadata.is_dir() {
        return Err(ImmutableArtifactError::Safety(
            "run directory must be a real directory".to_string(),
        ));
    }
    let canonical_run = run_directory.canonicalize()?;
    let parent = relative_path.parent().ok_or_else(|| {
        ImmutableArtifactError::Safety("artifact reference has no parent".to_string())
    })?;
    let mut current = run_directory.to_path_buf();
    for component in parent.components() {
        let Component::Normal(component) = component else {
            return Err(ImmutableArtifactError::Safety(
                "artifact parent is not a safe relative path".to_string(),
            ));
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ImmutableArtifactError::Safety(format!(
                "artifact layout parent is not a real directory: {}",
                current.display()
            )));
        }
    }
    let canonical_parent = current.canonicalize()?;
    if !canonical_parent.starts_with(&canonical_run) {
        return Err(ImmutableArtifactError::Safety(
            "artifact parent resolves outside the run directory".to_string(),
        ));
    }
    Ok(canonical_parent)
}

fn validate_relative_path(path: &str) -> Result<(), ImmutableArtifactError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ImmutableArtifactError::Safety(
            "artifact reference is not a safe relative path".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum ImmutableArtifactError {
    Safety(String),
    Collision(String),
    Io(std::io::Error),
}

impl fmt::Display for ImmutableArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Safety(message) => {
                write!(formatter, "immutable artifact safety error: {message}")
            }
            Self::Collision(message) => {
                write!(formatter, "immutable artifact collision: {message}")
            }
            Self::Io(error) => write!(formatter, "immutable artifact I/O error: {error}"),
        }
    }
}

impl Error for ImmutableArtifactError {}

impl From<std::io::Error> for ImmutableArtifactError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
