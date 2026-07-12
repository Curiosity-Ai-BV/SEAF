use std::{
    error::Error,
    ffi::{OsStr, OsString},
    fmt, fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::artifact_safety;

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
    artifact_safety::validate_opened_private_regular_file(path, &opened)?;
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
    publish_create_only_with_hook(run_directory, relative_path, bytes, |_| Ok(()))
}

fn publish_create_only_with_hook<F>(
    run_directory: &Path,
    relative_path: &str,
    bytes: &[u8],
    before_link: F,
) -> Result<(), ImmutableArtifactError>
where
    F: FnOnce(&Path) -> Result<(), ImmutableArtifactError>,
{
    validate_relative_path(relative_path)?;
    validate_real_run_parent(run_directory, Path::new(relative_path))?;
    let parent =
        artifact_safety::open_private_descendant_parent(run_directory, Path::new(relative_path))?;
    let file_name = Path::new(relative_path).file_name().ok_or_else(|| {
        ImmutableArtifactError::Safety("artifact has no flat file name".to_string())
    })?;
    let (temp_name, temp_path, mut temp) = create_unique_temp_file(&parent, file_name)?;
    let temp_identity = temp.metadata()?;
    let result = (|| {
        temp.write_all(bytes)?;
        temp.sync_all()?;
        drop(temp);
        before_link(&temp_path)?;
        parent.validate_identity()?;
        parent.validate_file(&temp_name, &temp_identity)?;

        match parent.hard_link(&temp_name, file_name) {
            Ok(()) => {
                let winner = parent.open_existing_file(file_name, true, false)?;
                let winner_identity = winner.metadata()?;
                parent.validate_file(file_name, &winner_identity)?;
                if !artifact_safety::same_file_identity(&temp_identity, &winner_identity) {
                    return Err(ImmutableArtifactError::Safety(
                        "immutable artifact target changed after link publication".to_string(),
                    ));
                }
                parent.sync_all()?;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_existing_winner_in(&parent, file_name, bytes, || Ok(()))?;
                parent.sync_all()?;
                Ok(())
            }
            Err(error) => Err(ImmutableArtifactError::Io(error)),
        }
    })();
    let cleanup = parent.unlink_if_same(&temp_name, &temp_identity);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(error), _) => Err(error),
    }
}

fn create_unique_temp_file(
    parent: &artifact_safety::PinnedPrivateDirectory,
    final_name: &OsStr,
) -> Result<(OsString, PathBuf, fs::File), ImmutableArtifactError> {
    let final_name = final_name.to_string_lossy();
    loop {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = OsString::from(format!(
            ".{final_name}.tmp-{}-{sequence}",
            std::process::id()
        ));
        let path = parent.path().join(&name);
        match parent.create_file(&name) {
            Ok(file) => return Ok((name, path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
fn verify_existing_winner(target: &Path, bytes: &[u8]) -> Result<(), ImmutableArtifactError> {
    verify_existing_winner_with_hook(target, bytes, || Ok(()))
}

#[cfg(test)]
fn verify_existing_winner_with_hook<F>(
    target: &Path,
    bytes: &[u8],
    after_open: F,
) -> Result<(), ImmutableArtifactError>
where
    F: FnOnce() -> Result<(), ImmutableArtifactError>,
{
    let parent = target.parent().ok_or_else(|| {
        ImmutableArtifactError::Safety("existing artifact has no parent".to_string())
    })?;
    let file_name = target.file_name().ok_or_else(|| {
        ImmutableArtifactError::Safety("existing artifact has no file name".to_string())
    })?;
    let parent = artifact_safety::PinnedPrivateDirectory::open(parent)?;
    verify_existing_winner_in(&parent, file_name, bytes, after_open)
}

fn verify_existing_winner_in<F>(
    parent: &artifact_safety::PinnedPrivateDirectory,
    file_name: &OsStr,
    bytes: &[u8],
    after_open: F,
) -> Result<(), ImmutableArtifactError>
where
    F: FnOnce() -> Result<(), ImmutableArtifactError>,
{
    let mut file = parent.open_existing_file(file_name, true, false)?;
    let opened = file.metadata()?;
    parent.validate_file(file_name, &opened)?;
    after_open()?;
    let mut current = Vec::new();
    file.read_to_end(&mut current)?;
    parent.validate_identity()?;
    parent.validate_file(file_name, &opened)?;
    if current == bytes {
        file.sync_all()?;
        Ok(())
    } else {
        Err(ImmutableArtifactError::Collision(
            "existing artifact has different bytes".to_string(),
        ))
    }
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
    artifact_safety::validate_private_directory(run_directory)?;
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
        artifact_safety::validate_private_directory(&current)?;
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
    use std::{
        fs,
        os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    };

    use super::*;

    fn private_run(temp: &tempfile::TempDir) -> PathBuf {
        let run = temp.path().join("run");
        artifact_safety::create_private_directory(&run).unwrap();
        run
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn create_only_publication_keeps_temp_and_final_inode_private() {
        let temp = tempfile::tempdir().unwrap();
        let run = private_run(&temp);
        publish_create_only_with_hook(&run, "artifact", b"approved", |temp_path| {
            assert_eq!(fs::symlink_metadata(temp_path)?.mode() & 0o777, 0o600);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            fs::symlink_metadata(run.join("artifact")).unwrap().mode() & 0o777,
            0o600
        );
        publish_create_only(&run, "artifact", b"approved").unwrap();
        assert_eq!(
            fs::symlink_metadata(run.join("artifact")).unwrap().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn existing_winner_retry_rejects_replacement_and_broad_mode_without_following() {
        let temp = tempfile::tempdir().unwrap();
        let run = private_run(&temp);
        let target = run.join("artifact");
        write_private(&target, b"approved");
        let parked = run.join("parked");
        let outside = temp.path().join("outside");
        fs::write(&outside, b"outside unchanged").unwrap();
        let error = verify_existing_winner_with_hook(&target, b"approved", || {
            fs::rename(&target, &parked)?;
            symlink(&outside, &target)?;
            Ok(())
        })
        .expect_err("winner replacement must fail closed");
        assert!(
            error.to_string().contains("identity") || error.to_string().contains("regular file"),
            "{error}"
        );
        assert_eq!(fs::read(&outside).unwrap(), b"outside unchanged");
        assert_eq!(fs::read(&parked).unwrap(), b"approved");

        fs::remove_file(&target).unwrap();
        fs::rename(&parked, &target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let before = fs::read(&target).unwrap();
        let error = verify_existing_winner(&target, b"approved")
            .expect_err("broad winner must not be adopted");
        assert!(error.to_string().contains("chmod 600"), "{error}");
        assert_eq!(fs::read(&target).unwrap(), before);
        assert_eq!(fs::symlink_metadata(&target).unwrap().mode() & 0o777, 0o644);
    }

    #[test]
    fn verified_read_rejects_a_symlink_replacement_after_initial_inspection() {
        let temp = tempfile::tempdir().expect("temp dir");
        let run = private_run(&temp);
        write_private(&run.join("artifact"), b"approved");
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
    fn create_only_parent_substitution_before_link_cannot_publish_externally() {
        let temp = tempfile::tempdir().unwrap();
        let run = private_run(&temp);
        artifact_safety::create_private_directory(&run.join("artifacts")).unwrap();
        let artifacts = run.join("artifacts");
        let parked = run.join("parked-artifacts");
        let outside = temp.path().join("outside");
        artifact_safety::create_private_directory(&outside).unwrap();
        let error = publish_create_only_with_hook(
            &run,
            "artifacts/escaped.json",
            b"must not escape",
            |_| {
                fs::rename(&artifacts, &parked)?;
                symlink(&outside, &artifacts)?;
                Ok(())
            },
        )
        .expect_err("parent substitution must fail before linkat");
        assert!(
            error.to_string().contains("directory") || error.to_string().contains("identity"),
            "{error}"
        );
        assert!(!outside.join("escaped.json").exists());
        assert!(!parked.join("escaped.json").exists());
    }

    #[test]
    fn verified_read_rejects_a_regular_file_replacement_after_initial_inspection() {
        let temp = tempfile::tempdir().expect("temp dir");
        let run = private_run(&temp);
        write_private(&run.join("artifact"), b"approved");

        let error = read_verified_regular_file_with_hook(&run, "artifact", "artifact", || {
            fs::rename(run.join("artifact"), run.join("old"))?;
            fs::write(run.join("artifact"), b"substituted")?;
            Ok(())
        })
        .expect_err("a replacement regular file must not satisfy the inspected authority");

        assert!(error.to_string().contains("identity"));
    }
}
