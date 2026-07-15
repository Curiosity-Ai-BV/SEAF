use std::{
    error::Error,
    ffi::{OsStr, OsString},
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

use crate::artifact_safety;

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
    directory: artifact_safety::PinnedPrivateDirectory,
    file: fs::File,
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
            true,
            true,
        )
    }

    pub(crate) fn acquire_existing(run_directory: &Path) -> Result<Self, RunPersistenceError> {
        Self::acquire_with_policy(
            run_directory,
            LockWaitPolicy {
                max_attempts: LOCK_MAX_ATTEMPTS,
                retry_interval: LOCK_RETRY_INTERVAL,
            },
            false,
            false,
        )
    }

    pub(crate) fn try_acquire_existing(run_directory: &Path) -> Result<Self, RunPersistenceError> {
        Self::acquire_with_policy(
            run_directory,
            LockWaitPolicy {
                max_attempts: 1,
                retry_interval: Duration::ZERO,
            },
            false,
            false,
        )
    }

    fn acquire_with_policy(
        run_directory: &Path,
        policy: LockWaitPolicy,
        create_if_missing: bool,
        cleanup_orphans: bool,
    ) -> Result<Self, RunPersistenceError> {
        let directory = artifact_safety::PinnedPrivateDirectory::open(run_directory)?;
        let path = run_directory.join(RUN_MUTATION_LOCK_FILE);
        let mut created = false;
        let lock_name = OsStr::new(RUN_MUTATION_LOCK_FILE);
        let file = match open_existing_lock_file(&directory) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_if_missing => {
                crate::artifact_storage::validate_entry_projection(&directory, 1)?;
                match directory.create_file(lock_name) {
                    Ok(file) => {
                        created = true;
                        file
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        open_existing_lock_file(&directory)?
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        };
        directory.validate_single_link_file(lock_name, &file.metadata()?)?;
        if created {
            file.sync_all()?;
            directory.sync_all()?;
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
        if let Err(error) = directory
            .validate_identity()
            .and_then(|()| directory.validate_single_link_file(lock_name, &file.metadata()?))
        {
            let _ = file.unlock();
            return Err(error.into());
        }
        if cleanup_orphans {
            if let Err(error) = cleanup_orphaned_run_replacement_temps(&directory) {
                let _ = file.unlock();
                return Err(error.into());
            }
        }
        Ok(Self {
            directory,
            file,
            locked: true,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), RunPersistenceError> {
        self.directory.validate_identity()?;
        self.directory.validate_single_link_file(
            OsStr::new(RUN_MUTATION_LOCK_FILE),
            &self.file.metadata()?,
        )?;
        Ok(())
    }

    pub(crate) fn validate_at_child(
        &self,
        parent: &artifact_safety::PinnedPrivateDirectory,
        name: &OsStr,
    ) -> Result<(), RunPersistenceError> {
        self.directory.validate_single_link_file(
            OsStr::new(RUN_MUTATION_LOCK_FILE),
            &self.file.metadata()?,
        )?;
        parent.validate_child_directory(name, &self.directory.metadata()?)?;
        Ok(())
    }

    pub(crate) fn run_directory(&self) -> &Path {
        self.directory.path()
    }

    pub(crate) fn validate_create_projection(
        &self,
        relative_path: &str,
        size: usize,
    ) -> Result<(), RunPersistenceError> {
        self.validate()?;
        crate::artifact_storage::validate_create_projection(&self.directory, relative_path, size)?;
        self.validate()
    }

    pub(crate) fn validate_atomic_replacement_projection_with_old(
        &self,
        relative_path: &str,
        size: usize,
        old_size: u64,
    ) -> Result<(), RunPersistenceError> {
        self.validate()?;
        crate::artifact_storage::validate_atomic_replacement_projection_with_old(
            &self.directory,
            relative_path,
            size,
            old_size,
        )?;
        self.validate()
    }

    fn validate_atomic_replacement_projection_for_bytes(
        &self,
        relative_path: &str,
        bytes: &[u8],
        old_size: u64,
    ) -> Result<(), RunPersistenceError> {
        if relative_path != crate::workspace::RUN_FILE
            || serde_json::from_slice::<seaf_core::LoopRun>(bytes).is_err()
        {
            return self.validate_atomic_replacement_projection_with_old(
                relative_path,
                bytes.len(),
                old_size,
            );
        }
        self.validate()?;
        let commitment = crate::storage_authority::derive_storage_commitment_for_run_bytes(
            self.run_directory(),
            bytes,
        )
        .map_err(RunPersistenceError::Invalid)?
        .unwrap_or_else(|| crate::artifact_storage::StorageCommitment {
            permanent_bytes: 0,
            transient_bytes: 0,
            permanent_entries: 0,
            transient_entries: 0,
            consumable_permanent_paths: Vec::new(),
            consumable_transient_path: None,
        });
        crate::artifact_storage::validate_atomic_replacement_projection_with_commitment(
            &self.directory,
            relative_path,
            bytes.len(),
            old_size,
            &commitment,
        )?;
        self.validate()
    }

    pub(crate) fn validate_active_provider_commitment(&self) -> Result<(), RunPersistenceError> {
        self.validate()?;
        let commitment = crate::provider_exchange::derive_active_provider_storage_commitment(
            self.run_directory(),
        )
        .map_err(|error| RunPersistenceError::Invalid(error.to_string()))?
        .ok_or_else(|| {
            RunPersistenceError::Invalid(
                "authoritative provider request tail has no active storage commitment".to_string(),
            )
        })?;
        crate::artifact_storage::validate_usage_with_commitment(&self.directory, &commitment)?;
        self.validate()
    }

    pub(crate) fn validate_active_storage_commitment(&self) -> Result<(), RunPersistenceError> {
        self.validate()?;
        let commitment =
            crate::storage_authority::derive_active_storage_commitment(self.run_directory())
                .map_err(RunPersistenceError::Invalid)?
                .ok_or_else(|| {
                    RunPersistenceError::Invalid(
                        "durable authority has no active storage commitment".into(),
                    )
                })?;
        crate::artifact_storage::validate_usage_with_commitment(&self.directory, &commitment)?;
        self.validate()
    }

    pub(crate) fn validate_create_activating_commitment(
        &self,
        relative_path: &str,
        size: usize,
        commitment: &crate::artifact_storage::StorageCommitment,
    ) -> Result<(), RunPersistenceError> {
        self.validate()?;
        crate::artifact_storage::validate_create_activating_commitment(
            &self.directory,
            relative_path,
            size,
            commitment,
        )?;
        self.validate()
    }

    pub(crate) fn validate_replacement_activating_provider_commitment(
        &self,
        relative_path: &str,
        size: usize,
        old_size: u64,
        commitment: &crate::artifact_storage::StorageCommitment,
    ) -> Result<(), RunPersistenceError> {
        self.validate()?;
        crate::artifact_storage::validate_replacement_activating_commitment(
            &self.directory,
            relative_path,
            size,
            old_size,
            commitment,
        )?;
        self.validate()
    }

    pub(crate) fn validate_provider_slot_create_projection(
        &self,
        relative_path: &str,
        size: usize,
    ) -> Result<(), RunPersistenceError> {
        self.validate()?;
        let commitment = crate::provider_exchange::derive_active_provider_storage_commitment(
            self.run_directory(),
        )
        .map_err(|error| RunPersistenceError::Invalid(error.to_string()))?
        .ok_or_else(|| {
            RunPersistenceError::Invalid(
                "provider slot publication requires an active request-tail commitment".to_string(),
            )
        })?;
        crate::artifact_storage::validate_provider_slot_create_projection(
            &self.directory,
            relative_path,
            size,
            &commitment,
        )?;
        self.validate()
    }

    pub(crate) fn validate_evaluation_slot_create_projection(
        &self,
        relative_path: &str,
        size: usize,
    ) -> Result<(), RunPersistenceError> {
        self.validate()?;
        let commitment =
            crate::storage_authority::derive_active_storage_commitment(self.run_directory())
                .map_err(RunPersistenceError::Invalid)?
                .ok_or_else(|| {
                    RunPersistenceError::Invalid(
                        "evaluation slot publication requires an active commitment".to_string(),
                    )
                })?;
        if relative_path.ends_with(".stdout.log") || relative_path.ends_with(".stderr.log") {
            crate::artifact_storage::validate_evaluation_log_create_projection(
                &self.directory,
                relative_path,
                size,
                &commitment,
            )?;
        } else {
            crate::artifact_storage::validate_provider_slot_create_projection(
                &self.directory,
                relative_path,
                size,
                &commitment,
            )?;
        }
        self.validate()
    }

    pub(crate) fn validate_provider_request_prefix_projection(
        &self,
        projection: &crate::artifact_storage::ProviderRequestPrefixProjection<'_>,
    ) -> Result<(), RunPersistenceError> {
        self.validate()?;
        crate::artifact_storage::validate_provider_request_prefix_projection(
            &self.directory,
            projection,
        )?;
        self.validate()
    }

    pub(crate) fn validate_staged_provider_request_projection(
        &self,
        record_path: &str,
        record_size: usize,
        adoption_run_size: usize,
        commitment: &crate::artifact_storage::StorageCommitment,
    ) -> Result<(), RunPersistenceError> {
        self.validate()?;
        let run_file = self.directory.open_existing_file(
            OsStr::new(crate::workspace::RUN_FILE),
            true,
            false,
        )?;
        let run_identity = run_file.metadata()?;
        self.directory
            .validate_single_link_file(OsStr::new(crate::workspace::RUN_FILE), &run_identity)?;
        crate::artifact_storage::validate_staged_provider_request_projection(
            &self.directory,
            record_path,
            record_size,
            adoption_run_size,
            run_identity.len(),
            commitment,
        )?;
        self.validate()
    }

    pub(crate) fn validate_existing_projection(
        &self,
        relative_path: &str,
        size: u64,
    ) -> Result<(), RunPersistenceError> {
        self.validate()?;
        crate::artifact_storage::validate_existing_projection(
            &self.directory,
            relative_path,
            size,
        )?;
        self.validate()
    }

    pub(crate) fn validate_entry_projection(
        &self,
        additional: usize,
    ) -> Result<(), RunPersistenceError> {
        self.validate()?;
        crate::artifact_storage::validate_entry_projection(&self.directory, additional)?;
        self.validate()
    }

    pub(crate) fn ensure_child_directory(&self, name: &OsStr) -> Result<(), RunPersistenceError> {
        self.validate()?;
        match self.directory.open_child_directory(name) {
            Ok(child) => child.validate_identity()?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.validate_entry_projection(1)?;
                let child = self.directory.create_child_directory(name)?;
                child.validate_identity()?;
                self.directory.sync_all()?;
            }
            Err(error) => return Err(error.into()),
        }
        self.validate()
    }

    pub(crate) fn unlock(mut self) -> Result<(), RunPersistenceError> {
        self.file.unlock()?;
        self.locked = false;
        Ok(())
    }

    pub(crate) fn remove_empty_locked_run_directory(
        mut self,
        parent: &artifact_safety::PinnedPrivateDirectory,
        name: &OsStr,
        relocated: &artifact_safety::PinnedPrivateDirectory,
    ) -> Result<(), RunPersistenceError> {
        self.validate_at_child(parent, name)?;
        relocated.validate_identity()?;
        if !artifact_safety::same_file_identity(&self.directory.metadata()?, &relocated.metadata()?)
        {
            return Err(RunPersistenceError::Invalid(
                "relocated purge directory no longer matches the locked run".to_string(),
            ));
        }
        let lock_name = OsStr::new(RUN_MUTATION_LOCK_FILE);
        let lock_identity = self.file.metadata()?;
        relocated.validate_single_link_file(lock_name, &lock_identity)?;
        relocated.unlink_if_same(lock_name, &lock_identity)?;
        relocated.sync_all()?;
        parent.validate_child_directory(name, &relocated.metadata()?)?;
        parent.remove_child_directory_if_same(name, relocated)?;
        parent.sync_all()?;
        self.file.unlock()?;
        self.locked = false;
        Ok(())
    }
}

fn cleanup_orphaned_run_replacement_temps(
    directory: &artifact_safety::PinnedPrivateDirectory,
) -> std::io::Result<()> {
    let mut stale = Vec::new();
    let mut entries = 0_usize;
    directory.for_each_entry_name(|name| {
        entries = entries.checked_add(1).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "run replacement temp enumeration overflowed",
            )
        })?;
        if entries > crate::artifact_storage::RUN_TREE_ENTRY_CAP {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "run replacement temp enumeration exceeds the run entry cap",
            ));
        }
        let Some(name_text) = name.to_str() else {
            return Ok(());
        };
        let Some(suffix) = name_text.strip_prefix(".run.json.run-state.tmp-") else {
            return Ok(());
        };
        let Some((pid, sequence)) = suffix.split_once('-') else {
            return Ok(());
        };
        let (Ok(pid_number), Ok(sequence_number)) = (pid.parse::<u32>(), sequence.parse::<u64>())
        else {
            return Ok(());
        };
        if format!("{pid_number}-{sequence_number}") != suffix {
            return Ok(());
        }
        let file = directory.open_existing_file(name, true, false)?;
        let metadata = file.metadata()?;
        directory.validate_single_link_file(name, &metadata)?;
        stale.push((name.to_os_string(), metadata));
        Ok(())
    })?;
    let changed = !stale.is_empty();
    for (name, metadata) in stale {
        directory.unlink_if_same(&name, &metadata)?;
    }
    if changed {
        directory.sync_all()?;
    }
    directory.validate_identity()
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

pub(crate) fn publish_replacement_consuming_provider_slot(
    guard: &RunMutationGuard,
    target: &Path,
    bytes: &[u8],
) -> Result<(), RunPersistenceError> {
    let commitment =
        crate::provider_exchange::derive_active_provider_storage_commitment(guard.run_directory())
            .map_err(|error| RunPersistenceError::Invalid(error.to_string()))?
            .ok_or_else(|| {
                RunPersistenceError::Invalid(
                    "provider replacement requires an active request-tail commitment".to_string(),
                )
            })?;
    let file_name = guard_target_name(guard, target)?;
    let current = guard.directory.open_existing_file(file_name, true, false)?;
    let current_identity = current.metadata()?;
    guard
        .directory
        .validate_single_link_file(file_name, &current_identity)?;
    let relative = file_name.to_str().ok_or_else(|| {
        RunPersistenceError::Invalid("run artifact name is not valid UTF-8".to_string())
    })?;
    crate::artifact_storage::validate_provider_slot_replacement_projection(
        &guard.directory,
        relative,
        bytes.len(),
        current_identity.len(),
        &commitment,
    )?;
    publish_replacement_core(guard, target, bytes, None, |_| Ok(()), true)
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
    publish_replacement_core(
        guard,
        target,
        bytes,
        None,
        |phase| match phase {
            PublishPhase::BeforeRename => before_rename
                .take()
                .expect("before-rename hook is called once")(
            ),
            PublishPhase::AfterRename => after_rename
                .take()
                .expect("after-rename hook is called once")(
            ),
        },
        false,
    )
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
    projection_prevalidated: bool,
) -> Result<(), RunPersistenceError>
where
    F: FnMut(PublishPhase) -> Result<(), RunPersistenceError>,
{
    let file_name = guard_target_name(guard, target)?;
    let current_target = guard.directory.open_existing_file(file_name, true, false)?;
    let current_identity = current_target.metadata()?;
    guard
        .directory
        .validate_single_link_file(file_name, &current_identity)?;
    let relative = file_name.to_str().ok_or_else(|| {
        RunPersistenceError::Invalid("run artifact name is not valid UTF-8".to_string())
    })?;
    if !projection_prevalidated {
        guard.validate_atomic_replacement_projection_for_bytes(
            relative,
            bytes,
            current_identity.len(),
        )?;
    }
    let (temp_name, _temp_path, mut temp) = create_temp(&guard.directory, file_name)?;
    let temp_identity = temp.metadata()?;
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
        guard
            .directory
            .validate_single_link_file(file_name, &current_identity)?;
        if fault == Some(InjectedPublicationFault::Publish) {
            return Err(injected_fault(InjectedPublicationFault::Publish));
        }
        guard.directory.rename(&temp_name, file_name)?;
        let published = guard.directory.open_existing_file(file_name, true, false)?;
        let published_identity = published.metadata()?;
        guard
            .directory
            .validate_file(file_name, &published_identity)?;
        if !artifact_safety::same_file_identity(&temp_identity, &published_identity) {
            return Err(RunPersistenceError::Invalid(
                "run-state target changed after replacement publication".to_string(),
            ));
        }
        hook(PublishPhase::AfterRename)?;
        if fault == Some(InjectedPublicationFault::ParentSync) {
            return Err(injected_fault(InjectedPublicationFault::ParentSync));
        }
        guard.directory.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = guard.directory.unlink_if_same(&temp_name, &temp_identity);
    }
    result
}

pub(crate) fn publish_create_only(
    guard: &RunMutationGuard,
    target: &Path,
    bytes: &[u8],
) -> Result<(), RunPersistenceError> {
    publish_create_only_core(guard, target, bytes, None, || Ok(()))
}

fn publish_create_only_core<F>(
    guard: &RunMutationGuard,
    target: &Path,
    bytes: &[u8],
    fault: Option<InjectedPublicationFault>,
    before_publish: F,
) -> Result<(), RunPersistenceError>
where
    F: FnOnce() -> Result<(), RunPersistenceError>,
{
    let file_name = guard_target_name(guard, target)?;
    let relative = file_name.to_str().ok_or_else(|| {
        RunPersistenceError::Invalid("run artifact name is not valid UTF-8".to_string())
    })?;
    guard.validate_create_projection(relative, bytes.len())?;
    let (temp_name, _temp_path, mut temp) = create_temp(&guard.directory, file_name)?;
    let temp_identity = temp.metadata()?;
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
        before_publish()?;
        guard.validate()?;
        if fault == Some(InjectedPublicationFault::Publish) {
            return Err(injected_fault(InjectedPublicationFault::Publish));
        }
        guard.directory.hard_link(&temp_name, file_name)?;
        let published = guard.directory.open_existing_file(file_name, true, false)?;
        let published_identity = published.metadata()?;
        guard
            .directory
            .validate_file(file_name, &published_identity)?;
        if !artifact_safety::same_file_identity(&temp_identity, &published_identity) {
            return Err(RunPersistenceError::Invalid(
                "run-state target changed after create-only publication".to_string(),
            ));
        }
        if fault == Some(InjectedPublicationFault::TempUnlink) {
            return Err(injected_fault(InjectedPublicationFault::TempUnlink));
        }
        guard.directory.unlink_if_same(&temp_name, &temp_identity)?;
        if fault == Some(InjectedPublicationFault::ParentSync) {
            return Err(injected_fault(InjectedPublicationFault::ParentSync));
        }
        guard.directory.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = guard.directory.unlink_if_same(&temp_name, &temp_identity);
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
    publish_replacement_core(guard, target, bytes, Some(fault), |_| Ok(()), false)
}

#[cfg(test)]
fn publish_create_only_with_fault(
    guard: &RunMutationGuard,
    target: &Path,
    bytes: &[u8],
    fault: InjectedPublicationFault,
) -> Result<(), RunPersistenceError> {
    publish_create_only_core(guard, target, bytes, Some(fault), || Ok(()))
}

pub(crate) fn sync_existing(
    guard: &RunMutationGuard,
    target: &Path,
) -> Result<(), RunPersistenceError> {
    guard.validate()?;
    let file_name = guard_target_name(guard, target)?;
    let file = guard.directory.open_existing_file(file_name, true, false)?;
    let identity = file.metadata()?;
    let relative = file_name.to_str().ok_or_else(|| {
        RunPersistenceError::Invalid("run artifact name is not valid UTF-8".to_string())
    })?;
    guard.validate_existing_projection(relative, identity.len())?;
    file.sync_all()?;
    guard.directory.validate_file(file_name, &identity)?;
    guard.validate()?;
    guard.directory.sync_all()?;
    Ok(())
}

pub(crate) fn read_regular_file(path: &Path) -> Result<Vec<u8>, RunPersistenceError> {
    let parent = path.parent().ok_or_else(|| {
        RunPersistenceError::Invalid("run file has no parent directory".to_string())
    })?;
    let name = path
        .file_name()
        .ok_or_else(|| RunPersistenceError::Invalid("run file has no file name".to_string()))?;
    let directory = artifact_safety::PinnedPrivateDirectory::open(parent)?;
    let mut file = directory.open_existing_file(name, true, false)?;
    let identity = file.metadata()?;
    let relative = name.to_str().ok_or_else(|| {
        RunPersistenceError::Invalid("run artifact name is not valid UTF-8".to_string())
    })?;
    crate::artifact_storage::validate_artifact_size_u64(relative, identity.len())?;
    let cap = crate::artifact_storage::artifact_byte_cap(relative);
    let mut bytes = Vec::new();
    (&mut file).take(cap + 1).read_to_end(&mut bytes)?;
    crate::artifact_storage::validate_artifact_size(relative, bytes.len())?;
    directory.validate_identity()?;
    directory.validate_file(name, &identity)?;
    let read_size = u64::try_from(bytes.len()).map_err(|_| {
        RunPersistenceError::Invalid("run artifact read size is not representable".to_string())
    })?;
    if file.metadata()?.len() != read_size {
        return Err(RunPersistenceError::Invalid(
            "run artifact changed while being read".to_string(),
        ));
    }
    Ok(bytes)
}

fn guard_target_name<'a>(
    guard: &RunMutationGuard,
    target: &'a Path,
) -> Result<&'a OsStr, RunPersistenceError> {
    let parent = target.parent().ok_or_else(|| {
        RunPersistenceError::Invalid("run file has no parent directory".to_string())
    })?;
    let file_name = target
        .file_name()
        .ok_or_else(|| RunPersistenceError::Invalid("run file has no file name".to_string()))?;
    if parent != guard.directory.path() {
        return Err(RunPersistenceError::Invalid(
            "run file parent does not match the pinned mutation directory".to_string(),
        ));
    }
    Ok(file_name)
}

fn open_existing_lock_file(
    directory: &artifact_safety::PinnedPrivateDirectory,
) -> std::io::Result<fs::File> {
    directory
        .open_existing_file(OsStr::new(RUN_MUTATION_LOCK_FILE), true, true)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                error
            } else {
                std::io::Error::new(
                    error.kind(),
                    format!("run-state mutation lock must be a real 0600 regular file: {error}"),
                )
            }
        })
}

fn create_temp(
    parent: &artifact_safety::PinnedPrivateDirectory,
    file_name: &OsStr,
) -> std::io::Result<(OsString, PathBuf, fs::File)> {
    loop {
        let sequence = RUN_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = OsString::from(format!(
            ".{}.run-state.tmp-{}-{sequence}",
            file_name.to_string_lossy(),
            std::process::id()
        ));
        let candidate = parent.path().join(&name);
        match parent.create_file(&name) {
            Ok(file) => return Ok((name, candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(all(test, unix))]
fn metadata_identity_matches(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(all(test, not(unix)))]
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

    fn private_temp() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        temp
    }

    fn initialized_target() -> (tempfile::TempDir, PathBuf, Vec<u8>) {
        let temp = private_temp();
        let target = temp.path().join("state.json");
        let old = b"old-valid-run\n".to_vec();
        fs::write(&target, &old).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        }
        (temp, target, old)
    }

    fn initialize_run_lock(root: &Path) {
        drop(RunMutationGuard::acquire(root).unwrap());
    }

    fn create_zero_byte_entries(root: &Path, count: usize, prefix: &str) {
        let directory = artifact_safety::PinnedPrivateDirectory::open(root).unwrap();
        for index in 0..count {
            directory
                .create_file(OsStr::new(&format!("{prefix}-{index:04}")))
                .unwrap();
        }
    }

    fn fill_individually_legal_files_over_aggregate_cap(root: &Path, prefix: &str) {
        let directory = artifact_safety::PinnedPrivateDirectory::open(root).unwrap();
        for index in 0..16 {
            directory
                .create_file(OsStr::new(&format!("{prefix}-{index:02}")))
                .unwrap()
                .set_len(2 * 1024 * 1024)
                .unwrap();
        }
        directory
            .create_file(OsStr::new(&format!("{prefix}-extra")))
            .unwrap()
            .set_len(1)
            .unwrap();
    }

    fn has_publication_temp(root: &Path) -> bool {
        fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".run-state.tmp-")
            })
    }

    #[test]
    fn first_run_lock_creation_respects_projected_entry_limit() {
        let exact_cap = private_temp();
        create_zero_byte_entries(
            exact_cap.path(),
            crate::artifact_storage::RUN_TREE_ENTRY_CAP,
            "lock-rejected",
        );
        let error = RunMutationGuard::acquire(exact_cap.path())
            .expect_err("first lock must not turn an exact-cap tree into cap plus one");
        assert!(error.to_string().contains("entry cap"), "{error}");
        assert!(!exact_cap.path().join(RUN_MUTATION_LOCK_FILE).exists());

        let exact_projection = private_temp();
        create_zero_byte_entries(
            exact_projection.path(),
            crate::artifact_storage::RUN_TREE_ENTRY_CAP - 1,
            "lock-exact",
        );
        drop(
            RunMutationGuard::acquire(exact_projection.path())
                .expect("first lock entry may reach the exact cap"),
        );
        assert!(exact_projection
            .path()
            .join(RUN_MUTATION_LOCK_FILE)
            .exists());
    }

    #[test]
    fn entry_only_creations_reject_existing_aggregate_byte_overage_before_mutation() {
        let lockless = private_temp();
        fill_individually_legal_files_over_aggregate_cap(lockless.path(), "lock-byte-overage");
        let pinned = artifact_safety::PinnedPrivateDirectory::open(lockless.path()).unwrap();
        let before = crate::artifact_storage::published_run_usage(&pinned).unwrap();
        let error = RunMutationGuard::acquire(lockless.path())
            .expect_err("first lock must reject an already byte-over-cap tree");
        assert!(error.to_string().contains("aggregate cap"), "{error}");
        assert!(!lockless.path().join(RUN_MUTATION_LOCK_FILE).exists());
        assert_eq!(
            crate::artifact_storage::published_run_usage(&pinned).unwrap(),
            before
        );

        let guarded = private_temp();
        initialize_run_lock(guarded.path());
        fill_individually_legal_files_over_aggregate_cap(guarded.path(), "child-byte-overage");
        let guard = RunMutationGuard::acquire(guarded.path()).unwrap();
        let pinned = artifact_safety::PinnedPrivateDirectory::open(guarded.path()).unwrap();
        let before = crate::artifact_storage::published_run_usage(&pinned).unwrap();
        let error = guard
            .ensure_child_directory(OsStr::new("child"))
            .expect_err("child directory must reject an already byte-over-cap tree");
        assert!(error.to_string().contains("aggregate cap"), "{error}");
        assert!(!guarded.path().join("child").exists());
        assert_eq!(
            crate::artifact_storage::published_run_usage(&pinned).unwrap(),
            before
        );
    }

    #[test]
    fn run_state_create_and_replacement_respect_projected_entry_peaks() {
        let exact_create = private_temp();
        initialize_run_lock(exact_create.path());
        create_zero_byte_entries(
            exact_create.path(),
            crate::artifact_storage::RUN_TREE_ENTRY_CAP - 3,
            "state-create-exact",
        );
        let guard = RunMutationGuard::acquire(exact_create.path()).unwrap();
        let target = exact_create.path().join("run.json");
        publish_create_only(&guard, &target, b"new")
            .expect("run-state temp plus final names may reach the exact entry cap");

        let rejected_create = private_temp();
        initialize_run_lock(rejected_create.path());
        create_zero_byte_entries(
            rejected_create.path(),
            crate::artifact_storage::RUN_TREE_ENTRY_CAP - 2,
            "state-create-rejected",
        );
        let guard = RunMutationGuard::acquire(rejected_create.path()).unwrap();
        let target = rejected_create.path().join("run.json");
        let error = publish_create_only(&guard, &target, b"new")
            .expect_err("run-state create projected entry cap plus one must fail");
        assert!(error.to_string().contains("entry cap"), "{error}");
        assert!(!target.exists());
        assert!(!has_publication_temp(rejected_create.path()));

        let exact_replace = private_temp();
        initialize_run_lock(exact_replace.path());
        crate::artifact_safety::write_private_fixture(
            exact_replace.path().join("run.json"),
            b"old",
        )
        .unwrap();
        create_zero_byte_entries(
            exact_replace.path(),
            crate::artifact_storage::RUN_TREE_ENTRY_CAP - 3,
            "state-replace-exact",
        );
        let guard = RunMutationGuard::acquire(exact_replace.path()).unwrap();
        let target = exact_replace.path().join("run.json");
        publish_replacement(&guard, &target, b"new")
            .expect("run-state replacement temp may reach the exact entry cap");
        assert_eq!(fs::read(&target).unwrap(), b"new");

        let rejected_replace = private_temp();
        initialize_run_lock(rejected_replace.path());
        crate::artifact_safety::write_private_fixture(
            rejected_replace.path().join("run.json"),
            b"old",
        )
        .unwrap();
        create_zero_byte_entries(
            rejected_replace.path(),
            crate::artifact_storage::RUN_TREE_ENTRY_CAP - 2,
            "state-replace-rejected",
        );
        let guard = RunMutationGuard::acquire(rejected_replace.path()).unwrap();
        let target = rejected_replace.path().join("run.json");
        let error = publish_replacement(&guard, &target, b"new")
            .expect_err("run-state replacement projected entry cap plus one must fail");
        assert!(error.to_string().contains("entry cap"), "{error}");
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert!(!has_publication_temp(rejected_replace.path()));
    }

    #[test]
    fn child_directory_creation_respects_projected_entry_limit() {
        let exact = private_temp();
        initialize_run_lock(exact.path());
        create_zero_byte_entries(
            exact.path(),
            crate::artifact_storage::RUN_TREE_ENTRY_CAP - 2,
            "directory-exact",
        );
        let guard = RunMutationGuard::acquire(exact.path()).unwrap();
        guard
            .ensure_child_directory(OsStr::new("child"))
            .expect("directory entry may reach the exact cap");

        let rejected = private_temp();
        initialize_run_lock(rejected.path());
        create_zero_byte_entries(
            rejected.path(),
            crate::artifact_storage::RUN_TREE_ENTRY_CAP - 1,
            "directory-rejected",
        );
        let guard = RunMutationGuard::acquire(rejected.path()).unwrap();
        let error = guard
            .ensure_child_directory(OsStr::new("child"))
            .expect_err("directory entry cap plus one must fail before creation");
        assert!(error.to_string().contains("entry cap"), "{error}");
        assert!(!rejected.path().join("child").exists());
    }

    #[test]
    fn run_reader_rejects_a_sparse_cap_plus_one_file_before_allocation() {
        let temp = private_temp();
        let target = temp.path().join("run.json");
        crate::artifact_safety::write_private_fixture(&target, b"").unwrap();
        fs::File::options()
            .write(true)
            .open(&target)
            .unwrap()
            .set_len(2 * 1024 * 1024 + 1)
            .unwrap();

        let error = read_regular_file(&target).expect_err("oversized run.json must fail closed");
        assert!(error.to_string().contains("byte cap"), "{error}");
        assert_eq!(fs::metadata(target).unwrap().len(), 2 * 1024 * 1024 + 1);
    }

    #[cfg(unix)]
    #[test]
    fn lock_temp_and_replacement_inodes_are_always_private() {
        use std::os::unix::fs::MetadataExt;

        let (temp, target, _) = initialized_target();
        let guard = RunMutationGuard::acquire(temp.path()).unwrap();
        assert_eq!(
            fs::symlink_metadata(temp.path().join(RUN_MUTATION_LOCK_FILE))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        publish_replacement_with_hooks(
            &guard,
            &target,
            b"intended-valid-run\n",
            || {
                let temp_entry = fs::read_dir(temp.path())?
                    .filter_map(Result::ok)
                    .find(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .contains(".run-state.tmp-")
                    })
                    .expect("reserved run-state temp");
                assert_eq!(temp_entry.metadata()?.mode() & 0o777, 0o600);
                Ok(())
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(fs::symlink_metadata(&target).unwrap().mode() & 0o777, 0o600);
        sync_existing(&guard, &target).unwrap();
        assert_eq!(fs::symlink_metadata(&target).unwrap().mode() & 0o777, 0o600);
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
            let temp = private_temp();
            let target = temp.path().join("state.json");
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
        let temp = private_temp();
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
        let pinned = artifact_safety::PinnedPrivateDirectory::open(temp.path()).unwrap();
        let (_reserved_name, reserved, file) =
            create_temp(&pinned, OsStr::new("run.json")).unwrap();
        drop(file);
        assert!(!orphans.contains(&reserved));
        for orphan in orphans {
            assert_eq!(fs::read(orphan).unwrap(), b"orphan");
        }
        fs::remove_file(reserved).unwrap();
    }

    #[test]
    fn reopening_the_run_guard_cleans_only_authenticated_orphan_replacement_temps() {
        let temp = private_temp();
        initialize_run_lock(temp.path());
        let stale = temp.path().join(".run.json.run-state.tmp-999999-1");
        crate::artifact_safety::write_private_fixture(&stale, b"synced intended run").unwrap();
        let unrelated = temp
            .path()
            .join(".run.json.run-state.tmp-not-a-canonical-owner");
        crate::artifact_safety::write_private_fixture(&unrelated, b"unrelated").unwrap();
        let leading_zero = temp.path().join(".run.json.run-state.tmp-0999999-1");
        crate::artifact_safety::write_private_fixture(&leading_zero, b"lookalike").unwrap();

        let guard = RunMutationGuard::acquire(temp.path())
            .expect("the next guard owner must reclaim a dead replacement temp");

        assert!(!stale.exists());
        assert_eq!(fs::read(&unrelated).unwrap(), b"unrelated");
        assert_eq!(fs::read(&leading_zero).unwrap(), b"lookalike");
        guard.validate().unwrap();
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
                true,
                true,
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
        let temp = private_temp();
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

        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            let before = fs::read(&path).unwrap();
            let error = RunMutationGuard::acquire(temp.path())
                .expect_err("broad stable lock must fail closed");
            assert!(error.to_string().contains("chmod 600"), "{error}");
            assert_eq!(fs::read(&path).unwrap(), before);
            assert_eq!(fs::symlink_metadata(&path).unwrap().mode() & 0o777, 0o644);
        }
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_or_replaced_lock_path_is_rejected_before_rename() {
        use std::os::unix::fs::symlink;

        for kind in ["symlink", "directory"] {
            let temp = private_temp();
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
        let error = publish_replacement_core(
            &guard,
            &target,
            b"replacement\n",
            None,
            |phase| {
                if phase == PublishPhase::BeforeRename {
                    fs::remove_file(&lock_path)?;
                    fs::write(&lock_path, b"replacement lock")?;
                }
                Ok(())
            },
            false,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("lock path changed")
                || error.to_string().contains("chmod 600"),
            "{error}"
        );
        assert_eq!(fs::read(target).unwrap(), old);

        let (temp, target, _old) = initialized_target();
        let outside = temp.path().join("outside-run");
        crate::artifact_safety::write_private_fixture(&outside, b"outside-unchanged\n").unwrap();
        let guard = RunMutationGuard::acquire(temp.path()).unwrap();
        let error = publish_replacement_core(
            &guard,
            &target,
            b"replacement\n",
            None,
            |phase| {
                if phase == PublishPhase::BeforeRename {
                    fs::remove_file(&target)?;
                    symlink(&outside, &target)?;
                }
                Ok(())
            },
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("regular file"), "{error}");
        assert_eq!(fs::read(outside).unwrap(), b"outside-unchanged\n");
    }

    #[cfg(unix)]
    #[test]
    fn pinned_run_directory_substitution_cannot_publish_or_cleanup_externally() {
        use std::os::unix::fs::symlink;

        let (temp, target, old) = initialized_target();
        let original_root = temp.path().to_path_buf();
        let parked = original_root.with_extension("parked-replacement");
        let outside = original_root.with_extension("outside-replacement");
        artifact_safety::create_private_directory(&outside).unwrap();
        fs::write(outside.join("state.json"), b"outside unchanged\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            outside.join("state.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let guard = RunMutationGuard::acquire(&original_root).unwrap();
        let error = publish_replacement_with_hooks(
            &guard,
            &target,
            b"replacement\n",
            || {
                fs::rename(&original_root, &parked)?;
                symlink(&outside, &original_root)?;
                Ok(())
            },
            || Ok(()),
        )
        .expect_err("replacement must reject substituted run directory");
        assert!(error.to_string().contains("directory"), "{error}");
        assert_eq!(fs::read(parked.join("state.json")).unwrap(), old);
        assert_eq!(
            fs::read(outside.join("state.json")).unwrap(),
            b"outside unchanged\n"
        );

        fs::remove_file(&original_root).unwrap();
        fs::rename(&parked, &original_root).unwrap();
        fs::remove_dir_all(outside).unwrap();

        let temp = private_temp();
        let original_root = temp.path().to_path_buf();
        let parked = original_root.with_extension("parked-create");
        let outside = original_root.with_extension("outside-create");
        artifact_safety::create_private_directory(&outside).unwrap();
        let target = original_root.join("run.json");
        let guard = RunMutationGuard::acquire(&original_root).unwrap();
        let error = publish_create_only_core(&guard, &target, b"initial\n", None, || {
            fs::rename(&original_root, &parked)?;
            symlink(&outside, &original_root)?;
            Ok(())
        })
        .expect_err("create-only must reject substituted run directory");
        assert!(error.to_string().contains("directory"), "{error}");
        assert!(!outside.join("run.json").exists());
        assert!(!parked.join("run.json").exists());
        fs::remove_file(&original_root).unwrap();
        fs::rename(&parked, &original_root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
