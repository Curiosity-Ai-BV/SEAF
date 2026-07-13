use std::{collections::BTreeSet, ffi::OsStr, io, path::Path};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use crate::artifact_safety::{PinnedEntryKind, PinnedPrivateDirectory};

pub(crate) const RUN_TREE_BYTE_CAP: u64 = 32 * 1024 * 1024;
pub(crate) const RUN_TREE_ENTRY_CAP: usize = 4096;
pub(crate) const RUN_TREE_DIRECTORY_DEPTH_CAP: usize = 8;
const DEFAULT_ARTIFACT_BYTE_CAP: u64 = 2 * 1024 * 1024;
pub(crate) const PROVIDER_RESPONSE_BYTE_CAP: u64 = 1024 * 1024;
pub(crate) const PROVIDER_RECORD_BYTE_CAP: u64 = 64 * 1024;
const LOG_BYTE_CAP: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunTreeUsage {
    pub(crate) bytes: u64,
    pub(crate) entries: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StorageCommitment {
    pub(crate) permanent_bytes: u64,
    pub(crate) transient_bytes: u64,
    pub(crate) permanent_entries: usize,
    pub(crate) transient_entries: usize,
    pub(crate) consumable_permanent_paths: Vec<(String, u64)>,
    pub(crate) consumable_transient_path: Option<String>,
}

impl StorageCommitment {
    pub(crate) fn after_consuming_provider_slot(&self, relative_path: &str) -> io::Result<Self> {
        let mut remaining = self.clone();
        if let Some(position) = remaining
            .consumable_permanent_paths
            .iter()
            .position(|(path, _)| path == relative_path)
        {
            let (_, bytes) = remaining.consumable_permanent_paths.remove(position);
            remaining.permanent_bytes = remaining
                .permanent_bytes
                .checked_sub(bytes)
                .ok_or_else(|| invalid("storage commitment byte accounting underflowed"))?;
            remaining.permanent_entries = remaining
                .permanent_entries
                .checked_sub(1)
                .ok_or_else(|| invalid("storage commitment entry accounting underflowed"))?;
        } else if remaining.consumable_transient_path.as_deref() == Some(relative_path) {
            remaining.transient_bytes = 0;
            remaining.transient_entries = 0;
            remaining.consumable_transient_path = None;
        }
        Ok(remaining)
    }
}

pub(crate) fn validate_artifact_size(relative_path: &str, size: usize) -> io::Result<()> {
    let size = u64::try_from(size).map_err(|_| invalid("artifact size is not representable"))?;
    validate_artifact_size_u64(relative_path, size)
}

pub(crate) fn validate_artifact_size_u64(relative_path: &str, size: u64) -> io::Result<()> {
    let cap = artifact_byte_cap(relative_path);
    if size > cap {
        return Err(invalid(format!(
            "run artifact {relative_path} exceeds its {cap}-byte cap: {size} bytes"
        )));
    }
    Ok(())
}

pub(crate) fn validate_create_projection(
    root: &PinnedPrivateDirectory,
    relative_path: &str,
    size: usize,
) -> io::Result<()> {
    validate_artifact_size(relative_path, size)?;
    let usage = published_run_usage(root)?;
    let active = active_commitment(root)?;
    validate_create_with_commitment(usage, relative_path, size, &active)
}

pub(crate) fn validate_atomic_replacement_projection_with_old(
    root: &PinnedPrivateDirectory,
    relative_path: &str,
    new_size: usize,
    old_size: u64,
) -> io::Result<()> {
    validate_artifact_size(relative_path, new_size)?;
    let usage = published_run_usage(root)?;
    let active = active_commitment(root)?;
    validate_atomic_replacement_with_commitment(usage, relative_path, new_size, old_size, &active)
}

pub(crate) fn validate_atomic_replacement_projection_with_commitment(
    root: &PinnedPrivateDirectory,
    relative_path: &str,
    new_size: usize,
    old_size: u64,
    commitment: &StorageCommitment,
) -> io::Result<()> {
    validate_artifact_size(relative_path, new_size)?;
    let usage = published_run_usage(root)?;
    validate_atomic_replacement_with_commitment(
        usage,
        relative_path,
        new_size,
        old_size,
        commitment,
    )
}

fn validate_atomic_replacement_with_commitment(
    usage: RunTreeUsage,
    _relative_path: &str,
    new_size: usize,
    old_size: u64,
    active: &StorageCommitment,
) -> io::Result<()> {
    validate_bytes(
        usage.bytes,
        u64::try_from(new_size).map_err(|_| invalid("artifact size is not representable"))?,
        0,
        "current operation peak",
    )?;
    validate_entries(usage.entries, 1, 0, "current operation peak")?;
    let steady = usage
        .bytes
        .checked_sub(old_size)
        .and_then(|bytes| bytes.checked_add(u64::try_from(new_size).ok()?))
        .ok_or_else(|| invalid("run artifact byte projection overflowed"))?;
    validate_bytes(
        steady,
        active.permanent_bytes,
        active.transient_bytes,
        "post-operation commitment",
    )?;
    validate_entries(
        usage.entries,
        active.permanent_entries,
        active.transient_entries,
        "post-operation commitment",
    )
}

pub(crate) fn validate_existing_projection(
    root: &PinnedPrivateDirectory,
    relative_path: &str,
    size: u64,
) -> io::Result<()> {
    let size = usize::try_from(size).map_err(|_| invalid("artifact size is not representable"))?;
    validate_artifact_size(relative_path, size)?;
    let usage = published_run_usage(root)?;
    let active = active_commitment(root)?;
    validate_bytes(
        usage.bytes,
        active.permanent_bytes,
        active.transient_bytes,
        "existing artifact commitment",
    )?;
    validate_entries(
        usage.entries,
        active.permanent_entries,
        active.transient_entries,
        "existing artifact commitment",
    )
}

pub(crate) fn validate_entry_projection(
    root: &PinnedPrivateDirectory,
    additional: usize,
) -> io::Result<()> {
    let usage = published_run_usage(root)?;
    let active = active_commitment(root)?;
    validate_bytes(
        usage.bytes,
        active.permanent_bytes,
        active.transient_bytes,
        "entry publication commitment",
    )?;
    validate_entries(
        usage.entries,
        additional,
        active
            .permanent_entries
            .checked_add(active.transient_entries)
            .ok_or_else(|| invalid("storage commitment entry accounting overflowed"))?,
        "entry publication commitment",
    )
}

pub(crate) fn validate_usage_with_commitment(
    root: &PinnedPrivateDirectory,
    commitment: &StorageCommitment,
) -> io::Result<()> {
    let usage = published_run_usage(root)?;
    validate_bytes(
        usage.bytes,
        commitment.permanent_bytes,
        commitment.transient_bytes,
        "active storage commitment",
    )?;
    validate_entries(
        usage.entries,
        commitment.permanent_entries,
        commitment.transient_entries,
        "active storage commitment",
    )
}

pub(crate) fn validate_replacement_activating_commitment(
    root: &PinnedPrivateDirectory,
    relative_path: &str,
    new_size: usize,
    old_size: u64,
    commitment: &StorageCommitment,
) -> io::Result<()> {
    validate_artifact_size(relative_path, new_size)?;
    let usage = published_run_usage(root)?;
    validate_bytes(
        usage.bytes,
        u64::try_from(new_size).map_err(|_| invalid("artifact size is not representable"))?,
        0,
        "request-tail publication peak",
    )?;
    validate_entries(usage.entries, 1, 0, "request-tail publication peak")?;
    let steady = usage
        .bytes
        .checked_sub(old_size)
        .and_then(|bytes| bytes.checked_add(u64::try_from(new_size).ok()?))
        .ok_or_else(|| invalid("run artifact byte projection overflowed"))?;
    validate_bytes(
        steady,
        commitment.permanent_bytes,
        commitment.transient_bytes,
        "request-tail post-publication commitment",
    )?;
    validate_entries(
        usage.entries,
        commitment.permanent_entries,
        commitment.transient_entries,
        "request-tail post-publication commitment",
    )
}

pub(crate) struct ProviderRequestPrefixProjection<'a> {
    pub(crate) request_path: &'a str,
    pub(crate) request_size: usize,
    pub(crate) record_path: &'a str,
    pub(crate) record_size: usize,
    pub(crate) old_run_size: u64,
    pub(crate) new_run_size: usize,
    pub(crate) commitment: &'a StorageCommitment,
}

pub(crate) fn validate_provider_request_prefix_projection(
    root: &PinnedPrivateDirectory,
    projection: &ProviderRequestPrefixProjection<'_>,
) -> io::Result<()> {
    validate_artifact_size(projection.request_path, projection.request_size)?;
    validate_artifact_size(projection.record_path, projection.record_size)?;
    validate_artifact_size("run.json", projection.new_run_size)?;
    let usage = published_run_usage(root)?;
    let request_size = u64::try_from(projection.request_size)
        .map_err(|_| invalid("provider request size is not representable"))?;
    let record_size = u64::try_from(projection.record_size)
        .map_err(|_| invalid("provider record size is not representable"))?;
    let new_run_size = u64::try_from(projection.new_run_size)
        .map_err(|_| invalid("provider run size is not representable"))?;
    validate_bytes(
        usage.bytes,
        request_size
            .checked_add(record_size)
            .and_then(|bytes| bytes.checked_add(new_run_size))
            .ok_or_else(|| invalid("provider request prefix byte projection overflowed"))?,
        0,
        "provider request prefix publication peak",
    )?;
    validate_entries(
        usage.entries,
        3,
        0,
        "provider request prefix publication peak",
    )?;
    let steady = usage
        .bytes
        .checked_sub(projection.old_run_size)
        .and_then(|bytes| bytes.checked_add(request_size))
        .and_then(|bytes| bytes.checked_add(record_size))
        .and_then(|bytes| bytes.checked_add(new_run_size))
        .ok_or_else(|| invalid("provider request prefix byte projection overflowed"))?;
    validate_bytes(
        steady,
        projection.commitment.permanent_bytes,
        projection.commitment.transient_bytes,
        "provider request prefix post-publication commitment",
    )?;
    validate_entries(
        usage.entries,
        2_usize
            .checked_add(projection.commitment.permanent_entries)
            .ok_or_else(|| invalid("provider request entry projection overflowed"))?,
        projection.commitment.transient_entries,
        "provider request prefix post-publication commitment",
    )
}

pub(crate) fn validate_staged_provider_request_projection(
    root: &PinnedPrivateDirectory,
    record_path: &str,
    record_size: usize,
    adoption_run_size: usize,
    old_run_size: u64,
    commitment: &StorageCommitment,
) -> io::Result<()> {
    validate_artifact_size(record_path, record_size)?;
    validate_artifact_size("run.json", adoption_run_size)?;
    let usage = published_run_usage(root)?;
    validate_bytes(
        usage.bytes,
        u64::try_from(record_size)
            .map_err(|_| invalid("provider record size is not representable"))?,
        0,
        "staged provider request publication peak",
    )?;
    validate_entries(
        usage.entries,
        2,
        0,
        "staged provider request publication peak",
    )?;
    let record_size = u64::try_from(record_size)
        .map_err(|_| invalid("provider record size is not representable"))?;
    let adoption_run_size = u64::try_from(adoption_run_size)
        .map_err(|_| invalid("provider adoption run size is not representable"))?;
    let post_adoption = adoption_run_size
        .checked_add(commitment.permanent_bytes)
        .and_then(|bytes| bytes.checked_add(commitment.transient_bytes))
        .ok_or_else(|| invalid("staged provider commitment overflowed"))?
        .saturating_sub(old_run_size);
    let required = record_size
        .checked_add(adoption_run_size.max(post_adoption))
        .ok_or_else(|| invalid("staged provider commitment overflowed"))?;
    validate_bytes(
        usage.bytes,
        required,
        0,
        "staged provider request publication and commitment peak",
    )?;
    validate_entries(
        usage.entries,
        1_usize
            .checked_add(commitment.permanent_entries)
            .ok_or_else(|| invalid("staged provider entry projection overflowed"))?,
        commitment.transient_entries,
        "staged provider request post-publication commitment",
    )
}

pub(crate) fn validate_provider_slot_create_projection(
    root: &PinnedPrivateDirectory,
    relative_path: &str,
    size: usize,
    commitment: &StorageCommitment,
) -> io::Result<()> {
    validate_artifact_size(relative_path, size)?;
    let remaining = commitment.after_consuming_provider_slot(relative_path)?;
    if remaining == *commitment {
        return Err(invalid(format!(
            "provider publication does not consume the active commitment slot: {relative_path}"
        )));
    }
    let usage = published_run_usage(root)?;
    let size = u64::try_from(size).map_err(|_| invalid("artifact size is not representable"))?;
    validate_bytes(usage.bytes, size, 0, "provider slot publication peak")?;
    validate_entries(usage.entries, 2, 0, "provider slot publication peak")?;
    validate_bytes(
        usage.bytes,
        size,
        remaining
            .permanent_bytes
            .checked_add(remaining.transient_bytes)
            .ok_or_else(|| invalid("storage commitment byte accounting overflowed"))?,
        "provider slot post-publication commitment",
    )?;
    validate_entries(
        usage.entries,
        1,
        remaining
            .permanent_entries
            .checked_add(remaining.transient_entries)
            .ok_or_else(|| invalid("storage commitment entry accounting overflowed"))?,
        "provider slot post-publication commitment",
    )
}

pub(crate) fn validate_provider_slot_replacement_projection(
    root: &PinnedPrivateDirectory,
    relative_path: &str,
    new_size: usize,
    old_size: u64,
    commitment: &StorageCommitment,
) -> io::Result<()> {
    validate_artifact_size(relative_path, new_size)?;
    let remaining = commitment.after_consuming_provider_slot(relative_path)?;
    if remaining == *commitment {
        return Err(invalid(format!(
            "provider replacement does not consume the active commitment slot: {relative_path}"
        )));
    }
    let usage = published_run_usage(root)?;
    validate_bytes(
        usage.bytes,
        u64::try_from(new_size).map_err(|_| invalid("artifact size is not representable"))?,
        0,
        "provider replacement peak",
    )?;
    validate_entries(usage.entries, 1, 0, "provider replacement peak")?;
    let steady = usage
        .bytes
        .checked_sub(old_size)
        .and_then(|bytes| bytes.checked_add(u64::try_from(new_size).ok()?))
        .ok_or_else(|| invalid("run artifact byte projection overflowed"))?;
    validate_bytes(
        steady,
        remaining.permanent_bytes,
        remaining.transient_bytes,
        "provider replacement post-publication commitment",
    )?;
    validate_entries(
        usage.entries,
        remaining.permanent_entries,
        remaining.transient_entries,
        "provider replacement post-publication commitment",
    )
}

fn active_commitment(root: &PinnedPrivateDirectory) -> io::Result<StorageCommitment> {
    crate::provider_exchange::derive_active_provider_storage_commitment(root.path())
        .map(|commitment| commitment.unwrap_or_else(empty_commitment))
        .map_err(|error| invalid(format!("provider storage commitment is invalid: {error}")))
}

fn empty_commitment() -> StorageCommitment {
    StorageCommitment {
        permanent_bytes: 0,
        transient_bytes: 0,
        permanent_entries: 0,
        transient_entries: 0,
        consumable_permanent_paths: Vec::new(),
        consumable_transient_path: None,
    }
}

fn validate_create_with_commitment(
    usage: RunTreeUsage,
    relative_path: &str,
    size: usize,
    active: &StorageCommitment,
) -> io::Result<()> {
    let _ = relative_path;
    let size = u64::try_from(size).map_err(|_| invalid("artifact size is not representable"))?;
    validate_bytes(usage.bytes, size, 0, "current operation peak")?;
    validate_entries(usage.entries, 2, 0, "current operation peak")?;
    validate_bytes(
        usage.bytes,
        size,
        active
            .permanent_bytes
            .checked_add(active.transient_bytes)
            .ok_or_else(|| invalid("storage commitment byte accounting overflowed"))?,
        "post-operation commitment",
    )?;
    validate_entries(
        usage.entries,
        1,
        active
            .permanent_entries
            .checked_add(active.transient_entries)
            .ok_or_else(|| invalid("storage commitment entry accounting overflowed"))?,
        "post-operation commitment",
    )
}

fn validate_bytes(total: u64, first: u64, second: u64, phase: &str) -> io::Result<()> {
    let projected = total
        .checked_add(first)
        .and_then(|bytes| bytes.checked_add(second))
        .ok_or_else(|| invalid("run artifact byte projection overflowed"))?;
    if projected > RUN_TREE_BYTE_CAP {
        return Err(invalid(format!(
            "run artifact {phase} would exceed the {RUN_TREE_BYTE_CAP}-byte aggregate cap: {projected} bytes"
        )));
    }
    Ok(())
}

fn validate_entries(total: usize, first: usize, second: usize, phase: &str) -> io::Result<()> {
    let projected = total
        .checked_add(first)
        .and_then(|entries| entries.checked_add(second))
        .ok_or_else(|| invalid("run artifact entry projection overflowed"))?;
    if projected > RUN_TREE_ENTRY_CAP {
        return Err(invalid(format!(
            "run artifact {phase} would exceed the {RUN_TREE_ENTRY_CAP}-entry cap: {projected} entries"
        )));
    }
    Ok(())
}

pub(crate) fn artifact_byte_cap(relative_path: &str) -> u64 {
    let path = Path::new(relative_path);
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    let parent = path
        .parent()
        .and_then(Path::file_name)
        .and_then(OsStr::to_str);
    if relative_path == "log.md" || name.ends_with(".stdout.log") || name.ends_with(".stderr.log") {
        LOG_BYTE_CAP
    } else if parent == Some("responses") && name.ends_with(".response.json") {
        PROVIDER_RESPONSE_BYTE_CAP
    } else if parent == Some("artifacts") && name.ends_with(".record.json") {
        PROVIDER_RECORD_BYTE_CAP
    } else {
        DEFAULT_ARTIFACT_BYTE_CAP
    }
}

#[cfg(all(test, unix))]
pub(crate) fn published_run_bytes(run_directory: &Path) -> io::Result<u64> {
    let root = PinnedPrivateDirectory::open(run_directory)?;
    Ok(published_run_usage(&root)?.bytes)
}

pub(crate) fn published_run_usage(root: &PinnedPrivateDirectory) -> io::Result<RunTreeUsage> {
    let mut identities = BTreeSet::new();
    let mut entries = 0;
    let bytes = scan_directory(root, Path::new(""), 0, &mut entries, &mut identities)?;
    root.validate_identity()?;
    Ok(RunTreeUsage { bytes, entries })
}

fn scan_directory(
    directory: &PinnedPrivateDirectory,
    relative_directory: &Path,
    depth: usize,
    entries: &mut usize,
    identities: &mut BTreeSet<(u64, u64)>,
) -> io::Result<u64> {
    let mut total = 0_u64;
    directory.for_each_entry_name(|name| {
        *entries = entries
            .checked_add(1)
            .ok_or_else(|| invalid("run artifact entry count overflowed"))?;
        if *entries > RUN_TREE_ENTRY_CAP {
            return Err(invalid(format!(
                "run tree exceeds its {RUN_TREE_ENTRY_CAP}-entry cap"
            )));
        }
        match directory.entry_kind(name)? {
            PinnedEntryKind::Directory => {
                let child_depth = depth
                    .checked_add(1)
                    .ok_or_else(|| invalid("run artifact directory depth overflowed"))?;
                if child_depth > RUN_TREE_DIRECTORY_DEPTH_CAP {
                    return Err(invalid(format!(
                        "run tree exceeds its {RUN_TREE_DIRECTORY_DEPTH_CAP}-directory depth cap"
                    )));
                }
                let child = directory.open_child_directory(name)?;
                let relative_child = relative_directory.join(name);
                total = total
                    .checked_add(scan_directory(
                        &child,
                        &relative_child,
                        child_depth,
                        entries,
                        identities,
                    )?)
                    .ok_or_else(|| invalid("run artifact byte total overflowed"))?;
                child.validate_identity()?;
            }
            PinnedEntryKind::RegularFile => {
                let file = directory
                    .open_existing_file(name, true, false)
                    .map_err(|error| {
                        invalid(format!(
                            "run artifact must be a real 0600 regular file: {}: {error}",
                            directory.path().join(name).display()
                        ))
                    })?;
                let metadata = file.metadata()?;
                directory.validate_file(name, &metadata)?;
                let relative_path = classified_relative_path(relative_directory, name)?;
                validate_artifact_size(
                    &relative_path,
                    usize::try_from(metadata.len())
                        .map_err(|_| invalid("existing run artifact size is not representable"))?,
                )?;
                #[cfg(unix)]
                if identities.insert((metadata.dev(), metadata.ino())) {
                    total = total
                        .checked_add(metadata.len())
                        .ok_or_else(|| invalid("run artifact byte total overflowed"))?;
                }
            }
            PinnedEntryKind::Other => {
                return Err(invalid(format!(
                    "run artifact entry is neither a private directory nor a real regular file: {}",
                    directory.path().join(name).display()
                )));
            }
        }
        directory.validate_identity()?;
        Ok(())
    })?;
    Ok(total)
}

fn classified_relative_path(relative_directory: &Path, name: &OsStr) -> io::Result<String> {
    let name = name
        .to_str()
        .ok_or_else(|| invalid("run artifact name is not valid UTF-8"))?;
    let intended = name
        .strip_prefix('.')
        .and_then(|name| {
            name.split_once(".run-state.tmp-")
                .or_else(|| name.split_once(".tmp-"))
                .map(|(target, _)| target)
        })
        .filter(|target| !target.is_empty())
        .unwrap_or(name);
    let relative = relative_directory.join(intended);
    relative
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| invalid("run artifact path is not valid UTF-8"))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::{Arc, Barrier},
        thread,
    };

    use super::*;

    fn private_temp() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        temp
    }

    fn sparse_private(root: &Path, name: &str, size: u64) {
        let directory = crate::artifact_safety::PinnedPrivateDirectory::open(root).unwrap();
        let file = directory.create_file(OsStr::new(name)).unwrap();
        file.set_len(size).unwrap();
    }

    fn fill_to(root: &Path, total: u64) {
        let mut remaining = total;
        let mut index = 0;
        while remaining > 0 {
            let size = remaining.min(DEFAULT_ARTIFACT_BYTE_CAP);
            sparse_private(root, &format!("filler-{index:02}"), size);
            remaining -= size;
            index += 1;
        }
    }

    fn create_zero_byte_files(root: &Path, count: usize) {
        let directory = crate::artifact_safety::PinnedPrivateDirectory::open(root).unwrap();
        for index in 0..count {
            directory
                .create_file(OsStr::new(&format!("entry-{index:04}")))
                .unwrap();
        }
    }

    fn create_directory_chain(root: &Path, depth: usize) -> std::path::PathBuf {
        let mut current = root.to_path_buf();
        for index in 1..=depth {
            current.push(format!("level-{index:02}"));
            crate::artifact_safety::create_private_directory(&current).unwrap();
        }
        current
    }

    fn test_commitment(permanent_bytes: u64, permanent_entries: usize) -> StorageCommitment {
        StorageCommitment {
            permanent_bytes,
            transient_bytes: 100,
            permanent_entries,
            transient_entries: 1,
            consumable_permanent_paths: vec![(
                "responses/expected.response.json".to_string(),
                permanent_bytes,
            )],
            consumable_transient_path: Some("run.json".to_string()),
        }
    }

    #[test]
    fn provider_slot_consumption_rejects_inconsistent_byte_accounting_underflow() {
        let commitment = StorageCommitment {
            permanent_bytes: 0,
            transient_bytes: 0,
            permanent_entries: 1,
            transient_entries: 0,
            consumable_permanent_paths: vec![("responses/slot.response.json".to_string(), 1)],
            consumable_transient_path: None,
        };
        let error = commitment
            .after_consuming_provider_slot("responses/slot.response.json")
            .expect_err("inconsistent commitment bytes must fail checked subtraction");
        assert!(error.to_string().contains("underflow"), "{error}");
    }

    #[test]
    fn provider_slot_consumption_rejects_inconsistent_entry_accounting_underflow() {
        let commitment = StorageCommitment {
            permanent_bytes: 1,
            transient_bytes: 0,
            permanent_entries: 0,
            transient_entries: 0,
            consumable_permanent_paths: vec![("responses/slot.response.json".to_string(), 1)],
            consumable_transient_path: None,
        };
        let error = commitment
            .after_consuming_provider_slot("responses/slot.response.json")
            .expect_err("inconsistent commitment entries must fail checked subtraction");
        assert!(error.to_string().contains("underflow"), "{error}");
    }

    #[test]
    fn staged_request_projection_rejects_commitment_arithmetic_overflow() {
        let temp = private_temp();
        let commitment = StorageCommitment {
            permanent_bytes: u64::MAX,
            transient_bytes: 1,
            permanent_entries: 0,
            transient_entries: 1,
            consumable_permanent_paths: Vec::new(),
            consumable_transient_path: Some("run.json".to_string()),
        };
        let error = validate_staged_provider_request_projection(
            &PinnedPrivateDirectory::open(temp.path()).unwrap(),
            "artifacts/request.record.json",
            1,
            1,
            1,
            &commitment,
        )
        .expect_err("commitment overflow must fail before publication");
        assert!(error.to_string().contains("overflow"), "{error}");
    }

    #[test]
    fn staged_request_projection_rejects_entry_accounting_overflow() {
        let temp = private_temp();
        let commitment = StorageCommitment {
            permanent_bytes: 0,
            transient_bytes: 0,
            permanent_entries: usize::MAX,
            transient_entries: 1,
            consumable_permanent_paths: Vec::new(),
            consumable_transient_path: Some("run.json".to_string()),
        };
        let error = validate_staged_provider_request_projection(
            &PinnedPrivateDirectory::open(temp.path()).unwrap(),
            "artifacts/request.record.json",
            1,
            1,
            1,
            &commitment,
        )
        .expect_err("entry commitment overflow must fail before publication");
        assert!(error.to_string().contains("overflow"), "{error}");
    }

    #[test]
    fn provider_request_prefix_accepts_exact_byte_headroom_and_rejects_one_byte_less() {
        let commitment = test_commitment(1_000, 1);
        let exact = private_temp();
        fill_to(exact.path(), RUN_TREE_BYTE_CAP - 1_400);
        validate_provider_request_prefix_projection(
            &PinnedPrivateDirectory::open(exact.path()).unwrap(),
            &ProviderRequestPrefixProjection {
                request_path: "prompts/request.md",
                request_size: 100,
                record_path: "artifacts/request.record.json",
                record_size: 100,
                old_run_size: 0,
                new_run_size: 100,
                commitment: &commitment,
            },
        )
        .expect("exact committed byte headroom");

        let short = private_temp();
        fill_to(short.path(), RUN_TREE_BYTE_CAP - 1_399);
        let error = validate_provider_request_prefix_projection(
            &PinnedPrivateDirectory::open(short.path()).unwrap(),
            &ProviderRequestPrefixProjection {
                request_path: "prompts/request.md",
                request_size: 100,
                record_path: "artifacts/request.record.json",
                record_size: 100,
                old_run_size: 0,
                new_run_size: 100,
                commitment: &commitment,
            },
        )
        .expect_err("one missing committed byte must fail");
        assert!(error.to_string().contains("post-publication"), "{error}");
    }

    #[test]
    fn provider_request_prefix_accepts_exact_entry_headroom_and_rejects_one_entry_less() {
        let commitment = test_commitment(0, 1);
        let exact = private_temp();
        create_zero_byte_files(exact.path(), RUN_TREE_ENTRY_CAP - 4);
        validate_provider_request_prefix_projection(
            &PinnedPrivateDirectory::open(exact.path()).unwrap(),
            &ProviderRequestPrefixProjection {
                request_path: "prompts/request.md",
                request_size: 0,
                record_path: "artifacts/request.record.json",
                record_size: 0,
                old_run_size: 0,
                new_run_size: 0,
                commitment: &commitment,
            },
        )
        .expect("exact committed entry headroom");

        let short = private_temp();
        create_zero_byte_files(short.path(), RUN_TREE_ENTRY_CAP - 3);
        let error = validate_provider_request_prefix_projection(
            &PinnedPrivateDirectory::open(short.path()).unwrap(),
            &ProviderRequestPrefixProjection {
                request_path: "prompts/request.md",
                request_size: 0,
                record_path: "artifacts/request.record.json",
                record_size: 0,
                old_run_size: 0,
                new_run_size: 0,
                commitment: &commitment,
            },
        )
        .expect_err("one missing committed entry must fail");
        assert!(error.to_string().contains("post-publication"), "{error}");
    }

    #[test]
    fn path_policy_accepts_exact_caps_and_rejects_one_extra_byte() {
        for (path, cap) in [
            (
                "prompts/01-research.attempt-001.exchange-001.initial.request.md",
                2 * 1024 * 1024,
            ),
            ("responses/01-research.attempt-001.md", 2 * 1024 * 1024),
            (
                "responses/01-research.attempt-001.exchange-001.initial.response.json",
                1024 * 1024,
            ),
            (
                "artifacts/01-research.attempt-001.exchange-001.initial.request.record.json",
                64 * 1024,
            ),
            (
                "artifacts/01-research.attempt-001.exchange-001.initial.response.record.json",
                64 * 1024,
            ),
            ("artifacts/07-testing.check-001.stdout.log", 1024 * 1024),
            ("artifacts/07-testing.check-001.stderr.log", 1024 * 1024),
            ("log.md", 1024 * 1024),
            ("inputs/eval-config.json", 2 * 1024 * 1024),
            ("run.json", 2 * 1024 * 1024),
            ("artifacts/evidence.json", 2 * 1024 * 1024),
        ] {
            validate_artifact_size(path, cap).unwrap_or_else(|error| panic!("{path}: {error}"));
            let error = validate_artifact_size(path, cap + 1).expect_err(path);
            assert!(error.to_string().contains("byte cap"), "{path}: {error}");
        }
    }

    #[test]
    fn aggregate_counts_unique_regular_inodes_and_orphan_temps() {
        let temp = private_temp();
        crate::artifact_safety::write_private_fixture(temp.path().join("first"), b"1234").unwrap();
        fs::hard_link(temp.path().join("first"), temp.path().join("second")).unwrap();
        crate::artifact_safety::write_private_fixture(
            temp.path().join(".orphan.run-state.tmp-1-1"),
            b"123",
        )
        .unwrap();
        assert_eq!(published_run_bytes(temp.path()).unwrap(), 7);
        assert_eq!(published_run_bytes(temp.path()).unwrap(), 7);
    }

    #[test]
    fn aggregate_accepts_exact_entry_limit_and_rejects_limit_plus_one() {
        let exact = private_temp();
        create_zero_byte_files(exact.path(), RUN_TREE_ENTRY_CAP);
        assert_eq!(published_run_bytes(exact.path()).unwrap(), 0);

        let oversized = private_temp();
        create_zero_byte_files(oversized.path(), RUN_TREE_ENTRY_CAP + 1);
        let error = published_run_bytes(oversized.path())
            .expect_err("entry limit plus one must fail before unbounded accounting");
        assert!(error.to_string().contains("entry cap"), "{error}");
    }

    #[test]
    fn aggregate_accepts_exact_directory_depth_and_rejects_depth_plus_one() {
        let exact = private_temp();
        let exact_leaf = create_directory_chain(exact.path(), RUN_TREE_DIRECTORY_DEPTH_CAP);
        crate::artifact_safety::write_private_fixture(exact_leaf.join("evidence"), b"x").unwrap();
        assert_eq!(published_run_bytes(exact.path()).unwrap(), 1);

        let oversized = private_temp();
        create_directory_chain(oversized.path(), RUN_TREE_DIRECTORY_DEPTH_CAP + 1);
        let error = published_run_bytes(oversized.path())
            .expect_err("directory depth limit plus one must fail before unbounded recursion");
        assert!(error.to_string().contains("depth cap"), "{error}");
    }

    #[test]
    fn aggregate_rejects_nonregular_entries() {
        let temp = private_temp();
        std::os::unix::fs::symlink("outside", temp.path().join("unsafe")).unwrap();
        let error = published_run_bytes(temp.path()).expect_err("symlink must fail closed");
        assert!(error.to_string().contains("regular file"), "{error}");
    }

    #[test]
    fn aggregate_rejects_a_fifo_without_opening_or_blocking_on_it() {
        use std::os::unix::ffi::OsStrExt;

        let temp = private_temp();
        let path = temp.path().join("unsafe-fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a valid NUL-terminated path and mode is explicit.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let error = published_run_bytes(temp.path()).expect_err("FIFO must fail closed");
        assert!(error.to_string().contains("regular file"), "{error}");
    }

    #[test]
    fn exact_retry_at_aggregate_cap_succeeds_but_unrelated_extra_byte_blocks_it() {
        let temp = private_temp();
        fill_to(temp.path(), RUN_TREE_BYTE_CAP);
        let exact = vec![0; DEFAULT_ARTIFACT_BYTE_CAP as usize];
        crate::immutable_artifact::publish_create_only(temp.path(), "filler-00", &exact)
            .expect("exact immutable retry at the aggregate cap");

        sparse_private(temp.path(), "unrelated-extra", 1);
        let before = fs::metadata(temp.path().join("filler-00")).unwrap().len();
        let error =
            crate::immutable_artifact::publish_create_only(temp.path(), "filler-00", &exact)
                .expect_err("over-cap tree must fail closed even for an exact retry");
        assert!(error.to_string().contains("aggregate cap"), "{error}");
        assert_eq!(
            fs::metadata(temp.path().join("filler-00")).unwrap().len(),
            before
        );
    }

    #[test]
    fn existing_oversized_artifact_blocks_an_unrelated_target() {
        let temp = private_temp();
        sparse_private(
            temp.path(),
            "oversized-evidence",
            DEFAULT_ARTIFACT_BYTE_CAP + 1,
        );
        let target = temp.path().join("unrelated");
        let error =
            crate::immutable_artifact::publish_create_only(temp.path(), "unrelated", b"safe")
                .expect_err("existing per-artifact violation must block publication");
        assert!(error.to_string().contains("byte cap"), "{error}");
        assert!(!target.exists());
    }

    #[test]
    fn cap_rejection_happens_before_target_creation_or_replacement() {
        let temp = private_temp();
        crate::artifact_safety::create_private_directory(&temp.path().join("responses")).unwrap();
        let relative = "responses/01-research.attempt-001.exchange-001.initial.response.json";
        let target = temp.path().join(relative);
        let oversized = vec![0; PROVIDER_RESPONSE_BYTE_CAP as usize + 1];
        let error =
            crate::immutable_artifact::publish_create_only(temp.path(), relative, &oversized)
                .expect_err("response cap+1 must reject before publication");
        assert!(error.to_string().contains("byte cap"), "{error}");
        assert!(!target.exists());
    }

    #[test]
    fn atomic_replacement_rejects_when_the_synced_temp_would_exceed_the_cap() {
        let temp = private_temp();
        fill_to(temp.path(), RUN_TREE_BYTE_CAP);
        let guard = crate::run_persistence::RunMutationGuard::acquire(temp.path()).unwrap();
        let target = temp.path().join("filler-00");
        let before = fs::metadata(&target).unwrap().len();
        let error =
            crate::immutable_artifact::publish_mutable_with_guard(&guard, "filler-00", b"smaller")
                .expect_err("atomic replacement must budget the coexisting temp inode");
        assert!(error.to_string().contains("aggregate cap"), "{error}");
        assert_eq!(fs::metadata(target).unwrap().len(), before);
    }

    #[test]
    fn concurrent_publishers_cannot_oversubscribe_the_aggregate_cap() {
        let temp = private_temp();
        fill_to(temp.path(), RUN_TREE_BYTE_CAP - 1);
        let root = temp.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for name in ["winner-a", "winner-b"] {
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                crate::immutable_artifact::publish_create_only(&root, name, b"x")
            }));
        }
        barrier.wait();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        assert_eq!(published_run_bytes(temp.path()).unwrap(), RUN_TREE_BYTE_CAP);
    }
}
