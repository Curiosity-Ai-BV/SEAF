use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    ffi::OsStr,
    fmt, fs, io,
    io::Read,
    path::{Path, PathBuf},
};

use seaf_core::{
    canonical_json_bytes, is_portable_artifact_path, validate_eval_report, validate_loop_run,
    validate_policy, validate_policy_decision, validate_ticket_spec, ArtifactReference, EvalReport,
    LoopRun, Policy, PolicyDecision, ProviderExchangeRecord, TicketSpec,
    DURABLE_ARTIFACT_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MIGRATION_ID: &str = "v0-v1";
const RESULT_FILE: &str = "migration-v0-v1.result.json";
const STAGED_OWNERSHIP_FILE: &str = ".migration-v0-v1.owner.json";
const INTENT_SCHEMA_VERSION: u32 = 1;
const RESULT_SCHEMA_VERSION: u32 = 1;
const STAGED_OWNERSHIP_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
thread_local! {
    static INVENTORY_ENUMERATION_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStatus {
    Migrated,
    Recovered,
    AlreadyCurrent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationOutcome {
    pub command: String,
    pub run_id: String,
    pub status: MigrationStatus,
    pub from_schema_version: u32,
    pub to_schema_version: u32,
    pub run_directory: String,
    pub backup_directory: Option<String>,
    pub result_path: Option<String>,
    pub migrated_artifacts: Vec<String>,
}

#[derive(Debug)]
pub struct MigrationError {
    message: String,
    #[cfg(test)]
    interrupted: bool,
}

impl MigrationError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            #[cfg(test)]
            interrupted: false,
        }
    }

    fn interrupted(phase: PublicationPhase) -> Self {
        Self {
            message: format!("injected migration interruption at {phase:?}"),
            #[cfg(test)]
            interrupted: true,
        }
    }

    fn context(context: impl fmt::Display, error: impl fmt::Display) -> Self {
        Self::new(format!("{context}: {error}"))
    }
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for MigrationError {}

impl From<io::Error> for MigrationError {
    fn from(error: io::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<serde_json::Error> for MigrationError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Debug, Clone)]
struct MigrationPaths {
    source: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
    intent: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublicationPhase {
    None,
    AfterIntent,
    DuringCopy,
    DuringRewrite,
    #[cfg(test)]
    RebindSelectedAfterLock,
    #[cfg(test)]
    RebindStagedAfterLock,
    #[cfg(test)]
    RebindRunsRootBeforeIntentCreate,
    #[cfg(test)]
    RebindRunsRootBeforeIntentRemove,
    #[cfg(test)]
    DivergeStagedAfterProjection,
    #[cfg(test)]
    RebindCompletedSourceBeforeCleanup,
    #[cfg(test)]
    RebindCompletedSourceAfterMarkerRemoval,
    AfterStaged,
    AfterBackup,
    AfterPublish,
    AfterOwnershipRemoval,
}

impl MigrationPaths {
    fn new(runs_root: &Path, run_id: &str) -> Self {
        let prefix = format!(".{run_id}.migration-v0-v1");
        Self {
            source: runs_root.join(run_id),
            staged: runs_root.join(format!("{prefix}.staged")),
            backup: runs_root.join(format!("{prefix}.backup")),
            intent: runs_root.join(format!("{prefix}.intent.json")),
        }
    }
}

#[derive(Debug)]
struct AuthenticatedRun {
    managed_paths: BTreeSet<String>,
    development_evidence_paths: BTreeSet<String>,
    graph_json_paths: BTreeSet<String>,
    typed_rewrite_paths: BTreeSet<String>,
    legacy_paths: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum GraphArtifactKind {
    Generic,
    DevelopmentEvidence,
    EvalReport,
    TestingEvidence,
    EvaluationIntent,
    ProviderExchangeRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphReference {
    path: String,
    digest: String,
    kind: GraphArtifactKind,
}

#[derive(Debug)]
struct AuthenticatedGraph {
    json_paths: BTreeSet<String>,
    typed_rewrite_paths: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RunTreeEntry {
    Directory,
    File { len: u64, digest: [u8; 32] },
    Symlink(Vec<u8>),
}

type RunTreeInventory = BTreeMap<Vec<u8>, RunTreeEntry>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MigrationIntent {
    schema_version: u32,
    migration_id: String,
    run_id: String,
    source_run_digest: String,
    source_tree_digest: String,
    target_schema_version: u32,
    staged_ownership_token: String,
    projected_staged_inventory_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StagedOwnership {
    schema_version: u32,
    migration_id: String,
    run_id: String,
    token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MigrationResultStatus {
    Migrated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MigrationResult {
    schema_version: u32,
    migration_id: String,
    run_id: String,
    from_schema_version: u32,
    to_schema_version: u32,
    status: MigrationResultStatus,
    backup_directory: String,
    migrated_artifacts: Vec<String>,
    source_run_digest: String,
    source_tree_digest: String,
    migrated_run_digest: String,
}

#[derive(Debug)]
struct MigrationPlan {
    rewritten_artifacts: BTreeMap<String, Vec<u8>>,
    result: MigrationResult,
    result_bytes: Vec<u8>,
    projected_staged_inventory: RunTreeInventory,
}

pub fn migrate_loop_run(
    runs_root: &Path,
    run_id: &str,
) -> Result<MigrationOutcome, MigrationError> {
    migrate_loop_run_with_fault(runs_root, run_id, PublicationPhase::None)
}

pub(crate) fn pending_migration_source_is_protected(
    runs_root: &crate::artifact_safety::PinnedPrivateDirectory,
    run_id: &str,
) -> Result<bool, MigrationError> {
    validate_run_id(run_id)?;
    let paths = MigrationPaths::new(runs_root.path(), run_id);
    let intent_name = paths.intent.file_name().expect("intent name");
    if !pending_migration_entry_exists(runs_root, intent_name)? {
        return Ok(false);
    }
    if pinned_entry_exists(runs_root, paths.backup.file_name().expect("backup name"))? {
        return Err(MigrationError::new(
            "pending migration source has a backup but no published migration result",
        ));
    }

    let intent = load_bound_intent(runs_root, intent_name, run_id, &paths.source)?;
    if pinned_entry_exists(runs_root, paths.staged.file_name().expect("staged name"))? {
        authenticate_current_staged_run(&paths.staged, &paths.source, run_id, &intent)?;
    }
    Ok(true)
}

fn pending_migration_entry_exists(
    runs_root: &crate::artifact_safety::PinnedPrivateDirectory,
    name: &OsStr,
) -> Result<bool, MigrationError> {
    runs_root.validate_identity()?;
    match runs_root.entry_kind(name) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ENAMETOOLONG) => Ok(false),
        Err(error) => Err(MigrationError::context(
            "could not inspect pending migration entry",
            error,
        )),
    }
}

fn migrate_loop_run_with_fault(
    runs_root: &Path,
    run_id: &str,
    fault: PublicationPhase,
) -> Result<MigrationOutcome, MigrationError> {
    validate_run_id(run_id)?;
    let paths = MigrationPaths::new(runs_root, run_id);
    let intent_name = paths.intent.file_name().expect("intent name");
    validate_runs_root(runs_root)?;
    let runs_root_directory =
        crate::artifact_safety::PinnedPrivateDirectory::open_parent(runs_root)
            .map_err(|error| MigrationError::context("could not pin runs root", error))?;

    let selected_name = if paths.source.exists() {
        paths.source.file_name()
    } else if paths.backup.exists() {
        paths.backup.file_name()
    } else {
        None
    };
    let selected_guard = if paths.source.exists() {
        Some(acquire_existing_run_lock(&paths.source, "selected run")?)
    } else if paths.backup.exists() {
        Some(acquire_existing_run_lock(
            &paths.backup,
            "migration backup",
        )?)
    } else {
        None
    };

    #[cfg(test)]
    if fault == PublicationPhase::RebindSelectedAfterLock {
        rebind_directory_for_test(&paths.source)?;
    }

    if let (Some(guard), Some(name)) = (selected_guard.as_ref(), selected_name) {
        validate_guard_at_child(guard, &runs_root_directory, name, "selected run")?;
    }

    if let Some(outcome) = recover_existing_transaction(
        &runs_root_directory,
        run_id,
        &paths,
        selected_guard.as_ref(),
        fault,
    )? {
        return Ok(outcome);
    }

    let selected_guard = selected_guard.as_ref().ok_or_else(|| {
        MigrationError::new("selected run disappeared before migration authentication")
    })?;
    validate_guard_at_child(
        selected_guard,
        &runs_root_directory,
        paths.source.file_name().expect("source name"),
        "selected run",
    )?;
    let authenticated = authenticate_run(&paths.source, run_id)?;
    selected_guard.validate().map_err(|error| {
        MigrationError::context("selected run changed after authentication", error)
    })?;
    validate_guard_at_child(
        selected_guard,
        &runs_root_directory,
        paths.source.file_name().expect("source name"),
        "selected run",
    )?;
    if authenticated.legacy_paths.is_empty() {
        ensure_no_staged_ownership(&paths.source)?;
        validate_current_final_authority(&paths.source)?;
        return Ok(current_outcome(run_id, &paths));
    }
    if paths.backup.exists() {
        return Err(MigrationError::new(format!(
            "refusing migration because deterministic backup {} already exists without a recoverable intent",
            paths.backup.display()
        )));
    }

    let run_bytes = read_regular_file(&paths.source.join("run.json"), "LoopRun")?;
    let mut intent = MigrationIntent {
        schema_version: INTENT_SCHEMA_VERSION,
        migration_id: MIGRATION_ID.to_string(),
        run_id: run_id.to_string(),
        source_run_digest: digest(&run_bytes),
        source_tree_digest: digest_run_tree(&paths.source)?,
        target_schema_version: DURABLE_ARTIFACT_SCHEMA_VERSION,
        staged_ownership_token: generate_staged_ownership_token()?,
        projected_staged_inventory_digest: String::new(),
    };
    let staged_ownership = staged_ownership_from_intent(&intent);
    let migration_plan = build_migration_plan(
        &paths.source,
        run_id,
        &authenticated,
        &intent,
        &staged_ownership,
    )?;
    intent.projected_staged_inventory_digest =
        digest_run_tree_inventory(&migration_plan.projected_staged_inventory);
    validate_guard_at_child(
        selected_guard,
        &runs_root_directory,
        paths.source.file_name().expect("source name"),
        "selected run before intent publication",
    )?;
    #[cfg(test)]
    if fault == PublicationPhase::RebindRunsRootBeforeIntentCreate {
        rebind_directory_for_test(runs_root)?;
    }
    write_create_only_value_in_directory(&runs_root_directory, intent_name, &intent)?;
    #[cfg(test)]
    inject_fault(fault, PublicationPhase::RebindRunsRootBeforeIntentCreate)?;
    inject_fault(fault, PublicationPhase::AfterIntent)?;

    let result = (|| {
        copy_tree_no_follow(&paths.source, &paths.staged, Some(&staged_ownership), fault)?;
        validate_guard_at_child(
            selected_guard,
            &runs_root_directory,
            paths.source.file_name().expect("source name"),
            "selected run after staged copy",
        )?;
        let staged_guard = acquire_existing_run_lock(&paths.staged, "staged run")?;
        #[cfg(test)]
        if fault == PublicationPhase::RebindStagedAfterLock {
            rebind_directory_for_test(&paths.staged)?;
        }
        validate_guard_at_child(
            &staged_guard,
            &runs_root_directory,
            paths.staged.file_name().expect("staged name"),
            "staged run",
        )?;
        let migrated_artifacts = migrate_staged_tree(
            &paths.staged,
            &migration_plan,
            &intent,
            fault,
            &staged_guard,
            &runs_root_directory,
        )?;
        validate_guard_at_child(
            &staged_guard,
            &runs_root_directory,
            paths.staged.file_name().expect("staged name"),
            "staged run after rewrite",
        )?;
        authenticate_current_staged_run(&paths.staged, &paths.source, run_id, &intent)?;
        validate_guard_at_child(
            selected_guard,
            &runs_root_directory,
            paths.source.file_name().expect("source name"),
            "selected run before publication",
        )?;
        validate_guard_at_child(
            &staged_guard,
            &runs_root_directory,
            paths.staged.file_name().expect("staged name"),
            "staged run before publication",
        )?;
        runs_root_directory.sync_all()?;
        inject_fault(fault, PublicationPhase::AfterStaged)?;
        runs_root_directory
            .rename(
                paths.source.file_name().expect("source name"),
                paths.backup.file_name().expect("backup name"),
            )
            .map_err(|error| {
                MigrationError::context(
                    format!(
                        "could not publish byte-exact backup {}",
                        paths.backup.display()
                    ),
                    error,
                )
            })?;
        validate_guard_at_child(
            selected_guard,
            &runs_root_directory,
            paths.backup.file_name().expect("backup name"),
            "published migration backup",
        )?;
        runs_root_directory.sync_all()?;
        inject_fault(fault, PublicationPhase::AfterBackup)?;
        validate_guard_at_child(
            &staged_guard,
            &runs_root_directory,
            paths.staged.file_name().expect("staged name"),
            "staged run before selected publication",
        )?;
        runs_root_directory
            .rename(
                paths.staged.file_name().expect("staged name"),
                paths.source.file_name().expect("source name"),
            )
            .map_err(|error| {
                MigrationError::context(
                    format!("could not publish migrated run {}", paths.source.display()),
                    error,
                )
            })?;
        validate_guard_at_child(
            &staged_guard,
            &runs_root_directory,
            paths.source.file_name().expect("source name"),
            "published migrated run",
        )?;
        runs_root_directory.sync_all()?;
        inject_fault(fault, PublicationPhase::AfterPublish)?;
        #[cfg(test)]
        if fault == PublicationPhase::RebindRunsRootBeforeIntentRemove {
            rebind_directory_for_test(runs_root)?;
        }
        remove_staged_ownership(
            &runs_root_directory,
            paths.source.file_name().expect("source name"),
            &intent,
            Some(&staged_guard),
        )?;
        inject_fault(fault, PublicationPhase::AfterOwnershipRemoval)?;
        remove_intent(&runs_root_directory, intent_name)?;
        #[cfg(test)]
        inject_fault(fault, PublicationPhase::RebindRunsRootBeforeIntentRemove)?;
        Ok::<_, MigrationError>(migrated_artifacts)
    })();

    match result {
        Ok(migrated_artifacts) => Ok(migrated_outcome(
            run_id,
            &paths,
            MigrationStatus::Migrated,
            migrated_artifacts,
        )),
        Err(error) => Err(error),
    }
}

fn inject_fault(
    selected: PublicationPhase,
    current: PublicationPhase,
) -> Result<(), MigrationError> {
    if selected == current {
        Err(MigrationError::interrupted(current))
    } else {
        Ok(())
    }
}

fn recover_existing_transaction(
    runs_root_directory: &crate::artifact_safety::PinnedPrivateDirectory,
    run_id: &str,
    paths: &MigrationPaths,
    selected_guard: Option<&crate::run_persistence::RunMutationGuard>,
    fault: PublicationPhase,
) -> Result<Option<MigrationOutcome>, MigrationError> {
    #[cfg(not(test))]
    let _ = fault;
    let source = paths.source.exists();
    let staged = paths.staged.exists();
    let backup = paths.backup.exists();
    let intent_name = paths.intent.file_name().expect("intent name");
    let intent = pinned_entry_exists(runs_root_directory, intent_name)?;

    match (source, staged, backup, intent) {
        (true, false, false, false) => Ok(None),
        (true, false, true, false) => {
            let selected_guard = require_guard(selected_guard, "completed migrated run")?;
            validate_guard_at_child(
                selected_guard,
                runs_root_directory,
                paths.source.file_name().expect("source name"),
                "completed migrated run",
            )?;
            let backup_guard = acquire_existing_run_lock(&paths.backup, "migration backup")?;
            validate_guard_at_child(
                &backup_guard,
                runs_root_directory,
                paths.backup.file_name().expect("backup name"),
                "migration backup",
            )?;
            ensure_no_staged_ownership(&paths.source)?;
            validate_completed_migration(&paths.source, &paths.backup, run_id)?;
            Ok(Some(current_outcome(run_id, paths)))
        }
        (true, false, false, true) => {
            let selected_guard = require_guard(selected_guard, "selected run")?;
            validate_guard_at_child(
                selected_guard,
                runs_root_directory,
                paths.source.file_name().expect("source name"),
                "selected run",
            )?;
            load_bound_intent(
                runs_root_directory,
                intent_name,
                run_id,
                &paths.source,
            )?;
            remove_intent(runs_root_directory, intent_name)?;
            Ok(None)
        }
        (true, true, false, true) => {
            let selected_guard = require_guard(selected_guard, "selected run")?;
            validate_guard_at_child(
                selected_guard,
                runs_root_directory,
                paths.source.file_name().expect("source name"),
                "selected run",
            )?;
            let intent = load_bound_intent(
                runs_root_directory,
                intent_name,
                run_id,
                &paths.source,
            )?;
            let staged_name = paths.staged.file_name().expect("staged name");
            let staged_child = runs_root_directory
                .open_child_directory(staged_name)
                .map_err(|error| {
                    MigrationError::context("could not pin staged recovery candidate", error)
                })?;
            validate_staged_ownership_in_directory(&staged_child, run_id, &intent)?;
            runs_root_directory.validate_child_directory(staged_name, &staged_child.metadata()?)?;
            let staged_guard = match crate::run_persistence::RunMutationGuard::acquire_existing(
                &paths.staged,
            ) {
                Ok(guard) => Some(guard),
                Err(crate::run_persistence::RunPersistenceError::Io(error))
                    if error.kind() == io::ErrorKind::NotFound =>
                {
                    None
                }
                Err(error) => {
                    return Err(MigrationError::context(
                        "could not acquire the existing mutation lock for staged run",
                        error,
                    ))
                }
            };
            if let Some(staged_guard) = staged_guard.as_ref() {
                validate_guard_at_child(
                    staged_guard,
                    runs_root_directory,
                    paths.staged.file_name().expect("staged name"),
                    "staged run",
                )?;
            }
            let staged_validation = authenticate_current_staged_run_unbound(
                &paths.staged,
                &paths.source,
                run_id,
                &intent,
            );
            if let Err(staged_error) = staged_validation {
                validate_guard_at_child(
                    selected_guard,
                    runs_root_directory,
                    paths.source.file_name().expect("source name"),
                    "selected run before scratch rebuild",
                )?;
                if let Some(staged_guard) = staged_guard.as_ref() {
                    staged_guard.validate().map_err(|error| {
                        MigrationError::context(
                            "staged run changed before scratch removal",
                            error,
                        )
                    })?;
                    validate_guard_at_child(
                        staged_guard,
                        runs_root_directory,
                        paths.staged.file_name().expect("staged name"),
                        "staged run before scratch removal",
                    )?;
                }
                runs_root_directory
                    .validate_child_directory(staged_name, &staged_child.metadata()?)
                    .map_err(|error| {
                        MigrationError::context(
                            "staged recovery candidate identity changed before scratch removal",
                            error,
                        )
                    })?;
                validate_staged_ownership_in_directory(&staged_child, run_id, &intent)?;
                remove_pinned_child_tree(
                    runs_root_directory,
                    staged_name,
                    &staged_child,
                )?;
                remove_intent(runs_root_directory, intent_name)?;
                runs_root_directory.sync_all()?;
                let _ = staged_error;
                return Ok(None);
            }
            let staged_inventory = inventory_run_tree(&paths.staged)?;
            validate_projected_staged_inventory(&staged_inventory, &intent)?;
            let staged_guard = staged_guard.ok_or_else(|| {
                MigrationError::new("valid staged migration has no mutation lock")
            })?;
            validate_guard_at_child(
                selected_guard,
                runs_root_directory,
                paths.source.file_name().expect("source name"),
                "selected run before recovery publication",
            )?;
            runs_root_directory.rename(
                paths.source.file_name().expect("source name"),
                paths.backup.file_name().expect("backup name"),
            )?;
            validate_guard_at_child(
                selected_guard,
                runs_root_directory,
                paths.backup.file_name().expect("backup name"),
                "recovered migration backup",
            )?;
            runs_root_directory.sync_all()?;
            validate_guard_at_child(
                &staged_guard,
                runs_root_directory,
                paths.staged.file_name().expect("staged name"),
                "staged run before recovery publication",
            )?;
            runs_root_directory.rename(
                paths.staged.file_name().expect("staged name"),
                paths.source.file_name().expect("source name"),
            )?;
            validate_guard_at_child(
                &staged_guard,
                runs_root_directory,
                paths.source.file_name().expect("source name"),
                "recovered migrated run",
            )?;
            runs_root_directory.sync_all()?;
            remove_staged_ownership(
                runs_root_directory,
                paths.source.file_name().expect("source name"),
                &intent,
                Some(&staged_guard),
            )?;
            remove_intent(runs_root_directory, intent_name)?;
            Ok(Some(migrated_outcome(
                run_id,
                paths,
                MigrationStatus::Recovered,
                read_result_paths(&paths.source)?,
            )))
        }
        (false, true, true, true) => {
            let selected_guard = require_guard(selected_guard, "migration backup")?;
            validate_guard_at_child(
                selected_guard,
                runs_root_directory,
                paths.backup.file_name().expect("backup name"),
                "migration backup",
            )?;
            let intent = load_bound_intent(
                runs_root_directory,
                intent_name,
                run_id,
                &paths.backup,
            )?;
            let staged_guard = acquire_existing_run_lock(&paths.staged, "staged run")?;
            validate_guard_at_child(
                &staged_guard,
                runs_root_directory,
                paths.staged.file_name().expect("staged name"),
                "staged run",
            )?;
            authenticate_current_staged_run(&paths.staged, &paths.backup, run_id, &intent)?;
            validate_guard_at_child(
                &staged_guard,
                runs_root_directory,
                paths.staged.file_name().expect("staged name"),
                "staged run before recovery publication",
            )?;
            runs_root_directory.rename(
                paths.staged.file_name().expect("staged name"),
                paths.source.file_name().expect("source name"),
            )?;
            validate_guard_at_child(
                &staged_guard,
                runs_root_directory,
                paths.source.file_name().expect("source name"),
                "recovered migrated run",
            )?;
            runs_root_directory.sync_all()?;
            remove_staged_ownership(
                runs_root_directory,
                paths.source.file_name().expect("source name"),
                &intent,
                Some(&staged_guard),
            )?;
            remove_intent(runs_root_directory, intent_name)?;
            Ok(Some(migrated_outcome(
                run_id,
                paths,
                MigrationStatus::Recovered,
                read_result_paths(&paths.source)?,
            )))
        }
        (true, false, true, true) => {
            let selected_guard = require_guard(selected_guard, "migrated selected run")?;
            validate_guard_at_child(
                selected_guard,
                runs_root_directory,
                paths.source.file_name().expect("source name"),
                "migrated selected run",
            )?;
            let backup_guard = acquire_existing_run_lock(&paths.backup, "migration backup")?;
            validate_guard_at_child(
                &backup_guard,
                runs_root_directory,
                paths.backup.file_name().expect("backup name"),
                "migration backup",
            )?;
            let intent = load_bound_intent(
                runs_root_directory,
                intent_name,
                run_id,
                &paths.backup,
            )?;
            let ownership_present = validate_optional_staged_ownership(
                &paths.source,
                run_id,
                &intent,
            )?;
            validate_completed_migration_with_intent(
                &paths.source,
                &paths.backup,
                run_id,
                &intent,
            )?;
            validate_completed_projected_inventory(
                &paths.source,
                &intent,
                ownership_present,
            )?;
            #[cfg(test)]
            if fault == PublicationPhase::RebindCompletedSourceBeforeCleanup {
                rebind_directory_for_test(&paths.source)?;
            }
            validate_guard_at_child(
                selected_guard,
                runs_root_directory,
                paths.source.file_name().expect("source name"),
                "completed migrated run before transaction cleanup",
            )?;
            validate_guard_at_child(
                &backup_guard,
                runs_root_directory,
                paths.backup.file_name().expect("backup name"),
                "migration backup before transaction cleanup",
            )?;
            if ownership_present {
                remove_staged_ownership(
                    runs_root_directory,
                    paths.source.file_name().expect("source name"),
                    &intent,
                    Some(selected_guard),
                )?;
                #[cfg(test)]
                if fault == PublicationPhase::RebindCompletedSourceAfterMarkerRemoval {
                    rebind_directory_for_test(&paths.source)?;
                }
                validate_guard_at_child(
                    selected_guard,
                    runs_root_directory,
                    paths.source.file_name().expect("source name"),
                    "completed migrated run after ownership cleanup",
                )?;
                validate_guard_at_child(
                    &backup_guard,
                    runs_root_directory,
                    paths.backup.file_name().expect("backup name"),
                    "migration backup after ownership cleanup",
                )?;
            }
            remove_intent(runs_root_directory, intent_name)?;
            Ok(Some(migrated_outcome(
                run_id,
                paths,
                MigrationStatus::Recovered,
                read_result_paths(&paths.source)?,
            )))
        }
        _ => Err(MigrationError::new(format!(
            "ambiguous migration recovery state for run {run_id}: source={source}, staged={staged}, backup={backup}, intent={intent}"
        ))),
    }
}

fn acquire_existing_run_lock(
    run_directory: &Path,
    label: &str,
) -> Result<crate::run_persistence::RunMutationGuard, MigrationError> {
    crate::run_persistence::RunMutationGuard::acquire_existing(run_directory).map_err(|error| {
        MigrationError::context(
            format!(
                "could not acquire the existing mutation lock for {label} {}",
                run_directory.display()
            ),
            error,
        )
    })
}

fn require_guard<'a>(
    guard: Option<&'a crate::run_persistence::RunMutationGuard>,
    label: &str,
) -> Result<&'a crate::run_persistence::RunMutationGuard, MigrationError> {
    guard.ok_or_else(|| MigrationError::new(format!("{label} has no retained mutation guard")))
}

fn validate_guard_at_child(
    guard: &crate::run_persistence::RunMutationGuard,
    runs_root: &crate::artifact_safety::PinnedPrivateDirectory,
    name: &OsStr,
    label: &str,
) -> Result<(), MigrationError> {
    guard
        .validate_at_child(runs_root, name)
        .map_err(|error| MigrationError::context(format!("{label} identity changed"), error))
}

fn authenticate_run(
    run_directory: &Path,
    run_id: &str,
) -> Result<AuthenticatedRun, MigrationError> {
    validate_real_directory(run_directory, "selected run")?;
    let run_bytes = read_authenticated_file(run_directory, "run.json", "LoopRun")?;
    let run_value: Value = serde_json::from_slice(&run_bytes)
        .map_err(|error| MigrationError::context("invalid run.json JSON", error))?;
    let run: LoopRun = serde_json::from_value(run_value.clone())
        .map_err(|error| MigrationError::context("invalid LoopRun schema", error))?;
    let validation_run = normalize_legacy_run_for_validation(&run_value)?;
    let errors = validate_loop_run(&validation_run);
    if !errors.is_empty() {
        return Err(MigrationError::new(format!(
            "invalid LoopRun: {}",
            format_field_errors(errors)
        )));
    }
    if run.run_id != run_id {
        return Err(MigrationError::new(
            "persisted run_id does not match the selected run ID",
        ));
    }

    let mut managed_paths = BTreeSet::from([
        "inputs/ticket.json".to_string(),
        "ticket.snapshot.json".to_string(),
        "inputs/policy.json".to_string(),
        "run.json".to_string(),
    ]);
    let mut legacy_paths = BTreeSet::new();

    let ticket = load_contract::<TicketSpec>(
        run_directory,
        "inputs/ticket.json",
        "TicketSpec",
        &mut legacy_paths,
    )?;
    let ticket_snapshot = load_contract::<TicketSpec>(
        run_directory,
        "ticket.snapshot.json",
        "TicketSpec",
        &mut legacy_paths,
    )?;
    if ticket != ticket_snapshot {
        return Err(MigrationError::new(
            "ticket.snapshot.json does not match inputs/ticket.json",
        ));
    }
    ensure_valid("TicketSpec", validate_ticket_spec(&ticket))?;
    let ticket_bytes = read_authenticated_file(run_directory, "inputs/ticket.json", "TicketSpec")?;
    let snapshot_bytes =
        read_authenticated_file(run_directory, "ticket.snapshot.json", "TicketSpec")?;
    if ticket_bytes != snapshot_bytes || digest(&ticket_bytes) != run.input_digests.ticket {
        return Err(MigrationError::new(
            "ticket input/snapshot bytes or digest do not match LoopRun authority",
        ));
    }

    let policy = load_contract::<Policy>(
        run_directory,
        "inputs/policy.json",
        "Policy",
        &mut legacy_paths,
    )?;
    ensure_valid("Policy", validate_policy(&policy))?;
    let policy_bytes = read_authenticated_file(run_directory, "inputs/policy.json", "Policy")?;
    if digest(&policy_bytes) != run.input_digests.policy {
        return Err(MigrationError::new(
            "policy input digest does not match LoopRun authority",
        ));
    }

    for (relative, expected) in [
        (
            "inputs/config.json",
            Some(run.input_digests.config.as_str()),
        ),
        (
            "inputs/repository.json",
            Some(run.input_digests.repository.as_str()),
        ),
        (
            "inputs/eval-config.json",
            run.input_digests.eval_config.as_deref(),
        ),
    ] {
        if let Some(expected) = expected {
            verify_referenced_file(run_directory, relative, expected)?;
        }
    }

    let decision_paths = managed_policy_decision_paths(run_directory, &run)?;
    for relative in decision_paths {
        let decision = load_contract::<PolicyDecision>(
            run_directory,
            &relative,
            "PolicyDecision",
            &mut legacy_paths,
        )?;
        ensure_valid("PolicyDecision", validate_policy_decision(&decision))?;
        if !run.policy_decisions.contains(&decision) {
            return Err(MigrationError::new(format!(
                "managed {relative} is not one of the embedded LoopRun policy decisions"
            )));
        }
        managed_paths.insert(relative);
    }

    if let Some(decisions) = run_value.get("policy_decisions").and_then(Value::as_array) {
        for decision in decisions {
            if schema_state(decision, "embedded PolicyDecision")? == SchemaState::Legacy {
                legacy_paths.insert("run.json".to_string());
            }
        }
    }
    if schema_state(&run_value, "LoopRun")? == SchemaState::Legacy {
        legacy_paths.insert("run.json".to_string());
    }

    let eval_paths = managed_eval_report_paths(&run);
    for relative in eval_paths {
        let report =
            load_contract::<EvalReport>(run_directory, &relative, "EvalReport", &mut legacy_paths)?;
        ensure_valid("EvalReport", validate_eval_report(&report))?;
        managed_paths.insert(relative);
    }

    let graph = authenticate_reference_graph(run_directory, &run)?;
    let mut development_evidence_paths = BTreeSet::new();
    let workspace = crate::LoopWorkspace::open_staged_migration(run_directory)
        .map_err(|error| MigrationError::context("could not open migration run", error))?;
    for step in &run.steps {
        if step.name != seaf_core::LoopStepName::Development {
            continue;
        }
        match (&step.artifact_path, &step.artifact_digest) {
            (Some(path), Some(expected_digest)) => {
                crate::DevelopmentEvidence::load(&workspace, path, expected_digest, run_id)
                    .map_err(|error| {
                        MigrationError::context(
                            format!("invalid managed Development evidence at {path}"),
                            error,
                        )
                    })?;
                let bytes =
                    read_authenticated_file(run_directory, path, "managed Development evidence")?;
                let value: Value = serde_json::from_slice(&bytes)?;
                let decision = value.get("policy_decision").ok_or_else(|| {
                    MigrationError::new(format!(
                        "managed Development evidence {path} has no policy_decision"
                    ))
                })?;
                if schema_state(decision, "embedded Development PolicyDecision")?
                    == SchemaState::Legacy
                {
                    legacy_paths.insert(path.clone());
                    development_evidence_paths.insert(path.clone());
                }
            }
            (None, None) => {}
            _ => {
                return Err(MigrationError::new(
                    "Development step artifact path/digest authority is malformed",
                ))
            }
        }
    }
    let mut typed_rewrite_paths = graph.typed_rewrite_paths;
    typed_rewrite_paths.extend(managed_paths.iter().cloned());
    typed_rewrite_paths.extend(development_evidence_paths.iter().cloned());
    if !legacy_paths.is_empty()
        && matches!(
            run.status,
            seaf_core::LoopStatus::EvalPassed
                | seaf_core::LoopStatus::Promoted
                | seaf_core::LoopStatus::Failed
        )
        && run.human_approval.is_some()
        && run.latest_recovery.is_some()
    {
        return Err(MigrationError::new(
            "legacy terminal runs with evaluation recovery history are not supported by v0-to-v1 migration; preserve the run and use the current version that created its recovery authority",
        ));
    }
    Ok(AuthenticatedRun {
        managed_paths,
        development_evidence_paths,
        graph_json_paths: graph.json_paths,
        typed_rewrite_paths,
        legacy_paths,
    })
}

fn normalize_legacy_run_for_validation(run_value: &Value) -> Result<LoopRun, MigrationError> {
    let mut aliases = BTreeMap::new();
    if let Some(decisions) = run_value.get("policy_decisions").and_then(Value::as_array) {
        for raw in decisions {
            let typed: PolicyDecision = serde_json::from_value(raw.clone()).map_err(|error| {
                MigrationError::context("invalid embedded PolicyDecision schema", error)
            })?;
            let legacy_digest = digest(&canonical_json_bytes(raw)?);
            let current_digest = digest(&canonical_json_bytes(&typed)?);
            if legacy_digest != current_digest {
                aliases.insert(legacy_digest, current_digest);
            }
        }
    }
    let mut normalized = run_value.clone();
    replace_digest_fields(&mut normalized, &aliases);
    serde_json::from_value(normalized)
        .map_err(|error| MigrationError::context("invalid normalized LoopRun schema", error))
}

fn build_migration_plan(
    source: &Path,
    run_id: &str,
    authenticated: &AuthenticatedRun,
    intent: &MigrationIntent,
    staged_ownership: &StagedOwnership,
) -> Result<MigrationPlan, MigrationError> {
    let source_inventory = inventory_run_tree(source)?;
    if source_inventory.contains_key(STAGED_OWNERSHIP_FILE.as_bytes()) {
        return Err(MigrationError::new(format!(
            "legacy migration source contains reserved ownership path {STAGED_OWNERSHIP_FILE}"
        )));
    }
    if source_inventory.contains_key(RESULT_FILE.as_bytes()) {
        return Err(MigrationError::new(format!(
            "legacy migration source contains reserved result path {RESULT_FILE}"
        )));
    }
    let mut values = BTreeMap::<String, Value>::new();
    for relative in authenticated
        .graph_json_paths
        .iter()
        .chain(authenticated.managed_paths.iter())
    {
        let bytes = read_authenticated_file(source, relative, "authenticated migration artifact")?;
        if let Ok(value) = serde_json::from_slice(&bytes) {
            values.insert(relative.clone(), value);
        }
    }

    let original_values = values.clone();
    for relative in &authenticated.managed_paths {
        let value = values.get_mut(relative).ok_or_else(|| {
            MigrationError::new(format!("managed migration artifact {relative} is not JSON"))
        })?;
        insert_current_schema_version(value, relative)?;
        if relative == "run.json" {
            migrate_run_policy_decisions(value)?;
        }
    }
    for relative in &authenticated.development_evidence_paths {
        let value = values.get_mut(relative).ok_or_else(|| {
            MigrationError::new(format!(
                "managed Development evidence {relative} is not JSON"
            ))
        })?;
        migrate_development_policy_decision(value)?;
    }

    let mut aliases = BTreeMap::new();
    collect_value_digest_aliases(&original_values, &values, &mut aliases)?;
    let mut settled = false;
    for _ in 0..values.len().saturating_mul(4).saturating_add(16) {
        let before = values.clone();
        for (path, value) in &mut values {
            if authenticated.typed_rewrite_paths.contains(path) {
                replace_digest_fields(value, &aliases);
            }
        }
        collect_value_digest_aliases(&before, &values, &mut aliases)?;
        if before == values {
            settled = true;
            break;
        }
    }
    if !settled {
        return Err(MigrationError::new(
            "authenticated digest rewrite graph did not converge",
        ));
    }

    let mut rewritten_artifacts = BTreeMap::new();
    for (relative, value) in &values {
        if original_values.get(relative) != Some(value) {
            rewritten_artifacts.insert(relative.clone(), canonical_json_bytes(value)?);
        }
    }
    let run_after = rewritten_artifacts
        .get("run.json")
        .cloned()
        .unwrap_or(read_regular_file(
            &source.join("run.json"),
            "migrated LoopRun",
        )?);
    let result = MigrationResult {
        schema_version: RESULT_SCHEMA_VERSION,
        migration_id: MIGRATION_ID.to_string(),
        run_id: run_id.to_string(),
        from_schema_version: 0,
        to_schema_version: DURABLE_ARTIFACT_SCHEMA_VERSION,
        status: MigrationResultStatus::Migrated,
        backup_directory: format!(".{run_id}.migration-v0-v1.backup"),
        migrated_artifacts: rewritten_artifacts.keys().cloned().collect(),
        source_run_digest: intent.source_run_digest.clone(),
        source_tree_digest: intent.source_tree_digest.clone(),
        migrated_run_digest: digest(&run_after),
    };
    let result_bytes = canonical_json_bytes(&result)?;
    let marker_bytes = canonical_json_bytes(staged_ownership)?;
    let mut projected_staged_inventory = source_inventory;
    for (relative, bytes) in &rewritten_artifacts {
        let path = relative.as_bytes().to_vec();
        if !matches!(
            projected_staged_inventory.get(&path),
            Some(RunTreeEntry::File { .. })
        ) {
            return Err(MigrationError::new(format!(
                "projected migration rewrite is not an existing regular file: {relative}"
            )));
        }
        crate::artifact_storage::validate_artifact_size(relative, bytes.len())?;
        projected_staged_inventory.insert(path, inventory_file_entry(bytes));
    }
    for (relative, bytes) in [
        (RESULT_FILE, result_bytes.as_slice()),
        (STAGED_OWNERSHIP_FILE, marker_bytes.as_slice()),
    ] {
        crate::artifact_storage::validate_artifact_size(relative, bytes.len())?;
        projected_staged_inventory
            .insert(relative.as_bytes().to_vec(), inventory_file_entry(bytes));
    }
    validate_projected_migration_inventory(&projected_staged_inventory)?;

    Ok(MigrationPlan {
        rewritten_artifacts,
        result,
        result_bytes,
        projected_staged_inventory,
    })
}

fn migrate_staged_tree(
    staged: &Path,
    plan: &MigrationPlan,
    intent: &MigrationIntent,
    fault: PublicationPhase,
    staged_guard: &crate::run_persistence::RunMutationGuard,
    runs_root: &crate::artifact_safety::PinnedPrivateDirectory,
) -> Result<Vec<String>, MigrationError> {
    let mut wrote_migration_artifact = false;
    for (relative, bytes) in &plan.rewritten_artifacts {
        staged_guard.validate().map_err(|error| {
            MigrationError::context(
                format!("staged run changed before rewriting {relative}"),
                error,
            )
        })?;
        validate_guard_at_child(
            staged_guard,
            runs_root,
            staged.file_name().expect("staged name"),
            "staged run",
        )?;
        write_existing_bytes(staged, relative, bytes)?;
        staged_guard.validate().map_err(|error| {
            MigrationError::context(
                format!("staged run changed after rewriting {relative}"),
                error,
            )
        })?;
        validate_guard_at_child(
            staged_guard,
            runs_root,
            staged.file_name().expect("staged name"),
            "staged run",
        )?;
        if !wrote_migration_artifact {
            wrote_migration_artifact = true;
            inject_fault(fault, PublicationPhase::DuringRewrite)?;
        }
    }
    staged_guard.validate().map_err(|error| {
        MigrationError::context("staged run changed before result creation", error)
    })?;
    validate_guard_at_child(
        staged_guard,
        runs_root,
        staged.file_name().expect("staged name"),
        "staged run",
    )?;
    write_create_only_bytes(&staged.join(RESULT_FILE), &plan.result_bytes)?;
    sync_tree(staged)?;
    staged_guard.validate().map_err(|error| {
        MigrationError::context("staged run changed after result creation", error)
    })?;
    validate_guard_at_child(
        staged_guard,
        runs_root,
        staged.file_name().expect("staged name"),
        "staged run",
    )?;
    #[cfg(test)]
    if fault == PublicationPhase::DivergeStagedAfterProjection {
        crate::artifact_safety::write_private_fixture(
            staged.join("projection-drift.bin"),
            b"unprojected staged bytes\n",
        )
        .map_err(|error| {
            MigrationError::context("could not inject staged projection divergence", error)
        })?;
    }
    let actual_inventory = inventory_run_tree(staged)?;
    validate_projected_staged_inventory(&actual_inventory, intent)?;
    Ok(plan.result.migrated_artifacts.clone())
}

fn migrate_run_policy_decisions(value: &mut Value) -> Result<(), MigrationError> {
    let decisions = value
        .get_mut("policy_decisions")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| MigrationError::new("LoopRun policy_decisions must be an array"))?;
    for decision in decisions {
        insert_current_schema_version(decision, "embedded LoopRun PolicyDecision")?;
    }
    Ok(())
}

fn migrate_development_policy_decision(value: &mut Value) -> Result<(), MigrationError> {
    let decision = value.get_mut("policy_decision").ok_or_else(|| {
        MigrationError::new("managed Development evidence has no policy_decision")
    })?;
    insert_current_schema_version(decision, "embedded Development PolicyDecision")
}

fn authenticate_current_staged_run(
    staged: &Path,
    original: &Path,
    run_id: &str,
    intent: &MigrationIntent,
) -> Result<(), MigrationError> {
    authenticate_current_staged_run_unbound(staged, original, run_id, intent)?;
    let inventory = inventory_run_tree(staged)?;
    validate_projected_staged_inventory(&inventory, intent)
}

fn authenticate_current_staged_run_unbound(
    staged: &Path,
    original: &Path,
    run_id: &str,
    intent: &MigrationIntent,
) -> Result<(), MigrationError> {
    validate_staged_ownership(staged, run_id, intent)?;
    let authenticated = authenticate_run(staged, run_id)?;
    if !authenticated.legacy_paths.is_empty() {
        return Err(MigrationError::new(format!(
            "staged migration remains legacy at {}",
            authenticated
                .legacy_paths
                .into_iter()
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let result = load_migration_result(staged, run_id)?;
    if result.source_run_digest != intent.source_run_digest
        || result.source_tree_digest != intent.source_tree_digest
    {
        return Err(MigrationError::new(
            "staged migration result does not match the bound source authority",
        ));
    }
    validate_migrated_artifact_set(original, staged, &result.migrated_artifacts)?;
    validate_current_final_authority(staged)?;
    Ok(())
}

fn validate_completed_migration(
    source: &Path,
    backup: &Path,
    run_id: &str,
) -> Result<(), MigrationError> {
    let authenticated = authenticate_run(source, run_id)?;
    if !authenticated.legacy_paths.is_empty() {
        return Err(MigrationError::new(
            "completed migration selected run still contains legacy artifacts",
        ));
    }
    validate_current_final_authority(source)?;
    let result = load_migration_result(source, run_id)?;
    validate_source_binding(
        backup,
        &result.source_run_digest,
        &result.source_tree_digest,
        "retained migration backup",
    )?;
    validate_migrated_artifact_set(backup, source, &result.migrated_artifacts)
}

fn validate_completed_migration_with_intent(
    source: &Path,
    backup: &Path,
    run_id: &str,
    intent: &MigrationIntent,
) -> Result<(), MigrationError> {
    validate_completed_migration(source, backup, run_id)?;
    let result = load_migration_result(source, run_id)?;
    if result.source_run_digest != intent.source_run_digest
        || result.source_tree_digest != intent.source_tree_digest
    {
        return Err(MigrationError::new(
            "completed migration result does not match the retained intent",
        ));
    }
    Ok(())
}

fn load_migration_result(
    run_directory: &Path,
    run_id: &str,
) -> Result<MigrationResult, MigrationError> {
    let path = run_directory.join(RESULT_FILE);
    let result: MigrationResult = read_canonical_contract(&path, "migration result")?;
    if result.schema_version != RESULT_SCHEMA_VERSION
        || result.migration_id != MIGRATION_ID
        || result.run_id != run_id
        || result.from_schema_version != 0
        || result.to_schema_version != DURABLE_ARTIFACT_SCHEMA_VERSION
        || result.status != MigrationResultStatus::Migrated
        || result.backup_directory != format!(".{run_id}.migration-v0-v1.backup")
        || !is_digest(&result.source_run_digest)
        || !is_digest(&result.source_tree_digest)
        || !is_digest(&result.migrated_run_digest)
    {
        return Err(MigrationError::new(
            "migration result does not match the selected v0-to-v1 run",
        ));
    }
    if !result
        .migrated_artifacts
        .iter()
        .all(|path| is_portable_artifact_path(path))
        || !result
            .migrated_artifacts
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    {
        return Err(MigrationError::new(
            "migration result artifact list is not sorted, unique, and portable",
        ));
    }
    let migrated_run = read_authenticated_file(run_directory, "run.json", "migrated LoopRun")?;
    if digest(&migrated_run) != result.migrated_run_digest {
        return Err(MigrationError::new(
            "migration result migrated run digest does not match run.json",
        ));
    }
    Ok(result)
}

fn validate_current_final_authority(run_directory: &Path) -> Result<(), MigrationError> {
    let run_bytes = read_regular_file(&run_directory.join("run.json"), "LoopRun")?;
    let run: LoopRun = serde_json::from_slice(&run_bytes)
        .map_err(|error| MigrationError::context("invalid current LoopRun", error))?;
    if !matches!(
        run.status,
        seaf_core::LoopStatus::EvalPassed | seaf_core::LoopStatus::Promoted
    ) && !(run.status == seaf_core::LoopStatus::Failed && run.human_approval.is_some())
    {
        return Ok(());
    }
    let workspace = crate::LoopWorkspace::open_staged_migration(run_directory)
        .map_err(|error| MigrationError::context("could not open migrated final run", error))?;
    crate::final_evaluation_authority::load_verified_staged_final_evaluation_authority(
        &workspace, &run,
    )
    .map_err(|error| {
        MigrationError::context(
            "migrated final run failed existing final-evaluation authority verification",
            error,
        )
    })?;
    Ok(())
}

fn load_contract<T>(
    run_directory: &Path,
    relative: &str,
    kind: &str,
    legacy_paths: &mut BTreeSet<String>,
) -> Result<T, MigrationError>
where
    T: serde::de::DeserializeOwned,
{
    let bytes = read_authenticated_file(run_directory, relative, kind)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        MigrationError::context(format!("invalid {kind} JSON at {relative}"), error)
    })?;
    if schema_state(&value, kind)? == SchemaState::Legacy {
        legacy_paths.insert(relative.to_string());
    }
    serde_json::from_value(value).map_err(|error| {
        MigrationError::context(format!("invalid {kind} schema at {relative}"), error)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaState {
    Legacy,
    Current,
}

fn schema_state(value: &Value, kind: &str) -> Result<SchemaState, MigrationError> {
    let object = value
        .as_object()
        .ok_or_else(|| MigrationError::new(format!("{kind} must be a JSON object")))?;
    match object.get("schema_version") {
        None => Ok(SchemaState::Legacy),
        Some(Value::Number(number)) if number.as_u64() == Some(1) => Ok(SchemaState::Current),
        Some(Value::Number(number)) => Err(MigrationError::new(format!(
            "unsupported {kind} schema_version {number}; only missing legacy v0 or current v1 can be migrated"
        ))),
        Some(_) => Err(MigrationError::new(format!(
            "invalid {kind} schema_version; expected integer 1 or an omitted legacy version"
        ))),
    }
}

fn managed_policy_decision_paths(
    run_directory: &Path,
    run: &LoopRun,
) -> Result<BTreeSet<String>, MigrationError> {
    let mut paths = BTreeSet::new();
    let root = crate::artifact_safety::PinnedPrivateDirectory::open(run_directory)
        .map_err(|error| MigrationError::context("could not pin selected run", error))?;
    let artifacts = match root.open_child_directory(OsStr::new("artifacts")) {
        Ok(artifacts) => artifacts,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(paths),
        Err(error) => {
            return Err(MigrationError::context(
                "managed artifacts directory must be a real private directory",
                error,
            ))
        }
    };
    let stems = run
        .policy_decisions
        .iter()
        .map(|decision| safe_artifact_stem(&decision.patch_id))
        .collect::<BTreeSet<_>>();
    let mut names = Vec::new();
    artifacts
        .for_each_entry_name(|name| {
            names.push(
                name.to_str()
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "artifact name is not UTF-8")
                    })?
                    .to_string(),
            );
            Ok(())
        })
        .map_err(|error| MigrationError::context("could not enumerate managed artifacts", error))?;
    names.sort();
    for name in names {
        if stems
            .iter()
            .any(|stem| policy_decision_name_matches(&name, stem))
        {
            paths.insert(format!("artifacts/{name}"));
        }
    }
    Ok(paths)
}

fn policy_decision_name_matches(name: &str, stem: &str) -> bool {
    if name == format!("{stem}.policy-decision.json") {
        return true;
    }
    let Some(attempt) = name
        .strip_prefix(&format!("{stem}.attempt-"))
        .and_then(|tail| tail.strip_suffix(".policy-decision.json"))
    else {
        return false;
    };
    attempt.len() >= 3
        && attempt.bytes().all(|byte| byte.is_ascii_digit())
        && attempt
            .parse::<u32>()
            .is_ok_and(|value| value >= 2 && format!("{value:03}") == attempt)
}

fn safe_artifact_stem(patch_id: &str) -> String {
    let stem = patch_id
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(*character, '.' | '_' | '-')
        })
        .collect::<String>();
    if stem.is_empty() {
        "patch".to_string()
    } else {
        stem
    }
}

fn managed_eval_report_paths(run: &LoopRun) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    if let Some(path) = &run.eval_report_path {
        paths.insert(path.clone());
    }
    for step in &run.steps {
        if step.name == seaf_core::LoopStepName::EvalReport {
            if let Some(path) = &step.artifact_path {
                paths.insert(path.clone());
            }
        }
    }
    if let Some(promotion) = &run.promotion {
        paths.insert(promotion.eval_report.path.clone());
    }
    paths
}

fn authenticate_reference_graph(
    run_directory: &Path,
    run: &LoopRun,
) -> Result<AuthenticatedGraph, MigrationError> {
    let mut json_paths = BTreeSet::from(["run.json".to_string()]);
    let mut queue = VecDeque::new();
    collect_loop_run_references(run, &mut queue)?;
    let mut visited = BTreeMap::<String, String>::new();
    let mut processed = BTreeSet::<(String, GraphArtifactKind)>::new();
    let mut typed_rewrite_paths = BTreeSet::from(["run.json".to_string()]);
    let workspace = crate::LoopWorkspace::open_staged_migration(run_directory)
        .map_err(|error| MigrationError::context("could not open migration graph", error))?;
    while let Some(reference) = queue.pop_front() {
        if let Some(previous) = visited.get(&reference.path) {
            if previous != &reference.digest {
                return Err(MigrationError::new(format!(
                    "authenticated artifact {} has conflicting digests {previous} and {}",
                    reference.path, reference.digest
                )));
            }
        } else {
            verify_referenced_file(run_directory, &reference.path, &reference.digest)?;
            visited.insert(reference.path.clone(), reference.digest.clone());
        }
        if !processed.insert((reference.path.clone(), reference.kind)) {
            continue;
        }
        let bytes =
            read_authenticated_file(run_directory, &reference.path, "authenticated reference")?;
        if reference.path.ends_with(".json") {
            let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
                MigrationError::context(
                    format!("authenticated JSON reference {} is invalid", reference.path),
                    error,
                )
            })?;
            let canonical = canonical_json_bytes(&value)?;
            if canonical != bytes {
                return Err(MigrationError::new(format!(
                    "authenticated JSON reference {} is not canonical",
                    reference.path
                )));
            }
            match reference.kind {
                GraphArtifactKind::Generic => {}
                GraphArtifactKind::DevelopmentEvidence => {
                    typed_rewrite_paths.insert(reference.path.clone());
                }
                GraphArtifactKind::EvalReport => {
                    let report: EvalReport =
                        serde_json::from_value(value.clone()).map_err(|error| {
                            MigrationError::context(
                                format!("invalid typed EvalReport at {}", reference.path),
                                error,
                            )
                        })?;
                    ensure_valid("EvalReport", validate_eval_report(&report))?;
                    collect_eval_report_references(&report, &mut queue)?;
                    typed_rewrite_paths.insert(reference.path.clone());
                }
                GraphArtifactKind::TestingEvidence => {
                    if value.get("schema_version").is_some() {
                        let testing: crate::TestingEvidence = serde_json::from_value(value.clone())
                            .map_err(|error| {
                                MigrationError::context(
                                    format!("invalid typed Testing evidence at {}", reference.path),
                                    error,
                                )
                            })?;
                        testing.validate().map_err(|error| {
                            MigrationError::context(
                                format!("invalid typed Testing evidence at {}", reference.path),
                                error,
                            )
                        })?;
                        collect_testing_references(&testing, &mut queue)?;
                        typed_rewrite_paths.insert(reference.path.clone());
                    }
                }
                GraphArtifactKind::EvaluationIntent => {
                    let artifact = ArtifactReference {
                        path: reference.path.clone(),
                        digest: reference.digest.clone(),
                    };
                    let intent = crate::evaluation_attempt::load_intent(&workspace, &artifact)
                        .map_err(|error| {
                            MigrationError::context(
                                format!("invalid typed evaluation intent at {}", reference.path),
                                error,
                            )
                        })?;
                    collect_evaluation_intent_references(&intent, &mut queue)?;
                    typed_rewrite_paths.insert(reference.path.clone());
                }
                GraphArtifactKind::ProviderExchangeRecord => {
                    let record: ProviderExchangeRecord =
                        serde_json::from_value(value).map_err(|error| {
                            MigrationError::context(
                                format!(
                                    "invalid typed provider exchange record at {}",
                                    reference.path
                                ),
                                error,
                            )
                        })?;
                    collect_provider_record_references(&record, &mut queue)?;
                }
            }
            json_paths.insert(reference.path);
        } else if reference.kind != GraphArtifactKind::Generic {
            return Err(MigrationError::new(format!(
                "typed authenticated artifact {} must be canonical JSON",
                reference.path
            )));
        }
    }
    Ok(AuthenticatedGraph {
        json_paths,
        typed_rewrite_paths,
    })
}

fn collect_loop_run_references(
    run: &LoopRun,
    queue: &mut VecDeque<GraphReference>,
) -> Result<(), MigrationError> {
    for step in &run.steps {
        match (&step.artifact_path, &step.artifact_digest) {
            (Some(path), Some(digest)) => {
                let kind = match step.name {
                    seaf_core::LoopStepName::Development => GraphArtifactKind::DevelopmentEvidence,
                    seaf_core::LoopStepName::Testing => GraphArtifactKind::TestingEvidence,
                    seaf_core::LoopStepName::EvalReport => GraphArtifactKind::EvalReport,
                    _ => GraphArtifactKind::Generic,
                };
                enqueue_reference(queue, path, digest, kind)?;
            }
            (None, None) => {}
            _ => {
                return Err(MigrationError::new(
                    "step artifact path/digest authority is malformed",
                ))
            }
        }
    }
    for record in &run.provider_exchange_records {
        enqueue_reference(
            queue,
            &record.path,
            &record.digest,
            GraphArtifactKind::ProviderExchangeRecord,
        )?;
    }
    if let Some(candidate) = &run.candidate_workspace {
        if let Some(transaction) = &candidate.patch_transaction {
            enqueue_artifact(queue, &transaction.intent, GraphArtifactKind::Generic)?;
            if let Some(applied) = &transaction.applied_evidence {
                enqueue_artifact(queue, applied, GraphArtifactKind::Generic)?;
            }
        }
    }
    if let Some(approval) = &run.human_approval {
        enqueue_artifact(queue, &approval.candidate_diff, GraphArtifactKind::Generic)?;
        enqueue_artifact(queue, &approval.output_review, GraphArtifactKind::Generic)?;
        enqueue_reference(
            queue,
            &approval.output_review_request.path,
            &approval.output_review_request.digest,
            GraphArtifactKind::ProviderExchangeRecord,
        )?;
        enqueue_reference(
            queue,
            &approval.output_review_response.path,
            &approval.output_review_response.digest,
            GraphArtifactKind::ProviderExchangeRecord,
        )?;
    }
    if let Some(promotion) = &run.promotion {
        enqueue_artifact(queue, &promotion.intent, GraphArtifactKind::Generic)?;
        enqueue_artifact(queue, &promotion.candidate_diff, GraphArtifactKind::Generic)?;
        enqueue_artifact(
            queue,
            &promotion.testing_evidence,
            GraphArtifactKind::TestingEvidence,
        )?;
        enqueue_artifact(queue, &promotion.eval_report, GraphArtifactKind::EvalReport)?;
    }
    if let Some(recovery) = &run.latest_recovery {
        enqueue_artifact(queue, &recovery.artifact, GraphArtifactKind::Generic)?;
    }
    Ok(())
}

fn collect_eval_report_references(
    report: &EvalReport,
    queue: &mut VecDeque<GraphReference>,
) -> Result<(), MigrationError> {
    if let Some(evidence) = &report.loop_evidence {
        enqueue_artifact(queue, &evidence.eval_config, GraphArtifactKind::Generic)?;
        enqueue_artifact(queue, &evidence.candidate_diff, GraphArtifactKind::Generic)?;
        enqueue_artifact(
            queue,
            &evidence.testing_evidence,
            GraphArtifactKind::TestingEvidence,
        )?;
    }
    collect_check_references(&report.checks, queue)
}

fn collect_testing_references(
    testing: &crate::TestingEvidence,
    queue: &mut VecDeque<GraphReference>,
) -> Result<(), MigrationError> {
    if let Some(Some(recovery)) = &testing.recovery {
        enqueue_artifact(queue, &recovery.artifact, GraphArtifactKind::Generic)?;
    }
    if let Some(intent) = &testing.execution_intent {
        enqueue_artifact(queue, intent, GraphArtifactKind::EvaluationIntent)?;
    }
    enqueue_artifact(queue, &testing.eval_config, GraphArtifactKind::Generic)?;
    enqueue_artifact(queue, &testing.candidate_diff, GraphArtifactKind::Generic)?;
    collect_check_references(&testing.checks, queue)
}

fn collect_check_references(
    checks: &[seaf_core::EvalCheck],
    queue: &mut VecDeque<GraphReference>,
) -> Result<(), MigrationError> {
    for check in checks {
        for (path, digest) in [
            (&check.stdout_path, &check.stdout_digest),
            (&check.stderr_path, &check.stderr_digest),
        ] {
            match (path, digest) {
                (Some(path), Some(digest)) => {
                    enqueue_reference(queue, path, digest, GraphArtifactKind::Generic)?;
                }
                (None, None) => {}
                _ => {
                    return Err(MigrationError::new(
                        "typed evaluation check path/digest authority is malformed",
                    ))
                }
            }
        }
    }
    Ok(())
}

fn collect_provider_record_references(
    record: &ProviderExchangeRecord,
    queue: &mut VecDeque<GraphReference>,
) -> Result<(), MigrationError> {
    enqueue_artifact(queue, &record.request, GraphArtifactKind::Generic)?;
    if let Some(response) = &record.response {
        enqueue_artifact(queue, response, GraphArtifactKind::Generic)?;
    }
    if let Some(expansion) = &record.expansion {
        enqueue_artifact(queue, expansion, GraphArtifactKind::Generic)?;
    }
    Ok(())
}

fn collect_evaluation_intent_references(
    intent: &crate::evaluation_attempt::ApprovedEvaluationIntent,
    queue: &mut VecDeque<GraphReference>,
) -> Result<(), MigrationError> {
    use crate::evaluation_attempt::ApprovedEvaluationIntent;

    match intent {
        ApprovedEvaluationIntent::V1(intent) => {
            enqueue_artifact(queue, &intent.ticket, GraphArtifactKind::Generic)?;
            enqueue_artifact(queue, &intent.eval_config, GraphArtifactKind::Generic)?;
            enqueue_artifact(queue, &intent.candidate_diff, GraphArtifactKind::Generic)?;
        }
        ApprovedEvaluationIntent::V2(intent) => {
            enqueue_artifact(queue, &intent.ticket, GraphArtifactKind::Generic)?;
            enqueue_artifact(queue, &intent.eval_config, GraphArtifactKind::Generic)?;
            enqueue_artifact(queue, &intent.candidate_diff, GraphArtifactKind::Generic)?;
            if let Some(recovery) = &intent.recovery {
                enqueue_artifact(queue, &recovery.artifact, GraphArtifactKind::Generic)?;
            }
        }
        ApprovedEvaluationIntent::V3(intent) => {
            enqueue_artifact(queue, &intent.ticket, GraphArtifactKind::Generic)?;
            enqueue_artifact(queue, &intent.eval_config, GraphArtifactKind::Generic)?;
            enqueue_artifact(queue, &intent.candidate_diff, GraphArtifactKind::Generic)?;
            if let Some(recovery) = &intent.recovery {
                enqueue_artifact(queue, &recovery.artifact, GraphArtifactKind::Generic)?;
            }
        }
    }
    Ok(())
}

fn enqueue_artifact(
    queue: &mut VecDeque<GraphReference>,
    reference: &ArtifactReference,
    kind: GraphArtifactKind,
) -> Result<(), MigrationError> {
    enqueue_reference(queue, &reference.path, &reference.digest, kind)
}

fn enqueue_reference(
    queue: &mut VecDeque<GraphReference>,
    path: &str,
    digest: &str,
    kind: GraphArtifactKind,
) -> Result<(), MigrationError> {
    validate_reference(path, digest)?;
    queue.push_back(GraphReference {
        path: path.to_string(),
        digest: digest.to_string(),
        kind,
    });
    Ok(())
}

fn validate_reference(path: &str, digest_value: &str) -> Result<(), MigrationError> {
    if !is_portable_artifact_path(path) {
        return Err(MigrationError::new(format!(
            "unsafe authenticated artifact path {path}"
        )));
    }
    if !is_digest(digest_value) {
        return Err(MigrationError::new(format!(
            "invalid authenticated digest for {path}"
        )));
    }
    Ok(())
}

fn verify_referenced_file(
    run_directory: &Path,
    relative: &str,
    expected_digest: &str,
) -> Result<(), MigrationError> {
    if !is_portable_artifact_path(relative) {
        return Err(MigrationError::new(format!(
            "unsafe authenticated artifact path {relative}"
        )));
    }
    let bytes = read_authenticated_file(run_directory, relative, "authenticated artifact")?;
    let actual = digest(&bytes);
    if actual != expected_digest {
        return Err(MigrationError::new(format!(
            "authenticated artifact {relative} digest mismatch: expected {expected_digest}, got {actual}"
        )));
    }
    Ok(())
}

fn collect_value_digest_aliases(
    before: &BTreeMap<String, Value>,
    after: &BTreeMap<String, Value>,
    aliases: &mut BTreeMap<String, String>,
) -> Result<(), MigrationError> {
    for (path, before) in before {
        if let Some(after) = after.get(path) {
            collect_node_digest_aliases(before, after, aliases)?;
            if path == "run.json" {
                collect_historical_run_digest_aliases(before, after, aliases)?;
            }
        }
    }
    Ok(())
}

fn collect_historical_run_digest_aliases(
    before: &Value,
    after: &Value,
    aliases: &mut BTreeMap<String, String>,
) -> Result<(), MigrationError> {
    let Some(before_approved) = reconstruct_unrecovered_approved_value(before)? else {
        return Ok(());
    };
    let Some(after_approved) = reconstruct_unrecovered_approved_value(after)? else {
        return Ok(());
    };
    let before_digest = digest(&canonical_json_bytes(&before_approved)?);
    let after_digest = digest(&canonical_json_bytes(&after_approved)?);
    if before_digest != after_digest {
        aliases.insert(before_digest, after_digest);
    }
    Ok(())
}

fn reconstruct_unrecovered_approved_value(
    final_run: &Value,
) -> Result<Option<Value>, MigrationError> {
    let Some(status) = final_run.get("status").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !matches!(status, "eval_passed" | "promoted" | "failed")
        || final_run.get("human_approval").is_none_or(Value::is_null)
        || final_run
            .get("latest_recovery")
            .is_some_and(|value| !value.is_null())
    {
        return Ok(None);
    }
    let typed: LoopRun = serde_json::from_value(final_run.clone()).map_err(|error| {
        MigrationError::context("invalid final LoopRun during historical projection", error)
    })?;
    let projected = crate::final_evaluation_authority::project_unrecovered_approved_authority(
        &typed,
    )
    .map_err(|error| {
        MigrationError::context("could not project historical Approved authority", error)
    })?;
    let mut projected = serde_json::to_value(projected)?;
    if schema_state(final_run, "LoopRun")? == SchemaState::Legacy {
        projected
            .as_object_mut()
            .expect("projected LoopRun object")
            .remove("schema_version");
    }
    if let (Some(source), Some(projected_decisions)) = (
        final_run.get("policy_decisions").and_then(Value::as_array),
        projected
            .get_mut("policy_decisions")
            .and_then(Value::as_array_mut),
    ) {
        for (source, projected) in source.iter().zip(projected_decisions) {
            if schema_state(source, "embedded PolicyDecision")? == SchemaState::Legacy {
                projected
                    .as_object_mut()
                    .expect("projected PolicyDecision object")
                    .remove("schema_version");
            }
        }
    }
    Ok(Some(projected))
}

fn collect_node_digest_aliases(
    before: &Value,
    after: &Value,
    aliases: &mut BTreeMap<String, String>,
) -> Result<(), MigrationError> {
    let before_digest = digest(&canonical_json_bytes(before)?);
    let after_digest = digest(&canonical_json_bytes(after)?);
    if before_digest != after_digest {
        aliases.insert(before_digest, after_digest);
    }
    match (before, after) {
        (Value::Object(before), Value::Object(after)) => {
            for (key, before) in before {
                if let Some(after) = after.get(key) {
                    collect_node_digest_aliases(before, after, aliases)?;
                }
            }
        }
        (Value::Array(before), Value::Array(after)) if before.len() == after.len() => {
            for (before, after) in before.iter().zip(after) {
                collect_node_digest_aliases(before, after, aliases)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn replace_digest_fields(value: &mut Value, aliases: &BTreeMap<String, String>) {
    match value {
        Value::Object(object) => {
            let mut digest_keys = BTreeSet::new();
            if object.get("path").is_some_and(Value::is_string)
                && object.get("digest").is_some_and(Value::is_string)
            {
                digest_keys.insert("digest".to_string());
            }
            for (key, path) in object.iter() {
                if path.is_string() {
                    if let Some(prefix) = key.strip_suffix("_path") {
                        let digest_key = format!("{prefix}_digest");
                        if object.get(&digest_key).is_some_and(Value::is_string) {
                            digest_keys.insert(digest_key);
                        }
                    }
                }
            }
            for key in object.keys() {
                if is_known_typed_digest_field(key) {
                    digest_keys.insert(key.clone());
                }
            }
            for (key, child) in object {
                if digest_keys.contains(key) {
                    if let Value::String(string) = child {
                        replace_digest_value(string, aliases);
                    }
                } else if key == "input_digests" {
                    replace_named_digest_map(child, aliases);
                } else {
                    replace_digest_fields(child, aliases);
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                replace_digest_fields(child, aliases);
            }
        }
        _ => {}
    }
}

fn is_known_typed_digest_field(key: &str) -> bool {
    matches!(
        key,
        "approved_run_digest"
            | "artifact_digest"
            | "candidate_diff_digest"
            | "candidate_state_digest"
            | "developer_response_digest"
            | "eval_passed_run_digest"
            | "eval_report_digest"
            | "expected_final_projection_digest"
            | "expected_reset_projection_digest"
            | "human_approval_digest"
            | "patch_digest"
            | "policy_decision_digest"
            | "previous_record_digest"
            | "repository_identity_digest"
            | "run_directory_digest"
            | "source_run_digest"
            | "source_worktree_state_digest"
            | "stderr_digest"
            | "stdout_digest"
            | "ticket_digest"
    )
}

fn replace_named_digest_map(value: &mut Value, aliases: &BTreeMap<String, String>) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    for value in object.values_mut() {
        if let Value::String(value) = value {
            replace_digest_value(value, aliases);
        }
    }
}

fn replace_digest_value(value: &mut String, aliases: &BTreeMap<String, String>) {
    let mut current = value.clone();
    let mut seen = BTreeSet::new();
    while seen.insert(current.clone()) {
        let Some(next) = aliases.get(&current) else {
            break;
        };
        current = next.clone();
    }
    *value = current;
}

fn insert_current_schema_version(value: &mut Value, kind: &str) -> Result<(), MigrationError> {
    match schema_state(value, kind)? {
        SchemaState::Legacy => {
            value
                .as_object_mut()
                .expect("schema state checked object")
                .insert(
                    "schema_version".to_string(),
                    json!(DURABLE_ARTIFACT_SCHEMA_VERSION),
                );
        }
        SchemaState::Current => {}
    }
    Ok(())
}

fn validate_run_id(run_id: &str) -> Result<(), MigrationError> {
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
        Err(MigrationError::new(
            "invalid run ID; use only ASCII letters, numbers, '-' or '_'",
        ))
    }
}

fn validate_runs_root(runs_root: &Path) -> Result<(), MigrationError> {
    validate_real_directory(runs_root, "runs root")?;
    Ok(())
}

fn validate_real_directory(path: &Path, kind: &str) -> Result<(), MigrationError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        MigrationError::context(
            format!("could not inspect {kind} {}", path.display()),
            error,
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(MigrationError::new(format!(
            "{kind} {} must be a real directory",
            path.display()
        )));
    }
    Ok(())
}

fn read_regular_file(path: &Path, kind: &str) -> Result<Vec<u8>, MigrationError> {
    let parent_path = path
        .parent()
        .ok_or_else(|| MigrationError::new(format!("{kind} path has no parent")))?;
    let name = path
        .file_name()
        .ok_or_else(|| MigrationError::new(format!("{kind} path has no file name")))?;
    let parent = crate::artifact_safety::PinnedPrivateDirectory::open_parent(parent_path)
        .map_err(|error| MigrationError::context(format!("could not pin {kind} parent"), error))?;
    let mut file = parent
        .open_existing_regular_file_any_mode(name)
        .map_err(|error| {
            MigrationError::context(
                format!("could not safely open {kind} {}", path.display()),
                error,
            )
        })?;
    let metadata = file.metadata()?;
    let relative = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| MigrationError::new(format!("{kind} file name is not portable UTF-8")))?;
    crate::artifact_storage::validate_artifact_size_u64(relative, metadata.len())
        .map_err(|error| MigrationError::context(format!("invalid {kind} size"), error))?;
    let bytes = read_bounded_file(&mut file, metadata.len(), relative, kind)?;
    parent
        .validate_single_link_file(name, &metadata)
        .map_err(|error| MigrationError::context(format!("{kind} identity changed"), error))?;
    Ok(bytes)
}

fn read_authenticated_file(
    run_directory: &Path,
    relative: &str,
    kind: &str,
) -> Result<Vec<u8>, MigrationError> {
    if !is_portable_artifact_path(relative) {
        return Err(MigrationError::new(format!(
            "unsafe authenticated artifact path {relative}"
        )));
    }
    let relative_path = Path::new(relative);
    let parent =
        crate::artifact_safety::open_private_descendant_parent(run_directory, relative_path)
            .map_err(|error| {
                MigrationError::context(
                    format!("authenticated {kind} parent for {relative} is unsafe"),
                    error,
                )
            })?;
    let name = relative_path.file_name().ok_or_else(|| {
        MigrationError::new(format!(
            "authenticated {kind} path has no file name: {relative}"
        ))
    })?;
    let mut file = parent
        .open_existing_file(name, true, false)
        .map_err(|error| {
            MigrationError::context(
                format!("could not safely open authenticated {kind} {relative}"),
                error,
            )
        })?;
    let metadata = file.metadata()?;
    parent
        .validate_single_link_file(name, &metadata)
        .map_err(|error| {
            MigrationError::context(
                format!("authenticated {kind} identity changed for {relative}"),
                error,
            )
        })?;
    crate::artifact_storage::validate_artifact_size_u64(relative, metadata.len())
        .map_err(|error| MigrationError::context(format!("invalid {kind} size"), error))?;
    let bytes = read_bounded_file(&mut file, metadata.len(), relative, kind)?;
    parent
        .validate_single_link_file(name, &metadata)
        .map_err(|error| {
            MigrationError::context(
                format!("authenticated {kind} identity changed for {relative}"),
                error,
            )
        })?;
    Ok(bytes)
}

fn read_bounded_file(
    mut file: impl Read,
    expected_len: u64,
    relative: &str,
    kind: &str,
) -> Result<Vec<u8>, MigrationError> {
    let cap = crate::artifact_storage::artifact_byte_cap(relative);
    if expected_len > cap {
        return Err(MigrationError::new(format!(
            "{kind} {relative} exceeds its {cap}-byte cap: {expected_len} bytes"
        )));
    }
    let capacity = usize::try_from(expected_len)
        .map_err(|_| MigrationError::new(format!("{kind} size is not representable")))?;
    let mut bytes = Vec::with_capacity(capacity);
    file.by_ref()
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| {
            MigrationError::context(format!("could not read {kind} {relative}"), error)
        })?;
    if bytes.len() as u64 != expected_len {
        return Err(MigrationError::new(format!(
            "{kind} {relative} changed length while it was read"
        )));
    }
    Ok(bytes)
}

fn digest_run_tree(run_directory: &Path) -> Result<String, MigrationError> {
    let inventory = inventory_run_tree(run_directory)?;
    Ok(digest_run_tree_inventory(&inventory))
}

fn digest_run_tree_inventory(inventory: &RunTreeInventory) -> String {
    let mut hasher = Sha256::new();
    for (path, entry) in inventory {
        match entry {
            RunTreeEntry::Directory => hash_tree_record(&mut hasher, b"directory", path, &[]),
            RunTreeEntry::File { len, digest } => {
                let mut payload = Vec::with_capacity(40);
                payload.extend_from_slice(&len.to_be_bytes());
                payload.extend_from_slice(digest);
                hash_tree_record(&mut hasher, b"file", path, &payload);
            }
            RunTreeEntry::Symlink(target) => {
                hash_tree_record(&mut hasher, b"symlink", path, target)
            }
        }
    }
    hex::encode(hasher.finalize())
}

fn inventory_file_entry(bytes: &[u8]) -> RunTreeEntry {
    RunTreeEntry::File {
        len: bytes.len() as u64,
        digest: Sha256::digest(bytes).into(),
    }
}

fn validate_projected_migration_inventory(
    inventory: &RunTreeInventory,
) -> Result<(), MigrationError> {
    if inventory.len() > crate::artifact_storage::RUN_TREE_ENTRY_CAP {
        return Err(MigrationError::new(format!(
            "projected migration tree exceeds its {}-entry cap",
            crate::artifact_storage::RUN_TREE_ENTRY_CAP
        )));
    }
    let bytes = inventory.values().try_fold(0_u64, |total, entry| {
        let len = match entry {
            RunTreeEntry::File { len, .. } => *len,
            RunTreeEntry::Directory | RunTreeEntry::Symlink(_) => 0,
        };
        total
            .checked_add(len)
            .ok_or_else(|| MigrationError::new("projected migration byte total overflowed"))
    })?;
    if bytes > crate::artifact_storage::RUN_TREE_BYTE_CAP {
        return Err(MigrationError::new(format!(
            "projected migration tree exceeds its {}-byte cap",
            crate::artifact_storage::RUN_TREE_BYTE_CAP
        )));
    }
    Ok(())
}

fn validate_projected_staged_inventory(
    inventory: &RunTreeInventory,
    intent: &MigrationIntent,
) -> Result<(), MigrationError> {
    let actual = digest_run_tree_inventory(inventory);
    if actual != intent.projected_staged_inventory_digest {
        return Err(MigrationError::new(format!(
            "actual staged migration inventory does not match the intent-bound projection: expected {}, got {actual}",
            intent.projected_staged_inventory_digest
        )));
    }
    Ok(())
}

fn validate_completed_projected_inventory(
    source: &Path,
    intent: &MigrationIntent,
    ownership_present: bool,
) -> Result<(), MigrationError> {
    let mut inventory = inventory_run_tree(source)?;
    if !ownership_present {
        let ownership = staged_ownership_from_intent(intent);
        let bytes = canonical_json_bytes(&ownership)?;
        let replaced = inventory.insert(
            STAGED_OWNERSHIP_FILE.as_bytes().to_vec(),
            inventory_file_entry(&bytes),
        );
        if replaced.is_some() {
            return Err(MigrationError::new(
                "completed migration ownership-marker state changed during validation",
            ));
        }
    }
    validate_projected_staged_inventory(&inventory, intent)
}

fn inventory_run_tree(run_directory: &Path) -> Result<RunTreeInventory, MigrationError> {
    let root = crate::artifact_safety::PinnedPrivateDirectory::open(run_directory)
        .map_err(|error| MigrationError::context("could not pin run tree for inventory", error))?;
    inventory_pinned_tree(&root)
}

fn inventory_pinned_tree(
    root: &crate::artifact_safety::PinnedPrivateDirectory,
) -> Result<RunTreeInventory, MigrationError> {
    let mut inventory = BTreeMap::new();
    let mut entries = 0;
    let mut bytes = 0;
    inventory_pinned_directory(
        root,
        Path::new(""),
        0,
        &mut entries,
        &mut bytes,
        &mut inventory,
    )?;
    Ok(inventory)
}

fn inventory_pinned_directory(
    directory: &crate::artifact_safety::PinnedPrivateDirectory,
    prefix: &Path,
    depth: usize,
    entries: &mut usize,
    total_bytes: &mut u64,
    inventory: &mut RunTreeInventory,
) -> Result<(), MigrationError> {
    directory
        .validate_identity()
        .map_err(|error| MigrationError::context("run tree directory changed", error))?;
    let names = bounded_sorted_entry_names(directory, "run tree")?;
    for name in names {
        *entries = entries
            .checked_add(1)
            .ok_or_else(|| MigrationError::new("run tree entry count overflowed"))?;
        if *entries > crate::artifact_storage::RUN_TREE_ENTRY_CAP {
            return Err(MigrationError::new(format!(
                "run tree exceeds its {}-entry cap",
                crate::artifact_storage::RUN_TREE_ENTRY_CAP
            )));
        }
        let relative = prefix.join(&name);
        let relative_bytes = path_bytes(&relative);
        match directory
            .entry_kind(&name)
            .map_err(|error| MigrationError::context("could not inspect run tree entry", error))?
        {
            crate::artifact_safety::PinnedEntryKind::Directory => {
                let child_depth = depth
                    .checked_add(1)
                    .ok_or_else(|| MigrationError::new("run tree directory depth overflowed"))?;
                if child_depth > crate::artifact_storage::RUN_TREE_DIRECTORY_DEPTH_CAP {
                    return Err(MigrationError::new(format!(
                        "run tree exceeds its {}-directory depth cap",
                        crate::artifact_storage::RUN_TREE_DIRECTORY_DEPTH_CAP
                    )));
                }
                inventory.insert(relative_bytes, RunTreeEntry::Directory);
                let child = directory.open_child_directory(&name).map_err(|error| {
                    MigrationError::context(
                        format!("could not pin run tree directory {}", relative.display()),
                        error,
                    )
                })?;
                inventory_pinned_directory(
                    &child,
                    &relative,
                    child_depth,
                    entries,
                    total_bytes,
                    inventory,
                )?;
            }
            crate::artifact_safety::PinnedEntryKind::RegularFile => {
                let mut file = directory
                    .open_existing_regular_file_any_mode(&name)
                    .map_err(|error| {
                        MigrationError::context(
                            format!("could not safely read run tree file {}", relative.display()),
                            error,
                        )
                    })?;
                let metadata = file.metadata()?;
                let relative_string = relative.to_str().ok_or_else(|| {
                    MigrationError::new("run artifact path is not portable UTF-8")
                })?;
                crate::artifact_storage::validate_artifact_size_u64(
                    relative_string,
                    metadata.len(),
                )
                .map_err(|error| MigrationError::context("invalid run artifact size", error))?;
                *total_bytes = total_bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| MigrationError::new("run tree byte total overflowed"))?;
                if *total_bytes > crate::artifact_storage::RUN_TREE_BYTE_CAP {
                    return Err(MigrationError::new(format!(
                        "run tree exceeds its {}-byte cap",
                        crate::artifact_storage::RUN_TREE_BYTE_CAP
                    )));
                }
                let mut hasher = Sha256::new();
                let mut copied = 0_u64;
                let mut limited = file.by_ref().take(metadata.len().saturating_add(1));
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let read = limited.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    copied = copied
                        .checked_add(read as u64)
                        .ok_or_else(|| MigrationError::new("run tree file length overflowed"))?;
                    hasher.update(&buffer[..read]);
                }
                if copied != metadata.len() {
                    return Err(MigrationError::new(format!(
                        "run tree file {} changed length while it was inventoried",
                        relative.display()
                    )));
                }
                directory
                    .validate_single_link_file(&name, &metadata)
                    .map_err(|error| {
                        MigrationError::context(
                            format!("run tree file identity changed: {}", relative.display()),
                            error,
                        )
                    })?;
                inventory.insert(
                    relative_bytes,
                    RunTreeEntry::File {
                        len: metadata.len(),
                        digest: hasher.finalize().into(),
                    },
                );
            }
            crate::artifact_safety::PinnedEntryKind::Other => {
                let target = directory.read_symlink(&name, 4096).map_err(|error| {
                    MigrationError::context(
                        format!(
                            "run tree contains an unsafe special entry {}",
                            relative.display()
                        ),
                        error,
                    )
                })?;
                inventory.insert(relative_bytes, RunTreeEntry::Symlink(target));
            }
        }
    }
    directory
        .validate_identity()
        .map_err(|error| MigrationError::context("run tree directory changed", error))?;
    Ok(())
}

fn validate_migrated_artifact_set(
    original_directory: &Path,
    migrated_directory: &Path,
    reported: &[String],
) -> Result<(), MigrationError> {
    let original = inventory_run_tree(original_directory)?;
    let migrated = inventory_run_tree(migrated_directory)?;
    let result_path = RESULT_FILE.as_bytes().to_vec();
    let ownership_path = STAGED_OWNERSHIP_FILE.as_bytes().to_vec();
    if original.contains_key(&result_path)
        || !matches!(migrated.get(&result_path), Some(RunTreeEntry::File { .. }))
    {
        return Err(MigrationError::new(
            "migration result path must be the only new regular file in the migrated tree",
        ));
    }
    if original.contains_key(&ownership_path) {
        return Err(MigrationError::new(
            "original migration authority contains the reserved staged ownership marker",
        ));
    }

    let mut paths = original
        .keys()
        .chain(migrated.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    paths.remove(&result_path);
    paths.remove(&ownership_path);
    let mut actual = Vec::new();
    for path in paths {
        let before = original.get(&path);
        let after = migrated.get(&path);
        if before == after {
            continue;
        }
        match (before, after) {
            (Some(RunTreeEntry::File { .. }), Some(RunTreeEntry::File { .. })) => {
                let path = String::from_utf8(path).map_err(|_| {
                    MigrationError::new("changed migration artifact path is not portable UTF-8")
                })?;
                if !is_portable_artifact_path(&path) {
                    return Err(MigrationError::new(format!(
                        "changed migration artifact path is not portable: {path}"
                    )));
                }
                actual.push(path);
            }
            _ => {
                return Err(MigrationError::new(
                    "migration changed the source/backup artifact set or an entry type",
                ))
            }
        }
    }
    if actual != reported {
        return Err(MigrationError::new(format!(
            "migration result artifact list does not match the deterministic changed set: reported={reported:?}, actual={actual:?}"
        )));
    }
    Ok(())
}

fn hash_tree_record(hasher: &mut Sha256, kind: &[u8], path: &[u8], payload: &[u8]) {
    for field in [kind, path, payload] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

fn copy_tree_no_follow(
    source: &Path,
    target: &Path,
    staged_ownership: Option<&StagedOwnership>,
    fault: PublicationPhase,
) -> Result<(), MigrationError> {
    let parent_path = source
        .parent()
        .ok_or_else(|| MigrationError::new("migration source has no containing runs directory"))?;
    if target.parent() != Some(parent_path) {
        return Err(MigrationError::new(
            "migration source and staged target must share one pinned runs root",
        ));
    }
    let source_name = source
        .file_name()
        .ok_or_else(|| MigrationError::new("migration source has no directory name"))?;
    let target_name = target
        .file_name()
        .ok_or_else(|| MigrationError::new("staged migration target has no directory name"))?;
    let parent = crate::artifact_safety::PinnedPrivateDirectory::open_parent(parent_path)
        .map_err(|error| MigrationError::context("could not pin migration runs root", error))?;
    let source = parent
        .open_child_directory(source_name)
        .map_err(|error| MigrationError::context("could not pin migration copy source", error))?;
    let source_inventory = inventory_pinned_tree(&source)?;
    if staged_ownership.is_some() && source_inventory.contains_key(STAGED_OWNERSHIP_FILE.as_bytes())
    {
        return Err(MigrationError::new(format!(
            "migration source contains reserved staged ownership path {STAGED_OWNERSHIP_FILE}"
        )));
    }
    let target = parent
        .create_child_directory(target_name)
        .map_err(|error| {
            MigrationError::context("could not create pinned staged migration root", error)
        })?;
    if let Some(staged_ownership) = staged_ownership {
        write_create_only_value_in_directory(
            &target,
            OsStr::new(STAGED_OWNERSHIP_FILE),
            staged_ownership,
        )?;
    }
    let mut copied_entry = false;
    let mut usage = MigrationTreeUsage::default();
    copy_directory_entries(
        &source,
        &target,
        Path::new(""),
        0,
        &mut usage,
        fault,
        &mut copied_entry,
    )?;
    target.sync_all()?;
    parent.sync_all()?;
    Ok(())
}

#[derive(Debug, Default)]
struct MigrationTreeUsage {
    bytes: u64,
    entries: usize,
}

fn copy_directory_entries(
    source: &crate::artifact_safety::PinnedPrivateDirectory,
    target: &crate::artifact_safety::PinnedPrivateDirectory,
    relative_directory: &Path,
    depth: usize,
    usage: &mut MigrationTreeUsage,
    fault: PublicationPhase,
    copied_entry: &mut bool,
) -> Result<(), MigrationError> {
    source.validate_identity()?;
    target.validate_identity()?;
    let names = bounded_sorted_entry_names(source, "migration copy source")?;
    for name in names {
        usage.entries = usage
            .entries
            .checked_add(1)
            .ok_or_else(|| MigrationError::new("migration copy entry count overflowed"))?;
        if usage.entries > crate::artifact_storage::RUN_TREE_ENTRY_CAP {
            return Err(MigrationError::new(format!(
                "run tree exceeds its {}-entry cap during copy",
                crate::artifact_storage::RUN_TREE_ENTRY_CAP
            )));
        }
        let relative = relative_directory.join(&name);
        match source.entry_kind(&name)? {
            crate::artifact_safety::PinnedEntryKind::Directory => {
                let child_depth = depth
                    .checked_add(1)
                    .ok_or_else(|| MigrationError::new("migration copy depth overflowed"))?;
                if child_depth > crate::artifact_storage::RUN_TREE_DIRECTORY_DEPTH_CAP {
                    return Err(MigrationError::new(format!(
                        "run tree exceeds its {}-directory depth cap during copy",
                        crate::artifact_storage::RUN_TREE_DIRECTORY_DEPTH_CAP
                    )));
                }
                let source_child = source.open_child_directory(&name)?;
                let target_child = target.create_child_directory(&name)?;
                copy_directory_entries(
                    &source_child,
                    &target_child,
                    &relative,
                    child_depth,
                    usage,
                    fault,
                    copied_entry,
                )?;
                source_child.validate_identity()?;
                target_child.sync_all()?;
                target_child.validate_identity()?;
            }
            crate::artifact_safety::PinnedEntryKind::RegularFile => {
                let mut source_file = source.open_existing_regular_file_any_mode(&name)?;
                let metadata = source_file.metadata()?;
                let relative_string = relative.to_str().ok_or_else(|| {
                    MigrationError::new("migration copy path is not portable UTF-8")
                })?;
                crate::artifact_storage::validate_artifact_size_u64(
                    relative_string,
                    metadata.len(),
                )?;
                usage.bytes = usage
                    .bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| MigrationError::new("migration copy byte total overflowed"))?;
                if usage.bytes > crate::artifact_storage::RUN_TREE_BYTE_CAP {
                    return Err(MigrationError::new(format!(
                        "run tree exceeds its {}-byte cap during copy",
                        crate::artifact_storage::RUN_TREE_BYTE_CAP
                    )));
                }
                let mut target_file = target.create_file(&name)?;
                let copied = io::copy(
                    &mut source_file.by_ref().take(metadata.len().saturating_add(1)),
                    &mut target_file,
                )?;
                if copied != metadata.len() {
                    return Err(MigrationError::new(format!(
                        "migration copy source changed length: {}",
                        relative.display()
                    )));
                }
                source.validate_single_link_file(&name, &metadata)?;
                target_file.sync_all()?;
                target.validate_single_link_file(&name, &target_file.metadata()?)?;
            }
            crate::artifact_safety::PinnedEntryKind::Other => {
                let symlink_target = source.read_symlink(&name, 4096).map_err(|error| {
                    MigrationError::context(
                        format!(
                            "migration source special entry is unsafe: {}",
                            relative.display()
                        ),
                        error,
                    )
                })?;
                target.create_symlink(&symlink_target, &name)?;
            }
        }
        if !*copied_entry {
            *copied_entry = true;
            inject_fault(fault, PublicationPhase::DuringCopy)?;
        }
        source.validate_identity()?;
        target.validate_identity()?;
    }
    Ok(())
}

#[cfg(test)]
fn rebind_directory_for_test(path: &Path) -> Result<(), MigrationError> {
    let parked = path.with_extension("locked-directory");
    fs::rename(path, &parked)?;
    copy_tree_no_follow(&parked, path, None, PublicationPhase::None)
}

#[cfg(test)]
fn sorted_entries(directory: &Path) -> Result<Vec<fs::DirEntry>, MigrationError> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn sync_tree(root: &Path) -> Result<(), MigrationError> {
    let root = crate::artifact_safety::PinnedPrivateDirectory::open(root)?;
    sync_pinned_tree(&root)
}

fn sync_pinned_tree(
    directory: &crate::artifact_safety::PinnedPrivateDirectory,
) -> Result<(), MigrationError> {
    directory.validate_identity()?;
    let names = bounded_sorted_entry_names(directory, "migration sync tree")?;
    for name in names {
        match directory.entry_kind(&name)? {
            crate::artifact_safety::PinnedEntryKind::Directory => {
                let child = directory.open_child_directory(&name)?;
                sync_pinned_tree(&child)?;
                child.validate_identity()?;
            }
            crate::artifact_safety::PinnedEntryKind::RegularFile => {
                let file = directory.open_existing_regular_file_any_mode(&name)?;
                let metadata = file.metadata()?;
                file.sync_all()?;
                directory.validate_single_link_file(&name, &metadata)?;
            }
            crate::artifact_safety::PinnedEntryKind::Other => {
                directory.read_symlink(&name, 4096)?;
            }
        }
        directory.validate_identity()?;
    }
    directory.sync_all()?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), MigrationError> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn write_create_only_bytes(path: &Path, bytes: &[u8]) -> Result<(), MigrationError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    io::Write::write_all(&mut file, bytes)?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn write_create_only_value_in_directory(
    directory: &crate::artifact_safety::PinnedPrivateDirectory,
    name: &OsStr,
    value: &impl Serialize,
) -> Result<(), MigrationError> {
    let bytes = canonical_json_bytes(value)?;
    let relative = name
        .to_str()
        .ok_or_else(|| MigrationError::new("migration contract name is not portable UTF-8"))?;
    crate::artifact_storage::validate_artifact_size(relative, bytes.len())?;
    directory.validate_identity()?;
    let mut file = directory.create_file(name)?;
    io::Write::write_all(&mut file, &bytes)?;
    file.sync_all()?;
    directory.validate_single_link_file(name, &file.metadata()?)?;
    directory.sync_all()?;
    directory.validate_identity()?;
    Ok(())
}

fn generate_staged_ownership_token() -> Result<String, MigrationError> {
    #[cfg(unix)]
    {
        let mut bytes = [0_u8; 32];
        fs::File::open("/dev/urandom")
            .and_then(|mut random| random.read_exact(&mut bytes))
            .map_err(|error| {
                MigrationError::context("could not generate staged ownership token", error)
            })?;
        Ok(hex::encode(bytes))
    }
    #[cfg(not(unix))]
    {
        Err(MigrationError::new(
            "staged ownership tokens are unsupported on this platform",
        ))
    }
}

fn staged_ownership_from_intent(intent: &MigrationIntent) -> StagedOwnership {
    StagedOwnership {
        schema_version: STAGED_OWNERSHIP_SCHEMA_VERSION,
        migration_id: MIGRATION_ID.to_string(),
        run_id: intent.run_id.clone(),
        token: intent.staged_ownership_token.clone(),
    }
}

fn validate_staged_ownership(
    run_directory: &Path,
    run_id: &str,
    intent: &MigrationIntent,
) -> Result<(), MigrationError> {
    let directory = crate::artifact_safety::PinnedPrivateDirectory::open(run_directory)
        .map_err(|error| MigrationError::context("could not pin staged ownership root", error))?;
    validate_staged_ownership_in_directory(&directory, run_id, intent)
}

fn validate_staged_ownership_in_directory(
    directory: &crate::artifact_safety::PinnedPrivateDirectory,
    run_id: &str,
    intent: &MigrationIntent,
) -> Result<(), MigrationError> {
    let name = OsStr::new(STAGED_OWNERSHIP_FILE);
    let mut file = directory
        .open_existing_file(name, true, false)
        .map_err(|error| MigrationError::context("staged ownership marker is missing", error))?;
    let metadata = file.metadata()?;
    directory.validate_single_link_file(name, &metadata)?;
    crate::artifact_storage::validate_artifact_size_u64(STAGED_OWNERSHIP_FILE, metadata.len())?;
    let bytes = read_bounded_file(
        &mut file,
        metadata.len(),
        STAGED_OWNERSHIP_FILE,
        "staged ownership marker",
    )?;
    directory.validate_single_link_file(name, &metadata)?;
    let marker: StagedOwnership = serde_json::from_slice(&bytes)
        .map_err(|error| MigrationError::context("invalid staged ownership marker", error))?;
    if canonical_json_bytes(&marker)? != bytes
        || marker.schema_version != STAGED_OWNERSHIP_SCHEMA_VERSION
        || marker.migration_id != MIGRATION_ID
        || marker.run_id != run_id
        || marker.token != intent.staged_ownership_token
        || !is_digest(&marker.token)
    {
        return Err(MigrationError::new(
            "staged ownership marker does not match the bound migration intent",
        ));
    }
    Ok(())
}

fn ensure_no_staged_ownership(run_directory: &Path) -> Result<(), MigrationError> {
    let directory = crate::artifact_safety::PinnedPrivateDirectory::open(run_directory)
        .map_err(|error| MigrationError::context("could not pin completed migration", error))?;
    match directory.entry_kind(OsStr::new(STAGED_OWNERSHIP_FILE)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(MigrationError::new(
            "completed migration retains a staged ownership marker without an intent",
        )),
        Err(error) => Err(MigrationError::context(
            "could not inspect staged ownership marker",
            error,
        )),
    }
}

fn validate_optional_staged_ownership(
    run_directory: &Path,
    run_id: &str,
    intent: &MigrationIntent,
) -> Result<bool, MigrationError> {
    let directory = crate::artifact_safety::PinnedPrivateDirectory::open(run_directory)
        .map_err(|error| MigrationError::context("could not pin completed migration", error))?;
    match directory.entry_kind(OsStr::new(STAGED_OWNERSHIP_FILE)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Ok(_) => {
            validate_staged_ownership_in_directory(&directory, run_id, intent)?;
            Ok(true)
        }
        Err(error) => Err(MigrationError::context(
            "could not inspect staged ownership marker",
            error,
        )),
    }
}

fn remove_staged_ownership(
    runs_root: &crate::artifact_safety::PinnedPrivateDirectory,
    run_name: &OsStr,
    intent: &MigrationIntent,
    guard: Option<&crate::run_persistence::RunMutationGuard>,
) -> Result<(), MigrationError> {
    runs_root.validate_identity()?;
    let run = runs_root.open_child_directory(run_name)?;
    validate_staged_ownership_in_directory(&run, &intent.run_id, intent)?;
    if let Some(guard) = guard {
        validate_guard_at_child(guard, runs_root, run_name, "owned staged run")?;
    }
    runs_root.validate_child_directory(run_name, &run.metadata()?)?;
    let name = OsStr::new(STAGED_OWNERSHIP_FILE);
    let file = run.open_existing_regular_file_any_mode(name)?;
    let metadata = file.metadata()?;
    run.unlink_regular_file_if_same_any_mode(name, &metadata)?;
    run.sync_all()?;
    runs_root.validate_child_directory(run_name, &run.metadata()?)?;
    runs_root.sync_all()?;
    Ok(())
}

fn write_existing_bytes(
    run_directory: &Path,
    relative: &str,
    bytes: &[u8],
) -> Result<(), MigrationError> {
    let relative_path = Path::new(relative);
    let parent =
        crate::artifact_safety::open_private_descendant_parent(run_directory, relative_path)
            .map_err(|error| {
                MigrationError::context(
                    format!("managed migration artifact parent is unsafe: {relative}"),
                    error,
                )
            })?;
    let name = relative_path.file_name().ok_or_else(|| {
        MigrationError::new(format!(
            "managed migration artifact has no file name: {relative}"
        ))
    })?;
    let mut file = parent
        .open_existing_file(name, true, true)
        .map_err(|error| {
            MigrationError::context(
                format!("could not safely open managed migration artifact {relative}"),
                error,
            )
        })?;
    file.set_len(0)?;
    io::Write::write_all(&mut file, bytes)?;
    file.sync_all()?;
    parent.sync_all()?;
    Ok(())
}

fn remove_intent(
    runs_root: &crate::artifact_safety::PinnedPrivateDirectory,
    intent_name: &OsStr,
) -> Result<(), MigrationError> {
    runs_root.validate_identity()?;
    let file = runs_root
        .open_existing_regular_file_any_mode(intent_name)
        .map_err(|error| MigrationError::context("could not pin migration intent", error))?;
    let metadata = file.metadata()?;
    runs_root.unlink_regular_file_if_same_any_mode(intent_name, &metadata)?;
    runs_root.sync_all()?;
    runs_root.validate_identity()?;
    Ok(())
}

fn remove_pinned_child_tree(
    parent: &crate::artifact_safety::PinnedPrivateDirectory,
    name: &OsStr,
    child: &crate::artifact_safety::PinnedPrivateDirectory,
) -> Result<(), MigrationError> {
    parent
        .validate_child_directory(name, &child.metadata()?)
        .map_err(|error| {
            MigrationError::context("unpublished staged scratch identity changed", error)
        })?;
    remove_pinned_directory_contents(child)?;
    parent
        .remove_child_directory_if_same(name, child)
        .map_err(|error| {
            MigrationError::context("could not remove unpublished staged scratch", error)
        })?;
    parent.sync_all()?;
    Ok(())
}

fn remove_pinned_directory_contents(
    directory: &crate::artifact_safety::PinnedPrivateDirectory,
) -> Result<(), MigrationError> {
    directory.validate_identity()?;
    let names = bounded_sorted_entry_names(directory, "migration scratch cleanup")?;
    for name in names {
        match directory.entry_kind(&name)? {
            crate::artifact_safety::PinnedEntryKind::Directory => {
                let child = directory.open_child_directory(&name)?;
                remove_pinned_directory_contents(&child)?;
                directory.remove_child_directory_if_same(&name, &child)?;
            }
            crate::artifact_safety::PinnedEntryKind::RegularFile => {
                let file = directory.open_existing_regular_file_any_mode(&name)?;
                let metadata = file.metadata()?;
                directory.unlink_regular_file_if_same_any_mode(&name, &metadata)?;
            }
            crate::artifact_safety::PinnedEntryKind::Other => {
                directory.read_symlink(&name, 4096)?;
                directory.unlink(&name)?;
            }
        }
        directory.validate_identity()?;
    }
    directory.sync_all()?;
    Ok(())
}

fn bounded_sorted_entry_names(
    directory: &crate::artifact_safety::PinnedPrivateDirectory,
    label: &str,
) -> Result<Vec<std::ffi::OsString>, MigrationError> {
    let mut names = Vec::new();
    directory
        .for_each_entry_name(|name| {
            #[cfg(test)]
            INVENTORY_ENUMERATION_VISITS.with(|visits| visits.set(visits.get() + 1));
            if names.len() >= crate::artifact_storage::RUN_TREE_ENTRY_CAP {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{label} exceeds its {}-entry cap during enumeration",
                        crate::artifact_storage::RUN_TREE_ENTRY_CAP
                    ),
                ));
            }
            names.push(name.to_os_string());
            Ok(())
        })
        .map_err(|error| MigrationError::context(format!("could not enumerate {label}"), error))?;
    names.sort();
    Ok(names)
}

fn pinned_entry_exists(
    directory: &crate::artifact_safety::PinnedPrivateDirectory,
    name: &OsStr,
) -> Result<bool, MigrationError> {
    directory.validate_identity()?;
    match directory.entry_kind(name) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(MigrationError::context(
            "could not inspect migration transaction entry",
            error,
        )),
    }
}

fn load_bound_intent(
    runs_root: &crate::artifact_safety::PinnedPrivateDirectory,
    intent_name: &OsStr,
    run_id: &str,
    original_directory: &Path,
) -> Result<MigrationIntent, MigrationError> {
    let intent: MigrationIntent =
        read_canonical_contract_in_directory(runs_root, intent_name, "migration intent")?;
    if intent.schema_version != INTENT_SCHEMA_VERSION
        || intent.migration_id != MIGRATION_ID
        || intent.run_id != run_id
        || intent.target_schema_version != DURABLE_ARTIFACT_SCHEMA_VERSION
        || !is_digest(&intent.source_run_digest)
        || !is_digest(&intent.source_tree_digest)
        || !is_digest(&intent.staged_ownership_token)
        || !is_digest(&intent.projected_staged_inventory_digest)
    {
        return Err(MigrationError::new(
            "migration intent does not match the selected run and v0-to-v1 transaction",
        ));
    }
    validate_source_binding(
        original_directory,
        &intent.source_run_digest,
        &intent.source_tree_digest,
        "migration intent source",
    )?;
    Ok(intent)
}

fn validate_source_binding(
    original_directory: &Path,
    expected_run_digest: &str,
    expected_tree_digest: &str,
    label: &str,
) -> Result<(), MigrationError> {
    let run_bytes = read_authenticated_file(original_directory, "run.json", label)?;
    let actual_run_digest = digest(&run_bytes);
    if actual_run_digest != expected_run_digest {
        return Err(MigrationError::new(format!(
            "{label} source run digest mismatch: expected {expected_run_digest}, got {actual_run_digest}"
        )));
    }
    let actual_tree_digest = digest_run_tree(original_directory)?;
    if actual_tree_digest != expected_tree_digest {
        return Err(MigrationError::new(format!(
            "{label} source tree digest mismatch: expected {expected_tree_digest}, got {actual_tree_digest}"
        )));
    }
    Ok(())
}

fn read_canonical_contract<T>(path: &Path, kind: &str) -> Result<T, MigrationError>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    let bytes = read_regular_file(path, kind)?;
    let contract: T = serde_json::from_slice(&bytes)
        .map_err(|error| MigrationError::context(format!("invalid {kind}"), error))?;
    if canonical_json_bytes(&contract)? != bytes {
        return Err(MigrationError::new(format!(
            "{kind} is not canonical typed JSON"
        )));
    }
    Ok(contract)
}

fn read_canonical_contract_in_directory<T>(
    directory: &crate::artifact_safety::PinnedPrivateDirectory,
    name: &OsStr,
    kind: &str,
) -> Result<T, MigrationError>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    directory.validate_identity()?;
    let mut file = directory
        .open_existing_regular_file_any_mode(name)
        .map_err(|error| MigrationError::context(format!("could not open {kind}"), error))?;
    let metadata = file.metadata()?;
    let relative = name
        .to_str()
        .ok_or_else(|| MigrationError::new(format!("{kind} name is not portable UTF-8")))?;
    crate::artifact_storage::validate_artifact_size_u64(relative, metadata.len())?;
    let bytes = read_bounded_file(&mut file, metadata.len(), relative, kind)?;
    directory.validate_single_link_file(name, &metadata)?;
    directory.validate_identity()?;
    let contract: T = serde_json::from_slice(&bytes)
        .map_err(|error| MigrationError::context(format!("invalid {kind}"), error))?;
    if canonical_json_bytes(&contract)? != bytes {
        return Err(MigrationError::new(format!(
            "{kind} is not canonical typed JSON"
        )));
    }
    Ok(contract)
}

fn read_result_paths(run_directory: &Path) -> Result<Vec<String>, MigrationError> {
    let run_bytes = read_authenticated_file(run_directory, "run.json", "migrated LoopRun")?;
    let run: LoopRun = serde_json::from_slice(&run_bytes)
        .map_err(|error| MigrationError::context("invalid migrated LoopRun", error))?;
    Ok(load_migration_result(run_directory, &run.run_id)?.migrated_artifacts)
}

fn migrated_outcome(
    run_id: &str,
    paths: &MigrationPaths,
    status: MigrationStatus,
    migrated_artifacts: Vec<String>,
) -> MigrationOutcome {
    MigrationOutcome {
        command: "migrate".to_string(),
        run_id: run_id.to_string(),
        status,
        from_schema_version: 0,
        to_schema_version: DURABLE_ARTIFACT_SCHEMA_VERSION,
        run_directory: paths.source.display().to_string(),
        backup_directory: Some(paths.backup.display().to_string()),
        result_path: Some(paths.source.join(RESULT_FILE).display().to_string()),
        migrated_artifacts,
    }
}

fn current_outcome(run_id: &str, paths: &MigrationPaths) -> MigrationOutcome {
    MigrationOutcome {
        command: "migrate".to_string(),
        run_id: run_id.to_string(),
        status: MigrationStatus::AlreadyCurrent,
        from_schema_version: DURABLE_ARTIFACT_SCHEMA_VERSION,
        to_schema_version: DURABLE_ARTIFACT_SCHEMA_VERSION,
        run_directory: paths.source.display().to_string(),
        backup_directory: paths
            .backup
            .exists()
            .then(|| paths.backup.display().to_string()),
        result_path: paths
            .source
            .join(RESULT_FILE)
            .is_file()
            .then(|| paths.source.join(RESULT_FILE).display().to_string()),
        migrated_artifacts: Vec::new(),
    }
}

fn ensure_valid(kind: &str, errors: Vec<seaf_core::FieldError>) -> Result<(), MigrationError> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(MigrationError::new(format!(
            "invalid {kind}: {}",
            format_field_errors(errors)
        )))
    }
}

fn format_field_errors(errors: Vec<seaf_core::FieldError>) -> String {
    errors
        .into_iter()
        .map(|error| format!("{}: {}", error.field, error.message))
        .collect::<Vec<_>>()
        .join("; ")
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use seaf_core::LoopInputDigests;

    #[cfg(unix)]
    fn write_private_sparse_file(path: &Path, len: u64) {
        use std::os::unix::fs::OpenOptionsExt;

        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(crate::artifact_safety::PRIVATE_FILE_MODE)
            .open(path)
            .unwrap();
        file.set_len(len).unwrap();
    }

    fn tree_file_bytes(path: &Path) -> u64 {
        sorted_entries(path)
            .unwrap()
            .into_iter()
            .map(|entry| {
                let metadata = fs::symlink_metadata(entry.path()).unwrap();
                if metadata.is_dir() {
                    tree_file_bytes(&entry.path())
                } else if metadata.is_file() {
                    metadata.len()
                } else {
                    0
                }
            })
            .sum()
    }

    fn tree_entry_count(path: &Path) -> usize {
        sorted_entries(path)
            .unwrap()
            .into_iter()
            .map(|entry| {
                let metadata = fs::symlink_metadata(entry.path()).unwrap();
                1 + if metadata.is_dir() {
                    tree_entry_count(&entry.path())
                } else {
                    0
                }
            })
            .sum()
    }

    #[cfg(unix)]
    fn fill_tree_to_bytes(run_directory: &Path, target: u64) {
        let mut remaining = target.checked_sub(tree_file_bytes(run_directory)).unwrap();
        let cap = crate::artifact_storage::artifact_byte_cap("aggregate-fill.bin");
        let mut index = 0;
        while remaining > 0 {
            let len = remaining.min(cap);
            write_private_sparse_file(
                &run_directory.join(format!("aggregate-fill-{index:03}.bin")),
                len,
            );
            remaining -= len;
            index += 1;
        }
        assert_eq!(tree_file_bytes(run_directory), target);
    }

    #[cfg(unix)]
    fn fill_tree_to_entries(run_directory: &Path, target: usize) {
        let existing = tree_entry_count(run_directory);
        for index in existing..target {
            write_private_sparse_file(&run_directory.join(format!("entry-fill-{index:04}.bin")), 0);
        }
        assert_eq!(tree_entry_count(run_directory), target);
    }

    #[cfg(unix)]
    fn add_private_directory_chain(run_directory: &Path, depth: usize) {
        let mut current = run_directory.to_path_buf();
        for index in 1..=depth {
            current.push(format!("depth-{index}"));
            fs::create_dir(&current).unwrap();
            crate::artifact_safety::make_private_directory_fixture(&current).unwrap();
        }
    }

    fn assert_no_transient_migration_state(runs_root: &Path, run_id: &str) {
        let prefix = format!(".{run_id}.migration-v0-v1");
        assert!(!runs_root.join(format!("{prefix}.intent.json")).exists());
        assert!(!runs_root.join(format!("{prefix}.staged")).exists());
        assert!(!runs_root.join(format!("{prefix}.backup")).exists());
    }

    fn write_legacy_fixture(runs_root: &Path, run_id: &str) -> Vec<(String, Vec<u8>)> {
        #[cfg(unix)]
        crate::artifact_safety::make_private_directory_fixture(runs_root).unwrap();
        let run_directory = runs_root.join(run_id);
        fs::create_dir_all(run_directory.join("inputs")).unwrap();
        #[cfg(unix)]
        {
            crate::artifact_safety::make_private_directory_fixture(&run_directory).unwrap();
            crate::artifact_safety::make_private_directory_fixture(&run_directory.join("inputs"))
                .unwrap();
        }
        let mut ticket = json!({
            "schema_version": 1,
            "ticket_id": "T-FAULT-001",
            "goal_id": "migration-fault",
            "title": "Recover migration publication",
            "status": "ready",
            "priority": "p1",
            "problem": "A crash must not lose the selected run",
            "research_questions": [],
            "context": {"relevant_files": ["src/lib.rs"], "forbidden_files": []},
            "autonomy": {"level": 1, "apply_patch": false, "allow_shell_commands": []},
            "acceptance_criteria": ["Retry finishes deterministically"]
        });
        ticket.as_object_mut().unwrap().remove("schema_version");
        let mut policy: Value =
            serde_json::from_str(seaf_core::templates::DEFAULT_POLICY_JSON).unwrap();
        policy.as_object_mut().unwrap().remove("schema_version");
        let ticket_bytes = canonical_json_bytes(&ticket).unwrap();
        let policy_bytes = canonical_json_bytes(&policy).unwrap();
        let config = canonical_json_bytes(&json!({"config": "fault"})).unwrap();
        let repository = canonical_json_bytes(&json!({"repository": "fault"})).unwrap();
        let mut run = crate::state::create_run(crate::state::NewLoopRun {
            run_id: run_id.to_string(),
            ticket_id: "T-FAULT-001".to_string(),
            goal_id: "migration-fault".to_string(),
            provider: "fake".to_string(),
            model: "fake".to_string(),
            input_digests: LoopInputDigests {
                ticket: digest(&ticket_bytes),
                policy: digest(&policy_bytes),
                config: digest(&config),
                repository: digest(&repository),
                eval_config: None,
            },
        });
        run.started_at = "1".to_string();
        run.updated_at = "1".to_string();
        let mut run_value = serde_json::to_value(run).unwrap();
        run_value.as_object_mut().unwrap().remove("schema_version");
        let files = vec![
            ("inputs/ticket.json".to_string(), ticket_bytes.clone()),
            ("ticket.snapshot.json".to_string(), ticket_bytes),
            ("inputs/policy.json".to_string(), policy_bytes),
            ("inputs/config.json".to_string(), config),
            ("inputs/repository.json".to_string(), repository),
            (
                "run.json".to_string(),
                canonical_json_bytes(&run_value).unwrap(),
            ),
            (
                "forensic.txt".to_string(),
                b"preserve me exactly\n".to_vec(),
            ),
            (
                crate::run_persistence::RUN_MUTATION_LOCK_FILE.to_string(),
                Vec::new(),
            ),
        ];
        for (relative, bytes) in &files {
            let path = run_directory.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            #[cfg(unix)]
            crate::artifact_safety::write_private_fixture(path, bytes).unwrap();
            #[cfg(not(unix))]
            fs::write(path, bytes).unwrap();
        }
        files
    }

    #[test]
    fn every_publication_cut_recovers_without_losing_the_selected_or_backup_run() {
        for phase in [
            PublicationPhase::AfterIntent,
            PublicationPhase::AfterStaged,
            PublicationPhase::AfterBackup,
            PublicationPhase::AfterPublish,
            PublicationPhase::AfterOwnershipRemoval,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let runs_root = temp.path().join("runs");
            fs::create_dir(&runs_root).unwrap();
            let run_id = format!("fault-{phase:?}").to_ascii_lowercase();
            let original = write_legacy_fixture(&runs_root, &run_id);

            let error = migrate_loop_run_with_fault(&runs_root, &run_id, phase)
                .expect_err("fault must interrupt the transaction");
            assert!(error.interrupted, "{phase:?}: {error}");

            let outcome = migrate_loop_run(&runs_root, &run_id)
                .expect("ordinary retry must recover or finish");
            assert!(matches!(
                outcome.status,
                MigrationStatus::Migrated | MigrationStatus::Recovered
            ));
            assert!(runs_root.join(&run_id).join(RESULT_FILE).is_file());
            let backup = runs_root.join(format!(".{run_id}.migration-v0-v1.backup"));
            for (relative, bytes) in &original {
                assert_eq!(
                    fs::read(backup.join(relative)).unwrap(),
                    *bytes,
                    "{phase:?}: {relative}"
                );
            }
            assert!(!runs_root
                .join(format!(".{run_id}.migration-v0-v1.intent.json"))
                .exists());
            assert!(!runs_root
                .join(format!(".{run_id}.migration-v0-v1.staged"))
                .exists());
        }
    }

    #[test]
    fn retention_preserves_completed_source_during_pending_migration_publication() {
        for phase in [PublicationPhase::AfterIntent, PublicationPhase::AfterStaged] {
            let temp = tempfile::tempdir().unwrap();
            let runs_root = temp.path().join("runs");
            fs::create_dir(&runs_root).unwrap();
            let run_id = format!("retention-{phase:?}").to_ascii_lowercase();
            write_legacy_fixture(&runs_root, &run_id);
            let run_path = runs_root.join(&run_id).join("run.json");
            let mut run: Value = serde_json::from_slice(&fs::read(&run_path).unwrap()).unwrap();
            run["status"] = json!("completed");
            fs::write(&run_path, canonical_json_bytes(&run).unwrap()).unwrap();

            let error = migrate_loop_run_with_fault(&runs_root, &run_id, phase)
                .expect_err("fault must leave a real pending migration transaction");
            assert!(error.interrupted, "{phase:?}: {error}");
            assert!(runs_root
                .join(format!(".{run_id}.migration-v0-v1.intent.json"))
                .is_file());
            assert_eq!(
                runs_root
                    .join(format!(".{run_id}.migration-v0-v1.staged"))
                    .is_dir(),
                phase == PublicationPhase::AfterStaged
            );

            let report = crate::retention::purge_loop_runs(
                &runs_root,
                crate::retention::RetentionPolicy {
                    max_managed_bytes: 0,
                },
                crate::retention::PurgeMode::Apply,
            )
            .expect("retention must protect the authenticated migration source");

            assert!(runs_root.join(&run_id).is_dir(), "{phase:?}");
            assert_eq!(
                report
                    .decision
                    .snapshot
                    .protected_migration_evidence
                    .as_slice(),
                std::slice::from_ref(&run_id),
                "{phase:?}"
            );
            let outcome = migrate_loop_run(&runs_root, &run_id)
                .expect("ordinary migration retry must recover protected source");
            assert!(matches!(
                outcome.status,
                MigrationStatus::Migrated | MigrationStatus::Recovered
            ));
            assert!(runs_root.join(&run_id).join(RESULT_FILE).is_file());
        }
    }

    #[cfg(unix)]
    #[test]
    fn migration_accepts_the_per_file_cap_and_rejects_cap_plus_one_before_staging() {
        let cap = crate::artifact_storage::artifact_byte_cap("forensic.txt");
        for (suffix, size, should_pass) in [("exact", cap, true), ("plus-one", cap + 1, false)] {
            let temp = tempfile::tempdir().unwrap();
            let runs_root = temp.path().join("runs");
            fs::create_dir(&runs_root).unwrap();
            let run_id = format!("file-limit-{suffix}");
            write_legacy_fixture(&runs_root, &run_id);
            let forensic = runs_root.join(&run_id).join("forensic.txt");
            fs::remove_file(&forensic).unwrap();
            write_private_sparse_file(&forensic, size);

            let outcome = migrate_loop_run(&runs_root, &run_id);

            if should_pass {
                outcome.expect("the exact semantic per-file cap must be accepted");
            } else {
                let error = outcome.expect_err("per-file cap plus one must fail before staging");
                assert!(error.to_string().contains("byte cap"), "{error}");
                assert_no_transient_migration_state(&runs_root, &run_id);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn migration_rejects_aggregate_cap_plus_one_before_staging() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).unwrap();
        let run_id = "aggregate-limit-plus-one";
        write_legacy_fixture(&runs_root, run_id);
        let run_directory = runs_root.join(run_id);
        fill_tree_to_bytes(
            &run_directory,
            crate::artifact_storage::RUN_TREE_BYTE_CAP + 1,
        );

        let error = migrate_loop_run(&runs_root, run_id)
            .expect_err("aggregate cap plus one must fail before staging");

        assert!(error.to_string().contains("byte cap"), "{error}");
        assert_no_transient_migration_state(&runs_root, run_id);
    }

    #[cfg(unix)]
    #[test]
    fn migration_rejects_entry_cap_plus_one_before_staging() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).unwrap();
        let run_id = "entry-limit-plus-one";
        write_legacy_fixture(&runs_root, run_id);
        fill_tree_to_entries(
            &runs_root.join(run_id),
            crate::artifact_storage::RUN_TREE_ENTRY_CAP + 1,
        );

        let error = migrate_loop_run(&runs_root, run_id)
            .expect_err("entry cap plus one must fail before staging");

        assert!(error.to_string().contains("entry cap"), "{error}");
        assert_no_transient_migration_state(&runs_root, run_id);
    }

    #[cfg(unix)]
    #[test]
    fn inventory_stops_enumeration_at_entry_cap_plus_one() {
        let temp = tempfile::tempdir().unwrap();
        let run = temp.path().join("enumeration-limit");
        fs::create_dir(&run).unwrap();
        crate::artifact_safety::make_private_directory_fixture(&run).unwrap();
        fill_tree_to_entries(&run, crate::artifact_storage::RUN_TREE_ENTRY_CAP + 100);
        INVENTORY_ENUMERATION_VISITS.with(|visits| visits.set(0));

        let error = inventory_run_tree(&run)
            .expect_err("entry cap plus one must stop descriptor enumeration immediately");

        assert!(error.to_string().contains("entry cap"), "{error}");
        INVENTORY_ENUMERATION_VISITS.with(|visits| {
            assert_eq!(
                visits.get(),
                crate::artifact_storage::RUN_TREE_ENTRY_CAP + 1,
                "enumeration must not collect the remaining directory names after cap+1"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn migration_rejects_directory_depth_cap_plus_one_before_staging() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).unwrap();
        let run_id = "depth-limit-plus-one";
        write_legacy_fixture(&runs_root, run_id);
        add_private_directory_chain(
            &runs_root.join(run_id),
            crate::artifact_storage::RUN_TREE_DIRECTORY_DEPTH_CAP + 1,
        );

        let error = migrate_loop_run(&runs_root, run_id)
            .expect_err("directory depth cap plus one must fail before staging");

        assert!(error.to_string().contains("depth cap"), "{error}");
        assert_no_transient_migration_state(&runs_root, run_id);
    }

    #[cfg(unix)]
    #[test]
    fn migration_inventory_accepts_exact_shared_tree_limits() {
        let aggregate = tempfile::tempdir().unwrap();
        let aggregate_run = aggregate.path().join("aggregate-exact");
        fs::create_dir(&aggregate_run).unwrap();
        crate::artifact_safety::make_private_directory_fixture(&aggregate_run).unwrap();
        fill_tree_to_bytes(&aggregate_run, crate::artifact_storage::RUN_TREE_BYTE_CAP);
        inventory_run_tree(&aggregate_run).expect("the exact 32 MiB aggregate cap must be valid");

        let entries = tempfile::tempdir().unwrap();
        let entries_run = entries.path().join("entries-exact");
        fs::create_dir(&entries_run).unwrap();
        crate::artifact_safety::make_private_directory_fixture(&entries_run).unwrap();
        fill_tree_to_entries(&entries_run, crate::artifact_storage::RUN_TREE_ENTRY_CAP);
        inventory_run_tree(&entries_run).expect("the exact 4096-entry cap must be valid");

        let depth = tempfile::tempdir().unwrap();
        let depth_run = depth.path().join("depth-exact");
        fs::create_dir(&depth_run).unwrap();
        crate::artifact_safety::make_private_directory_fixture(&depth_run).unwrap();
        add_private_directory_chain(
            &depth_run,
            crate::artifact_storage::RUN_TREE_DIRECTORY_DEPTH_CAP,
        );
        inventory_run_tree(&depth_run).expect("the exact depth-8 cap must be valid");
    }

    #[cfg(unix)]
    #[test]
    fn migration_projects_exact_byte_growth_and_rejects_one_byte_short_before_intent() {
        let probe = tempfile::tempdir().unwrap();
        let probe_root = probe.path().join("runs");
        fs::create_dir(&probe_root).unwrap();
        let probe_id = "projection-000";
        write_legacy_fixture(&probe_root, probe_id);
        let probe_source_bytes = tree_file_bytes(&probe_root.join(probe_id));
        migrate_loop_run(&probe_root, probe_id).expect("probe migration must succeed");
        let final_growth = tree_file_bytes(&probe_root.join(probe_id))
            .checked_sub(probe_source_bytes)
            .expect("migration must have nonnegative final byte growth");

        let marker_probe = tempfile::tempdir().unwrap();
        let marker_root = marker_probe.path().join("runs");
        fs::create_dir(&marker_root).unwrap();
        let marker_id = "projection-001";
        write_legacy_fixture(&marker_root, marker_id);
        migrate_loop_run_with_fault(&marker_root, marker_id, PublicationPhase::DuringCopy)
            .expect_err("copy fault must expose the durable staged marker");
        let marker_bytes = fs::metadata(
            marker_root
                .join(format!(".{marker_id}.migration-v0-v1.staged"))
                .join(STAGED_OWNERSHIP_FILE),
        )
        .unwrap()
        .len();

        for (run_id, extra_byte, should_pass) in [
            ("projection-002", 0_u64, true),
            ("projection-003", 1_u64, false),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let runs_root = temp.path().join("runs");
            fs::create_dir(&runs_root).unwrap();
            write_legacy_fixture(&runs_root, run_id);
            let source = runs_root.join(run_id);
            let source_target = crate::artifact_storage::RUN_TREE_BYTE_CAP
                .checked_sub(final_growth)
                .and_then(|bytes| bytes.checked_sub(marker_bytes))
                .and_then(|bytes| bytes.checked_add(extra_byte))
                .expect("projection fixture must fit the shared cap");
            fill_tree_to_bytes(&source, source_target);
            let before = digest_run_tree(&source).unwrap();

            let outcome = migrate_loop_run(&runs_root, run_id);

            if should_pass {
                outcome.expect("the exact migration-owned byte peak must be accepted");
                inventory_run_tree(&source)
                    .expect("the final migrated run must remain inside shared bounds");
                assert_eq!(
                    tree_file_bytes(&source),
                    crate::artifact_storage::RUN_TREE_BYTE_CAP - marker_bytes
                );
            } else {
                let error = outcome
                    .expect_err("one byte short of migration headroom must fail before intent");
                assert!(error.to_string().contains("byte cap"), "{error}");
                assert_no_transient_migration_state(&runs_root, run_id);
                assert_eq!(digest_run_tree(&source).unwrap(), before);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn migration_projects_marker_and_result_entries_before_intent() {
        for (run_id, source_entries, should_pass) in [
            (
                "entry-projection-exact",
                crate::artifact_storage::RUN_TREE_ENTRY_CAP - 2,
                true,
            ),
            (
                "entry-projection-short",
                crate::artifact_storage::RUN_TREE_ENTRY_CAP - 1,
                false,
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let runs_root = temp.path().join("runs");
            fs::create_dir(&runs_root).unwrap();
            write_legacy_fixture(&runs_root, run_id);
            let source = runs_root.join(run_id);
            fill_tree_to_entries(&source, source_entries);
            let before = digest_run_tree(&source).unwrap();

            let outcome = migrate_loop_run(&runs_root, run_id);

            if should_pass {
                outcome.expect("the exact marker-plus-result entry peak must be accepted");
                let final_inventory = inventory_run_tree(&source)
                    .expect("the final migrated run must remain inside shared bounds");
                assert_eq!(
                    final_inventory.len(),
                    crate::artifact_storage::RUN_TREE_ENTRY_CAP - 1
                );
            } else {
                let error = outcome
                    .expect_err("missing marker/result entry headroom must fail before intent");
                assert!(error.to_string().contains("entry cap"), "{error}");
                assert_no_transient_migration_state(&runs_root, run_id);
                assert_eq!(digest_run_tree(&source).unwrap(), before);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn legacy_source_with_reserved_ownership_path_fails_byte_inert_before_intent() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).unwrap();
        let run_id = "reserved-ownership-source";
        write_legacy_fixture(&runs_root, run_id);
        let source = runs_root.join(run_id);
        crate::artifact_safety::write_private_fixture(
            source.join(STAGED_OWNERSHIP_FILE),
            b"operator-owned reserved path\n",
        )
        .unwrap();
        let before = digest_run_tree(&source).unwrap();

        let error = migrate_loop_run(&runs_root, run_id)
            .expect_err("reserved ownership path must fail before transaction creation");

        assert!(error.to_string().contains("reserved"), "{error}");
        assert_no_transient_migration_state(&runs_root, run_id);
        assert_eq!(digest_run_tree(&source).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn legacy_source_with_reserved_result_path_fails_byte_inert_before_intent() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).unwrap();
        let run_id = "reserved-result-source";
        write_legacy_fixture(&runs_root, run_id);
        let source = runs_root.join(run_id);
        crate::artifact_safety::write_private_fixture(
            source.join(RESULT_FILE),
            b"operator-owned reserved result\n",
        )
        .unwrap();
        let before = digest_run_tree(&source).unwrap();

        let error = migrate_loop_run(&runs_root, run_id)
            .expect_err("reserved result path must fail before transaction creation");

        assert!(error.to_string().contains("reserved"), "{error}");
        assert_no_transient_migration_state(&runs_root, run_id);
        assert_eq!(digest_run_tree(&source).unwrap(), before);
    }

    #[test]
    fn recovery_refuses_a_coherent_extra_staged_change_not_bound_by_the_intent() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).unwrap();
        let run_id = "coherent-extra-staged-change";
        write_legacy_fixture(&runs_root, run_id);
        let source = runs_root.join(run_id);
        let source_before = digest_run_tree(&source).unwrap();
        migrate_loop_run_with_fault(&runs_root, run_id, PublicationPhase::AfterStaged)
            .expect_err("fault must retain a fully validated staged candidate");
        let prefix = format!(".{run_id}.migration-v0-v1");
        let staged = runs_root.join(format!("{prefix}.staged"));
        fs::write(staged.join("forensic.txt"), b"coherent forged change\n").unwrap();
        let result_path = staged.join(RESULT_FILE);
        let mut result: Value = serde_json::from_slice(&fs::read(&result_path).unwrap()).unwrap();
        let artifacts = result["migrated_artifacts"].as_array_mut().unwrap();
        artifacts.push(json!("forensic.txt"));
        artifacts.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
        fs::write(&result_path, canonical_json_bytes(&result).unwrap()).unwrap();

        let error = migrate_loop_run(&runs_root, run_id)
            .expect_err("recovery must refuse a coherent change outside the admitted projection");

        assert!(
            error.to_string().contains("projection") || error.to_string().contains("bound"),
            "{error}"
        );
        assert_eq!(digest_run_tree(&source).unwrap(), source_before);
        assert!(staged.is_dir(), "forged staged evidence must be preserved");
        assert!(runs_root.join(format!("{prefix}.intent.json")).is_file());
        assert!(!runs_root.join(format!("{prefix}.backup")).exists());
    }

    #[test]
    fn marker_absent_completed_recovery_revalidates_exact_children_before_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).unwrap();
        let run_id = "completed-cleanup-rebound";
        write_legacy_fixture(&runs_root, run_id);
        migrate_loop_run_with_fault(&runs_root, run_id, PublicationPhase::AfterOwnershipRemoval)
            .expect_err("fault must leave completed source, backup, and intent without marker");
        let prefix = format!(".{run_id}.migration-v0-v1");
        let source = runs_root.join(run_id);
        let parked = source.with_extension("locked-directory");
        let intent = runs_root.join(format!("{prefix}.intent.json"));
        let backup = runs_root.join(format!("{prefix}.backup"));
        assert!(!source.join(STAGED_OWNERSHIP_FILE).exists());

        let error = migrate_loop_run_with_fault(
            &runs_root,
            run_id,
            PublicationPhase::RebindCompletedSourceBeforeCleanup,
        )
        .expect_err("completed recovery must reject a rebound selected child before cleanup");

        assert!(error.to_string().contains("identity"), "{error}");
        assert!(intent.is_file(), "intent must remain for operator recovery");
        assert!(backup.is_dir(), "retained backup must remain");
        assert!(parked.is_dir(), "locked completed tree must remain parked");
        assert!(
            source.is_dir(),
            "replacement selected tree must be preserved"
        );
        assert!(parked.join(RESULT_FILE).is_file());
        assert!(source.join(RESULT_FILE).is_file());
    }

    #[test]
    fn marker_present_completed_recovery_revalidates_exact_children_after_marker_removal() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).unwrap();
        let run_id = "completed-marker-cleanup-rebound";
        write_legacy_fixture(&runs_root, run_id);
        migrate_loop_run_with_fault(&runs_root, run_id, PublicationPhase::AfterPublish)
            .expect_err("fault must leave completed source, backup, intent, and marker");
        let prefix = format!(".{run_id}.migration-v0-v1");
        let source = runs_root.join(run_id);
        let parked = source.with_extension("locked-directory");
        let intent = runs_root.join(format!("{prefix}.intent.json"));
        let backup = runs_root.join(format!("{prefix}.backup"));
        assert!(source.join(STAGED_OWNERSHIP_FILE).is_file());

        let error = migrate_loop_run_with_fault(
            &runs_root,
            run_id,
            PublicationPhase::RebindCompletedSourceAfterMarkerRemoval,
        )
        .expect_err("completed recovery must reject a rebound after marker removal");

        assert!(error.to_string().contains("identity"), "{error}");
        assert!(intent.is_file(), "intent must remain for operator recovery");
        assert!(backup.is_dir(), "retained backup must remain");
        assert!(parked.is_dir(), "locked completed tree must remain parked");
        assert!(
            source.is_dir(),
            "replacement selected tree must be preserved"
        );
        assert!(parked.join(RESULT_FILE).is_file());
        assert!(source.join(RESULT_FILE).is_file());
        assert!(!parked.join(STAGED_OWNERSHIP_FILE).exists());
        assert!(!source.join(STAGED_OWNERSHIP_FILE).exists());
    }

    #[test]
    fn staged_inventory_divergence_from_pre_intent_projection_fails_before_publication() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).unwrap();
        let run_id = "projection-divergence";
        write_legacy_fixture(&runs_root, run_id);

        let error = migrate_loop_run_with_fault(
            &runs_root,
            run_id,
            PublicationPhase::DivergeStagedAfterProjection,
        )
        .expect_err("unprojected staged bytes must fail before publication");

        assert!(error.to_string().contains("projection"), "{error}");
        assert!(runs_root.join(run_id).is_dir());
        assert!(!runs_root
            .join(format!(".{run_id}.migration-v0-v1.backup"))
            .exists());
    }

    #[test]
    fn retry_rebuilds_unpublished_partial_staging_after_copy_or_rewrite_interruption() {
        for phase in [
            PublicationPhase::DuringCopy,
            PublicationPhase::DuringRewrite,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let runs_root = temp.path().join("runs");
            fs::create_dir(&runs_root).unwrap();
            let run_id = format!("partial-{phase:?}").to_ascii_lowercase();
            let original = write_legacy_fixture(&runs_root, &run_id);

            let error = migrate_loop_run_with_fault(&runs_root, &run_id, phase)
                .expect_err("the deterministic mid-staging fault must interrupt migration");
            assert!(error.interrupted, "{phase:?}: {error}");
            let prefix = format!(".{run_id}.migration-v0-v1");
            assert!(runs_root.join(&run_id).is_dir(), "{phase:?}");
            assert!(
                runs_root.join(format!("{prefix}.staged")).is_dir(),
                "{phase:?}"
            );
            assert!(
                runs_root.join(format!("{prefix}.intent.json")).is_file(),
                "{phase:?}"
            );
            assert!(
                !runs_root.join(format!("{prefix}.backup")).exists(),
                "{phase:?}"
            );

            let outcome = migrate_loop_run(&runs_root, &run_id)
                .expect("ordinary retry must discard unpublished scratch and converge");

            assert!(matches!(
                outcome.status,
                MigrationStatus::Migrated | MigrationStatus::Recovered
            ));
            let source = runs_root.join(&run_id);
            let backup = runs_root.join(format!("{prefix}.backup"));
            validate_completed_migration(&source, &backup, &run_id)
                .expect("retry result and audit set must remain valid");
            for (relative, bytes) in &original {
                assert_eq!(
                    fs::read(backup.join(relative)).unwrap(),
                    *bytes,
                    "{phase:?}: {relative}"
                );
            }
            assert!(
                !runs_root.join(format!("{prefix}.staged")).exists(),
                "{phase:?}"
            );
            assert!(
                !runs_root.join(format!("{prefix}.intent.json")).exists(),
                "{phase:?}"
            );
        }
    }

    #[test]
    fn locked_selected_or_staged_directory_rebound_is_never_authenticated_or_published() {
        for phase in [
            PublicationPhase::RebindSelectedAfterLock,
            PublicationPhase::RebindStagedAfterLock,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let runs_root = temp.path().join("runs");
            fs::create_dir(&runs_root).unwrap();
            let run_id = format!("rebound-{phase:?}").to_ascii_lowercase();
            write_legacy_fixture(&runs_root, &run_id);

            let error = migrate_loop_run_with_fault(&runs_root, &run_id, phase)
                .expect_err("a replacement at a locked run name must fail closed");

            assert!(
                error.to_string().contains("identity")
                    || error.to_string().contains("same real")
                    || error.to_string().contains("changed"),
                "{phase:?}: {error}"
            );
            assert!(
                !runs_root.join(&run_id).join(RESULT_FILE).exists(),
                "{phase:?}"
            );
            assert!(
                !runs_root
                    .join(format!(".{run_id}.migration-v0-v1.backup"))
                    .exists(),
                "{phase:?}"
            );
        }
    }

    #[test]
    fn recovery_preserves_a_substituted_unproven_staged_tree() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).unwrap();
        let run_id = "substituted-staged-scratch";
        write_legacy_fixture(&runs_root, run_id);
        migrate_loop_run_with_fault(&runs_root, run_id, PublicationPhase::DuringRewrite)
            .expect_err("fault must leave transaction-owned staged scratch");
        let prefix = format!(".{run_id}.migration-v0-v1");
        let staged = runs_root.join(format!("{prefix}.staged"));
        let original_staged = runs_root.join(format!("{prefix}.original-staged"));
        let intent = runs_root.join(format!("{prefix}.intent.json"));
        fs::rename(&staged, &original_staged).unwrap();
        fs::create_dir(&staged).unwrap();
        #[cfg(unix)]
        crate::artifact_safety::make_private_directory_fixture(&staged).unwrap();
        let replacement_sentinel = staged.join("replacement-sentinel.txt");
        #[cfg(unix)]
        crate::artifact_safety::write_private_fixture(
            &replacement_sentinel,
            b"unproven replacement\n",
        )
        .unwrap();
        #[cfg(not(unix))]
        fs::write(&replacement_sentinel, b"unproven replacement\n").unwrap();

        let error = migrate_loop_run(&runs_root, run_id)
            .expect_err("recovery must not delete or adopt an unproven staged replacement");

        assert!(
            error.to_string().contains("ownership")
                || error.to_string().contains("staged")
                || error.to_string().contains("identity"),
            "{error}"
        );
        assert!(original_staged.is_dir());
        assert!(staged.is_dir());
        assert_eq!(
            fs::read(&replacement_sentinel).unwrap(),
            b"unproven replacement\n"
        );
        assert!(intent.is_file());
    }

    #[test]
    fn same_call_staged_rebound_preserves_locked_original_and_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).unwrap();
        let run_id = "same-call-staged-rebound";
        write_legacy_fixture(&runs_root, run_id);
        let prefix = format!(".{run_id}.migration-v0-v1");
        let staged = runs_root.join(format!("{prefix}.staged"));
        let locked_original = staged.with_extension("locked-directory");
        let intent = runs_root.join(format!("{prefix}.intent.json"));

        let error = migrate_loop_run_with_fault(
            &runs_root,
            run_id,
            PublicationPhase::RebindStagedAfterLock,
        )
        .expect_err("staged rebound must fail its retained guard identity");

        assert!(error.to_string().contains("identity"), "{error}");
        assert!(
            locked_original.is_dir(),
            "locked original must be preserved"
        );
        assert!(staged.is_dir(), "unproven replacement must be preserved");
        assert!(intent.is_file(), "intent must remain for operator recovery");
    }

    #[test]
    fn runs_root_rebound_never_creates_or_removes_intent_in_the_replacement_root() {
        for phase in [
            PublicationPhase::RebindRunsRootBeforeIntentCreate,
            PublicationPhase::RebindRunsRootBeforeIntentRemove,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let runs_root = temp.path().join("runs");
            fs::create_dir(&runs_root).unwrap();
            let run_id = format!("root-rebound-{phase:?}").to_ascii_lowercase();
            write_legacy_fixture(&runs_root, &run_id);
            let intent_name = format!(".{run_id}.migration-v0-v1.intent.json");

            let error = migrate_loop_run_with_fault(&runs_root, &run_id, phase)
                .expect_err("runs-root rebound must fail the retained root identity");

            assert!(
                error.interrupted || error.to_string().contains("identity"),
                "{error}"
            );
            let original_root = runs_root.with_extension("locked-directory");
            match phase {
                PublicationPhase::RebindRunsRootBeforeIntentCreate => {
                    assert!(!original_root.join(&intent_name).exists());
                    assert!(!runs_root.join(&intent_name).exists());
                }
                PublicationPhase::RebindRunsRootBeforeIntentRemove => {
                    assert!(original_root.join(&intent_name).is_file());
                    assert!(runs_root.join(&intent_name).is_file());
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn recovery_rejects_foreign_or_non_contract_intents_without_mutating_the_source() {
        for case in ["foreign-digest", "unknown-field"] {
            let temp = tempfile::tempdir().unwrap();
            let runs_root = temp.path().join("runs");
            fs::create_dir(&runs_root).unwrap();
            let run_id = format!("intent-{case}");
            let original = write_legacy_fixture(&runs_root, &run_id);
            migrate_loop_run_with_fault(&runs_root, &run_id, PublicationPhase::AfterIntent)
                .expect_err("fault must preserve the intent");
            let intent = runs_root.join(format!(".{run_id}.migration-v0-v1.intent.json"));
            let mut value: Value = serde_json::from_slice(&fs::read(&intent).unwrap()).unwrap();
            match case {
                "foreign-digest" => value["source_run_digest"] = json!("f".repeat(64)),
                "unknown-field" => value["unexpected"] = json!(true),
                _ => unreachable!(),
            }
            fs::write(&intent, canonical_json_bytes(&value).unwrap()).unwrap();

            let error = migrate_loop_run(&runs_root, &run_id)
                .expect_err("recovery must reject a substituted intent");

            assert!(
                error.to_string().contains("intent")
                    || error.to_string().contains("source run digest"),
                "{case}: {error}"
            );
            for (relative, bytes) in &original {
                assert_eq!(
                    fs::read(runs_root.join(&run_id).join(relative)).unwrap(),
                    *bytes,
                    "{case}: {relative}"
                );
            }
            assert!(intent.is_file(), "{case}");
            assert!(!runs_root
                .join(format!(".{run_id}.migration-v0-v1.staged"))
                .exists());
            assert!(!runs_root
                .join(format!(".{run_id}.migration-v0-v1.backup"))
                .exists());
        }
    }

    #[test]
    fn recovery_rejects_a_substituted_backup_before_adopting_staged_output() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).unwrap();
        let run_id = "substituted-backup";
        write_legacy_fixture(&runs_root, run_id);
        migrate_loop_run_with_fault(&runs_root, run_id, PublicationPhase::AfterBackup)
            .expect_err("fault must stop between publication renames");
        let prefix = format!(".{run_id}.migration-v0-v1");
        let backup = runs_root.join(format!("{prefix}.backup"));
        let staged = runs_root.join(format!("{prefix}.staged"));
        let intent = runs_root.join(format!("{prefix}.intent.json"));
        let substituted = b"substituted original authority\n";
        fs::write(backup.join("run.json"), substituted).unwrap();

        let error = migrate_loop_run(&runs_root, run_id)
            .expect_err("recovery must bind staged output to the retained original");

        assert!(
            error.to_string().contains("source") || error.to_string().contains("digest"),
            "{error}"
        );
        assert!(!runs_root.join(run_id).exists());
        assert!(staged.is_dir());
        assert!(intent.is_file());
        assert_eq!(fs::read(backup.join("run.json")).unwrap(), substituted);
    }

    #[test]
    fn recovery_rejects_a_substituted_source_before_the_first_rename() {
        let temp = tempfile::tempdir().unwrap();
        let runs_root = temp.path().join("runs");
        fs::create_dir(&runs_root).unwrap();
        let run_id = "substituted-source";
        write_legacy_fixture(&runs_root, run_id);
        migrate_loop_run_with_fault(&runs_root, run_id, PublicationPhase::AfterStaged)
            .expect_err("fault must stop before the first publication rename");
        let prefix = format!(".{run_id}.migration-v0-v1");
        let source = runs_root.join(run_id);
        let staged = runs_root.join(format!("{prefix}.staged"));
        let intent = runs_root.join(format!("{prefix}.intent.json"));
        fs::write(source.join("forensic.txt"), b"substituted source\n").unwrap();

        let error = migrate_loop_run(&runs_root, run_id)
            .expect_err("recovery must bind staged output to the selected source");

        assert!(error.to_string().contains("source tree digest"), "{error}");
        assert!(source.is_dir());
        assert!(staged.is_dir());
        assert!(intent.is_file());
        assert!(!runs_root.join(format!("{prefix}.backup")).exists());
    }

    #[test]
    fn completed_migration_retry_rejects_backup_or_result_audit_tampering() {
        for case in [
            "backup-bytes",
            "result-contract",
            "result-empty-list",
            "result-extra-list",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let runs_root = temp.path().join("runs");
            fs::create_dir(&runs_root).unwrap();
            let run_id = format!("audit-{case}");
            write_legacy_fixture(&runs_root, &run_id);
            migrate_loop_run(&runs_root, &run_id).expect("initial migration");
            let source = runs_root.join(&run_id);
            let backup = runs_root.join(format!(".{run_id}.migration-v0-v1.backup"));
            let source_run_before = fs::read(source.join("run.json")).unwrap();
            match case {
                "backup-bytes" => {
                    fs::write(backup.join("forensic.txt"), b"tampered backup\n").unwrap();
                }
                "result-contract" => {
                    let result_path = source.join(RESULT_FILE);
                    let mut result: Value =
                        serde_json::from_slice(&fs::read(&result_path).unwrap()).unwrap();
                    result["unexpected"] = json!(true);
                    fs::write(result_path, canonical_json_bytes(&result).unwrap()).unwrap();
                }
                "result-empty-list" => {
                    let result_path = source.join(RESULT_FILE);
                    let mut result: Value =
                        serde_json::from_slice(&fs::read(&result_path).unwrap()).unwrap();
                    result["migrated_artifacts"] = json!([]);
                    fs::write(result_path, canonical_json_bytes(&result).unwrap()).unwrap();
                }
                "result-extra-list" => {
                    let result_path = source.join(RESULT_FILE);
                    let mut result: Value =
                        serde_json::from_slice(&fs::read(&result_path).unwrap()).unwrap();
                    let artifacts = result["migrated_artifacts"].as_array_mut().unwrap();
                    artifacts.push(json!("forensic.json"));
                    artifacts
                        .sort_by(|left, right| left.as_str().unwrap().cmp(right.as_str().unwrap()));
                    fs::write(result_path, canonical_json_bytes(&result).unwrap()).unwrap();
                }
                _ => unreachable!(),
            }

            let error = migrate_loop_run(&runs_root, &run_id)
                .expect_err("completed audit substitution must fail closed");

            assert!(
                error.to_string().contains("result")
                    || error.to_string().contains("backup")
                    || error.to_string().contains("digest")
                    || error.to_string().contains("unknown field"),
                "{case}: {error}"
            );
            assert_eq!(
                fs::read(source.join("run.json")).unwrap(),
                source_run_before
            );
        }
    }
}
