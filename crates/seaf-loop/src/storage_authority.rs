use std::path::Path;

use seaf_core::LoopRun;

use crate::{artifact_storage::StorageCommitment, immutable_artifact::read_verified_regular_file};

pub(crate) fn derive_active_storage_commitment(
    run_directory: &Path,
) -> Result<Option<StorageCommitment>, String> {
    let provider =
        crate::provider_exchange::derive_active_provider_storage_commitment(run_directory)
            .map_err(|error| error.to_string())?;
    let root = crate::artifact_safety::PinnedPrivateDirectory::open(run_directory)
        .map_err(|error| error.to_string())?;
    match root.entry_kind(std::ffi::OsStr::new("run.json")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(provider),
        Ok(crate::artifact_safety::PinnedEntryKind::RegularFile) => {}
        Ok(_) => return Err("run.json is not a real regular file".into()),
        Err(error) => return Err(error.to_string()),
    }
    let bytes = read_verified_regular_file(run_directory, "run.json", "storage authority run")
        .map_err(|error| error.to_string())?;
    let run = match parse_canonical_run(&bytes) {
        Ok(run) => run,
        Err(_) if !has_evaluation_artifact_namespace(&root)? => return Ok(provider),
        Err(error) => return Err(error),
    };
    combine(
        provider,
        crate::evaluation_storage::derive_active_evaluation_storage_commitment(
            run_directory,
            &run,
        )?,
    )
}

fn has_evaluation_artifact_namespace(
    root: &crate::artifact_safety::PinnedPrivateDirectory,
) -> Result<bool, String> {
    let artifacts = match root.open_child_directory(std::ffi::OsStr::new("artifacts")) {
        Ok(artifacts) => artifacts,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    let mut found = false;
    let mut entries = 0_usize;
    artifacts
        .for_each_entry_name(|name| {
            entries = entries.checked_add(1).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "evaluation namespace enumeration overflowed",
                )
            })?;
            if entries > crate::artifact_storage::RUN_TREE_ENTRY_CAP {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "evaluation namespace enumeration exceeds the run entry cap",
                ));
            }
            if name.to_str().is_some_and(|name| {
                name.starts_with("07-testing") || name.starts_with("08-eval-report")
            }) {
                found = true;
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    artifacts
        .validate_identity()
        .map_err(|error| error.to_string())?;
    Ok(found)
}

pub(crate) fn derive_storage_commitment_for_run_bytes(
    run_directory: &Path,
    bytes: &[u8],
) -> Result<Option<StorageCommitment>, String> {
    let provider = crate::provider_exchange::derive_provider_storage_commitment_for_run_bytes(
        run_directory,
        bytes,
    )
    .map_err(|error| error.to_string())?;
    let run = parse_canonical_run(bytes)?;
    combine(
        provider,
        crate::evaluation_storage::derive_active_evaluation_storage_commitment(
            run_directory,
            &run,
        )?,
    )
}

fn combine(
    provider: Option<StorageCommitment>,
    evaluation: Option<StorageCommitment>,
) -> Result<Option<StorageCommitment>, String> {
    match (provider, evaluation) {
        (Some(_), Some(_)) => Err(
            "provider and evaluation storage commitments cannot be active simultaneously".into(),
        ),
        (Some(commitment), None) | (None, Some(commitment)) => Ok(Some(commitment)),
        (None, None) => Ok(None),
    }
}

fn parse_canonical_run(bytes: &[u8]) -> Result<LoopRun, String> {
    let run: LoopRun = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let mut canonical = serde_json::to_vec_pretty(&run).map_err(|error| error.to_string())?;
    canonical.push(b'\n');
    if canonical != bytes {
        return Err("storage authority run is not canonical".into());
    }
    let errors = seaf_core::validate_loop_run(&run);
    if !errors.is_empty() {
        return Err(errors
            .into_iter()
            .map(|error| format!("{}: {}", error.field, error.message))
            .collect::<Vec<_>>()
            .join("; "));
    }
    Ok(run)
}
