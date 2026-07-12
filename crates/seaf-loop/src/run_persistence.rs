use std::{
    error::Error,
    ffi::OsStr,
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

pub(crate) const RUN_MUTATION_LOCK_FILE: &str = "provider-exchange.lock";
static RUN_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const LOCK_MAX_ATTEMPTS: usize = 500;

#[derive(Debug, Clone, Copy)]
struct LockWaitPolicy {
    max_attempts: usize,
    retry_interval: Duration,
}

#[derive(Debug)]
pub(crate) struct RunMutationGuard {
    file: fs::File,
    path: PathBuf,
    locked: bool,
}

impl RunMutationGuard {
    pub(crate) fn acquire(run_directory: &Path) -> Result<Self, RunPersistenceError> {
        Self::acquire_with_policy(
            run_directory,
            LockWaitPolicy {
                max_attempts: LOCK_MAX_ATTEMPTS,
                retry_interval: LOCK_RETRY_INTERVAL,
            },
        )
    }

    fn acquire_with_policy(
        run_directory: &Path,
        policy: LockWaitPolicy,
    ) -> Result<Self, RunPersistenceError> {
        let path = run_directory.join(RUN_MUTATION_LOCK_FILE);
        let mut created = false;
        let file = match inspect_lock_path(&path) {
            Ok(()) => open_existing_lock_file(&path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match create_lock_file(&path) {
                    Ok(file) => {
                        created = true;
                        file
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        inspect_lock_path(&path)?;
                        open_existing_lock_file(&path)?
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        };
        validate_opened_lock_file(&file, &path)?;
        if created {
            file.sync_all()?;
            sync_directory(run_directory)?;
        }

        for attempt in 0..policy.max_attempts.max(1) {
            match file.try_lock() {
                Ok(()) => break,
                Err(fs::TryLockError::WouldBlock) if attempt + 1 < policy.max_attempts => {
                    thread::sleep(policy.retry_interval);
                }
                Err(fs::TryLockError::WouldBlock) => {
                    return Err(RunPersistenceError::Busy(path));
                }
                Err(fs::TryLockError::Error(error))
                    if error.kind() == std::io::ErrorKind::Interrupted
                        && attempt + 1 < policy.max_attempts =>
                {
                    continue;
                }
                Err(fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        if let Err(error) = validate_opened_lock_file(&file, &path) {
            let _ = file.unlock();
            return Err(error.into());
        }
        Ok(Self {
            file,
            path,
            locked: true,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), RunPersistenceError> {
        validate_opened_lock_file(&self.file, &self.path)?;
        Ok(())
    }

    pub(crate) fn unlock(mut self) -> Result<(), RunPersistenceError> {
        self.file.unlock()?;
        self.locked = false;
        Ok(())
    }
}

impl Drop for RunMutationGuard {
    fn drop(&mut self) {
        if self.locked {
            let _ = self.file.unlock();
        }
    }
}

pub(crate) fn publish_replacement(
    guard: &RunMutationGuard,
    target: &Path,
    bytes: &[u8],
) -> Result<(), RunPersistenceError> {
    publish_replacement_with_hooks(guard, target, bytes, || Ok(()), || Ok(()))
}

pub(crate) fn publish_replacement_with_hooks<BeforeRename, AfterRename>(
    guard: &RunMutationGuard,
    target: &Path,
    bytes: &[u8],
    before_rename: BeforeRename,
    after_rename: AfterRename,
) -> Result<(), RunPersistenceError>
where
    BeforeRename: FnOnce() -> Result<(), RunPersistenceError>,
    AfterRename: FnOnce() -> Result<(), RunPersistenceError>,
{
    let mut before_rename = Some(before_rename);
    let mut after_rename = Some(after_rename);
    publish_replacement_core(guard, target, bytes, None, |phase| match phase {
        PublishPhase::BeforeRename => before_rename
            .take()
            .expect("before-rename hook is called once")(),
        PublishPhase::AfterRename => after_rename
            .take()
            .expect("after-rename hook is called once")(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublishPhase {
    BeforeRename,
    AfterRename,
}

fn publish_replacement_core<F>(
    guard: &RunMutationGuard,
    target: &Path,
    bytes: &[u8],
    fault: Option<InjectedPublicationFault>,
    mut hook: F,
) -> Result<(), RunPersistenceError>
where
    F: FnMut(PublishPhase) -> Result<(), RunPersistenceError>,
{
    let (parent, file_name) = target_coordinates(target)?;
    let current_target = open_regular_file_no_follow(target)?;
    let (temp_path, mut temp) = create_temp(parent, file_name)?;
    let result = (|| {
        if fault == Some(InjectedPublicationFault::PartialTempWrite) {
            temp.write_all(&bytes[..bytes.len().div_ceil(2)])?;
            return Err(injected_fault(InjectedPublicationFault::PartialTempWrite));
        }
        temp.write_all(bytes)?;
        if fault == Some(InjectedPublicationFault::TempSync) {
            return Err(injected_fault(InjectedPublicationFault::TempSync));
        }
        temp.sync_all()?;
        drop(temp);
        hook(PublishPhase::BeforeRename)?;
        guard.validate()?;
        validate_opened_regular_file(&current_target, target)?;
        if fault == Some(InjectedPublicationFault::Publish) {
            return Err(injected_fault(InjectedPublicationFault::Publish));
        }
        atomic_replace(&temp_path, target)?;
        hook(PublishPhase::AfterRename)?;
        if fault == Some(InjectedPublicationFault::ParentSync) {
            return Err(injected_fault(InjectedPublicationFault::ParentSync));
        }
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

pub(crate) fn publish_create_only(
    guard: &RunMutationGuard,
    target: &Path,
    bytes: &[u8],
) -> Result<(), RunPersistenceError> {
    publish_create_only_core(guard, target, bytes, None)
}

fn publish_create_only_core(
    guard: &RunMutationGuard,
    target: &Path,
    bytes: &[u8],
    fault: Option<InjectedPublicationFault>,
) -> Result<(), RunPersistenceError> {
    let (parent, file_name) = target_coordinates(target)?;
    let (temp_path, mut temp) = create_temp(parent, file_name)?;
    let result = (|| {
        if fault == Some(InjectedPublicationFault::PartialTempWrite) {
            temp.write_all(&bytes[..bytes.len().div_ceil(2)])?;
            return Err(injected_fault(InjectedPublicationFault::PartialTempWrite));
        }
        temp.write_all(bytes)?;
        if fault == Some(InjectedPublicationFault::TempSync) {
            return Err(injected_fault(InjectedPublicationFault::TempSync));
        }
        temp.sync_all()?;
        drop(temp);
        guard.validate()?;
        if fault == Some(InjectedPublicationFault::Publish) {
            return Err(injected_fault(InjectedPublicationFault::Publish));
        }
        fs::hard_link(&temp_path, target)?;
        if fault == Some(InjectedPublicationFault::TempUnlink) {
            return Err(injected_fault(InjectedPublicationFault::TempUnlink));
        }
        fs::remove_file(&temp_path)?;
        if fault == Some(InjectedPublicationFault::ParentSync) {
            return Err(injected_fault(InjectedPublicationFault::ParentSync));
        }
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
enum InjectedPublicationFault {
    PartialTempWrite,
    TempSync,
    Publish,
    TempUnlink,
    ParentSync,
}

fn injected_fault(fault: InjectedPublicationFault) -> RunPersistenceError {
    RunPersistenceError::Invalid(format!("injected {fault:?} failure"))
}

#[cfg(test)]
fn publish_replacement_with_fault(
    guard: &RunMutationGuard,
    target: &Path,
    bytes: &[u8],
    fault: InjectedPublicationFault,
) -> Result<(), RunPersistenceError> {
    publish_replacement_core(guard, target, bytes, Some(fault), |_| Ok(()))
}

#[cfg(test)]
fn publish_create_only_with_fault(
    guard: &RunMutationGuard,
    target: &Path,
    bytes: &[u8],
    fault: InjectedPublicationFault,
) -> Result<(), RunPersistenceError> {
    publish_create_only_core(guard, target, bytes, Some(fault))
}

pub(crate) fn sync_existing(
    guard: &RunMutationGuard,
    target: &Path,
) -> Result<(), RunPersistenceError> {
    guard.validate()?;
    let parent = target.parent().ok_or_else(|| {
        RunPersistenceError::Invalid("run file has no parent directory".to_string())
    })?;
    let file = open_regular_file_no_follow(target)?;
    file.sync_all()?;
    validate_opened_regular_file(&file, target)?;
    guard.validate()?;
    sync_directory(parent)?;
    Ok(())
}

pub(crate) fn read_regular_file(path: &Path) -> Result<Vec<u8>, RunPersistenceError> {
    let mut file = open_regular_file_no_follow(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    validate_opened_regular_file(&file, path)?;
    Ok(bytes)
}

fn target_coordinates(target: &Path) -> Result<(&Path, &OsStr), RunPersistenceError> {
    let parent = target.parent().ok_or_else(|| {
        RunPersistenceError::Invalid("run file has no parent directory".to_string())
    })?;
    let file_name = target
        .file_name()
        .ok_or_else(|| RunPersistenceError::Invalid("run file has no file name".to_string()))?;
    Ok((parent, file_name))
}

fn create_temp(parent: &Path, file_name: &OsStr) -> std::io::Result<(PathBuf, fs::File)> {
    loop {
        let sequence = RUN_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{}.run-state.tmp-{}-{sequence}",
            file_name.to_string_lossy(),
            std::process::id()
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        set_no_follow(&mut options);
        match options.open(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

fn create_lock_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    set_no_follow(&mut options);
    options.open(path)
}

fn open_existing_lock_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    set_no_follow(&mut options);
    options.open(path)
}

fn open_regular_file_no_follow(path: &Path) -> std::io::Result<fs::File> {
    inspect_regular_file(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    set_no_follow(&mut options);
    let file = options.open(path)?;
    validate_opened_regular_file(&file, path)?;
    Ok(file)
}

fn validate_opened_regular_file(file: &fs::File, path: &Path) -> std::io::Result<()> {
    inspect_regular_file(path)?;
    let opened = file.metadata()?;
    let current = fs::metadata(path)?;
    if !metadata_identity_matches(&opened, &current) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "run file path changed while it was opened",
        ));
    }
    Ok(())
}

fn inspect_lock_path(path: &Path) -> std::io::Result<()> {
    inspect_regular_file(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            error
        } else {
            std::io::Error::new(
                error.kind(),
                format!("run-state mutation lock must be a real regular file: {error}"),
            )
        }
    })
}

fn inspect_regular_file(path: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path must be a real regular file",
        ));
    }
    Ok(())
}

fn validate_opened_lock_file(file: &fs::File, path: &Path) -> std::io::Result<()> {
    inspect_lock_path(path)?;
    let opened = file.metadata()?;
    let current = fs::metadata(path)?;
    if !metadata_identity_matches(&opened, &current) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "run-state mutation lock path changed while it was opened",
        ));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn atomic_replace(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn atomic_replace(_source: &Path, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic run-state replacement is only supported on macOS and Linux",
    ))
}

#[cfg(target_os = "macos")]
fn set_no_follow(options: &mut fs::OpenOptions) {
    options.custom_flags(0x100);
}

#[cfg(target_os = "linux")]
fn set_no_follow(options: &mut fs::OpenOptions) {
    options.custom_flags(0x20_000);
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn set_no_follow(_options: &mut fs::OpenOptions) {}

#[cfg(unix)]
fn metadata_identity_matches(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn metadata_identity_matches(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    false
}

#[derive(Debug)]
pub(crate) enum RunPersistenceError {
    Busy(PathBuf),
    Invalid(String),
    Io(std::io::Error),
}

impl fmt::Display for RunPersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(path) => write!(
                formatter,
                "run-state mutation lock remained busy: {}",
                path.display()
            ),
            Self::Invalid(message) => write!(formatter, "invalid run-state publication: {message}"),
            Self::Io(error) => write!(formatter, "run-state publication I/O error: {error}"),
        }
    }
}

impl Error for RunPersistenceError {}

impl From<std::io::Error> for RunPersistenceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn initialized_target() -> (tempfile::TempDir, PathBuf, Vec<u8>) {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("run.json");
        let old = b"old-valid-run\n".to_vec();
        fs::write(&target, &old).unwrap();
        (temp, target, old)
    }

    #[test]
    fn replacement_write_sync_and_rename_failures_keep_old_bytes() {
        for fault in [
            InjectedPublicationFault::PartialTempWrite,
            InjectedPublicationFault::TempSync,
            InjectedPublicationFault::Publish,
        ] {
            let (temp, target, old) = initialized_target();
            let guard = RunMutationGuard::acquire(temp.path()).unwrap();
            let error =
                publish_replacement_with_fault(&guard, &target, b"intended-valid-run\n", fault)
                    .unwrap_err();
            assert!(error.to_string().contains("injected"), "{error}");
            assert_eq!(fs::read(&target).unwrap(), old, "fault {fault:?}");
            assert!(
                fs::read_dir(temp.path())
                    .unwrap()
                    .filter_map(Result::ok)
                    .all(|entry| !entry
                        .file_name()
                        .to_string_lossy()
                        .contains(".run-state.tmp-")),
                "fault {fault:?} left a reserved temp"
            );
        }
    }

    #[test]
    fn post_rename_fault_leaves_complete_intended_bytes_and_retry_resyncs() {
        let (temp, target, _old) = initialized_target();
        let intended = b"intended-valid-run\n";
        let guard = RunMutationGuard::acquire(temp.path()).unwrap();
        let error = publish_replacement_with_fault(
            &guard,
            &target,
            intended,
            InjectedPublicationFault::ParentSync,
        )
        .unwrap_err();
        assert!(error.to_string().contains("ParentSync"), "{error}");
        assert_eq!(fs::read(&target).unwrap(), intended);
        guard.unlock().unwrap();

        let retry = RunMutationGuard::acquire(temp.path()).unwrap();
        sync_existing(&retry, &target).expect("exact retry closes durability uncertainty");
        assert_eq!(fs::read(&target).unwrap(), intended);
    }

    #[test]
    fn create_only_faults_leave_absent_or_complete_intended_state_and_retry_resyncs() {
        let intended = b"initial-valid-run\n";
        for fault in [
            InjectedPublicationFault::PartialTempWrite,
            InjectedPublicationFault::TempSync,
            InjectedPublicationFault::Publish,
            InjectedPublicationFault::TempUnlink,
            InjectedPublicationFault::ParentSync,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let target = temp.path().join("run.json");
            let guard = RunMutationGuard::acquire(temp.path()).unwrap();
            let error = publish_create_only_with_fault(&guard, &target, intended, fault)
                .expect_err("injected create-only cut");
            assert!(error.to_string().contains("injected"), "{error}");
            match fault {
                InjectedPublicationFault::PartialTempWrite
                | InjectedPublicationFault::TempSync
                | InjectedPublicationFault::Publish => assert!(!target.exists(), "{fault:?}"),
                InjectedPublicationFault::TempUnlink | InjectedPublicationFault::ParentSync => {
                    assert_eq!(fs::read(&target).unwrap(), intended, "{fault:?}");
                    guard.unlock().unwrap();
                    let retry = RunMutationGuard::acquire(temp.path()).unwrap();
                    sync_existing(&retry, &target)
                        .expect("exact create retry closes durability uncertainty");
                    assert_eq!(fs::read(&target).unwrap(), intended);
                    continue;
                }
            }
            assert!(
                fs::read_dir(temp.path())
                    .unwrap()
                    .filter_map(Result::ok)
                    .all(|entry| !entry
                        .file_name()
                        .to_string_lossy()
                        .contains(".run-state.tmp-")),
                "{fault:?} left a reserved temp"
            );
        }
    }

    #[test]
    fn temp_reservation_skips_collisions_without_touching_them() {
        let temp = tempfile::tempdir().unwrap();
        let start = RUN_TEMP_SEQUENCE.load(Ordering::Relaxed);
        let mut orphans = Vec::new();
        for sequence in start..start + 128 {
            let orphan = temp.path().join(format!(
                ".run.json.run-state.tmp-{}-{sequence}",
                std::process::id()
            ));
            fs::write(&orphan, b"orphan").unwrap();
            orphans.push(orphan);
        }
        let (reserved, file) = create_temp(temp.path(), OsStr::new("run.json")).unwrap();
        drop(file);
        assert!(!orphans.contains(&reserved));
        for orphan in orphans {
            assert_eq!(fs::read(orphan).unwrap(), b"orphan");
        }
        fs::remove_file(reserved).unwrap();
    }

    #[test]
    fn busy_lock_is_bounded_and_cannot_mutate_the_target() {
        let (temp, target, old) = initialized_target();
        let held = RunMutationGuard::acquire(temp.path()).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let worker_root = temp.path().to_path_buf();
        let worker_barrier = Arc::clone(&barrier);
        let worker = thread::spawn(move || {
            worker_barrier.wait();
            RunMutationGuard::acquire_with_policy(
                &worker_root,
                LockWaitPolicy {
                    max_attempts: 2,
                    retry_interval: Duration::ZERO,
                },
            )
        });
        barrier.wait();
        let error = worker.join().unwrap().unwrap_err();
        assert!(matches!(error, RunPersistenceError::Busy(_)), "{error}");
        assert_eq!(fs::read(target).unwrap(), old);
        drop(held);
    }

    #[test]
    fn released_lock_is_permanent_and_reused_by_inode() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(RUN_MUTATION_LOCK_FILE);
        RunMutationGuard::acquire(temp.path())
            .unwrap()
            .unlock()
            .unwrap();
        let first = fs::metadata(&path).unwrap();
        RunMutationGuard::acquire(temp.path())
            .unwrap()
            .unlock()
            .unwrap();
        let second = fs::metadata(&path).unwrap();
        assert!(metadata_identity_matches(&first, &second));
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_or_replaced_lock_path_is_rejected_before_rename() {
        use std::os::unix::fs::symlink;

        for kind in ["symlink", "directory"] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join(RUN_MUTATION_LOCK_FILE);
            if kind == "symlink" {
                let outside = temp.path().join("outside");
                fs::write(&outside, b"outside").unwrap();
                symlink(outside, &path).unwrap();
            } else {
                fs::create_dir(&path).unwrap();
            }
            assert!(RunMutationGuard::acquire(temp.path()).is_err(), "{kind}");
        }

        let (temp, target, old) = initialized_target();
        let guard = RunMutationGuard::acquire(temp.path()).unwrap();
        let lock_path = temp.path().join(RUN_MUTATION_LOCK_FILE);
        let error = publish_replacement_core(&guard, &target, b"replacement\n", None, |phase| {
            if phase == PublishPhase::BeforeRename {
                fs::remove_file(&lock_path)?;
                fs::write(&lock_path, b"replacement lock")?;
            }
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("lock path changed"), "{error}");
        assert_eq!(fs::read(target).unwrap(), old);

        let (temp, target, _old) = initialized_target();
        let outside = temp.path().join("outside-run");
        fs::write(&outside, b"outside-unchanged\n").unwrap();
        let guard = RunMutationGuard::acquire(temp.path()).unwrap();
        let error = publish_replacement_core(&guard, &target, b"replacement\n", None, |phase| {
            if phase == PublishPhase::BeforeRename {
                fs::remove_file(&target)?;
                symlink(&outside, &target)?;
            }
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("real regular file"), "{error}");
        assert_eq!(fs::read(outside).unwrap(), b"outside-unchanged\n");
    }
}
