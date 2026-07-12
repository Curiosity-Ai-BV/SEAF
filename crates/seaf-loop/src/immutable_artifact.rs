use std::{
    error::Error,
    fmt, fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn read_verified_regular_file(
    run_directory: &Path,
    relative_path: &str,
    label: &str,
) -> Result<Vec<u8>, ImmutableArtifactError> {
    read_verified_regular_file_with_hook(run_directory, relative_path, label, || Ok(()))
}

fn read_verified_regular_file_with_hook<F>(
    run_directory: &Path,
    relative_path: &str,
    label: &str,
    after_inspect: F,
) -> Result<Vec<u8>, ImmutableArtifactError>
where
    F: FnOnce() -> Result<(), ImmutableArtifactError>,
{
    validate_relative_path(relative_path)?;
    let relative = Path::new(relative_path);
    let canonical_parent = validate_real_run_parent(run_directory, relative)?;
    let file_name = relative
        .file_name()
        .ok_or_else(|| ImmutableArtifactError::Safety(format!("{label} has no flat file name")))?;
    let path = canonical_parent.join(file_name);
    let parent_identity = fs::symlink_metadata(&canonical_parent)?;
    read_opened_verified_file(
        &path,
        &canonical_parent,
        &parent_identity,
        label,
        after_inspect,
    )
}

#[cfg(unix)]
fn read_opened_verified_file<F>(
    path: &Path,
    parent: &Path,
    parent_identity: &fs::Metadata,
    label: &str,
    after_inspect: F,
) -> Result<Vec<u8>, ImmutableArtifactError>
where
    F: FnOnce() -> Result<(), ImmutableArtifactError>,
{
    validate_parent_identity(parent, parent_identity, label)?;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| {
            let context = if error.kind() == std::io::ErrorKind::NotFound {
                "could not be inspected"
            } else {
                "could not be opened without following links"
            };
            ImmutableArtifactError::Safety(format!("{label} {context}: {error}"))
        })?;
    let opened = file.metadata().map_err(|error| {
        ImmutableArtifactError::Safety(format!("{label} could not be inspected: {error}"))
    })?;
    if !opened.is_file() {
        return Err(ImmutableArtifactError::Safety(format!(
            "{label} is not a real regular file"
        )));
    }
    validate_opened_file_identity(path, &opened, label)?;
    after_inspect()?;
    validate_parent_identity(parent, parent_identity, label)?;
    validate_opened_file_identity(path, &opened, label)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    validate_parent_identity(parent, parent_identity, label)?;
    validate_opened_file_identity(path, &opened, label)?;
    let after = file.metadata()?;
    if !metadata_identity_matches(&opened, &after) {
        return Err(ImmutableArtifactError::Safety(format!(
            "{label} opened file identity changed while reading"
        )));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn validate_parent_identity(
    path: &Path,
    opened: &fs::Metadata,
    label: &str,
) -> Result<(), ImmutableArtifactError> {
    let current = fs::symlink_metadata(path).map_err(|error| {
        ImmutableArtifactError::Safety(format!(
            "{label} parent identity could not be revalidated: {error}"
        ))
    })?;
    if current.file_type().is_symlink()
        || !current.is_dir()
        || !metadata_identity_matches(opened, &current)
    {
        return Err(ImmutableArtifactError::Safety(format!(
            "{label} parent identity changed while reading"
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_opened_file_identity(
    path: &Path,
    opened: &fs::Metadata,
    label: &str,
) -> Result<(), ImmutableArtifactError> {
    let current = fs::symlink_metadata(path).map_err(|error| {
        ImmutableArtifactError::Safety(format!(
            "{label} path identity could not be revalidated: {error}"
        ))
    })?;
    if current.file_type().is_symlink()
        || !current.is_file()
        || !metadata_identity_matches(opened, &current)
    {
        return Err(ImmutableArtifactError::Safety(format!(
            "{label} path identity changed while reading"
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn metadata_identity_matches(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn read_opened_verified_file<F>(
    _path: &Path,
    _parent: &Path,
    _parent_identity: &fs::Metadata,
    label: &str,
    _after_inspect: F,
) -> Result<Vec<u8>, ImmutableArtifactError>
where
    F: FnOnce() -> Result<(), ImmutableArtifactError>,
{
    Err(ImmutableArtifactError::Safety(format!(
        "{label} verified no-follow reads are unsupported on this platform"
    )))
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

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, os::unix::fs::symlink};

    use super::*;

    #[test]
    fn verified_read_rejects_a_symlink_replacement_after_initial_inspection() {
        let temp = tempfile::tempdir().expect("temp dir");
        let run = temp.path().join("run");
        fs::create_dir(&run).unwrap();
        fs::write(run.join("artifact"), b"approved").unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, b"substituted").unwrap();

        let error = read_verified_regular_file_with_hook(&run, "artifact", "artifact", || {
            fs::remove_file(run.join("artifact"))?;
            symlink(&outside, run.join("artifact"))?;
            Ok(())
        })
        .expect_err("a replacement symlink must never be followed");

        assert!(error.to_string().contains("identity") || error.to_string().contains("regular"));
    }

    #[test]
    fn verified_read_rejects_a_regular_file_replacement_after_initial_inspection() {
        let temp = tempfile::tempdir().expect("temp dir");
        let run = temp.path().join("run");
        fs::create_dir(&run).unwrap();
        fs::write(run.join("artifact"), b"approved").unwrap();

        let error = read_verified_regular_file_with_hook(&run, "artifact", "artifact", || {
            fs::rename(run.join("artifact"), run.join("old"))?;
            fs::write(run.join("artifact"), b"substituted")?;
            Ok(())
        })
        .expect_err("a replacement regular file must not satisfy the inspected authority");

        assert!(error.to_string().contains("identity"));
    }
}
