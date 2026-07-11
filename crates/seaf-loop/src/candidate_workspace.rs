use std::{
    env,
    error::Error,
    fmt, fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use seaf_core::{CandidateWorkspaceLifecycle, CandidateWorkspaceState, LoopStatus};
use sha2::{Digest, Sha256};

use crate::workspace::LoopWorkspace;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

pub const CANDIDATE_WORKSPACE_SCHEMA_VERSION: u32 = 1;
const CANDIDATE_ROOT_DIR: &str = "seaf-candidates";
const CANDIDATE_LOCK_FILE: &str = ".candidate-workspace.lock";

pub fn create_candidate_workspace(
    run_directory: &Path,
    source_worktree_root: &Path,
    repository_identity_digest: &str,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError> {
    let lock = acquire_candidate_directory_lock(run_directory)?;
    let result = create_candidate_workspace_locked(
        run_directory,
        source_worktree_root,
        repository_identity_digest,
    );
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(CandidateWorkspaceError::Io(error)),
        (Err(error), _) => Err(error),
    }
}

fn create_candidate_workspace_locked(
    run_directory: &Path,
    source_worktree_root: &Path,
    repository_identity_digest: &str,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError> {
    validate_digest(repository_identity_digest, "repository identity")?;
    let run_id = run_directory
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| {
            !value.is_empty()
                && value.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
        })
        .ok_or_else(|| {
            CandidateWorkspaceError::Unsafe(
                "run directory must end in a safe UTF-8 run ID".to_string(),
            )
        })?;
    let source = canonical_real_directory(source_worktree_root, "source worktree")?;
    let starting_head = git_text(&source, &["rev-parse", "HEAD"])?;
    let starting_tree = git_text(&source, &["rev-parse", "HEAD^{tree}"])?;
    validate_object_id(&starting_head, "starting HEAD")?;
    validate_object_id(&starting_tree, "starting tree")?;
    let git_common_dir = git_common_dir(&source)?;

    let candidate_parent = create_candidate_parent(repository_identity_digest)?;
    let candidate_path = candidate_parent.join(run_id);
    if candidate_path.starts_with(&source) {
        return Err(CandidateWorkspaceError::Unsafe(
            "candidate path must be outside the source worktree".to_string(),
        ));
    }
    if fs::symlink_metadata(&candidate_path).is_ok() {
        return adopt_existing_candidate(
            &candidate_path,
            &source,
            repository_identity_digest,
            &git_common_dir,
            &starting_head,
            &starting_tree,
        );
    }

    git_success(
        &source,
        &[
            "worktree",
            "add",
            "--detach",
            "--no-checkout",
            path_text(&candidate_path, "candidate path")?,
            &starting_head,
        ],
    )?;
    let result = (|| {
        let candidate = canonical_real_directory(&candidate_path, "candidate worktree")?;
        if candidate.starts_with(&source) {
            return Err(CandidateWorkspaceError::Unsafe(
                "candidate path resolved inside the source worktree".to_string(),
            ));
        }
        set_and_validate_private_directory(&candidate)?;
        materialize_candidate_without_filters(&candidate)?;
        if git_text(&source, &["rev-parse", "HEAD"])? != starting_head
            || git_text(&source, &["rev-parse", "HEAD^{tree}"])? != starting_tree
        {
            return Err(CandidateWorkspaceError::Mismatch(
                "source HEAD or tree changed while creating the candidate".to_string(),
            ));
        }
        require_detached_head(&candidate)?;

        let mut state = CandidateWorkspaceState {
            schema_version: CANDIDATE_WORKSPACE_SCHEMA_VERSION,
            path: path_text(&candidate, "candidate path")?.to_string(),
            source_worktree_root: path_text(&source, "source worktree")?.to_string(),
            git_common_dir: path_text(&git_common_dir, "Git common directory")?.to_string(),
            repository_identity_digest: repository_identity_digest.to_string(),
            starting_head: starting_head.clone(),
            starting_tree: starting_tree.clone(),
            candidate_head: starting_head.clone(),
            candidate_tree: starting_tree.clone(),
            candidate_diff_digest: sha256_bytes(&[]),
            lifecycle: CandidateWorkspaceLifecycle::Active,
            cleanup_started_at: None,
            cleaned_at: None,
        };
        refresh_candidate_workspace(&mut state)?;
        Ok(state)
    })();
    match result {
        Ok(state) => Ok(state),
        Err(error) => {
            let rollback = if exact_owned_candidate_remnant(
                &source,
                &candidate_path,
                &git_common_dir,
                &starting_head,
            )
            .unwrap_or(false)
            {
                git_success(
                    &source,
                    &[
                        "worktree",
                        "remove",
                        "--force",
                        path_text(&candidate_path, "candidate path")?,
                    ],
                )
            } else {
                Err(CandidateWorkspaceError::Unsafe(
                    "candidate rollback refused because the remnant is not exact owned state"
                        .to_string(),
                ))
            };
            match rollback {
                Ok(()) => Err(error),
                Err(rollback) => Err(CandidateWorkspaceError::Unsafe(format!(
                    "{error}; candidate rollback failed: {rollback}"
                ))),
            }
        }
    }
}

pub fn validate_candidate_workspace(
    run_directory: &Path,
    source_worktree_root: &Path,
    state: &CandidateWorkspaceState,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError> {
    validate_candidate_physical(run_directory, source_worktree_root, state, true)
}

fn validate_candidate_physical(
    run_directory: &Path,
    source_worktree_root: &Path,
    state: &CandidateWorkspaceState,
    require_current_source_head: bool,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError> {
    if state.lifecycle != CandidateWorkspaceLifecycle::Active || state.cleaned_at.is_some() {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate workspace is not active".to_string(),
        ));
    }
    let (source, persisted) = validate_static_authority(
        run_directory,
        source_worktree_root,
        state,
        require_current_source_head,
    )?;
    let candidate = canonical_real_directory(&persisted, "candidate worktree")?;
    if candidate != persisted || candidate.starts_with(&source) {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate path is symlinked, substituted, or inside the source worktree".to_string(),
        ));
    }
    validate_private_directory(&candidate)?;
    if !worktree_registered(&source, &candidate)? {
        return Err(CandidateWorkspaceError::Mismatch(
            "active candidate is not registered in the authoritative repository".to_string(),
        ));
    }
    require_detached_head(&candidate)?;
    let candidate_common = git_common_dir(&candidate)?;
    if path_text(&candidate_common, "Git common directory")? != state.git_common_dir {
        return Err(CandidateWorkspaceError::Mismatch(
            "Git common directory does not match candidate authority".to_string(),
        ));
    }
    let mut observed = state.clone();
    refresh_candidate_workspace(&mut observed)?;
    if observed.candidate_head != state.candidate_head
        || observed.candidate_tree != state.candidate_tree
        || observed.candidate_diff_digest != state.candidate_diff_digest
    {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate HEAD, index tree, or diff digest does not match persisted evidence"
                .to_string(),
        ));
    }
    if state.candidate_head != state.starting_head {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate contains an unauthorized commit".to_string(),
        ));
    }
    Ok(state.clone())
}

fn refresh_candidate_workspace(
    state: &mut CandidateWorkspaceState,
) -> Result<(), CandidateWorkspaceError> {
    if state.lifecycle != CandidateWorkspaceLifecycle::Active {
        return Err(CandidateWorkspaceError::Mismatch(
            "cannot refresh a cleaned candidate".to_string(),
        ));
    }
    let candidate = canonical_real_directory(Path::new(&state.path), "candidate worktree")?;
    verify_worktree_matches_index(&candidate)?;
    let untracked = git_bytes(&candidate, &["ls-files", "--others", "-z"])?;
    if !untracked.is_empty() {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate contains untracked files outside its exact index tree".to_string(),
        ));
    }
    state.candidate_head = git_text(&candidate, &["rev-parse", "HEAD"])?;
    state.candidate_tree = git_text(&candidate, &["write-tree"])?;
    validate_object_id(&state.candidate_head, "candidate HEAD")?;
    validate_object_id(&state.candidate_tree, "candidate tree")?;
    let diff = git_bytes(
        &candidate,
        &[
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
            "--",
        ],
    )?;
    state.candidate_diff_digest = sha256_bytes(&diff);
    validate_bound_evidence(state)?;
    Ok(())
}

pub fn cleanup_candidate_workspace(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError> {
    cleanup_candidate_workspace_with_hook(workspace, source_worktree_root, |_| Ok(()))
}

fn cleanup_candidate_workspace_with_hook<F>(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
    mut hook: F,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError>
where
    F: FnMut(CandidateCleanupPhase) -> Result<(), CandidateWorkspaceError>,
{
    let lock = acquire_candidate_lock(workspace)?;
    let result = (|| {
        let mut run = crate::state::load_run(workspace)
            .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
        if matches!(run.status, LoopStatus::Pending | LoopStatus::Running) {
            return Err(CandidateWorkspaceError::Unsafe(
                "refusing to clean an active run candidate".to_string(),
            ));
        }
        let candidate = run.candidate_workspace.clone().ok_or_else(|| {
            CandidateWorkspaceError::Mismatch(
                "authoritative LoopRun has no candidate workspace".to_string(),
            )
        })?;

        let mut cleaning = match candidate.lifecycle {
            CandidateWorkspaceLifecycle::Active => {
                validate_candidate_physical(
                    workspace.run_directory(),
                    source_worktree_root,
                    &candidate,
                    false,
                )?;
                let mut cleaning = candidate;
                cleaning.lifecycle = CandidateWorkspaceLifecycle::Cleaning;
                cleaning.cleanup_started_at = Some(now_timestamp());
                cleaning.cleaned_at = None;
                let expected = run.clone();
                run.candidate_workspace = Some(cleaning.clone());
                hook(CandidateCleanupPhase::BeforeIntentPersisted)?;
                persist_candidate_run(workspace, &expected, &run)?;
                hook(CandidateCleanupPhase::IntentPersisted)?;
                cleaning
            }
            CandidateWorkspaceLifecycle::Cleaning => candidate,
            CandidateWorkspaceLifecycle::Cleaned => {
                let (source, persisted) = validate_static_authority(
                    workspace.run_directory(),
                    source_worktree_root,
                    &candidate,
                    false,
                )?;
                if fs::symlink_metadata(&persisted).is_ok()
                    || worktree_registered(&source, &persisted)?
                {
                    return Err(CandidateWorkspaceError::Mismatch(
                        "cleaned candidate path or registration reappeared".to_string(),
                    ));
                }
                return Ok(candidate);
            }
        };

        let (source, persisted) = validate_static_authority(
            workspace.run_directory(),
            source_worktree_root,
            &cleaning,
            false,
        )?;
        let path_exists = fs::symlink_metadata(&persisted).is_ok();
        let registered = worktree_registered(&source, &persisted)?;
        match (path_exists, registered) {
            (true, true) => {
                let mut active_view = cleaning.clone();
                active_view.lifecycle = CandidateWorkspaceLifecycle::Active;
                active_view.cleanup_started_at = None;
                active_view.cleaned_at = None;
                validate_candidate_physical(
                    workspace.run_directory(),
                    source_worktree_root,
                    &active_view,
                    false,
                )?;
                git_success(
                    &source,
                    &[
                        "worktree",
                        "remove",
                        "--force",
                        path_text(&persisted, "candidate path")?,
                    ],
                )?;
                if fs::symlink_metadata(&persisted).is_ok()
                    || worktree_registered(&source, &persisted)?
                {
                    return Err(CandidateWorkspaceError::Unsafe(
                        "candidate removal did not clear both path and registration".to_string(),
                    ));
                }
                hook(CandidateCleanupPhase::WorktreeRemoved)?;
            }
            (false, false) => {}
            _ => {
                return Err(CandidateWorkspaceError::Mismatch(
                    "candidate cleanup found ambiguous path and registration state".to_string(),
                ));
            }
        }

        cleaning.lifecycle = CandidateWorkspaceLifecycle::Cleaned;
        cleaning.cleaned_at = Some(now_timestamp());
        let expected = run.clone();
        run.candidate_workspace = Some(cleaning.clone());
        persist_candidate_run(workspace, &expected, &run)?;
        Ok(cleaning)
    })();
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(CandidateWorkspaceError::Io(error)),
        (Err(error), _) => Err(error),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateCleanupPhase {
    BeforeIntentPersisted,
    IntentPersisted,
    WorktreeRemoved,
}

fn persist_candidate_run(
    workspace: &LoopWorkspace,
    expected: &seaf_core::LoopRun,
    intended: &seaf_core::LoopRun,
) -> Result<(), CandidateWorkspaceError> {
    // Lock order is candidate-workspace lock, then provider-exchange lock. Code that already
    // holds the provider lock must never enter candidate cleanup.
    crate::provider_exchange::persist_run_with_full_compare(workspace, expected, intended)
        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))
}

fn acquire_candidate_lock(workspace: &LoopWorkspace) -> Result<fs::File, CandidateWorkspaceError> {
    acquire_candidate_directory_lock(workspace.run_directory())
}

fn acquire_candidate_directory_lock(
    run_directory: &Path,
) -> Result<fs::File, CandidateWorkspaceError> {
    let metadata = fs::symlink_metadata(run_directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CandidateWorkspaceError::Unsafe(
            "candidate run directory must be a real directory".to_string(),
        ));
    }
    let path = run_directory.join(CANDIDATE_LOCK_FILE);
    let mut created = false;
    let file = match inspect_candidate_lock_path(&path) {
        Ok(()) => open_candidate_lock(&path, false)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match open_candidate_lock(&path, true) {
                Ok(file) => {
                    created = true;
                    file
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    inspect_candidate_lock_path(&path)?;
                    open_candidate_lock(&path, false)?
                }
                Err(error) => return Err(CandidateWorkspaceError::Io(error)),
            }
        }
        Err(error) => return Err(CandidateWorkspaceError::Io(error)),
    };
    validate_candidate_lock_file(&file, &path)?;
    if created {
        file.sync_all()?;
        fs::File::open(run_directory)?.sync_all()?;
    }
    file.lock().map_err(CandidateWorkspaceError::Io)?;
    if let Err(error) = validate_candidate_lock_file(&file, &path) {
        let _ = file.unlock();
        return Err(CandidateWorkspaceError::Io(error));
    }
    Ok(file)
}

fn inspect_candidate_lock_path(path: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "candidate cleanup lock is not a regular file",
        ));
    }
    Ok(())
}

fn open_candidate_lock(path: &Path, create: bool) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    if create {
        options.create_new(true);
    }
    #[cfg(target_os = "macos")]
    options.custom_flags(0x100);
    #[cfg(target_os = "linux")]
    options.custom_flags(0x20000);
    options.open(path)
}

fn validate_candidate_lock_file(file: &fs::File, path: &Path) -> std::io::Result<()> {
    let opened = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if current.file_type().is_symlink() || !opened.is_file() || !current.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "candidate cleanup lock identity is unsafe",
        ));
    }
    #[cfg(unix)]
    if opened.dev() != current.dev() || opened.ino() != current.ino() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "candidate cleanup lock was replaced",
        ));
    }
    Ok(())
}

fn validate_static_authority(
    run_directory: &Path,
    source_worktree_root: &Path,
    state: &CandidateWorkspaceState,
    require_current_source_head: bool,
) -> Result<(PathBuf, PathBuf), CandidateWorkspaceError> {
    if state.schema_version != CANDIDATE_WORKSPACE_SCHEMA_VERSION {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate schema version does not match".to_string(),
        ));
    }
    validate_digest(&state.repository_identity_digest, "repository identity")?;
    validate_digest(&state.candidate_diff_digest, "candidate diff")?;
    validate_object_id(&state.starting_head, "starting HEAD")?;
    validate_object_id(&state.starting_tree, "starting tree")?;
    validate_object_id(&state.candidate_head, "candidate HEAD")?;
    validate_object_id(&state.candidate_tree, "candidate tree")?;
    validate_bound_evidence(state)?;
    let source = canonical_real_directory(source_worktree_root, "source worktree")?;
    if path_text(&source, "source worktree")? != state.source_worktree_root {
        return Err(CandidateWorkspaceError::Mismatch(
            "source worktree root does not match candidate authority".to_string(),
        ));
    }
    let expected = existing_candidate_parent(&state.repository_identity_digest)?.join(
        run_directory
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| CandidateWorkspaceError::Unsafe("run ID is not UTF-8".to_string()))?,
    );
    let persisted = PathBuf::from(&state.path);
    if persisted != expected {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate path is not the deterministic path bound to this run".to_string(),
        ));
    }
    let source_common = git_common_dir(&source)?;
    if path_text(&source_common, "Git common directory")? != state.git_common_dir {
        return Err(CandidateWorkspaceError::Mismatch(
            "Git common directory does not match candidate authority".to_string(),
        ));
    }
    if require_current_source_head
        && git_text(&source, &["rev-parse", "HEAD"])? != state.starting_head
    {
        return Err(CandidateWorkspaceError::Mismatch(
            "source HEAD no longer matches the candidate starting HEAD".to_string(),
        ));
    }
    if require_current_source_head
        && git_text(&source, &["rev-parse", "HEAD^{tree}"])? != state.starting_tree
    {
        return Err(CandidateWorkspaceError::Mismatch(
            "source HEAD tree no longer matches the candidate starting tree".to_string(),
        ));
    }
    Ok((source, persisted))
}

fn adopt_existing_candidate(
    path: &Path,
    source: &Path,
    repository_identity_digest: &str,
    common_dir: &Path,
    starting_head: &str,
    starting_tree: &str,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError> {
    let candidate = canonical_real_directory(path, "existing candidate worktree")?;
    if candidate != path || candidate.starts_with(source) {
        return Err(CandidateWorkspaceError::Mismatch(
            "existing candidate is symlinked, substituted, or inside the source worktree"
                .to_string(),
        ));
    }
    validate_private_directory(&candidate)?;
    if !worktree_registered(source, &candidate)? || git_common_dir(&candidate)? != common_dir {
        return Err(CandidateWorkspaceError::Mismatch(
            "existing candidate is not the registered worktree for the authoritative repository"
                .to_string(),
        ));
    }
    require_detached_head(&candidate)?;
    if git_text(&candidate, &["rev-parse", "HEAD"])? != starting_head
        || git_text(&candidate, &["write-tree"])? != starting_tree
    {
        return Err(CandidateWorkspaceError::Mismatch(
            "existing candidate does not match the authoritative starting HEAD and tree"
                .to_string(),
        ));
    }
    let mut state = CandidateWorkspaceState {
        schema_version: CANDIDATE_WORKSPACE_SCHEMA_VERSION,
        path: path_text(&candidate, "candidate path")?.to_string(),
        source_worktree_root: path_text(source, "source worktree")?.to_string(),
        git_common_dir: path_text(common_dir, "Git common directory")?.to_string(),
        repository_identity_digest: repository_identity_digest.to_string(),
        starting_head: starting_head.to_string(),
        starting_tree: starting_tree.to_string(),
        candidate_head: starting_head.to_string(),
        candidate_tree: starting_tree.to_string(),
        candidate_diff_digest: sha256_bytes(&[]),
        lifecycle: CandidateWorkspaceLifecycle::Active,
        cleanup_started_at: None,
        cleaned_at: None,
    };
    refresh_candidate_workspace(&mut state)?;
    Ok(state)
}

fn exact_owned_candidate_remnant(
    source: &Path,
    candidate_path: &Path,
    common_dir: &Path,
    starting_head: &str,
) -> Result<bool, CandidateWorkspaceError> {
    let candidate = match canonical_real_directory(candidate_path, "candidate remnant") {
        Ok(candidate) if candidate == candidate_path => candidate,
        Ok(_) => return Ok(false),
        Err(_) => return Ok(false),
    };
    if !worktree_registered(source, &candidate)?
        || git_common_dir(&candidate)? != common_dir
        || require_detached_head(&candidate).is_err()
        || git_text(&candidate, &["rev-parse", "HEAD"])? != starting_head
    {
        return Ok(false);
    }
    Ok(true)
}

fn worktree_registered(source: &Path, candidate: &Path) -> Result<bool, CandidateWorkspaceError> {
    let output = git_text(source, &["worktree", "list", "--porcelain"])?;
    for line in output.lines() {
        let Some(value) = line.strip_prefix("worktree ") else {
            continue;
        };
        let path = PathBuf::from(value);
        if path.canonicalize().ok().as_deref() == Some(candidate) {
            return Ok(true);
        }
        if path == candidate {
            return Ok(true);
        }
    }
    Ok(false)
}

fn materialize_candidate_without_filters(candidate: &Path) -> Result<(), CandidateWorkspaceError> {
    git_success(candidate, &["read-tree", "HEAD"])?;
    let entries = load_index_entries(candidate)?;
    stream_index_blobs(candidate, &entries, |entry, size, reader| {
        materialize_index_entry(candidate, entry, size, reader)
    })
}

fn create_candidate_parent(
    repository_identity_digest: &str,
) -> Result<PathBuf, CandidateWorkspaceError> {
    let root = std::env::temp_dir().join(CANDIDATE_ROOT_DIR);
    ensure_private_authority_directory(&root)?;
    let repository = root.join(repository_identity_digest);
    ensure_private_authority_directory(&repository)?;
    canonical_real_directory(&repository, "candidate repository root")
}

fn existing_candidate_parent(
    repository_identity_digest: &str,
) -> Result<PathBuf, CandidateWorkspaceError> {
    let root = std::env::temp_dir().join(CANDIDATE_ROOT_DIR);
    validate_private_directory(&root)?;
    let repository = root.join(repository_identity_digest);
    validate_private_directory(&repository)?;
    canonical_real_directory(&repository, "candidate repository root")
}

fn ensure_private_authority_directory(path: &Path) -> Result<(), CandidateWorkspaceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(CandidateWorkspaceError::Unsafe(format!(
                "candidate authority path is not a real directory: {}",
                path.display()
            )))
        }
        Ok(_) => validate_private_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(CandidateWorkspaceError::Io)?;
            set_and_validate_private_directory(path)
        }
        Err(error) => Err(CandidateWorkspaceError::Io(error)),
    }
}

fn set_and_validate_private_directory(path: &Path) -> Result<(), CandidateWorkspaceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    validate_private_directory(path)
}

fn validate_private_directory(path: &Path) -> Result<(), CandidateWorkspaceError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CandidateWorkspaceError::Unsafe(format!(
            "candidate authority is not a real directory: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(CandidateWorkspaceError::Unsafe(format!(
                "candidate authority directory is not private (0700): {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn canonical_real_directory(path: &Path, kind: &str) -> Result<PathBuf, CandidateWorkspaceError> {
    let metadata = fs::symlink_metadata(path).map_err(CandidateWorkspaceError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CandidateWorkspaceError::Unsafe(format!(
            "{kind} must be a real directory: {}",
            path.display()
        )));
    }
    path.canonicalize().map_err(CandidateWorkspaceError::Io)
}

fn git_common_dir(worktree: &Path) -> Result<PathBuf, CandidateWorkspaceError> {
    let output = git_text(worktree, &["rev-parse", "--git-common-dir"])?;
    let path = PathBuf::from(output);
    let path = if path.is_absolute() {
        path
    } else {
        worktree.join(path)
    };
    path.canonicalize().map_err(CandidateWorkspaceError::Io)
}

fn git_text(worktree: &Path, args: &[&str]) -> Result<String, CandidateWorkspaceError> {
    let bytes = git_bytes(worktree, args)?;
    String::from_utf8(bytes)
        .map(|value| value.trim().to_string())
        .map_err(|error| CandidateWorkspaceError::Git(format!("Git output was not UTF-8: {error}")))
}

fn git_bytes(worktree: &Path, args: &[&str]) -> Result<Vec<u8>, CandidateWorkspaceError> {
    let output = sanitized_git_command()
        .args(args)
        .current_dir(worktree)
        .output()
        .map_err(CandidateWorkspaceError::Io)?;
    if !output.status.success() {
        return Err(CandidateWorkspaceError::Git(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

fn git_success(worktree: &Path, args: &[&str]) -> Result<(), CandidateWorkspaceError> {
    let output = sanitized_git_command()
        .args(args)
        .current_dir(worktree)
        .output()
        .map_err(CandidateWorkspaceError::Io)?;
    if !output.status.success() {
        return Err(CandidateWorkspaceError::Git(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[derive(Debug)]
struct CandidateIndexEntry {
    mode: String,
    object: String,
    path: Vec<u8>,
}

fn load_index_entries(
    worktree: &Path,
) -> Result<Vec<CandidateIndexEntry>, CandidateWorkspaceError> {
    let raw_entries = git_bytes(worktree, &["ls-files", "--stage", "-z"])?;
    let mut entries = Vec::new();
    for entry in raw_entries
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| {
                CandidateWorkspaceError::Git(
                    "git ls-files returned a malformed index entry".to_string(),
                )
            })?;
        let header = std::str::from_utf8(&entry[..tab]).map_err(|_| {
            CandidateWorkspaceError::Git("Git index metadata was not UTF-8".to_string())
        })?;
        let mut fields = header.split_whitespace();
        let mode = fields.next().unwrap_or("");
        let object = fields.next().unwrap_or("");
        let stage = fields.next().unwrap_or("");
        if fields.next().is_some() || stage != "0" {
            return Err(CandidateWorkspaceError::Mismatch(
                "candidate index contains an unmerged or malformed entry".to_string(),
            ));
        }
        if !matches!(mode, "100644" | "100755" | "120000") {
            return Err(CandidateWorkspaceError::Unsafe(format!(
                "candidate index mode is not supported safely: {mode}"
            )));
        }
        validate_object_id(object, "candidate index object")?;
        index_relative_path(&entry[tab + 1..])?;
        entries.push(CandidateIndexEntry {
            mode: mode.to_string(),
            object: object.to_string(),
            path: entry[tab + 1..].to_vec(),
        });
    }
    Ok(entries)
}

fn index_relative_path(bytes: &[u8]) -> Result<PathBuf, CandidateWorkspaceError> {
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()))
    };
    #[cfg(not(unix))]
    let path = PathBuf::from(std::str::from_utf8(bytes).map_err(|_| {
        CandidateWorkspaceError::Unsafe(
            "candidate index path is not UTF-8 on this platform".to_string(),
        )
    })?);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(CandidateWorkspaceError::Unsafe(
            "candidate index path is not a safe relative path".to_string(),
        ));
    }
    Ok(path)
}

fn materialize_index_entry(
    root: &Path,
    entry: &CandidateIndexEntry,
    size: usize,
    reader: &mut dyn std::io::Read,
) -> Result<(), CandidateWorkspaceError> {
    let relative = index_relative_path(&entry.path)?;
    let path = root.join(&relative);
    ensure_materialization_parent(root, &relative)?;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(CandidateWorkspaceError::Unsafe(format!(
                "candidate materialization target already exists: {}",
                relative.to_string_lossy()
            )));
        }
        Err(error) => return Err(CandidateWorkspaceError::Io(error)),
    }
    match entry.mode.as_str() {
        "100644" | "100755" => {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(if entry.mode == "100755" { 0o755 } else { 0o644 });
            let mut file = options.open(&path)?;
            let copied = std::io::copy(reader, &mut file)?;
            if copied != size as u64 {
                return Err(CandidateWorkspaceError::Git(
                    "git cat-file ended before the indexed blob was materialized".to_string(),
                ));
            }
            file.sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &path,
                    fs::Permissions::from_mode(if entry.mode == "100755" { 0o755 } else { 0o644 }),
                )?;
            }
        }
        "120000" => {
            let bytes = read_bounded_symlink_blob(reader, size)?;
            create_symlink_from_bytes(&bytes, &path)?;
        }
        _ => unreachable!("index modes are checked before materialization"),
    }
    Ok(())
}

fn ensure_materialization_parent(
    root: &Path,
    relative: &Path,
) -> Result<(), CandidateWorkspaceError> {
    let mut current = root.to_path_buf();
    let Some(parent) = relative.parent() else {
        return Ok(());
    };
    for component in parent.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(CandidateWorkspaceError::Unsafe(
                "candidate parent contains an unsafe path component".to_string(),
            ));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(CandidateWorkspaceError::Unsafe(format!(
                    "candidate parent is not a real directory: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(CandidateWorkspaceError::Io(error)),
                }
                let metadata = fs::symlink_metadata(&current)?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(CandidateWorkspaceError::Unsafe(format!(
                        "candidate parent was substituted during creation: {}",
                        current.display()
                    )));
                }
            }
            Err(error) => return Err(CandidateWorkspaceError::Io(error)),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn create_symlink_from_bytes(bytes: &[u8], path: &Path) -> Result<(), CandidateWorkspaceError> {
    use std::os::unix::ffi::OsStringExt;
    std::os::unix::fs::symlink(std::ffi::OsString::from_vec(bytes.to_vec()), path)
        .map_err(CandidateWorkspaceError::Io)
}

#[cfg(not(unix))]
fn create_symlink_from_bytes(_bytes: &[u8], _path: &Path) -> Result<(), CandidateWorkspaceError> {
    Err(CandidateWorkspaceError::Unsafe(
        "raw symbolic-link materialization is unsupported on this platform".to_string(),
    ))
}

#[cfg(unix)]
fn read_symlink_bytes(path: &Path) -> Result<Vec<u8>, CandidateWorkspaceError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(fs::read_link(path)?.into_os_string().into_vec())
}

const MAX_SYMLINK_TARGET_BYTES: usize = 4096;

fn read_bounded_symlink_blob(
    reader: &mut dyn std::io::Read,
    size: usize,
) -> Result<Vec<u8>, CandidateWorkspaceError> {
    if size > MAX_SYMLINK_TARGET_BYTES {
        return Err(CandidateWorkspaceError::Unsafe(format!(
            "candidate symlink target exceeds {MAX_SYMLINK_TARGET_BYTES} bytes"
        )));
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_symlink_bytes(_path: &Path) -> Result<Vec<u8>, CandidateWorkspaceError> {
    Err(CandidateWorkspaceError::Unsafe(
        "raw symbolic-link verification is unsupported on this platform".to_string(),
    ))
}

fn verify_worktree_matches_index(worktree: &Path) -> Result<(), CandidateWorkspaceError> {
    let entries = load_index_entries(worktree)?;
    stream_index_blobs(worktree, &entries, |entry, size, reader| {
        let relative = index_relative_path(&entry.path)?;
        let path = worktree.join(&relative);
        let display = relative.to_string_lossy();
        match entry.mode.as_str() {
            "100644" | "100755" => {
                let metadata = fs::symlink_metadata(&path).map_err(CandidateWorkspaceError::Io)?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(CandidateWorkspaceError::Mismatch(format!(
                        "candidate worktree entry has the wrong file type: {display}"
                    )));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let executable = metadata.permissions().mode() & 0o111 != 0;
                    if executable != (entry.mode == "100755") {
                        return Err(CandidateWorkspaceError::Mismatch(format!(
                            "candidate executable mode differs from its index: {display}"
                        )));
                    }
                }
                compare_regular_file_to_blob(&path, size, reader)?;
            }
            "120000" => {
                let expected = read_bounded_symlink_blob(reader, size)?;
                if read_symlink_bytes(&path)? != expected {
                    return Err(CandidateWorkspaceError::Mismatch(format!(
                        "candidate worktree differs from its index: {display}"
                    )));
                }
            }
            _ => {
                return Err(CandidateWorkspaceError::Unsafe(format!(
                    "candidate index mode is not supported safely: {} for {display}",
                    entry.mode
                )));
            }
        }
        Ok(())
    })
}

fn compare_regular_file_to_blob(
    path: &Path,
    size: usize,
    reader: &mut dyn Read,
) -> Result<(), CandidateWorkspaceError> {
    let mut file = fs::File::open(path)?;
    let mut remaining = size;
    let mut expected = [0_u8; 8192];
    let mut actual = [0_u8; 8192];
    while remaining > 0 {
        let chunk = remaining.min(expected.len());
        reader.read_exact(&mut expected[..chunk])?;
        file.read_exact(&mut actual[..chunk]).map_err(|error| {
            CandidateWorkspaceError::Mismatch(format!(
                "candidate worktree file is shorter than its index blob {}: {error}",
                path.display()
            ))
        })?;
        if expected[..chunk] != actual[..chunk] {
            return Err(CandidateWorkspaceError::Mismatch(format!(
                "candidate worktree differs from its index: {}",
                path.display()
            )));
        }
        remaining -= chunk;
    }
    let mut extra = [0_u8; 1];
    if file.read(&mut extra)? != 0 {
        return Err(CandidateWorkspaceError::Mismatch(format!(
            "candidate worktree file is longer than its index blob: {}",
            path.display()
        )));
    }
    Ok(())
}

fn stream_index_blobs<F>(
    worktree: &Path,
    entries: &[CandidateIndexEntry],
    mut consume: F,
) -> Result<(), CandidateWorkspaceError>
where
    F: FnMut(&CandidateIndexEntry, usize, &mut dyn Read) -> Result<(), CandidateWorkspaceError>,
{
    use std::process::Stdio;

    if entries.is_empty() {
        return Ok(());
    }
    let mut child = sanitized_git_command()
        .args(["cat-file", "--batch"])
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        CandidateWorkspaceError::Git("git cat-file stdin was unavailable".to_string())
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        CandidateWorkspaceError::Git("git cat-file stdout was unavailable".to_string())
    })?;
    let mut stdout = BufReader::new(stdout);

    let result = (|| {
        for entry in entries {
            stdin.write_all(entry.object.as_bytes())?;
            stdin.write_all(b"\n")?;
            stdin.flush()?;
            let mut header = String::new();
            if stdout.read_line(&mut header)? == 0 {
                return Err(CandidateWorkspaceError::Git(
                    "git cat-file --batch returned a truncated header".to_string(),
                ));
            }
            let mut fields = header.split_whitespace();
            let object = fields.next().unwrap_or("");
            let kind = fields.next().unwrap_or("");
            let size = fields
                .next()
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or_else(|| {
                    CandidateWorkspaceError::Git(
                        "git cat-file --batch returned an invalid object size".to_string(),
                    )
                })?;
            if fields.next().is_some() || object != entry.object || kind != "blob" {
                return Err(CandidateWorkspaceError::Git(
                    "git cat-file --batch returned an unexpected object".to_string(),
                ));
            }
            {
                let mut blob = Read::take(&mut stdout, size as u64);
                consume(entry, size, &mut blob)?;
                if blob.limit() != 0 {
                    return Err(CandidateWorkspaceError::Git(
                        "blob consumer did not read the complete indexed object".to_string(),
                    ));
                }
            }
            let mut newline = [0_u8; 1];
            stdout.read_exact(&mut newline)?;
            if newline != [b'\n'] {
                return Err(CandidateWorkspaceError::Git(
                    "git cat-file --batch blob terminator was malformed".to_string(),
                ));
            }
        }
        Ok(())
    })();
    drop(stdin);
    drop(stdout);
    if let Err(error) = result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(CandidateWorkspaceError::Git(format!(
            "git cat-file --batch failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn require_detached_head(worktree: &Path) -> Result<(), CandidateWorkspaceError> {
    let output = sanitized_git_command()
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(worktree)
        .output()
        .map_err(CandidateWorkspaceError::Io)?;
    match output.status.code() {
        Some(1) => Ok(()),
        Some(0) => Err(CandidateWorkspaceError::Mismatch(
            "candidate HEAD must remain detached".to_string(),
        )),
        _ => Err(CandidateWorkspaceError::Git(format!(
            "git symbolic-ref -q HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

fn sanitized_git_command() -> Command {
    let mut command = Command::new("git");
    command.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        &format!("core.hooksPath={}", null_device()),
    ]);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_ATTR_NOSYSTEM",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_EXTERNAL_DIFF",
        "GIT_DIFF_OPTS",
        "GIT_PAGER",
        "GIT_EDITOR",
        "GIT_SEQUENCE_EDITOR",
        "GIT_ASKPASS",
        "SSH_ASKPASS",
    ] {
        command.env_remove(name);
    }
    for (name, _) in env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("GIT_CONFIG_KEY_") || name.starts_with("GIT_CONFIG_VALUE_") {
            command.env_remove(name.as_ref());
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1");
    command
}

fn null_device() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

fn validate_bound_evidence(state: &CandidateWorkspaceState) -> Result<(), CandidateWorkspaceError> {
    if state.candidate_head != state.starting_head {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate HEAD does not equal its starting HEAD".to_string(),
        ));
    }
    let empty = sha256_bytes(&[]);
    if state.candidate_tree == state.starting_tree && state.candidate_diff_digest == empty {
        Ok(())
    } else {
        Err(CandidateWorkspaceError::Mismatch(
            "M1-05a candidate evidence must bind the starting tree and empty diff".to_string(),
        ))
    }
}

fn validate_digest(value: &str, kind: &str) -> Result<(), CandidateWorkspaceError> {
    if value.len() == 64
        && value
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(CandidateWorkspaceError::Mismatch(format!(
            "{kind} digest is not lowercase SHA-256"
        )))
    }
}

fn validate_object_id(value: &str, kind: &str) -> Result<(), CandidateWorkspaceError> {
    if matches!(value.len(), 40 | 64)
        && value
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(CandidateWorkspaceError::Mismatch(format!(
            "{kind} is not a valid Git object ID"
        )))
    }
}

fn path_text<'a>(path: &'a Path, kind: &str) -> Result<&'a str, CandidateWorkspaceError> {
    path.to_str().ok_or_else(|| {
        CandidateWorkspaceError::Unsafe(format!("{kind} is not valid UTF-8: {}", path.display()))
    })
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn now_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[derive(Debug)]
pub enum CandidateWorkspaceError {
    Unsafe(String),
    Mismatch(String),
    Git(String),
    State(String),
    Io(std::io::Error),
}

impl fmt::Display for CandidateWorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsafe(message) => write!(formatter, "unsafe candidate workspace: {message}"),
            Self::Mismatch(message) => write!(formatter, "candidate workspace mismatch: {message}"),
            Self::Git(message) => write!(formatter, "candidate Git operation failed: {message}"),
            Self::State(message) => {
                write!(formatter, "candidate state operation failed: {message}")
            }
            Self::Io(error) => write!(formatter, "candidate workspace I/O error: {error}"),
        }
    }
}

impl Error for CandidateWorkspaceError {}

impl From<std::io::Error> for CandidateWorkspaceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seaf_core::LoopInputDigests;
    use std::process::Command;

    #[test]
    fn injected_post_remove_failure_leaves_durable_cleaning_for_retry() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("source");
        fs::create_dir(&source).expect("source");
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "seaf@example.invalid"],
            vec!["config", "user.name", "SEAF Test"],
        ] {
            test_git(&source, &args);
        }
        fs::write(source.join("tracked.txt"), "source\n").expect("tracked");
        test_git(&source, &["add", "tracked.txt"]);
        test_git(&source, &["commit", "-qm", "initial"]);
        let workspace =
            LoopWorkspace::create(&temp.path().join("runs"), "cleanup-crash").expect("workspace");
        let repository_identity_digest = sha256_bytes(source.as_os_str().as_encoded_bytes());
        let candidate = create_candidate_workspace(
            workspace.run_directory(),
            &source,
            &repository_identity_digest,
        )
        .expect("candidate");
        let mut run = crate::state::create_run(crate::state::NewLoopRun {
            run_id: "cleanup-crash".to_string(),
            ticket_id: "T-1".to_string(),
            goal_id: "goal".to_string(),
            provider: "fake".to_string(),
            model: "model".to_string(),
            input_digests: LoopInputDigests {
                ticket: "1".repeat(64),
                policy: "2".repeat(64),
                config: "3".repeat(64),
                repository: repository_identity_digest,
            },
        });
        run.status = LoopStatus::Completed;
        run.candidate_workspace = Some(candidate.clone());
        crate::state::save_run(&workspace, &run).expect("run");

        let error = cleanup_candidate_workspace_with_hook(&workspace, &source, |phase| {
            if phase == CandidateCleanupPhase::WorktreeRemoved {
                Err(CandidateWorkspaceError::State(
                    "injected post-remove crash".to_string(),
                ))
            } else {
                Ok(())
            }
        })
        .expect_err("inject crash");
        assert!(error.to_string().contains("injected"), "{error}");
        assert!(!Path::new(&candidate.path).exists());
        assert_eq!(
            crate::state::load_run(&workspace)
                .unwrap()
                .candidate_workspace
                .unwrap()
                .lifecycle,
            CandidateWorkspaceLifecycle::Cleaning
        );
        let stale_publish =
            crate::provider_exchange::persist_run_with_provider_exchange_compare(&workspace, &run)
                .expect_err("stale Active state cannot replace durable Cleaning");
        assert!(
            stale_publish
                .to_string()
                .contains("candidate workspace changed"),
            "{stale_publish}"
        );
        assert_eq!(
            cleanup_candidate_workspace(&workspace, &source)
                .expect("retry")
                .lifecycle,
            CandidateWorkspaceLifecycle::Cleaned
        );
    }

    #[test]
    fn concurrent_run_change_before_cleanup_intent_fails_cas_without_removal() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("source");
        fs::create_dir(&source).expect("source");
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "seaf@example.invalid"],
            vec!["config", "user.name", "SEAF Test"],
        ] {
            test_git(&source, &args);
        }
        fs::write(source.join("tracked.txt"), "source\n").expect("tracked");
        test_git(&source, &["add", "tracked.txt"]);
        test_git(&source, &["commit", "-qm", "initial"]);
        let workspace =
            LoopWorkspace::create(&temp.path().join("runs"), "cleanup-cas").expect("workspace");
        let repository_identity_digest = sha256_bytes(source.as_os_str().as_encoded_bytes());
        let candidate = create_candidate_workspace(
            workspace.run_directory(),
            &source,
            &repository_identity_digest,
        )
        .expect("candidate");
        let mut run = crate::state::create_run(crate::state::NewLoopRun {
            run_id: "cleanup-cas".to_string(),
            ticket_id: "T-1".to_string(),
            goal_id: "goal".to_string(),
            provider: "fake".to_string(),
            model: "model".to_string(),
            input_digests: LoopInputDigests {
                ticket: "1".repeat(64),
                policy: "2".repeat(64),
                config: "3".repeat(64),
                repository: repository_identity_digest,
            },
        });
        run.status = LoopStatus::Completed;
        run.candidate_workspace = Some(candidate.clone());
        crate::state::save_run(&workspace, &run).expect("run");

        let error = cleanup_candidate_workspace_with_hook(&workspace, &source, |phase| {
            if phase == CandidateCleanupPhase::BeforeIntentPersisted {
                let mut concurrent = crate::state::load_run(&workspace)
                    .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
                concurrent.updated_at = "concurrent-change".to_string();
                crate::state::write_run_file(&workspace.run_file(), &concurrent)
                    .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
            }
            Ok(())
        })
        .expect_err("full LoopRun CAS rejects concurrent change");
        assert!(error.to_string().contains("compare-and-swap"), "{error}");
        assert!(Path::new(&candidate.path).is_dir());
        assert_eq!(
            crate::state::load_run(&workspace)
                .unwrap()
                .candidate_workspace
                .unwrap()
                .lifecycle,
            CandidateWorkspaceLifecycle::Active
        );
        test_git(
            &source,
            &["worktree", "remove", "--force", candidate.path.as_str()],
        );
    }

    fn test_git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
