use std::{
    error::Error,
    ffi::{OsStr, OsString},
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    artifact_safety::{PinnedEntryKind, PinnedPrivateDirectory},
    artifact_storage,
    run_persistence::{RunMutationGuard, RunPersistenceError, RUN_MUTATION_LOCK_FILE},
    state, LoopWorkspace,
};

const INTENT_FILE: &str = ".retention-purge.intent.json";
const RESULT_FILE: &str = ".retention-purge.result.json";
const RESULT_TEMP_FILE: &str = ".retention-purge.result.tmp";
const CONTROL_FILE_BYTE_CAP: usize = 2 * 1024 * 1024;
const RESULT_FILE_BYTE_CAP: usize = 8 * 1024 * 1024;
const MAX_MANAGED_RUNS: usize = 4096;
const MAX_RETENTION_CONTROL_ENTRIES: usize = 4;
const MIGRATION_RESULT_FILE: &str = "migration-v0-v1.result.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PurgeMode {
    DryRun,
    Apply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionPolicy {
    pub max_managed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurgeRunSummary {
    pub run_id: String,
    pub status: seaf_core::LoopStatus,
    pub updated_at: String,
    pub bytes: u64,
    pub run_digest: String,
    pub tree_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurgeReport {
    pub schema_version: u32,
    pub mode: PurgeMode,
    pub policy: RetentionPolicy,
    pub decision: PurgeDecisionEvidence,
    pub projected_managed_bytes_after: u64,
    pub deleted: Vec<PurgeRunSummary>,
    pub converged: Option<PurgeStateSnapshot>,
    pub intent_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_audit_digest: Option<String>,
    #[serde(
        default,
        serialize_with = "serialize_audit_path",
        deserialize_with = "deserialize_audit_path"
    )]
    pub audit_path: Option<PathBuf>,
    pub audit_digest: String,
    #[serde(default)]
    pub continuation_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurgeDecisionEvidence {
    pub snapshot: PurgeStateSnapshot,
    pub selected: Vec<PurgeRunSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurgeStateSnapshot {
    pub inventory_digest: String,
    pub managed_bytes: u64,
    pub protected_active: Vec<String>,
    pub protected_locked: Vec<String>,
    pub protected_migration_evidence: Vec<String>,
    pub excluded_root_entries: Vec<String>,
    pub control_state: PurgeControlState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurgeControlState {
    pub intent_present: bool,
    pub result_present: bool,
    pub result_temp_present: bool,
    pub tombstones: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PurgeIntent {
    schema_version: u32,
    policy: RetentionPolicy,
    decision: PurgeDecisionEvidence,
    projected_managed_bytes_after: u64,
    selected: Vec<IntentRun>,
    prior_audit_digest: Option<String>,
    intent_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentRun {
    summary: PurgeRunSummary,
    directory_device: u64,
    directory_inode: u64,
    tombstone_name: String,
    tree_manifest: Vec<IntentManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentManifestEntry {
    entry_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunTreeManifestEntry {
    path: String,
    kind: ManifestEntryKind,
    bytes: u64,
    digest: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ManifestEntryKind {
    Directory,
    File,
}

#[derive(Debug)]
struct ManagedRun {
    summary: PurgeRunSummary,
    identity: fs::Metadata,
    tree_manifest: Vec<RunTreeManifestEntry>,
    guard: Option<RunMutationGuard>,
    protection: Protection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protection {
    Eligible,
    Active,
    Locked,
    MigrationEvidence,
}

#[derive(Debug)]
struct Inventory {
    runs: Vec<ManagedRun>,
    excluded_root_entries: Vec<String>,
    control_state: PurgeControlState,
    total_bytes: u64,
    digest: String,
}

pub fn purge_loop_runs(
    runs_root: &Path,
    policy: RetentionPolicy,
    mode: PurgeMode,
) -> Result<PurgeReport, RetentionError> {
    let root = PinnedPrivateDirectory::open_parent(runs_root)
        .map_err(|error| RetentionError::context("could not pin runs root", error))?;
    root.validate_identity()?;

    if mode == PurgeMode::DryRun {
        return build_dry_run(&root, policy);
    }

    loop {
        let report = purge_apply_batch(&root, policy)?;
        if !report.continuation_required {
            return Ok(report);
        }
    }
}

fn purge_apply_batch(
    root: &PinnedPrivateDirectory,
    policy: RetentionPolicy,
) -> Result<PurgeReport, RetentionError> {
    match root.entry_kind(OsStr::new(INTENT_FILE)) {
        Ok(PinnedEntryKind::RegularFile) => {
            let intent = load_verified_intent(root)?;
            if intent.policy != policy {
                return Err(RetentionError::new(format!(
                    "a purge intent is pending for max-managed-bytes {}; retry with the same policy",
                    intent.policy.max_managed_bytes
                )));
            }
            return resume_intent(root, intent);
        }
        Ok(_) => {
            return Err(RetentionError::new(
                "purge intent path is not a regular file".to_string(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let mut prior_result = None;
    if let Some(result) = load_optional_verified_result(root)? {
        if result.policy == policy {
            let current = inventory_runs(root, false)?;
            if result.converged.as_ref() == Some(&snapshot(&current))
                && !result.continuation_required
            {
                return Ok(result);
            }
            if result.continuation_required {
                prior_result = Some(result);
            }
        }
    }

    start_purge_batch(root, policy, prior_result)
}

fn start_purge_batch(
    root: &PinnedPrivateDirectory,
    policy: RetentionPolicy,
    prior_result: Option<PurgeReport>,
) -> Result<PurgeReport, RetentionError> {
    let mut inventory = inventory_runs(root, true)?;
    validate_new_operation_control_state(&inventory.control_state)?;
    let mut selected_indexes = selected_indexes(&inventory, policy)?;
    selected_indexes.truncate(1);
    let selected = selected_indexes
        .iter()
        .map(|index| intent_run(&inventory.runs[*index]))
        .collect::<Result<Vec<_>, _>>()?;
    let projected_managed_bytes_after =
        selected
            .iter()
            .try_fold(inventory.total_bytes, |remaining, run| {
                remaining.checked_sub(run.summary.bytes).ok_or_else(|| {
                    RetentionError::new("purge byte projection underflowed".to_string())
                })
            })?;
    let mut intent = PurgeIntent {
        schema_version: 1,
        policy,
        decision: PurgeDecisionEvidence {
            snapshot: snapshot(&inventory),
            selected: selected.iter().map(|run| run.summary.clone()).collect(),
        },
        projected_managed_bytes_after,
        selected,
        prior_audit_digest: prior_result
            .as_ref()
            .map(|result| result.audit_digest.clone()),
        intent_digest: String::new(),
    };
    intent.intent_digest = digest_with_empty_field(&intent)?;
    test_mutate_after_inventory(root, &intent.selected)?;
    for index in &selected_indexes {
        validate_selected_snapshot_before_intent(root, &inventory.runs[*index])?;
    }
    for selected in &intent.selected {
        match root.entry_kind(OsStr::new(&selected.tombstone_name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(RetentionError::new(format!(
                    "purge tombstone already exists without a pending intent: {}",
                    selected.tombstone_name
                )))
            }
            Err(error) => return Err(error.into()),
        }
    }
    publish_create_only(
        root,
        OsStr::new(INTENT_FILE),
        &canonical_intent_bytes(&intent)?,
    )?;

    for (index, run) in inventory.runs.iter_mut().enumerate() {
        if !selected_indexes.contains(&index) {
            run.guard.take();
        }
    }

    for index in selected_indexes {
        let selected = intent
            .selected
            .iter()
            .find(|selected| selected.summary.run_id == inventory.runs[index].summary.run_id)
            .ok_or_else(|| RetentionError::new("selected run disappeared from intent"))?;
        let guard = inventory.runs[index]
            .guard
            .take()
            .ok_or_else(|| RetentionError::new("selected run guard was not retained"))?;
        delete_selected_run(root, selected, Some(guard))?;
    }
    resume_intent(root, intent)
}

fn validate_selected_snapshot_before_intent(
    root: &PinnedPrivateDirectory,
    run: &ManagedRun,
) -> Result<(), RetentionError> {
    let guard = run
        .guard
        .as_ref()
        .ok_or_else(|| RetentionError::new("selected run is missing its retained guard"))?;
    let name = OsStr::new(&run.summary.run_id);
    guard.validate_at_child(root, name)?;
    let directory = root.open_child_directory(name)?;
    root.validate_child_directory(name, &run.identity)?;
    let usage = artifact_storage::published_run_usage(&directory)?;
    let manifest = manifest_tree(&directory)?;
    if usage.bytes != run.summary.bytes || manifest != run.tree_manifest {
        return Err(RetentionError::new(format!(
            "selected run tree changed before purge intent publication: {}",
            run.summary.run_id
        )));
    }
    guard.validate_at_child(root, name)?;
    Ok(())
}

pub fn load_verified_purge_result(runs_root: &Path) -> Result<PurgeReport, RetentionError> {
    let root = PinnedPrivateDirectory::open_parent(runs_root)
        .map_err(|error| RetentionError::context("could not pin runs root", error))?;
    load_optional_verified_result(&root)?.ok_or_else(|| {
        RetentionError::new(format!(
            "purge audit result is missing: {}",
            runs_root.join(RESULT_FILE).display()
        ))
    })
}

fn build_dry_run(
    root: &PinnedPrivateDirectory,
    policy: RetentionPolicy,
) -> Result<PurgeReport, RetentionError> {
    let inventory = inventory_runs(root, false)?;
    let selected_indexes = selected_indexes(&inventory, policy)?;
    let selected = selected_indexes
        .iter()
        .map(|index| inventory.runs[*index].summary.clone())
        .collect::<Vec<_>>();
    let managed_bytes_after =
        selected
            .iter()
            .try_fold(inventory.total_bytes, |remaining, run| {
                remaining.checked_sub(run.bytes).ok_or_else(|| {
                    RetentionError::new("dry-run byte projection underflowed".to_string())
                })
            })?;
    let mut report = PurgeReport {
        schema_version: 1,
        mode: PurgeMode::DryRun,
        policy,
        decision: PurgeDecisionEvidence {
            snapshot: snapshot(&inventory),
            selected,
        },
        projected_managed_bytes_after: managed_bytes_after,
        deleted: Vec::new(),
        converged: None,
        intent_digest: None,
        prior_audit_digest: None,
        audit_path: None,
        audit_digest: String::new(),
        continuation_required: false,
    };
    report.audit_digest = digest_report(&report)?;
    Ok(report)
}

fn resume_intent(
    root: &PinnedPrivateDirectory,
    intent: PurgeIntent,
) -> Result<PurgeReport, RetentionError> {
    if let Some(existing) = load_optional_verified_result(root)? {
        if result_completes_intent(&existing, &intent)? {
            remove_control_file(root, OsStr::new(INTENT_FILE))?;
            let current = inventory_runs(root, false)?;
            if existing.converged.as_ref() == Some(&snapshot(&current)) {
                return Ok(existing);
            }
            return start_purge_batch(root, intent.policy, Some(existing));
        }
    }
    for selected in &intent.selected {
        delete_selected_run(root, selected, None)?;
    }

    let after = inventory_runs(root, false)?;
    let selected = intent
        .selected
        .iter()
        .map(|run| run.summary.clone())
        .collect::<Vec<_>>();
    let result_path = root.path().join(RESULT_FILE);
    let mut converged = snapshot(&after);
    converged.control_state = expected_converged_control_state();
    let prior = match (
        &intent.prior_audit_digest,
        load_optional_verified_result(root)?,
    ) {
        (Some(expected), Some(result)) if result.audit_digest == *expected => Some(result),
        (Some(_), _) => {
            return Err(RetentionError::new(
                "purge batch prior audit does not match its intent".to_string(),
            ))
        }
        (None, _) => None,
    };
    let mut deleted = prior
        .as_ref()
        .map(|report| report.deleted.clone())
        .unwrap_or_default();
    deleted.extend(selected);
    let continuation_required = after.total_bytes > intent.policy.max_managed_bytes
        && deleted.len() < MAX_MANAGED_RUNS
        && !selected_indexes(&after, intent.policy)?.is_empty();
    let mut report = PurgeReport {
        schema_version: 1,
        mode: PurgeMode::Apply,
        policy: intent.policy,
        decision: intent.decision.clone(),
        projected_managed_bytes_after: intent.projected_managed_bytes_after,
        deleted,
        converged: Some(converged),
        intent_digest: Some(intent.intent_digest.clone()),
        prior_audit_digest: intent.prior_audit_digest.clone(),
        audit_path: Some(result_path),
        audit_digest: String::new(),
        continuation_required,
    };
    report.audit_digest = digest_report(&report)?;
    publish_replace(
        root,
        OsStr::new(RESULT_TEMP_FILE),
        OsStr::new(RESULT_FILE),
        &canonical_report_bytes(&report)?,
    )?;
    test_interrupt_after_result_rename()?;
    remove_control_file(root, OsStr::new(INTENT_FILE))?;
    let actual = inventory_runs(root, false)?;
    if report.converged.as_ref() != Some(&snapshot(&actual)) {
        return Err(RetentionError::new(
            "purge converged state does not match the durable audit result".to_string(),
        ));
    }
    Ok(report)
}

fn result_completes_intent(
    result: &PurgeReport,
    intent: &PurgeIntent,
) -> Result<bool, RetentionError> {
    if result.intent_digest.as_deref() != Some(&intent.intent_digest) {
        return Ok(false);
    }
    let selected = intent
        .selected
        .iter()
        .map(|run| run.summary.clone())
        .collect::<Vec<_>>();
    let suffix_matches = result.deleted.ends_with(&selected);
    let exact_first_batch = intent.prior_audit_digest.is_some() || result.deleted == selected;
    let unique_deleted = result
        .deleted
        .iter()
        .map(|run| run.run_id.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        == result.deleted.len();
    let bound = result.deleted.len() <= MAX_MANAGED_RUNS;
    let complete = result.policy == intent.policy
        && result.decision == intent.decision
        && result.projected_managed_bytes_after == intent.projected_managed_bytes_after
        && result.prior_audit_digest == intent.prior_audit_digest
        && result.converged.is_some()
        && suffix_matches
        && exact_first_batch
        && unique_deleted
        && bound;
    if !complete {
        return Err(RetentionError::new(
            "purge result names the pending intent but does not authenticate its completed batch"
                .to_string(),
        ));
    }
    Ok(true)
}

fn snapshot(inventory: &Inventory) -> PurgeStateSnapshot {
    let mut protected_active = Vec::new();
    let mut protected_locked = Vec::new();
    let mut protected_migration_evidence = Vec::new();
    for run in &inventory.runs {
        match run.protection {
            Protection::Active => protected_active.push(run.summary.run_id.clone()),
            Protection::Locked => protected_locked.push(run.summary.run_id.clone()),
            Protection::MigrationEvidence => {
                protected_migration_evidence.push(run.summary.run_id.clone())
            }
            Protection::Eligible => {}
        }
    }
    PurgeStateSnapshot {
        inventory_digest: inventory.digest.clone(),
        managed_bytes: inventory.total_bytes,
        protected_active,
        protected_locked,
        protected_migration_evidence,
        excluded_root_entries: inventory.excluded_root_entries.clone(),
        control_state: inventory.control_state.clone(),
    }
}

fn expected_converged_control_state() -> PurgeControlState {
    PurgeControlState {
        intent_present: false,
        result_present: true,
        result_temp_present: false,
        tombstones: Vec::new(),
    }
}

fn validate_new_operation_control_state(control: &PurgeControlState) -> Result<(), RetentionError> {
    if control.intent_present || control.result_temp_present || !control.tombstones.is_empty() {
        return Err(RetentionError::new(
            "retention control state contains an unowned intent, result temporary, or tombstone",
        ));
    }
    Ok(())
}

fn inventory_runs(
    root: &PinnedPrivateDirectory,
    retain_eligible_guards: bool,
) -> Result<Inventory, RetentionError> {
    let mut names = Vec::new();
    let mut operator_entries = 0_usize;
    let mut control_entries = 0_usize;
    root.for_each_entry_name(|name| {
        if is_retention_control_name(name) {
            control_entries = control_entries
                .checked_add(1)
                .ok_or_else(|| invalid_io("retention control entry count overflowed"))?;
            if control_entries > MAX_RETENTION_CONTROL_ENTRIES {
                return Err(invalid_io(format!(
                    "runs root exceeds its {MAX_RETENTION_CONTROL_ENTRIES}-entry SEAF retention-control allowance"
                )));
            }
        } else {
            operator_entries = operator_entries
                .checked_add(1)
                .ok_or_else(|| invalid_io("operator entry count overflowed"))?;
            if operator_entries > MAX_MANAGED_RUNS {
                return Err(invalid_io(format!(
                    "runs root exceeds its {MAX_MANAGED_RUNS}-entry operator inventory cap"
                )));
            }
        }
        names.push(name.to_os_string());
        Ok(())
    })?;
    sort_names(&mut names);

    let mut runs = Vec::new();
    let mut excluded_root_entries = Vec::new();
    let mut control_state = PurgeControlState {
        intent_present: false,
        result_present: false,
        result_temp_present: false,
        tombstones: Vec::new(),
    };
    let mut total_bytes = 0_u64;
    for name in names {
        let name_text = name.to_str().ok_or_else(|| {
            RetentionError::new("runs root contains a non-UTF-8 entry name".to_string())
        })?;
        if name_text == INTENT_FILE {
            control_state.intent_present = true;
            continue;
        }
        if name_text == RESULT_FILE {
            control_state.result_present = true;
            continue;
        }
        if name_text == RESULT_TEMP_FILE {
            control_state.result_temp_present = true;
            continue;
        }
        if name_text.starts_with(".retention-purge.") && name_text.ends_with(".deleting") {
            control_state.tombstones.push(name_text.to_string());
            continue;
        }
        if name_text.starts_with('.') {
            excluded_root_entries.push(name_text.to_string());
            continue;
        }
        validate_run_id(name_text)?;
        if root.entry_kind(&name)? != PinnedEntryKind::Directory {
            return Err(RetentionError::new(format!(
                "managed runs-root entry must be a private run directory: {name_text}"
            )));
        }
        let directory = root.open_child_directory(&name)?;
        let identity = directory.metadata()?;
        root.validate_child_directory(&name, &identity)?;
        let guard = match RunMutationGuard::try_acquire_existing(directory.path()) {
            Ok(guard) => Some(guard),
            Err(RunPersistenceError::Busy(_)) => None,
            Err(error) => return Err(error.into()),
        };
        let usage = artifact_storage::published_run_usage(&directory)?;
        total_bytes = total_bytes
            .checked_add(usage.bytes)
            .ok_or_else(|| RetentionError::new("managed byte total overflowed"))?;
        let tree_manifest = manifest_tree(&directory)?;
        let tree_digest = seaf_core::canonical_sha256_digest(&tree_manifest).map_err(|error| {
            RetentionError::context("could not digest run-tree manifest", error)
        })?;
        let workspace = LoopWorkspace::open_minimal(root.path(), name_text)?;
        let run = state::load_run(&workspace)?;
        if run.run_id != name_text {
            return Err(RetentionError::new(format!(
                "run directory {name_text} contains authority for {}",
                run.run_id
            )));
        }
        if let Some(guard) = &guard {
            guard.validate_at_child(root, &name)?;
        }
        let run_digest = seaf_core::canonical_sha256_digest(&run)
            .map_err(|error| RetentionError::context("could not digest run authority", error))?;
        let migration_evidence = matches!(
            directory.entry_kind(OsStr::new(MIGRATION_RESULT_FILE)),
            Ok(PinnedEntryKind::RegularFile)
        );
        let has_live_candidate = candidate_lifecycle_is_live(
            run.candidate_workspace
                .as_ref()
                .map(|candidate| candidate.lifecycle),
        );
        let protection = if !eligible_status(run.status) || has_live_candidate {
            Protection::Active
        } else if migration_evidence {
            Protection::MigrationEvidence
        } else if guard.is_none() {
            Protection::Locked
        } else {
            Protection::Eligible
        };
        runs.push(ManagedRun {
            summary: PurgeRunSummary {
                run_id: run.run_id,
                status: run.status,
                updated_at: run.updated_at,
                bytes: usage.bytes,
                run_digest,
                tree_digest,
            },
            identity,
            tree_manifest,
            guard: if retain_eligible_guards && protection == Protection::Eligible {
                guard
            } else {
                None
            },
            protection,
        });
    }
    runs.sort_by(|left, right| left.summary.run_id.cmp(&right.summary.run_id));
    excluded_root_entries.sort();
    control_state.tombstones.sort();
    let digest = inventory_digest(&runs, &excluded_root_entries, total_bytes)?;
    Ok(Inventory {
        runs,
        excluded_root_entries,
        control_state,
        total_bytes,
        digest,
    })
}

fn is_retention_control_name(name: &OsStr) -> bool {
    name == OsStr::new(INTENT_FILE)
        || name == OsStr::new(RESULT_FILE)
        || name == OsStr::new(RESULT_TEMP_FILE)
        || name.to_str().is_some_and(|name| {
            name.starts_with(".retention-purge.") && name.ends_with(".deleting")
        })
}

fn selected_indexes(
    inventory: &Inventory,
    policy: RetentionPolicy,
) -> Result<Vec<usize>, RetentionError> {
    let mut eligible = inventory
        .runs
        .iter()
        .enumerate()
        .filter(|(_, run)| run.protection == Protection::Eligible)
        .map(|(index, run)| {
            Ok((
                index,
                parse_canonical_unix_seconds(&run.summary.updated_at)?,
                run.summary.run_id.clone(),
            ))
        })
        .collect::<Result<Vec<_>, RetentionError>>()?;
    eligible.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.2.cmp(&right.2)));
    let mut remaining = inventory.total_bytes;
    let mut selected = Vec::new();
    for (index, _, _) in eligible {
        if remaining <= policy.max_managed_bytes {
            break;
        }
        remaining = remaining
            .checked_sub(inventory.runs[index].summary.bytes)
            .ok_or_else(|| RetentionError::new("selection byte accounting underflowed"))?;
        selected.push(index);
    }
    Ok(selected)
}

fn intent_run(run: &ManagedRun) -> Result<IntentRun, RetentionError> {
    #[cfg(unix)]
    {
        let directory_device = run.identity.dev();
        let directory_inode = run.identity.ino();
        Ok(IntentRun {
            summary: run.summary.clone(),
            directory_device,
            directory_inode,
            tombstone_name: tombstone_name_for(&run.summary, directory_device, directory_inode),
            tree_manifest: run
                .tree_manifest
                .iter()
                .map(intent_manifest_entry)
                .collect(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = run;
        Err(RetentionError::new(
            "retention purge requires Unix directory identities".to_string(),
        ))
    }
}

fn tombstone_name_for(
    summary: &PurgeRunSummary,
    directory_device: u64,
    directory_inode: u64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"seaf-retention-tombstone-v1\0");
    hasher.update((summary.run_id.len() as u64).to_be_bytes());
    hasher.update(summary.run_id.as_bytes());
    hasher.update(directory_device.to_be_bytes());
    hasher.update(directory_inode.to_be_bytes());
    hasher.update(summary.run_digest.as_bytes());
    hasher.update(summary.tree_digest.as_bytes());
    format!(
        ".retention-purge.tombstone-v1-{}.deleting",
        hex::encode(hasher.finalize())
    )
}

fn delete_selected_run(
    root: &PinnedPrivateDirectory,
    selected: &IntentRun,
    existing_guard: Option<RunMutationGuard>,
) -> Result<(), RetentionError> {
    let source_name = OsStr::new(&selected.summary.run_id);
    let tombstone_name = OsStr::new(&selected.tombstone_name);
    let (mut directory, mut name, already_tombstoned) =
        match root.open_child_directory(tombstone_name) {
            Ok(directory) => (directory, tombstone_name, true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match root.open_child_directory(source_name) {
                    Ok(directory) => (directory, source_name, false),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        };
    validate_intent_identity(&directory, selected)?;

    let guard = match existing_guard {
        Some(guard) => guard,
        None => match RunMutationGuard::try_acquire_existing(directory.path()) {
            Ok(guard) => guard,
            Err(RunPersistenceError::Busy(_)) => {
                return Err(RetentionError::new(format!(
                    "selected run became locked while purge intent was pending: {}",
                    selected.summary.run_id
                )))
            }
            Err(error) if missing_lock(&error) && directory_is_empty(&directory)? => {
                root.validate_child_directory(name, &directory.metadata()?)?;
                root.remove_child_directory_if_same(name, &directory)?;
                root.sync_all()?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        },
    };
    guard.validate_at_child(root, name)?;
    let current_manifest = manifest_tree(&directory)?;
    if already_tombstoned {
        validate_manifest_subset(&current_manifest, &selected.tree_manifest, selected)?;
    } else if current_manifest
        .iter()
        .map(intent_manifest_entry)
        .collect::<Vec<_>>()
        != selected.tree_manifest
    {
        return Err(RetentionError::new(format!(
            "selected run tree changed before purge tombstone publication: {}",
            selected.summary.run_id
        )));
    }
    validate_run_authority_if_present(root, &directory, &guard, name, selected)?;
    if !already_tombstoned {
        guard.validate_at_child(root, source_name)?;
        root.rename(source_name, tombstone_name)?;
        root.validate_child_directory(tombstone_name, &directory.metadata()?)?;
        root.sync_all()?;
        directory = root.open_child_directory(tombstone_name)?;
        validate_intent_identity(&directory, selected)?;
        name = tombstone_name;
    }
    guard.validate_at_child(root, name)?;
    remove_run_contents_except_authority(&directory, 0)?;
    guard.validate_at_child(root, name)?;
    remove_optional_regular_file(&directory, OsStr::new(crate::workspace::RUN_FILE))?;
    directory.sync_all()?;
    guard.remove_empty_locked_run_directory(root, name, &directory)?;
    Ok(())
}

fn validate_run_authority_if_present(
    root: &PinnedPrivateDirectory,
    directory: &PinnedPrivateDirectory,
    guard: &RunMutationGuard,
    name: &OsStr,
    selected: &IntentRun,
) -> Result<(), RetentionError> {
    match directory.entry_kind(OsStr::new(crate::workspace::RUN_FILE)) {
        Ok(PinnedEntryKind::RegularFile) => {
            let workspace = LoopWorkspace::open_staged_migration(directory.path())?;
            let run = state::load_run_before_provider_reconciliation(&workspace)?;
            if seaf_core::canonical_sha256_digest(&run).map_err(|error| {
                RetentionError::context("could not digest selected run authority", error)
            })? != selected.summary.run_digest
                || run.status != selected.summary.status
                || run.updated_at != selected.summary.updated_at
            {
                return Err(RetentionError::new(format!(
                    "selected run authority changed after purge intent: {}",
                    selected.summary.run_id
                )));
            }
            guard.validate_at_child(root, name)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(RetentionError::new(format!(
            "selected run authority is no longer a regular file: {}",
            selected.summary.run_id
        ))),
        Err(error) => Err(error.into()),
    }
}

fn validate_manifest_subset(
    current: &[RunTreeManifestEntry],
    intended: &[IntentManifestEntry],
    selected: &IntentRun,
) -> Result<(), RetentionError> {
    for entry in current {
        if !intended.contains(&intent_manifest_entry(entry)) {
            return Err(RetentionError::new(format!(
                "purge tombstone contains an entry not bound by the intent for {}: {}",
                selected.summary.run_id, entry.path
            )));
        }
    }
    Ok(())
}

fn intent_manifest_entry(entry: &RunTreeManifestEntry) -> IntentManifestEntry {
    let mut hasher = Sha256::new();
    hasher.update((entry.path.len() as u64).to_be_bytes());
    hasher.update(entry.path.as_bytes());
    hasher.update([match entry.kind {
        ManifestEntryKind::Directory => 0,
        ManifestEntryKind::File => 1,
    }]);
    hasher.update(entry.bytes.to_be_bytes());
    if let Some(digest) = &entry.digest {
        hasher.update((digest.len() as u64).to_be_bytes());
        hasher.update(digest.as_bytes());
    } else {
        hasher.update(0_u64.to_be_bytes());
    }
    IntentManifestEntry {
        entry_digest: hex::encode(hasher.finalize()),
    }
}

fn remove_run_contents_except_authority(
    directory: &PinnedPrivateDirectory,
    depth: usize,
) -> Result<(), RetentionError> {
    let mut names = bounded_names(directory)?;
    sort_names(&mut names);
    for name in names {
        if depth == 0
            && (name == OsStr::new(crate::workspace::RUN_FILE)
                || name == OsStr::new(RUN_MUTATION_LOCK_FILE))
        {
            continue;
        }
        match directory.entry_kind(&name)? {
            PinnedEntryKind::Directory => {
                let next_depth = depth
                    .checked_add(1)
                    .ok_or_else(|| RetentionError::new("purge depth overflowed"))?;
                if next_depth > artifact_storage::RUN_TREE_DIRECTORY_DEPTH_CAP {
                    return Err(RetentionError::new("purge tree exceeds the run depth cap"));
                }
                let child = directory.open_child_directory(&name)?;
                remove_run_contents_except_authority(&child, next_depth)?;
                directory.remove_child_directory_if_same(&name, &child)?;
            }
            PinnedEntryKind::RegularFile => remove_optional_regular_file(directory, &name)?,
            PinnedEntryKind::Other => {
                return Err(RetentionError::new(format!(
                    "purge refuses unsafe run-tree entry: {}",
                    directory.path().join(name).display()
                )))
            }
        }
        directory.validate_identity()?;
        test_interrupt_after_entry()?;
    }
    directory.sync_all()?;
    Ok(())
}

fn remove_optional_regular_file(
    directory: &PinnedPrivateDirectory,
    name: &OsStr,
) -> Result<(), RetentionError> {
    match directory.open_existing_file(name, false, false) {
        Ok(file) => {
            let identity = file.metadata()?;
            directory.unlink_if_same(name, &identity)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_intent_identity(
    directory: &PinnedPrivateDirectory,
    selected: &IntentRun,
) -> Result<(), RetentionError> {
    #[cfg(unix)]
    {
        let identity = directory.metadata()?;
        if identity.dev() != selected.directory_device || identity.ino() != selected.directory_inode
        {
            return Err(RetentionError::new(format!(
                "selected run directory identity changed after purge intent: {}",
                selected.summary.run_id
            )));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (directory, selected);
        Err(RetentionError::new(
            "retention purge requires Unix directory identities".to_string(),
        ))
    }
}

fn directory_is_empty(directory: &PinnedPrivateDirectory) -> Result<bool, RetentionError> {
    let mut empty = true;
    directory.for_each_entry_name(|_| {
        empty = false;
        Ok(())
    })?;
    Ok(empty)
}

fn missing_lock(error: &RunPersistenceError) -> bool {
    matches!(error, RunPersistenceError::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
}

fn eligible_status(status: seaf_core::LoopStatus) -> bool {
    matches!(
        status,
        seaf_core::LoopStatus::Passed | seaf_core::LoopStatus::Completed
    )
}

fn candidate_lifecycle_is_live(lifecycle: Option<seaf_core::CandidateWorkspaceLifecycle>) -> bool {
    lifecycle.is_some_and(|lifecycle| lifecycle != seaf_core::CandidateWorkspaceLifecycle::Cleaned)
}

fn parse_canonical_unix_seconds(value: &str) -> Result<u64, RetentionError> {
    let parsed = value.parse::<u64>().map_err(|_| {
        RetentionError::new(format!(
            "retention updated_at must be canonical decimal Unix seconds within u64: {value}"
        ))
    })?;
    if parsed.to_string() != value {
        return Err(RetentionError::new(format!(
            "retention updated_at must be canonical decimal Unix seconds within u64: {value}"
        )));
    }
    Ok(parsed)
}

fn manifest_tree(
    root: &PinnedPrivateDirectory,
) -> Result<Vec<RunTreeManifestEntry>, RetentionError> {
    let mut entries = 0_usize;
    let mut total_bytes = 0_u64;
    let mut manifest = Vec::new();
    manifest_directory(root, "", 0, &mut entries, &mut total_bytes, &mut manifest)?;
    root.validate_identity()?;
    Ok(manifest)
}

fn manifest_directory(
    directory: &PinnedPrivateDirectory,
    relative: &str,
    depth: usize,
    entries: &mut usize,
    total_bytes: &mut u64,
    manifest: &mut Vec<RunTreeManifestEntry>,
) -> Result<(), RetentionError> {
    let mut names = bounded_names(directory)?;
    sort_names(&mut names);
    for name in names {
        *entries = entries
            .checked_add(1)
            .ok_or_else(|| RetentionError::new("run-tree digest entry overflowed"))?;
        if *entries > artifact_storage::RUN_TREE_ENTRY_CAP {
            return Err(RetentionError::new("run-tree digest exceeds the entry cap"));
        }
        let name_text = name.to_str().ok_or_else(|| {
            RetentionError::new("run-tree manifest entry name is not UTF-8".to_string())
        })?;
        let path = if relative.is_empty() {
            name_text.to_string()
        } else {
            format!("{relative}/{name_text}")
        };
        match directory.entry_kind(&name)? {
            PinnedEntryKind::Directory => {
                let child_depth = depth
                    .checked_add(1)
                    .ok_or_else(|| RetentionError::new("run-tree digest depth overflowed"))?;
                if child_depth > artifact_storage::RUN_TREE_DIRECTORY_DEPTH_CAP {
                    return Err(RetentionError::new("run-tree digest exceeds the depth cap"));
                }
                manifest.push(RunTreeManifestEntry {
                    path: path.clone(),
                    kind: ManifestEntryKind::Directory,
                    bytes: 0,
                    digest: None,
                });
                let child = directory.open_child_directory(&name)?;
                manifest_directory(&child, &path, child_depth, entries, total_bytes, manifest)?;
            }
            PinnedEntryKind::RegularFile => {
                let mut file = directory.open_existing_file(&name, true, false)?;
                let identity = file.metadata()?;
                directory.validate_single_link_file(&name, &identity)?;
                artifact_storage::validate_artifact_size_u64(&path, identity.len())?;
                *total_bytes = total_bytes
                    .checked_add(identity.len())
                    .ok_or_else(|| RetentionError::new("run-tree manifest bytes overflowed"))?;
                if *total_bytes > artifact_storage::RUN_TREE_BYTE_CAP {
                    return Err(RetentionError::new(
                        "run-tree manifest exceeds the aggregate byte cap",
                    ));
                }
                let mut file_hasher = Sha256::new();
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let read = file.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    file_hasher.update(&buffer[..read]);
                }
                directory.validate_single_link_file(&name, &identity)?;
                manifest.push(RunTreeManifestEntry {
                    path,
                    kind: ManifestEntryKind::File,
                    bytes: identity.len(),
                    digest: Some(format!("sha256:{}", hex::encode(file_hasher.finalize()))),
                });
            }
            PinnedEntryKind::Other => {
                return Err(RetentionError::new(format!(
                    "run-tree digest refuses unsafe entry: {}",
                    directory.path().join(name).display()
                )))
            }
        }
    }
    Ok(())
}

fn bounded_names(directory: &PinnedPrivateDirectory) -> Result<Vec<OsString>, RetentionError> {
    let mut names = Vec::new();
    directory.for_each_entry_name(|name| {
        if names.len() >= artifact_storage::RUN_TREE_ENTRY_CAP {
            return Err(invalid_io(
                "directory enumeration exceeds the run entry cap",
            ));
        }
        names.push(name.to_os_string());
        Ok(())
    })?;
    Ok(names)
}

fn sort_names(names: &mut [OsString]) {
    #[cfg(unix)]
    names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    #[cfg(not(unix))]
    names.sort();
}

fn inventory_digest(
    runs: &[ManagedRun],
    _excluded: &[String],
    total_bytes: u64,
) -> Result<String, RetentionError> {
    #[derive(Serialize)]
    struct DigestInventory<'a> {
        total_bytes: u64,
        runs: Vec<(&'a PurgeRunSummary, &'static str)>,
    }
    let runs = runs
        .iter()
        .map(|run| {
            let protection = match run.protection {
                Protection::Eligible => "eligible",
                Protection::Active => "active",
                Protection::Locked => "locked",
                Protection::MigrationEvidence => "migration_evidence",
            };
            (&run.summary, protection)
        })
        .collect();
    seaf_core::canonical_sha256_digest(&DigestInventory { total_bytes, runs })
        .map_err(|error| RetentionError::context("could not digest retention inventory", error))
}

fn load_verified_intent(root: &PinnedPrivateDirectory) -> Result<PurgeIntent, RetentionError> {
    let intent: PurgeIntent = load_canonical_control(root, OsStr::new(INTENT_FILE))?;
    let expected = digest_with_empty_field(&intent)?;
    if intent.schema_version != 1 || intent.intent_digest != expected {
        return Err(RetentionError::new(
            "purge intent schema or digest is invalid".to_string(),
        ));
    }
    if intent.selected.len() > 1
        || intent.selected.iter().any(|selected| {
            selected.tombstone_name
                != tombstone_name_for(
                    &selected.summary,
                    selected.directory_device,
                    selected.directory_inode,
                )
                || !intent.decision.selected.contains(&selected.summary)
        })
    {
        return Err(RetentionError::new(
            "purge intent selected batch or tombstone identity is invalid".to_string(),
        ));
    }
    Ok(intent)
}

fn load_optional_verified_result(
    root: &PinnedPrivateDirectory,
) -> Result<Option<PurgeReport>, RetentionError> {
    let bytes = match read_control(root, OsStr::new(RESULT_FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut result = verified_result_bytes(&bytes)?;
    resolve_audit_path(root, &mut result)?;
    Ok(Some(result))
}

fn resolve_audit_path(
    root: &PinnedPrivateDirectory,
    report: &mut PurgeReport,
) -> Result<(), RetentionError> {
    if report.audit_path.is_some() {
        report.audit_path = Some(root.path().join(RESULT_FILE));
    }
    Ok(())
}

fn serialize_audit_path<S>(audit_path: &Option<PathBuf>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    audit_path
        .as_ref()
        .map(|_| RESULT_FILE)
        .serialize(serializer)
}

fn deserialize_audit_path<'de, D>(deserializer: D) -> Result<Option<PathBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<PathBuf>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(path)
            if path == Path::new(RESULT_FILE)
                || (path.is_absolute() && path.file_name() == Some(OsStr::new(RESULT_FILE))) =>
        {
            Ok(Some(PathBuf::from(RESULT_FILE)))
        }
        Some(_) => Err(serde::de::Error::custom(
            "purge audit path must be the stable relative result identity",
        )),
    }
}

fn verified_result_bytes(bytes: &[u8]) -> Result<PurgeReport, RetentionError> {
    let raw: serde_json::Value = serde_json::from_slice(bytes)?;
    let result: PurgeReport = serde_json::from_slice(bytes)?;
    let current =
        canonical_report_bytes(&result)? == bytes && result.audit_digest == digest_report(&result)?;
    let legacy_absolute = !raw
        .as_object()
        .is_some_and(|object| object.contains_key("continuation_required"))
        && raw
            .get("audit_path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|path| {
                let path = Path::new(path);
                path.is_absolute() && path.file_name() == Some(OsStr::new(RESULT_FILE))
            })
        && verified_raw_report_digest(bytes, &raw)?;
    if result.schema_version != 1
        || result.mode != PurgeMode::Apply
        || (!current && !legacy_absolute)
    {
        return Err(RetentionError::new(
            "purge audit result canonical bytes, schema, or digest are invalid".to_string(),
        ));
    }
    Ok(result)
}

fn verified_raw_report_digest(
    bytes: &[u8],
    raw: &serde_json::Value,
) -> Result<bool, RetentionError> {
    if seaf_core::canonical_json_bytes(raw)? != bytes {
        return Ok(false);
    }
    let Some(expected) = raw.get("audit_digest").and_then(serde_json::Value::as_str) else {
        return Ok(false);
    };
    let mut unsigned = raw.clone();
    unsigned["audit_digest"] = serde_json::Value::String(String::new());
    Ok(seaf_core::canonical_sha256_digest(&unsigned)? == expected)
}

fn load_canonical_control<T>(
    root: &PinnedPrivateDirectory,
    name: &OsStr,
) -> Result<T, RetentionError>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    let bytes = read_control(root, name)?;
    let value = serde_json::from_slice(&bytes)?;
    if canonical_intent_bytes(&value)? != bytes {
        return Err(RetentionError::new(format!(
            "retention control file is not canonical JSON: {}",
            root.path().join(name).display()
        )));
    }
    Ok(value)
}

fn read_control(root: &PinnedPrivateDirectory, name: &OsStr) -> Result<Vec<u8>, RetentionError> {
    let file = root
        .open_existing_regular_file_any_mode(name)
        .map_err(RetentionError::from)?;
    let identity = file.metadata()?;
    let cap = control_file_cap(name);
    if identity.len() > cap as u64 {
        return Err(RetentionError::new(
            "retention control file exceeds its byte cap",
        ));
    }
    let mut bytes = Vec::with_capacity(identity.len() as usize);
    file.take((cap + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > cap {
        return Err(RetentionError::new(
            "retention control file exceeds its byte cap",
        ));
    }
    root.validate_single_link_file(name, &identity)?;
    Ok(bytes)
}

fn control_file_cap(name: &OsStr) -> usize {
    if name == OsStr::new(RESULT_FILE) || name == OsStr::new(RESULT_TEMP_FILE) {
        RESULT_FILE_BYTE_CAP
    } else {
        CONTROL_FILE_BYTE_CAP
    }
}

fn canonical_intent_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, RetentionError> {
    canonical_bytes_with_cap(value, CONTROL_FILE_BYTE_CAP)
}

fn canonical_report_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, RetentionError> {
    canonical_bytes_with_cap(value, RESULT_FILE_BYTE_CAP)
}

fn canonical_bytes_with_cap<T: Serialize>(
    value: &T,
    cap: usize,
) -> Result<Vec<u8>, RetentionError> {
    let bytes = seaf_core::canonical_json_bytes(value)?;
    if bytes.len() > cap {
        return Err(RetentionError::new(format!(
            "retention control JSON exceeds its {cap}-byte cap"
        )));
    }
    Ok(bytes)
}

fn digest_with_empty_field(intent: &PurgeIntent) -> Result<String, RetentionError> {
    let mut unsigned = intent.clone();
    unsigned.intent_digest.clear();
    seaf_core::canonical_sha256_digest(&unsigned)
        .map_err(|error| RetentionError::context("could not digest purge intent", error))
}

fn digest_report(report: &PurgeReport) -> Result<String, RetentionError> {
    let mut unsigned = report.clone();
    unsigned.audit_digest.clear();
    seaf_core::canonical_sha256_digest(&unsigned)
        .map_err(|error| RetentionError::context("could not digest purge audit", error))
}

fn publish_create_only(
    root: &PinnedPrivateDirectory,
    name: &OsStr,
    bytes: &[u8],
) -> Result<(), RetentionError> {
    root.validate_identity()?;
    let mut file = root.create_file(name)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    root.validate_single_link_file(name, &file.metadata()?)?;
    root.sync_all()?;
    Ok(())
}

fn publish_replace(
    root: &PinnedPrivateDirectory,
    temp_name: &OsStr,
    final_name: &OsStr,
    bytes: &[u8],
) -> Result<(), RetentionError> {
    let (temp_file, temp_identity) = match root.open_existing_regular_file_any_mode(temp_name) {
        Ok(file) => {
            let identity = file.metadata()?;
            let actual = read_control_with_identity(root, temp_name, &file, &identity, false)?;
            if actual != bytes {
                return Err(RetentionError::new(
                    "existing purge-result temporary is not owned by this intent".to_string(),
                ));
            }
            (file, identity)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            publish_create_only(root, temp_name, bytes)?;
            let file = root.open_existing_regular_file_any_mode(temp_name)?;
            let identity = file.metadata()?;
            root.validate_single_link_file(temp_name, &identity)?;
            (file, identity)
        }
        Err(error) => return Err(error.into()),
    };

    match root.open_existing_regular_file_any_mode(final_name) {
        Ok(final_file) => {
            let final_identity = final_file.metadata()?;
            let final_bytes =
                read_control_with_identity(root, final_name, &final_file, &final_identity, false)?;
            if crate::artifact_safety::same_file_identity(&temp_identity, &final_identity) {
                if final_bytes != bytes {
                    return Err(RetentionError::new(
                        "linked purge result does not match the pending intent".to_string(),
                    ));
                }
                root.unlink_if_same(temp_name, &temp_identity)?;
                root.sync_all()?;
                root.validate_single_link_file(final_name, &final_identity)?;
                return Ok(());
            }
            verified_result_bytes(&final_bytes)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    root.validate_single_link_file(temp_name, &temp_identity)?;
    test_interrupt_before_result_rename()?;
    root.rename(temp_name, final_name)?;
    root.sync_all()?;
    root.validate_single_link_file(final_name, &temp_identity)?;
    drop(temp_file);
    let actual = read_control(root, final_name)?;
    if actual != bytes {
        return Err(RetentionError::new(
            "published purge audit bytes changed during replacement".to_string(),
        ));
    }
    Ok(())
}

fn read_control_with_identity(
    root: &PinnedPrivateDirectory,
    name: &OsStr,
    file: &fs::File,
    identity: &fs::Metadata,
    require_single_link: bool,
) -> Result<Vec<u8>, RetentionError> {
    let cap = control_file_cap(name);
    if identity.len() > cap as u64 {
        return Err(RetentionError::new(
            "retention control file exceeds its byte cap",
        ));
    }
    let mut bytes = Vec::with_capacity(identity.len() as usize);
    std::io::Read::take(file, (cap + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > cap {
        return Err(RetentionError::new(
            "retention control file exceeds its byte cap",
        ));
    }
    if require_single_link {
        root.validate_single_link_file(name, identity)?;
    } else {
        root.validate_file(name, identity)?;
    }
    Ok(bytes)
}

fn remove_control_file(root: &PinnedPrivateDirectory, name: &OsStr) -> Result<(), RetentionError> {
    remove_control_file_if_present(root, name)?;
    root.sync_all()?;
    Ok(())
}

fn remove_control_file_if_present(
    root: &PinnedPrivateDirectory,
    name: &OsStr,
) -> Result<(), RetentionError> {
    match root.open_existing_regular_file_any_mode(name) {
        Ok(file) => {
            let identity = file.metadata()?;
            root.unlink_regular_file_if_same_any_mode(name, &identity)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_run_id(run_id: &str) -> Result<(), RetentionError> {
    if !run_id.is_empty()
        && run_id.trim() == run_id
        && run_id != "."
        && run_id != ".."
        && run_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        Ok(())
    } else {
        Err(RetentionError::new(
            "invalid managed run ID; use only ASCII letters, numbers, '-' or '_'",
        ))
    }
}

fn invalid_io(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
static INTERRUPT_AFTER_ENTRY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static MUTATE_AFTER_INVENTORY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static INTERRUPT_BEFORE_RESULT_RENAME: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static INTERRUPT_AFTER_RESULT_RENAME: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
fn test_mutate_after_inventory(
    root: &PinnedPrivateDirectory,
    selected: &[IntentRun],
) -> Result<(), RetentionError> {
    if !MUTATE_AFTER_INVENTORY.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Ok(());
    }
    let selected = selected
        .first()
        .ok_or_else(|| RetentionError::new("test mutation requires one selected run"))?;
    let path = root
        .path()
        .join(&selected.summary.run_id)
        .join("post-inventory.bin");
    fs::write(&path, b"not bound by snapshot")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(not(test))]
fn test_mutate_after_inventory(
    _root: &PinnedPrivateDirectory,
    _selected: &[IntentRun],
) -> Result<(), RetentionError> {
    Ok(())
}

#[cfg(test)]
fn test_interrupt_after_entry() -> Result<(), RetentionError> {
    if INTERRUPT_AFTER_ENTRY.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err(RetentionError::new(
            "deterministic purge interruption after one entry".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
fn test_interrupt_before_result_rename() -> Result<(), RetentionError> {
    if INTERRUPT_BEFORE_RESULT_RENAME.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err(RetentionError::new(
            "deterministic interruption before atomic result rename".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
fn test_interrupt_after_result_rename() -> Result<(), RetentionError> {
    if INTERRUPT_AFTER_RESULT_RENAME.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err(RetentionError::new(
            "deterministic interruption after result rename".to_string(),
        ));
    }
    Ok(())
}

#[cfg(not(test))]
fn test_interrupt_after_entry() -> Result<(), RetentionError> {
    Ok(())
}

#[cfg(not(test))]
fn test_interrupt_before_result_rename() -> Result<(), RetentionError> {
    Ok(())
}

#[cfg(not(test))]
fn test_interrupt_after_result_rename() -> Result<(), RetentionError> {
    Ok(())
}

#[derive(Debug)]
pub struct RetentionError {
    message: String,
    source: Option<Box<dyn Error + Send + Sync>>,
    not_found: bool,
}

impl RetentionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
            not_found: false,
        }
    }

    fn context(message: impl Into<String>, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
            not_found: false,
        }
    }

    fn is_not_found(&self) -> bool {
        self.not_found
    }
}

impl fmt::Display for RetentionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)?;
        if let Some(source) = &self.source {
            write!(formatter, ": {source}")?;
        }
        Ok(())
    }
}

impl Error for RetentionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

impl From<std::io::Error> for RetentionError {
    fn from(error: std::io::Error) -> Self {
        let not_found = error.kind() == std::io::ErrorKind::NotFound;
        Self {
            message: "retention filesystem operation failed".to_string(),
            source: Some(Box::new(error)),
            not_found,
        }
    }
}

impl From<serde_json::Error> for RetentionError {
    fn from(error: serde_json::Error) -> Self {
        Self::context("retention JSON operation failed", error)
    }
}

impl From<RunPersistenceError> for RetentionError {
    fn from(error: RunPersistenceError) -> Self {
        Self::context("retention run-lock operation failed", error)
    }
}

impl From<crate::WorkspaceError> for RetentionError {
    fn from(error: crate::WorkspaceError) -> Self {
        Self::context("retention workspace validation failed", error)
    }
}

impl From<crate::state::StateError> for RetentionError {
    fn from(error: crate::state::StateError) -> Self {
        Self::context("retention run authority validation failed", error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn completed_run(runs_root: &Path, run_id: &str) {
        let workspace = LoopWorkspace::create(runs_root, run_id).unwrap();
        let mut run = state::create_run(state::NewLoopRun {
            run_id: run_id.to_string(),
            ticket_id: "retention-interruption".to_string(),
            goal_id: "m3-03".to_string(),
            provider: "fake".to_string(),
            model: "fake-local".to_string(),
            input_digests: seaf_core::LoopInputDigests {
                ticket: "a".repeat(64),
                policy: "b".repeat(64),
                config: "c".repeat(64),
                repository: "d".repeat(64),
                eval_config: None,
            },
        });
        for step in state::LOOP_STEPS {
            state::finish_step(
                &mut run,
                step,
                seaf_core::LoopStepStatus::Completed,
                None,
                None,
            )
            .unwrap();
        }
        state::save_run(&workspace, &run).unwrap();
    }

    fn candidate_run(
        runs_root: &Path,
        run_id: &str,
        lifecycle: seaf_core::CandidateWorkspaceLifecycle,
    ) {
        let workspace = LoopWorkspace::create(runs_root, run_id).unwrap();
        let mut run = state::create_run(state::NewLoopRun {
            run_id: run_id.to_string(),
            ticket_id: "retention-candidate".to_string(),
            goal_id: "m3-03".to_string(),
            provider: "fake".to_string(),
            model: "fake-local".to_string(),
            input_digests: seaf_core::LoopInputDigests {
                ticket: "a".repeat(64),
                policy: "b".repeat(64),
                config: "c".repeat(64),
                repository: "d".repeat(64),
                eval_config: None,
            },
        });
        if lifecycle != seaf_core::CandidateWorkspaceLifecycle::Provisioning {
            for step in state::LOOP_STEPS {
                state::finish_step(
                    &mut run,
                    step,
                    seaf_core::LoopStepStatus::Completed,
                    None,
                    None,
                )
                .unwrap();
            }
        }
        let cleanup_started_at = matches!(
            lifecycle,
            seaf_core::CandidateWorkspaceLifecycle::Cleaning
                | seaf_core::CandidateWorkspaceLifecycle::Cleaned
        )
        .then(|| "2026-07-15T10:00:00Z".to_string());
        let cleaned_at = (lifecycle == seaf_core::CandidateWorkspaceLifecycle::Cleaned)
            .then(|| "2026-07-15T10:01:00Z".to_string());
        run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
        run.candidate_workspace = Some(seaf_core::CandidateWorkspaceState {
            schema_version: crate::CANDIDATE_WORKSPACE_SCHEMA_VERSION,
            run_directory_digest: Some("e".repeat(64)),
            path: format!("/tmp/{run_id}-candidate"),
            source_worktree_root: format!("/tmp/{run_id}-source"),
            git_common_dir: format!("/tmp/{run_id}-git-common"),
            repository_identity_digest: "d".repeat(64),
            starting_head: "1".repeat(40),
            starting_tree: "2".repeat(40),
            candidate_head: "1".repeat(40),
            candidate_tree: "2".repeat(40),
            candidate_diff_digest:
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            patch_transaction: None,
            lifecycle,
            cleanup_started_at,
            cleaned_at,
        });
        state::write_raw_canonical_run_fixture(&workspace.run_file(), &run).unwrap();
    }

    fn completed_provider_run(runs_root: &Path, run_id: &str) -> seaf_core::LoopRun {
        let workspace = LoopWorkspace::create(runs_root, run_id).unwrap();
        let mut run = state::create_run(state::NewLoopRun {
            run_id: run_id.to_string(),
            ticket_id: "retention-provider".to_string(),
            goal_id: "m3-03".to_string(),
            provider: "fake".to_string(),
            model: "fake-local".to_string(),
            input_digests: seaf_core::LoopInputDigests {
                ticket: "a".repeat(64),
                policy: "b".repeat(64),
                config: "c".repeat(64),
                repository: "d".repeat(64),
                eval_config: None,
            },
        });
        for step in [
            seaf_core::LoopStepName::Research,
            seaf_core::LoopStepName::Analysis,
            seaf_core::LoopStepName::SpecCreation,
            seaf_core::LoopStepName::SpecReview,
            seaf_core::LoopStepName::Development,
        ] {
            state::finish_step(
                &mut run,
                step,
                seaf_core::LoopStepStatus::Completed,
                None,
                None,
            )
            .unwrap();
        }
        state::save_run(&workspace, &run).unwrap();
        run = crate::provider_exchange::persist_test_output_review_ledger(&workspace, run_id);
        for step in [
            seaf_core::LoopStepName::OutputReview,
            seaf_core::LoopStepName::Testing,
            seaf_core::LoopStepName::EvalReport,
        ] {
            state::finish_step(
                &mut run,
                step,
                seaf_core::LoopStepStatus::Completed,
                None,
                None,
            )
            .unwrap();
        }
        state::write_raw_canonical_run_fixture(&workspace.run_file(), &run).unwrap();
        state::load_run(&workspace).unwrap();
        run
    }

    fn pending_tombstone_path(runs_root: &Path) -> PathBuf {
        let root = PinnedPrivateDirectory::open_parent(runs_root).unwrap();
        let intent = load_verified_intent(&root).unwrap();
        assert_eq!(intent.selected.len(), 1);
        runs_root.join(&intent.selected[0].tombstone_name)
    }

    #[test]
    fn interrupted_purge_retries_exact_intent_and_conflicting_policy_fails_closed() {
        let _serial = TEST_MUTEX.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        completed_run(&runs_root, "interrupted");
        INTERRUPT_AFTER_ENTRY.store(true, std::sync::atomic::Ordering::SeqCst);

        let error = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect_err("the deterministic cut must interrupt deletion");
        assert!(
            error
                .to_string()
                .contains("deterministic purge interruption"),
            "{error}"
        );
        assert!(runs_root.join(INTENT_FILE).is_file());
        assert!(!runs_root.join("interrupted").exists());
        assert!(pending_tombstone_path(&runs_root).is_dir());

        let before_conflict = fs::read(runs_root.join(INTENT_FILE)).unwrap();
        let conflict = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 1,
            },
            PurgeMode::Apply,
        )
        .expect_err("a conflicting retry must fail closed");
        assert!(conflict.to_string().contains("same policy"), "{conflict}");
        assert_eq!(
            fs::read(runs_root.join(INTENT_FILE)).unwrap(),
            before_conflict
        );

        let active_workspace = LoopWorkspace::create(&runs_root, "arrived-active").unwrap();
        let active = state::create_run(state::NewLoopRun {
            run_id: "arrived-active".to_string(),
            ticket_id: "retention-active".to_string(),
            goal_id: "m3-03".to_string(),
            provider: "fake".to_string(),
            model: "fake-local".to_string(),
            input_digests: seaf_core::LoopInputDigests {
                ticket: "a".repeat(64),
                policy: "b".repeat(64),
                config: "c".repeat(64),
                repository: "d".repeat(64),
                eval_config: None,
            },
        });
        state::save_run(&active_workspace, &active).unwrap();
        fs::create_dir(runs_root.join(".arrived-migration.backup")).unwrap();

        let retry = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect("ordinary retry must converge");
        assert_eq!(retry.deleted[0].run_id, "interrupted");
        assert!(!runs_root.join("interrupted").exists());
        assert!(!runs_root.join(INTENT_FILE).exists());
        assert!(runs_root.join(RESULT_FILE).is_file());
        assert!(retry.decision.snapshot.protected_active.is_empty());
        assert!(retry.decision.snapshot.excluded_root_entries.is_empty());
        let converged = retry.converged.as_ref().unwrap();
        assert_eq!(converged.protected_active, ["arrived-active"]);
        assert_eq!(
            converged.excluded_root_entries,
            [".arrived-migration.backup"]
        );
        assert_eq!(converged.control_state, expected_converged_control_state());

        let exact = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect("exact retry must validate converged exclusions and controls");
        assert_eq!(exact, retry);
    }

    #[test]
    fn selected_tree_is_revalidated_under_its_guard_before_intent_publication() {
        let _serial = TEST_MUTEX.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        completed_run(&runs_root, "changed-after-inventory");
        MUTATE_AFTER_INVENTORY.store(true, std::sync::atomic::Ordering::SeqCst);

        let error = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect_err("post-inventory mutation must fail before intent");

        assert!(error.to_string().contains("before purge intent"), "{error}");
        assert!(!runs_root.join(INTENT_FILE).exists());
        assert!(runs_root.join("changed-after-inventory").is_dir());
        assert!(runs_root
            .join("changed-after-inventory/post-inventory.bin")
            .is_file());
    }

    #[test]
    fn interrupted_tombstone_rejects_unbound_additions_and_substitution() {
        let _serial = TEST_MUTEX.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        completed_run(&runs_root, "tampered-tombstone");
        INTERRUPT_AFTER_ENTRY.store(true, std::sync::atomic::Ordering::SeqCst);
        purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect_err("deterministic interruption");
        let tombstone = pending_tombstone_path(&runs_root);
        let injected = tombstone.join("unbound.bin");
        fs::write(&injected, b"unbound").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&injected, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let tampered = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect_err("unbound tombstone content must fail closed");
        assert!(tampered.to_string().contains("not bound"), "{tampered}");
        assert!(runs_root.join(INTENT_FILE).is_file());

        fs::remove_file(injected).unwrap();
        let parked = runs_root.join(".parked-retention-tombstone");
        fs::rename(&tombstone, &parked).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&tombstone)
                .unwrap();
        }

        let substituted = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect_err("substituted tombstone must fail closed");
        assert!(
            substituted.to_string().contains("identity changed"),
            "{substituted}"
        );
        assert!(parked.is_dir());
        assert!(tombstone.is_dir());
        assert!(runs_root.join(INTENT_FILE).is_file());
    }

    #[test]
    fn partial_tombstone_retry_does_not_require_already_deleted_provider_records() {
        let _serial = TEST_MUTEX.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        let run = completed_provider_run(&runs_root, "provider-partial");
        INTERRUPT_AFTER_ENTRY.store(true, std::sync::atomic::Ordering::SeqCst);
        purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect_err("deterministic partial tombstone");
        let tombstone = pending_tombstone_path(&runs_root);
        let provider_record = run
            .provider_exchange_records
            .iter()
            .map(|reference| tombstone.join(&reference.path))
            .find(|path| path.is_file())
            .expect("a referenced provider record must remain after the first deletion cut");
        fs::remove_file(provider_record).unwrap();

        let retry = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect("remaining intent-bound authority must converge");
        assert_eq!(retry.deleted[0].run_id, "provider-partial");
        assert!(!tombstone.exists());
    }

    #[test]
    fn result_replacement_crash_retains_a_verified_old_or_new_final() {
        let _serial = TEST_MUTEX.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        completed_run(&runs_root, "first-result");
        let first = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .unwrap();
        completed_run(&runs_root, "second-result");
        INTERRUPT_BEFORE_RESULT_RENAME.store(true, std::sync::atomic::Ordering::SeqCst);

        let error = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect_err("result replacement cut must interrupt before atomic rename");
        assert!(error.to_string().contains("result rename"), "{error}");
        assert_eq!(load_verified_purge_result(&runs_root).unwrap(), first);

        let retry = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect("retry publishes the verified new result");
        assert_eq!(retry.deleted[0].run_id, "second-result");
        assert_eq!(load_verified_purge_result(&runs_root).unwrap(), retry);
    }

    #[test]
    fn continuation_carries_completed_batch_history_across_unrelated_arrival() {
        let _serial = TEST_MUTEX.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        completed_run(&runs_root, "batch-first");
        completed_run(&runs_root, "batch-second");
        let root = PinnedPrivateDirectory::open_parent(&runs_root).unwrap();
        let first = purge_apply_batch(
            &root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
        )
        .unwrap();
        assert!(first.continuation_required);
        assert_eq!(first.deleted.len(), 1);
        completed_run(&runs_root, "batch-arrival");

        let final_report = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .unwrap();
        assert_eq!(
            final_report
                .deleted
                .iter()
                .map(|run| run.run_id.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            ["batch-arrival", "batch-first", "batch-second"]
                .into_iter()
                .collect()
        );
        assert_eq!(final_report.deleted.len(), 3, "each deletion appears once");
    }

    #[test]
    fn completed_chained_result_is_adopted_after_rename_cut_and_unrelated_drift() {
        let _serial = TEST_MUTEX.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        completed_run(&runs_root, "rename-first");
        completed_run(&runs_root, "rename-second");
        completed_run(&runs_root, "rename-third");
        let root = PinnedPrivateDirectory::open_parent(&runs_root).unwrap();
        let first = purge_apply_batch(
            &root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
        )
        .unwrap();
        assert!(first.continuation_required);
        INTERRUPT_AFTER_RESULT_RENAME.store(true, std::sync::atomic::Ordering::SeqCst);
        let error = purge_apply_batch(
            &root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
        )
        .expect_err("cut after chained result rename");
        assert!(error.to_string().contains("after result rename"), "{error}");
        assert!(runs_root.join(INTENT_FILE).is_file());
        assert!(runs_root.join(RESULT_FILE).is_file());
        fs::create_dir(runs_root.join(".unrelated-arrival")).unwrap();

        let retry = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect("completed batch result must be adopted before current-state advancement");
        assert_eq!(
            retry
                .deleted
                .iter()
                .map(|run| run.run_id.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            ["rename-first", "rename-second", "rename-third"]
                .into_iter()
                .collect()
        );
        assert_eq!(retry.deleted.len(), 3);
        assert!(retry
            .converged
            .unwrap()
            .excluded_root_entries
            .contains(&".unrelated-arrival".to_string()));
    }

    #[test]
    fn final_batch_adoption_redecides_after_new_eligible_arrival() {
        let _serial = TEST_MUTEX.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        completed_run(&runs_root, "final-a");
        INTERRUPT_AFTER_RESULT_RENAME.store(true, std::sync::atomic::Ordering::SeqCst);
        purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect_err("final result rename cut");
        completed_run(&runs_root, "final-b");

        let retry = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::Apply,
        )
        .expect("adopt A, redecide current state, and purge B");
        assert_eq!(
            retry
                .deleted
                .iter()
                .map(|run| run.run_id.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            ["final-a", "final-b"].into_iter().collect()
        );
        assert_eq!(retry.deleted.len(), 2);
        assert_eq!(retry.converged.as_ref().unwrap().managed_bytes, 0);
        assert!(!runs_root.join("final-b").exists());
        assert_eq!(load_verified_purge_result(&runs_root).unwrap(), retry);
    }

    #[test]
    fn retention_status_and_candidate_matrix_preserves_authority_with_remaining_actions() {
        assert!(eligible_status(seaf_core::LoopStatus::Completed));
        assert!(eligible_status(seaf_core::LoopStatus::Passed));
        assert!(
            !eligible_status(seaf_core::LoopStatus::Promoted),
            "Promoted retains active candidate authority and supported cleanup is unavailable"
        );
        assert!(
            !eligible_status(seaf_core::LoopStatus::EvalPassed),
            "EvalPassed remains promotable and must stay protected"
        );
        assert!(!eligible_status(seaf_core::LoopStatus::Approved));
        assert!(!eligible_status(seaf_core::LoopStatus::Failed));
        assert!(candidate_lifecycle_is_live(Some(
            seaf_core::CandidateWorkspaceLifecycle::Active
        )));
        assert!(candidate_lifecycle_is_live(Some(
            seaf_core::CandidateWorkspaceLifecycle::Cleaning
        )));
        assert!(!candidate_lifecycle_is_live(Some(
            seaf_core::CandidateWorkspaceLifecycle::Cleaned
        )));
        assert!(!candidate_lifecycle_is_live(None));
    }

    #[test]
    fn purge_classifies_authenticated_status_and_candidate_authority_before_selecting_runs() {
        let _serial = TEST_MUTEX.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");

        completed_run(&runs_root, "passed");
        let passed_workspace = LoopWorkspace::open_minimal(&runs_root, "passed").unwrap();
        let mut passed = state::load_run(&passed_workspace).unwrap();
        passed.status = seaf_core::LoopStatus::Passed;
        state::write_raw_canonical_run_fixture(&passed_workspace.run_file(), &passed).unwrap();
        candidate_run(
            &runs_root,
            "provisioning",
            seaf_core::CandidateWorkspaceLifecycle::Provisioning,
        );
        candidate_run(
            &runs_root,
            "active",
            seaf_core::CandidateWorkspaceLifecycle::Active,
        );
        candidate_run(
            &runs_root,
            "cleaning",
            seaf_core::CandidateWorkspaceLifecycle::Cleaning,
        );
        candidate_run(
            &runs_root,
            "cleaned",
            seaf_core::CandidateWorkspaceLifecycle::Cleaned,
        );

        let report = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            PurgeMode::DryRun,
        )
        .unwrap();
        let selected = report
            .decision
            .selected
            .iter()
            .map(|run| run.run_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(selected, ["cleaned", "passed"].into_iter().collect());
        assert_eq!(
            report
                .decision
                .snapshot
                .protected_active
                .iter()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            ["active", "cleaning", "provisioning"].into_iter().collect(),
            "pending Provisioning and completed live candidates retain further-action authority"
        );
    }

    #[test]
    fn worst_case_batch_intent_and_cumulative_audit_fit_their_proven_caps() {
        let run_ids = (0..MAX_MANAGED_RUNS)
            .map(|index| format!("{index:04x}{}", "x".repeat(251)))
            .collect::<Vec<_>>();
        let summaries = run_ids
            .iter()
            .map(|run_id| PurgeRunSummary {
                run_id: run_id.clone(),
                status: seaf_core::LoopStatus::Completed,
                updated_at: u64::MAX.to_string(),
                bytes: artifact_storage::RUN_TREE_BYTE_CAP,
                run_digest: "a".repeat(64),
                tree_digest: "b".repeat(64),
            })
            .collect::<Vec<_>>();
        let snapshot = PurgeStateSnapshot {
            inventory_digest: "c".repeat(64),
            managed_bytes: u64::MAX,
            protected_active: run_ids,
            protected_locked: Vec::new(),
            protected_migration_evidence: Vec::new(),
            excluded_root_entries: Vec::new(),
            control_state: PurgeControlState {
                intent_present: false,
                result_present: true,
                result_temp_present: false,
                tombstones: Vec::new(),
            },
        };
        let mut intent = PurgeIntent {
            schema_version: 1,
            policy: RetentionPolicy {
                max_managed_bytes: 0,
            },
            decision: PurgeDecisionEvidence {
                snapshot: snapshot.clone(),
                selected: vec![summaries[0].clone()],
            },
            projected_managed_bytes_after: 0,
            selected: vec![IntentRun {
                summary: summaries[0].clone(),
                directory_device: u64::MAX,
                directory_inode: u64::MAX,
                tombstone_name: tombstone_name_for(&summaries[0], u64::MAX, u64::MAX),
                tree_manifest: (0..artifact_storage::RUN_TREE_ENTRY_CAP)
                    .map(|index| IntentManifestEntry {
                        entry_digest: format!("{index:064x}"),
                    })
                    .collect(),
            }],
            prior_audit_digest: Some("d".repeat(64)),
            intent_digest: String::new(),
        };
        intent.intent_digest = digest_with_empty_field(&intent).unwrap();
        assert!(canonical_intent_bytes(&intent).unwrap().len() <= CONTROL_FILE_BYTE_CAP);

        let mut report = PurgeReport {
            schema_version: 1,
            mode: PurgeMode::Apply,
            policy: intent.policy,
            decision: intent.decision.clone(),
            projected_managed_bytes_after: 0,
            deleted: summaries,
            converged: Some(snapshot),
            intent_digest: Some(intent.intent_digest),
            prior_audit_digest: intent.prior_audit_digest,
            audit_path: Some(PathBuf::from(RESULT_FILE)),
            audit_digest: String::new(),
            continuation_required: false,
        };
        report.audit_digest = digest_report(&report).unwrap();
        assert!(canonical_report_bytes(&report).unwrap().len() <= RESULT_FILE_BYTE_CAP);
    }
}
