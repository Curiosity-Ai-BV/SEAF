use std::{
    collections::HashSet,
    env,
    error::Error,
    fmt, fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, ArtifactReference, CandidatePatchPhase,
    CandidatePatchTransaction, CandidateWorkspaceLifecycle, CandidateWorkspaceState,
    LoopExecutionMode, LoopStatus, LoopStepName,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    context::{CandidateContextAuthority, CandidateContextAuthorityKind},
    immutable_artifact::{publish_create_only, read_verified_regular_file},
    workspace::{LoopWorkspace, ARTIFACTS_DIR},
    DevelopmentEvidence, PatchDecisionKind, PolicyDecision,
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

pub const CANDIDATE_WORKSPACE_SCHEMA_VERSION: u32 = 2;
const CANDIDATE_ROOT_DIR: &str = "seaf-candidates";
const CANDIDATE_LOCK_FILE: &str = ".candidate-workspace.lock";
const REPOSITORY_OPERATION_LOCKS_DIR: &str = ".repository-operation-locks";
const REPOSITORY_OPERATION_LOCK_FILE: &str = ".repository-operation.lock";
const PATCH_INTENT_PATH: &str = "artifacts/candidate-patch.intent.json";
const PATCH_EXPECTED_DIFF_PATH: &str = "artifacts/candidate-patch.expected.diff";
const PATCH_APPLIED_DIFF_PATH: &str = "artifacts/candidate-patch.applied.diff";
const PATCH_APPLIED_EVIDENCE_PATH: &str = "artifacts/candidate-patch.applied.json";
static PATCH_PLAN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedCandidatePatchEvidence {
    pub development_evidence: ArtifactReference,
    pub policy_decision: PolicyDecision,
    pub policy_decision_digest: String,
    pub candidate_authority: CandidateContextAuthority,
    pub intent: ArtifactReference,
    pub applied_evidence: ArtifactReference,
    pub candidate_tree: String,
    pub applied_diff: ArtifactReference,
    pub applied_diff_digest: String,
    pub applied_diff_content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateCleanupOutcome {
    pub run_id: String,
    pub status: LoopStatus,
    pub candidate: CandidateWorkspaceState,
}

impl CandidateCleanupOutcome {
    fn from_locked_run(
        run: &seaf_core::LoopRun,
        candidate: CandidateWorkspaceState,
    ) -> Result<Self, CandidateWorkspaceError> {
        if matches!(run.status, LoopStatus::Pending | LoopStatus::Running)
            || candidate.lifecycle != CandidateWorkspaceLifecycle::Cleaned
            || run.candidate_workspace.as_ref() != Some(&candidate)
        {
            return Err(CandidateWorkspaceError::Mismatch(
                "cleanup outcome is not the exact locked terminal Cleaned authority".to_string(),
            ));
        }
        Ok(Self {
            run_id: run.run_id.clone(),
            status: run.status,
            candidate,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidatePatchIntent {
    schema_version: u32,
    run_id: String,
    candidate_path: String,
    source_worktree_root: String,
    git_common_dir: String,
    repository_identity_digest: String,
    starting_head: String,
    starting_tree: String,
    development_evidence: ArtifactReference,
    patch_digest: String,
    policy_decision_digest: String,
    changed_paths: Vec<String>,
    expected_candidate_tree: String,
    expected_candidate_diff: ArtifactReference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidatePatchAppliedEvidence {
    schema_version: u32,
    run_id: String,
    intent: ArtifactReference,
    observed_candidate_tree: String,
    observed_candidate_diff: ArtifactReference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidatePatchApplicationPhase {
    BeforeApplyingPersisted,
    ApplyingPersisted,
    Materialized,
    AppliedPersisted,
}

pub fn apply_candidate_development_evidence(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError> {
    apply_candidate_development_evidence_with_hook(workspace, source_worktree_root, |_| Ok(()))
}

pub fn verify_candidate_patch_evidence(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
) -> Result<VerifiedCandidatePatchEvidence, CandidateWorkspaceError> {
    preflight_workspace_run_directory_authority(workspace)?;
    let lock = acquire_candidate_lock(workspace)?;
    let result = verify_candidate_patch_evidence_locked(workspace, source_worktree_root);
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(CandidateWorkspaceError::Io(error)),
        (Err(error), _) => Err(error),
    }
}

fn verify_candidate_patch_evidence_locked(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
) -> Result<VerifiedCandidatePatchEvidence, CandidateWorkspaceError> {
    let run = crate::state::load_run(workspace)
        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    if run.execution_mode != LoopExecutionMode::IsolatedCandidate {
        return Err(CandidateWorkspaceError::Unsafe(
            "candidate patch verification requires isolated_candidate execution".to_string(),
        ));
    }
    let candidate = run.candidate_workspace.as_ref().ok_or_else(|| {
        CandidateWorkspaceError::Mismatch(
            "isolated candidate run has no candidate workspace authority".to_string(),
        )
    })?;
    if candidate.lifecycle != CandidateWorkspaceLifecycle::Active {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate patch verification requires an active candidate".to_string(),
        ));
    }
    validate_candidate_physical(
        workspace.run_directory(),
        source_worktree_root,
        candidate,
        true,
    )?;
    let development_evidence = development_evidence_reference(&run)?;
    let evidence = DevelopmentEvidence::load(
        workspace,
        &development_evidence.path,
        &development_evidence.digest,
        &run.run_id,
    )
    .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    let policy_decision = authoritative_policy_decision(&run, &evidence)?;
    if policy_decision.applied
        || !matches!(
            policy_decision.decision,
            PatchDecisionKind::Allowed | PatchDecisionKind::RequiresHumanReview
        )
    {
        return Err(CandidateWorkspaceError::Unsafe(
            "verified candidate patch requires an unapplied Allowed or RequiresHumanReview policy decision"
                .to_string(),
        ));
    }
    let transaction = candidate.patch_transaction.as_ref().ok_or_else(|| {
        CandidateWorkspaceError::Mismatch(
            "verified candidate patch requires an Applied transaction".to_string(),
        )
    })?;
    if transaction.phase != CandidatePatchPhase::Applied {
        return Err(CandidateWorkspaceError::Mismatch(
            "verified candidate patch transaction is not Applied".to_string(),
        ));
    }
    let intent: CandidatePatchIntent = load_canonical_artifact(workspace, &transaction.intent)?;
    validate_patch_intent(
        workspace,
        &run,
        candidate,
        &development_evidence,
        &evidence,
        &policy_decision,
        &intent,
    )?;
    validate_applied_patch_evidence(workspace, &run, candidate, transaction, &intent)?;
    let applied_evidence = transaction.applied_evidence.clone().ok_or_else(|| {
        CandidateWorkspaceError::Mismatch(
            "Applied candidate transaction has no applied evidence".to_string(),
        )
    })?;
    let applied: CandidatePatchAppliedEvidence =
        load_canonical_artifact(workspace, &applied_evidence)?;
    let applied_diff_bytes = load_artifact_bytes(workspace, &applied.observed_candidate_diff)?;
    let applied_diff_content = String::from_utf8(applied_diff_bytes).map_err(|error| {
        CandidateWorkspaceError::Mismatch(format!(
            "verified candidate applied diff is not UTF-8: {error}"
        ))
    })?;
    let policy_decision_digest = canonical_sha256_digest(&policy_decision)
        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    Ok(VerifiedCandidatePatchEvidence {
        development_evidence,
        policy_decision,
        policy_decision_digest,
        candidate_authority: CandidateContextAuthority {
            kind: CandidateContextAuthorityKind::IsolatedCandidate,
            repository_identity_digest: candidate.repository_identity_digest.clone(),
            candidate_path_digest: sha256_bytes(candidate.path.as_bytes()),
            starting_head: candidate.starting_head.clone(),
            starting_tree: candidate.starting_tree.clone(),
        },
        intent: transaction.intent.clone(),
        applied_evidence,
        candidate_tree: candidate.candidate_tree.clone(),
        applied_diff_digest: applied.observed_candidate_diff.digest.clone(),
        applied_diff: applied.observed_candidate_diff,
        applied_diff_content,
    })
}

fn apply_candidate_development_evidence_with_hook<F>(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
    mut hook: F,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError>
where
    F: FnMut(CandidatePatchApplicationPhase) -> Result<(), CandidateWorkspaceError>,
{
    preflight_workspace_run_directory_authority(workspace)?;
    let lock = acquire_candidate_lock(workspace)?;
    let result =
        apply_candidate_development_evidence_locked(workspace, source_worktree_root, &mut hook);
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(CandidateWorkspaceError::Io(error)),
        (Err(error), _) => Err(error),
    }
}

fn apply_candidate_development_evidence_locked(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
    hook: &mut dyn FnMut(CandidatePatchApplicationPhase) -> Result<(), CandidateWorkspaceError>,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError> {
    let mut run = crate::state::load_run(workspace)
        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    if run.execution_mode != LoopExecutionMode::IsolatedCandidate {
        return Err(CandidateWorkspaceError::Unsafe(
            "candidate patch application requires isolated_candidate execution".to_string(),
        ));
    }
    if run.status != LoopStatus::Running {
        return Err(CandidateWorkspaceError::Unsafe(
            "candidate patch application requires a running LoopRun".to_string(),
        ));
    }
    let candidate = run.candidate_workspace.clone().ok_or_else(|| {
        CandidateWorkspaceError::Mismatch(
            "isolated candidate run has no candidate workspace authority".to_string(),
        )
    })?;
    if candidate.lifecycle != CandidateWorkspaceLifecycle::Active {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate patch application requires an active candidate".to_string(),
        ));
    }
    validate_candidate_application_identity(
        workspace.run_directory(),
        source_worktree_root,
        &candidate,
    )?;

    let development_reference = development_evidence_reference(&run)?;
    let evidence = DevelopmentEvidence::load(
        workspace,
        &development_reference.path,
        &development_reference.digest,
        &run.run_id,
    )
    .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    let policy = authoritative_policy_decision(&run, &evidence)?;
    if policy.applied
        || !matches!(
            policy.decision,
            PatchDecisionKind::Allowed | PatchDecisionKind::RequiresHumanReview
        )
    {
        return Err(CandidateWorkspaceError::Unsafe(
            "only an unapplied Allowed or RequiresHumanReview policy decision may materialize in the candidate"
                .to_string(),
        ));
    }

    let (intent_reference, intent, applying_run) = match &candidate.patch_transaction {
        None => {
            validate_candidate_physical(
                workspace.run_directory(),
                source_worktree_root,
                &candidate,
                true,
            )?;
            let plan = plan_candidate_patch(workspace, &candidate, &evidence)?;
            let expected_diff = write_create_only_artifact(
                workspace,
                PATCH_EXPECTED_DIFF_PATH,
                &plan.expected_diff,
            )?;
            let intent = CandidatePatchIntent {
                schema_version: 1,
                run_id: run.run_id.clone(),
                candidate_path: candidate.path.clone(),
                source_worktree_root: candidate.source_worktree_root.clone(),
                git_common_dir: candidate.git_common_dir.clone(),
                repository_identity_digest: candidate.repository_identity_digest.clone(),
                starting_head: candidate.starting_head.clone(),
                starting_tree: candidate.starting_tree.clone(),
                development_evidence: development_reference.clone(),
                patch_digest: evidence.patch_digest.clone(),
                policy_decision_digest: canonical_sha256_digest(&policy)
                    .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?,
                changed_paths: evidence.changed_paths.clone(),
                expected_candidate_tree: plan.expected_tree,
                expected_candidate_diff: expected_diff,
            };
            let intent_reference = write_create_only_artifact(
                workspace,
                PATCH_INTENT_PATH,
                &canonical_json_bytes(&intent)
                    .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?,
            )?;
            let expected = run.clone();
            let mut applying_candidate = candidate.clone();
            applying_candidate.patch_transaction = Some(CandidatePatchTransaction {
                schema_version: 1,
                phase: CandidatePatchPhase::Applying,
                intent: intent_reference.clone(),
                applied_evidence: None,
                started_at: now_timestamp(),
                applied_at: None,
            });
            run.candidate_workspace = Some(applying_candidate);
            hook(CandidatePatchApplicationPhase::BeforeApplyingPersisted)?;
            persist_candidate_run(workspace, &expected, &run)?;
            hook(CandidatePatchApplicationPhase::ApplyingPersisted)?;
            (intent_reference, intent, run.clone())
        }
        Some(transaction) if transaction.phase == CandidatePatchPhase::Applying => {
            let intent: CandidatePatchIntent =
                load_canonical_artifact(workspace, &transaction.intent)?;
            validate_patch_intent(
                workspace,
                &run,
                &candidate,
                &development_reference,
                &evidence,
                &policy,
                &intent,
            )?;
            (transaction.intent.clone(), intent, run.clone())
        }
        Some(transaction) => {
            let intent: CandidatePatchIntent =
                load_canonical_artifact(workspace, &transaction.intent)?;
            validate_candidate_physical(
                workspace.run_directory(),
                source_worktree_root,
                &candidate,
                true,
            )?;
            validate_patch_intent(
                workspace,
                &run,
                &candidate,
                &development_reference,
                &evidence,
                &policy,
                &intent,
            )?;
            validate_applied_patch_evidence(workspace, &run, &candidate, transaction, &intent)?;
            return Ok(candidate);
        }
    };

    materialize_planned_candidate_patch(&candidate, &evidence, &intent)?;
    hook(CandidatePatchApplicationPhase::Materialized)?;
    let observed_tree = git_text(Path::new(&candidate.path), &["write-tree"])?;
    let observed_diff = staged_diff(Path::new(&candidate.path))?;
    let observed_digest = sha256_bytes(&observed_diff);
    if observed_tree != intent.expected_candidate_tree
        || observed_digest != intent.expected_candidate_diff.digest
    {
        return Err(CandidateWorkspaceError::Mismatch(
            "materialized candidate tree or staged diff differs from immutable patch intent"
                .to_string(),
        ));
    }
    let applied_diff =
        write_create_only_artifact(workspace, PATCH_APPLIED_DIFF_PATH, &observed_diff)?;
    let applied_evidence = CandidatePatchAppliedEvidence {
        schema_version: 1,
        run_id: applying_run.run_id.clone(),
        intent: intent_reference.clone(),
        observed_candidate_tree: observed_tree.clone(),
        observed_candidate_diff: applied_diff,
    };
    let applied_reference = write_create_only_artifact(
        workspace,
        PATCH_APPLIED_EVIDENCE_PATH,
        &canonical_json_bytes(&applied_evidence)
            .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?,
    )?;
    let mut applied_run = applying_run.clone();
    let applied_candidate = applied_run.candidate_workspace.as_mut().ok_or_else(|| {
        CandidateWorkspaceError::State("Applying run lost candidate authority".to_string())
    })?;
    applied_candidate.candidate_tree = observed_tree;
    applied_candidate.candidate_diff_digest = observed_digest;
    let transaction = applied_candidate
        .patch_transaction
        .as_mut()
        .ok_or_else(|| {
            CandidateWorkspaceError::State("Applying run lost patch transaction".to_string())
        })?;
    transaction.phase = CandidatePatchPhase::Applied;
    transaction.applied_evidence = Some(applied_reference);
    transaction.applied_at = Some(now_timestamp());
    let applied_candidate = applied_candidate.clone();
    persist_candidate_run(workspace, &applying_run, &applied_run)?;
    hook(CandidatePatchApplicationPhase::AppliedPersisted)?;
    Ok(applied_candidate)
}

struct CandidatePatchPlan {
    expected_tree: String,
    expected_diff: Vec<u8>,
}

fn validate_candidate_application_identity(
    run_directory: &Path,
    source_worktree_root: &Path,
    state: &CandidateWorkspaceState,
) -> Result<(), CandidateWorkspaceError> {
    let (source, persisted) =
        validate_static_authority(run_directory, source_worktree_root, state, true)?;
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
    if git_common_dir(&candidate)? != Path::new(&state.git_common_dir)
        || git_text(&candidate, &["rev-parse", "HEAD"])? != state.starting_head
    {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate Git identity differs from patch authority".to_string(),
        ));
    }
    Ok(())
}

fn development_evidence_reference(
    run: &seaf_core::LoopRun,
) -> Result<ArtifactReference, CandidateWorkspaceError> {
    let record = run
        .steps
        .iter()
        .find(|record| record.name == LoopStepName::Development)
        .ok_or_else(|| {
            CandidateWorkspaceError::State("LoopRun has no Development step".to_string())
        })?;
    if record.status != seaf_core::LoopStepStatus::Completed {
        return Err(CandidateWorkspaceError::State(
            "candidate patch application requires completed Development evidence".to_string(),
        ));
    }
    match (&record.artifact_path, &record.artifact_digest) {
        (Some(path), Some(digest)) => Ok(ArtifactReference {
            path: path.clone(),
            digest: digest.clone(),
        }),
        _ => Err(CandidateWorkspaceError::State(
            "candidate patch application requires authoritative Development evidence".to_string(),
        )),
    }
}

fn authoritative_policy_decision(
    run: &seaf_core::LoopRun,
    evidence: &DevelopmentEvidence,
) -> Result<PolicyDecision, CandidateWorkspaceError> {
    let mut matching = run.policy_decisions.iter().filter(|entry| {
        entry.get("patch_id").and_then(serde_json::Value::as_str) == Some(run.run_id.as_str())
    });
    let entry = matching.next().ok_or_else(|| {
        CandidateWorkspaceError::State(
            "Development evidence is missing its authoritative policy decision".to_string(),
        )
    })?;
    if matching.next().is_some() {
        return Err(CandidateWorkspaceError::State(
            "Development evidence has multiple authoritative policy decisions".to_string(),
        ));
    }
    let decision: PolicyDecision = serde_json::from_value(
        serde_json::to_value(entry)
            .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?,
    )
    .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    if decision != evidence.policy_decision {
        return Err(CandidateWorkspaceError::Mismatch(
            "Development evidence policy decision differs from authoritative run state".to_string(),
        ));
    }
    Ok(decision)
}

fn plan_candidate_patch(
    workspace: &LoopWorkspace,
    candidate: &CandidateWorkspaceState,
    evidence: &DevelopmentEvidence,
) -> Result<CandidatePatchPlan, CandidateWorkspaceError> {
    let index_path = unique_patch_plan_index(workspace.run_directory())?;
    let candidate_path = Path::new(&candidate.path);
    let result = (|| {
        git_success_with_index(candidate_path, &["read-tree", "HEAD"], &index_path)?;
        git_apply_cached(
            candidate_path,
            &evidence.patch,
            Some(&index_path),
            &evidence.changed_paths,
        )?;
        let expected_tree = git_text_with_index(candidate_path, &["write-tree"], &index_path)?;
        validate_object_id(&expected_tree, "planned candidate tree")?;
        let expected_diff = git_bytes_with_index(
            candidate_path,
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
            &index_path,
        )?;
        if expected_tree == candidate.starting_tree || expected_diff.is_empty() {
            return Err(CandidateWorkspaceError::Mismatch(
                "Development patch produced no candidate tree transition".to_string(),
            ));
        }
        Ok(CandidatePatchPlan {
            expected_tree,
            expected_diff,
        })
    })();
    match fs::remove_file(&index_path) {
        Ok(()) => result,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => result,
        Err(error) => Err(CandidateWorkspaceError::Io(error)),
    }
}

fn unique_patch_plan_index(run_directory: &Path) -> Result<PathBuf, CandidateWorkspaceError> {
    loop {
        let sequence = PATCH_PLAN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = run_directory.join(format!(
            ".candidate-patch-plan.index-{}-{sequence}",
            std::process::id()
        ));
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(path),
            Ok(_) => continue,
            Err(error) => return Err(CandidateWorkspaceError::Io(error)),
        }
    }
}

fn validate_patch_intent(
    workspace: &LoopWorkspace,
    run: &seaf_core::LoopRun,
    candidate: &CandidateWorkspaceState,
    development_reference: &ArtifactReference,
    evidence: &DevelopmentEvidence,
    policy: &PolicyDecision,
    intent: &CandidatePatchIntent,
) -> Result<(), CandidateWorkspaceError> {
    let expected_policy_digest = canonical_sha256_digest(policy)
        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    if intent.schema_version != 1
        || intent.run_id != run.run_id
        || intent.candidate_path != candidate.path
        || intent.source_worktree_root != candidate.source_worktree_root
        || intent.git_common_dir != candidate.git_common_dir
        || intent.repository_identity_digest != candidate.repository_identity_digest
        || intent.starting_head != candidate.starting_head
        || intent.starting_tree != candidate.starting_tree
        || &intent.development_evidence != development_reference
        || intent.patch_digest != evidence.patch_digest
        || intent.policy_decision_digest != expected_policy_digest
        || intent.changed_paths != evidence.changed_paths
    {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate patch intent differs from authoritative run, candidate, Development, or policy evidence"
                .to_string(),
        ));
    }
    validate_object_id(&intent.expected_candidate_tree, "expected candidate tree")?;
    validate_digest(
        &intent.expected_candidate_diff.digest,
        "expected candidate diff",
    )?;
    let expected_diff = load_artifact_bytes(workspace, &intent.expected_candidate_diff)?;
    if expected_diff.is_empty() {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate patch intent expected diff is empty".to_string(),
        ));
    }
    let plan = plan_candidate_patch(workspace, candidate, evidence)?;
    if plan.expected_tree != intent.expected_candidate_tree || plan.expected_diff != expected_diff {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate patch intent does not derive from the authoritative Development patch"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_applied_patch_evidence(
    workspace: &LoopWorkspace,
    run: &seaf_core::LoopRun,
    candidate: &CandidateWorkspaceState,
    transaction: &CandidatePatchTransaction,
    intent: &CandidatePatchIntent,
) -> Result<(), CandidateWorkspaceError> {
    if transaction.phase != CandidatePatchPhase::Applied {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate patch evidence is not in Applied phase".to_string(),
        ));
    }
    let reference = transaction.applied_evidence.as_ref().ok_or_else(|| {
        CandidateWorkspaceError::Mismatch(
            "Applied candidate transaction has no applied evidence".to_string(),
        )
    })?;
    let evidence: CandidatePatchAppliedEvidence = load_canonical_artifact(workspace, reference)?;
    if evidence.schema_version != 1
        || evidence.run_id != run.run_id
        || evidence.intent != transaction.intent
        || evidence.observed_candidate_tree != candidate.candidate_tree
        || evidence.observed_candidate_tree != intent.expected_candidate_tree
        || evidence.observed_candidate_diff.digest != candidate.candidate_diff_digest
        || evidence.observed_candidate_diff.digest != intent.expected_candidate_diff.digest
    {
        return Err(CandidateWorkspaceError::Mismatch(
            "Applied candidate evidence differs from its run, intent, tree, or diff authority"
                .to_string(),
        ));
    }
    let expected_diff = load_artifact_bytes(workspace, &intent.expected_candidate_diff)?;
    let observed_diff = load_artifact_bytes(workspace, &evidence.observed_candidate_diff)?;
    if observed_diff != expected_diff || staged_diff(Path::new(&candidate.path))? != observed_diff {
        return Err(CandidateWorkspaceError::Mismatch(
            "Applied candidate staged diff bytes differ from immutable intent evidence".to_string(),
        ));
    }
    Ok(())
}

fn materialize_planned_candidate_patch(
    candidate: &CandidateWorkspaceState,
    evidence: &DevelopmentEvidence,
    intent: &CandidatePatchIntent,
) -> Result<(), CandidateWorkspaceError> {
    let candidate_path = Path::new(&candidate.path);
    let current_tree = git_text(candidate_path, &["write-tree"])?;
    if current_tree == candidate.starting_tree {
        verify_worktree_matches_index(candidate_path)?;
        git_apply_cached(
            candidate_path,
            &evidence.patch,
            None,
            &evidence.changed_paths,
        )?;
    } else if current_tree != intent.expected_candidate_tree {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate index is neither pristine nor the exact planned patch state".to_string(),
        ));
    }
    let planned_diff = staged_diff(candidate_path)?;
    if sha256_bytes(&planned_diff) != intent.expected_candidate_diff.digest {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate staged diff differs from immutable patch intent".to_string(),
        ));
    }
    raw_rematerialize_changed_paths(candidate_path, &evidence.changed_paths)?;
    verify_worktree_matches_index(candidate_path)?;
    let untracked = git_bytes(candidate_path, &["ls-files", "--others", "-z"])?;
    if !untracked.is_empty() {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate contains untracked files after patch materialization".to_string(),
        ));
    }
    Ok(())
}

fn raw_rematerialize_changed_paths(
    candidate: &Path,
    changed_paths: &[String],
) -> Result<(), CandidateWorkspaceError> {
    let changed = changed_paths
        .iter()
        .map(|path| index_relative_path(path.as_bytes()))
        .collect::<Result<HashSet<_>, _>>()?;
    let mut removals = changed.iter().cloned().collect::<Vec<_>>();
    removals.sort_by(|left, right| {
        right
            .components()
            .count()
            .cmp(&left.components().count())
            .then_with(|| right.cmp(left))
    });
    for relative in &removals {
        let path = candidate.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
                fs::remove_file(path)?;
            }
            Ok(metadata) if metadata.is_dir() => {
                fs::remove_dir(&path).map_err(|error| {
                    CandidateWorkspaceError::Mismatch(format!(
                        "changed candidate directory is not empty and cannot be safely replaced: {}: {error}",
                        relative.display()
                    ))
                })?;
            }
            Ok(_) => {
                return Err(CandidateWorkspaceError::Unsafe(format!(
                    "changed candidate path has an unsupported file type: {}",
                    relative.display()
                )));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => return Err(CandidateWorkspaceError::Io(error)),
        }
    }
    let entries = load_index_entries(candidate)?
        .into_iter()
        .filter(|entry| {
            index_relative_path(&entry.path)
                .map(|path| changed.contains(&path))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    stream_index_blobs(candidate, &entries, |entry, size, reader| {
        materialize_index_entry(candidate, entry, size, reader)
    })
}

fn staged_diff(candidate: &Path) -> Result<Vec<u8>, CandidateWorkspaceError> {
    git_bytes(
        candidate,
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
    )
}

fn write_create_only_artifact(
    workspace: &LoopWorkspace,
    relative: &str,
    bytes: &[u8],
) -> Result<ArtifactReference, CandidateWorkspaceError> {
    safe_artifact_relative_path(relative)?;
    let artifact_dir = workspace.run_directory().join(ARTIFACTS_DIR);
    let artifact_dir_created = match fs::symlink_metadata(&artifact_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(CandidateWorkspaceError::Unsafe(
                "candidate artifact directory is not a real directory".to_string(),
            ));
        }
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&artifact_dir)?;
            true
        }
        Err(error) => return Err(CandidateWorkspaceError::Io(error)),
    };
    if artifact_dir_created {
        fs::File::open(workspace.run_directory())?.sync_all()?;
    }
    let digest = sha256_bytes(bytes);
    publish_create_only(workspace.run_directory(), relative, bytes)
        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    Ok(ArtifactReference {
        path: relative.to_string(),
        digest,
    })
}

fn load_canonical_artifact<T>(
    workspace: &LoopWorkspace,
    reference: &ArtifactReference,
) -> Result<T, CandidateWorkspaceError>
where
    T: serde::de::DeserializeOwned,
{
    let bytes = load_artifact_bytes(workspace, reference)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    if canonical_json_bytes(&value)
        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?
        != bytes
    {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate evidence artifact is not canonical JSON".to_string(),
        ));
    }
    serde_json::from_value(value).map_err(|error| CandidateWorkspaceError::State(error.to_string()))
}

fn load_artifact_bytes(
    workspace: &LoopWorkspace,
    reference: &ArtifactReference,
) -> Result<Vec<u8>, CandidateWorkspaceError> {
    validate_digest(&reference.digest, "candidate artifact")?;
    safe_artifact_relative_path(&reference.path)?;
    let bytes = read_verified_regular_file(
        workspace.run_directory(),
        &reference.path,
        "candidate artifact",
    )
    .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    if sha256_bytes(&bytes) != reference.digest {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate artifact digest mismatch".to_string(),
        ));
    }
    Ok(bytes)
}

fn safe_artifact_relative_path(relative: &str) -> Result<PathBuf, CandidateWorkspaceError> {
    let path = Path::new(relative);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || path
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
            != Some(ARTIFACTS_DIR)
    {
        return Err(CandidateWorkspaceError::Unsafe(
            "candidate artifact path is not a safe artifacts-relative path".to_string(),
        ));
    }
    Ok(path.to_path_buf())
}

pub fn create_candidate_workspace(
    run_directory: &Path,
    source_worktree_root: &Path,
    repository_identity_digest: &str,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError> {
    validate_digest(repository_identity_digest, "repository identity")?;
    let run_id = safe_run_id(run_directory)?;
    let runs_root = run_directory.parent().ok_or_else(|| {
        CandidateWorkspaceError::Unsafe("run directory has no runs root".to_string())
    })?;
    let workspace = LoopWorkspace::open(runs_root, run_id)
        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    let run = crate::state::load_run(&workspace)
        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    let planned = run.candidate_workspace.as_ref().ok_or_else(|| {
        CandidateWorkspaceError::Mismatch(
            "authoritative LoopRun has no candidate workspace".to_string(),
        )
    })?;
    let source = canonical_real_directory(source_worktree_root, "source worktree")?;
    if planned.repository_identity_digest != repository_identity_digest
        || path_text(&source, "source worktree")? != planned.source_worktree_root
    {
        return Err(CandidateWorkspaceError::Mismatch(
            "requested source or repository differs from persisted candidate authority".to_string(),
        ));
    }
    provision_candidate_workspace(&workspace)
}

pub fn plan_candidate_workspace(
    run_directory: &Path,
    source_worktree_root: &Path,
    repository_identity_digest: &str,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError> {
    validate_digest(repository_identity_digest, "repository identity")?;
    let run_id = safe_run_id(run_directory)?;
    let source = canonical_real_directory(source_worktree_root, "source worktree")?;
    let starting_head = git_text(&source, &["rev-parse", "HEAD"])?;
    let starting_tree = git_text(&source, &["rev-parse", "HEAD^{tree}"])?;
    validate_object_id(&starting_head, "starting HEAD")?;
    validate_object_id(&starting_tree, "starting tree")?;
    let common_dir = git_common_dir(&source)?;
    let temp_root = env::temp_dir().canonicalize()?;
    let candidate_path = temp_root
        .join(CANDIDATE_ROOT_DIR)
        .join(repository_identity_digest)
        .join(run_id);
    if candidate_path.starts_with(&source) {
        return Err(CandidateWorkspaceError::Unsafe(
            "candidate path must be outside the source worktree".to_string(),
        ));
    }
    Ok(CandidateWorkspaceState {
        schema_version: CANDIDATE_WORKSPACE_SCHEMA_VERSION,
        run_directory_digest: Some(run_directory_digest(run_directory)?),
        path: path_text(&candidate_path, "candidate path")?.to_string(),
        source_worktree_root: path_text(&source, "source worktree")?.to_string(),
        git_common_dir: path_text(&common_dir, "Git common directory")?.to_string(),
        repository_identity_digest: repository_identity_digest.to_string(),
        starting_head: starting_head.clone(),
        starting_tree: starting_tree.clone(),
        candidate_head: starting_head,
        candidate_tree: starting_tree,
        candidate_diff_digest: sha256_bytes(&[]),
        patch_transaction: None,
        lifecycle: CandidateWorkspaceLifecycle::Provisioning,
        cleanup_started_at: None,
        cleaned_at: None,
    })
}

pub fn provision_candidate_workspace(
    workspace: &LoopWorkspace,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError> {
    provision_candidate_workspace_with_hook(workspace, |_| Ok(()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateProvisionPhase {
    BeforeWorktreeCreate,
    WorktreeCreated,
    ActivePersisted,
}

fn provision_candidate_workspace_with_hook<F>(
    workspace: &LoopWorkspace,
    mut hook: F,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError>
where
    F: FnMut(CandidateProvisionPhase) -> Result<(), CandidateWorkspaceError>,
{
    preflight_workspace_run_directory_authority(workspace)?;
    let lock = acquire_candidate_lock(workspace)?;
    let result = provision_candidate_workspace_locked(workspace, &mut hook);
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(CandidateWorkspaceError::Io(error)),
        (Err(error), _) => Err(error),
    }
}

fn provision_candidate_workspace_locked(
    workspace: &LoopWorkspace,
    hook: &mut dyn FnMut(CandidateProvisionPhase) -> Result<(), CandidateWorkspaceError>,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError> {
    let run = crate::state::load_run(workspace)
        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    if run.execution_mode != LoopExecutionMode::IsolatedCandidate {
        return Err(CandidateWorkspaceError::Unsafe(
            "candidate provisioning requires isolated_candidate execution".to_string(),
        ));
    }
    let planned = run.candidate_workspace.clone().ok_or_else(|| {
        CandidateWorkspaceError::Mismatch(
            "isolated candidate run has no candidate workspace authority".to_string(),
        )
    })?;
    if planned.lifecycle == CandidateWorkspaceLifecycle::Active {
        return validate_candidate_physical(
            workspace.run_directory(),
            Path::new(&planned.source_worktree_root),
            &planned,
            true,
        );
    }
    if planned.lifecycle != CandidateWorkspaceLifecycle::Provisioning {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate provisioning requires Provisioning or exact Active authority".to_string(),
        ));
    }
    hook(CandidateProvisionPhase::BeforeWorktreeCreate)?;
    let active = provision_planned_candidate(workspace.run_directory(), &planned)?;
    hook(CandidateProvisionPhase::WorktreeCreated)?;
    let mut intended = run.clone();
    intended.candidate_workspace = Some(active.clone());
    persist_candidate_run(workspace, &run, &intended)?;
    hook(CandidateProvisionPhase::ActivePersisted)?;
    Ok(active)
}

fn safe_run_id(run_directory: &Path) -> Result<&str, CandidateWorkspaceError> {
    run_directory
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
        })
}

fn provision_planned_candidate(
    run_directory: &Path,
    planned: &CandidateWorkspaceState,
) -> Result<CandidateWorkspaceState, CandidateWorkspaceError> {
    validate_provisioning_authority(run_directory, planned)?;
    let source = PathBuf::from(&planned.source_worktree_root);
    let common_dir = PathBuf::from(&planned.git_common_dir);

    let candidate_parent = create_candidate_parent(&planned.repository_identity_digest)?;
    let repository_lock = acquire_repository_operation_lock(&common_dir)?;
    let candidate_path = candidate_parent.join(safe_run_id(run_directory)?);
    if candidate_path != Path::new(&planned.path) {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate path differs from persisted deterministic authority".to_string(),
        ));
    }
    if fs::symlink_metadata(&candidate_path).is_ok() {
        let adopted = adopt_existing_candidate(
            &candidate_path,
            &source,
            planned.run_directory_digest.clone(),
            &planned.repository_identity_digest,
            &common_dir,
            &planned.starting_head,
            &planned.starting_tree,
        );
        let unlock = repository_lock.unlock();
        return match (adopted, unlock) {
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(error)) => Err(CandidateWorkspaceError::Io(error)),
            (Err(error), _) => Err(error),
        };
    }

    git_success(
        &source,
        &[
            "worktree",
            "add",
            "--detach",
            "--no-checkout",
            path_text(&candidate_path, "candidate path")?,
            &planned.starting_head,
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
        if git_text(&source, &["rev-parse", "HEAD"])? != planned.starting_head
            || git_text(&source, &["rev-parse", "HEAD^{tree}"])? != planned.starting_tree
        {
            return Err(CandidateWorkspaceError::Mismatch(
                "source HEAD or tree changed while creating the candidate".to_string(),
            ));
        }
        require_detached_head(&candidate)?;

        let mut state = planned.clone();
        state.path = path_text(&candidate, "candidate path")?.to_string();
        state.lifecycle = CandidateWorkspaceLifecycle::Active;
        refresh_candidate_workspace(&mut state)?;
        Ok(state)
    })();
    let result = match result {
        Ok(state) => Ok(state),
        Err(error) => {
            let rollback = if exact_owned_candidate_remnant(
                &source,
                &candidate_path,
                &common_dir,
                &planned.starting_head,
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
    };
    let unlock = repository_lock.unlock();
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(CandidateWorkspaceError::Io(error)),
        (Err(error), _) => Err(error),
    }
}

fn validate_provisioning_authority(
    run_directory: &Path,
    planned: &CandidateWorkspaceState,
) -> Result<(), CandidateWorkspaceError> {
    validate_run_directory_authority(run_directory, planned)?;
    if planned.lifecycle != CandidateWorkspaceLifecycle::Provisioning
        || planned.patch_transaction.is_some()
        || planned.cleanup_started_at.is_some()
        || planned.cleaned_at.is_some()
        || planned.candidate_head != planned.starting_head
        || planned.candidate_tree != planned.starting_tree
        || planned.candidate_diff_digest != sha256_bytes(&[])
    {
        return Err(CandidateWorkspaceError::Mismatch(
            "persisted candidate provisioning authority is not pristine".to_string(),
        ));
    }
    let source =
        canonical_real_directory(Path::new(&planned.source_worktree_root), "source worktree")?;
    let expected_path = env::temp_dir()
        .canonicalize()?
        .join(CANDIDATE_ROOT_DIR)
        .join(&planned.repository_identity_digest)
        .join(safe_run_id(run_directory)?);
    if Path::new(&planned.path) != expected_path
        || git_common_dir(&source)? != Path::new(&planned.git_common_dir)
        || git_text(&source, &["rev-parse", "HEAD"])? != planned.starting_head
        || git_text(&source, &["rev-parse", "HEAD^{tree}"])? != planned.starting_tree
    {
        return Err(CandidateWorkspaceError::Mismatch(
            "source identity, starting HEAD/tree, or candidate path differs from persisted provisioning authority"
                .to_string(),
        ));
    }
    Ok(())
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
    cleanup_candidate_workspace_outcome(workspace, source_worktree_root)
        .map(|outcome| outcome.candidate)
}

pub fn cleanup_candidate_workspace_outcome(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
) -> Result<CandidateCleanupOutcome, CandidateWorkspaceError> {
    cleanup_candidate_workspace_with_hook(workspace, source_worktree_root, |_| Ok(()))
}

fn cleanup_candidate_workspace_with_hook<F>(
    workspace: &LoopWorkspace,
    source_worktree_root: &Path,
    mut hook: F,
) -> Result<CandidateCleanupOutcome, CandidateWorkspaceError>
where
    F: FnMut(CandidateCleanupPhase) -> Result<(), CandidateWorkspaceError>,
{
    preflight_workspace_run_directory_authority(workspace)?;
    let lock = acquire_candidate_lock(workspace)?;
    let result = (|| {
        hook(CandidateCleanupPhase::CandidateLockAcquired)?;
        let mut run = crate::state::load_run(workspace)
            .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
        validate_workspace_run_id(workspace, &run)?;
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
        validate_run_directory_authority(workspace.run_directory(), &candidate)?;
        if candidate.lifecycle == CandidateWorkspaceLifecycle::Provisioning {
            return Err(CandidateWorkspaceError::Unsafe(
                "refusing to clean a candidate before provisioning completes".to_string(),
            ));
        }
        validate_static_authority(
            workspace.run_directory(),
            source_worktree_root,
            &candidate,
            false,
        )?;
        let repository_lock =
            acquire_repository_operation_lock(Path::new(&candidate.git_common_dir))?;

        let mut cleaning = match candidate.lifecycle {
            CandidateWorkspaceLifecycle::Provisioning => {
                unreachable!("Provisioning is rejected before repository lock selection")
            }
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
                repository_lock.unlock()?;
                return CandidateCleanupOutcome::from_locked_run(&run, candidate);
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
        hook(CandidateCleanupPhase::CleanedPersisted)?;
        repository_lock.unlock()?;
        CandidateCleanupOutcome::from_locked_run(&run, cleaning)
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
    CandidateLockAcquired,
    BeforeIntentPersisted,
    IntentPersisted,
    WorktreeRemoved,
    CleanedPersisted,
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

fn preflight_workspace_run_directory_authority(
    workspace: &LoopWorkspace,
) -> Result<(), CandidateWorkspaceError> {
    let run = crate::state::load_run(workspace)
        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
    validate_workspace_run_id(workspace, &run)?;
    let candidate = run.candidate_workspace.as_ref().ok_or_else(|| {
        CandidateWorkspaceError::Mismatch(
            "authoritative LoopRun has no candidate workspace".to_string(),
        )
    })?;
    validate_run_directory_authority(workspace.run_directory(), candidate)
}

fn validate_workspace_run_id(
    workspace: &LoopWorkspace,
    run: &seaf_core::LoopRun,
) -> Result<(), CandidateWorkspaceError> {
    if run.run_id != safe_run_id(workspace.run_directory())? {
        return Err(CandidateWorkspaceError::Mismatch(
            "persisted run ID does not match the authoritative run directory".to_string(),
        ));
    }
    Ok(())
}

fn validate_run_directory_authority(
    run_directory: &Path,
    state: &CandidateWorkspaceState,
) -> Result<(), CandidateWorkspaceError> {
    if state.schema_version == 1 {
        return Err(CandidateWorkspaceError::Unsafe(
            "candidate schema version 1 is forensic-only; start a new run or perform manually verified worktree recovery"
                .to_string(),
        ));
    }
    if state.schema_version != CANDIDATE_WORKSPACE_SCHEMA_VERSION {
        return Err(CandidateWorkspaceError::Mismatch(
            "candidate schema version does not match a supported operational authority".to_string(),
        ));
    }
    let persisted = state.run_directory_digest.as_deref().ok_or_else(|| {
        CandidateWorkspaceError::Mismatch(
            "candidate run directory authority digest is missing".to_string(),
        )
    })?;
    validate_digest(persisted, "run directory authority")?;
    let observed = run_directory_digest(run_directory)?;
    if persisted != observed {
        return Err(CandidateWorkspaceError::Mismatch(
            "current run directory does not match the candidate's authoritative original run directory"
                .to_string(),
        ));
    }
    Ok(())
}

fn run_directory_digest(run_directory: &Path) -> Result<String, CandidateWorkspaceError> {
    let canonical = canonical_real_directory(run_directory, "candidate run directory")?;
    Ok(sha256_bytes(canonical.as_os_str().as_encoded_bytes()))
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
    validate_run_directory_authority(run_directory, state)?;
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
    run_directory_digest: Option<String>,
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
        run_directory_digest,
        path: path_text(&candidate, "candidate path")?.to_string(),
        source_worktree_root: path_text(source, "source worktree")?.to_string(),
        git_common_dir: path_text(common_dir, "Git common directory")?.to_string(),
        repository_identity_digest: repository_identity_digest.to_string(),
        starting_head: starting_head.to_string(),
        starting_tree: starting_tree.to_string(),
        candidate_head: starting_head.to_string(),
        candidate_tree: starting_tree.to_string(),
        candidate_diff_digest: sha256_bytes(&[]),
        patch_transaction: None,
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
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(CandidateWorkspaceError::Io(error)),
            }
            set_and_validate_private_directory(path)
        }
        Err(error) => Err(CandidateWorkspaceError::Io(error)),
    }
}

fn repository_operation_lock_path(
    git_common_dir: &Path,
) -> Result<PathBuf, CandidateWorkspaceError> {
    let canonical = canonical_real_directory(git_common_dir, "Git common directory")?;
    if canonical != git_common_dir {
        return Err(CandidateWorkspaceError::Mismatch(
            "persisted Git common directory is not canonical".to_string(),
        ));
    }
    let common_dir_digest = sha256_bytes(canonical.as_os_str().as_encoded_bytes());
    Ok(std::env::temp_dir()
        .join(CANDIDATE_ROOT_DIR)
        .join(REPOSITORY_OPERATION_LOCKS_DIR)
        .join(common_dir_digest)
        .join(REPOSITORY_OPERATION_LOCK_FILE))
}

fn acquire_repository_operation_lock(
    git_common_dir: &Path,
) -> Result<fs::File, CandidateWorkspaceError> {
    let path = repository_operation_lock_path(git_common_dir)?;
    let candidate_root = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| {
            CandidateWorkspaceError::Unsafe(
                "repository operation lock has no candidate authority root".to_string(),
            )
        })?;
    let lock_namespace = path.parent().and_then(Path::parent).ok_or_else(|| {
        CandidateWorkspaceError::Unsafe(
            "repository operation lock has no private namespace".to_string(),
        )
    })?;
    let lock_parent = path.parent().ok_or_else(|| {
        CandidateWorkspaceError::Unsafe(
            "repository operation lock has no private parent".to_string(),
        )
    })?;
    ensure_private_authority_directory(candidate_root)?;
    ensure_private_authority_directory(lock_namespace)?;
    ensure_private_authority_directory(lock_parent)?;
    let file = match open_candidate_lock(&path, true) {
        Ok(file) => {
            file.sync_all()?;
            fs::File::open(lock_parent)?.sync_all()?;
            file
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            inspect_candidate_lock_path(&path)?;
            open_candidate_lock(&path, false)?
        }
        Err(error) => return Err(CandidateWorkspaceError::Io(error)),
    };
    validate_candidate_lock_file(&file, &path)?;
    file.lock().map_err(CandidateWorkspaceError::Io)?;
    if let Err(error) = validate_candidate_lock_file(&file, &path) {
        let _ = file.unlock();
        return Err(CandidateWorkspaceError::Io(error));
    }
    Ok(file)
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

fn git_success_with_index(
    worktree: &Path,
    args: &[&str],
    index_path: &Path,
) -> Result<(), CandidateWorkspaceError> {
    let output = sanitized_git_command()
        .args(args)
        .env("GIT_INDEX_FILE", index_path)
        .current_dir(worktree)
        .output()
        .map_err(CandidateWorkspaceError::Io)?;
    if !output.status.success() {
        return Err(CandidateWorkspaceError::Git(format!(
            "git {} with private index failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn git_text_with_index(
    worktree: &Path,
    args: &[&str],
    index_path: &Path,
) -> Result<String, CandidateWorkspaceError> {
    let bytes = git_bytes_with_index(worktree, args, index_path)?;
    String::from_utf8(bytes)
        .map(|value| value.trim().to_string())
        .map_err(|error| CandidateWorkspaceError::Git(format!("Git output was not UTF-8: {error}")))
}

fn git_bytes_with_index(
    worktree: &Path,
    args: &[&str],
    index_path: &Path,
) -> Result<Vec<u8>, CandidateWorkspaceError> {
    let output = sanitized_git_command()
        .args(args)
        .env("GIT_INDEX_FILE", index_path)
        .current_dir(worktree)
        .output()
        .map_err(CandidateWorkspaceError::Io)?;
    if !output.status.success() {
        return Err(CandidateWorkspaceError::Git(format!(
            "git {} with private index failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

fn git_apply_cached(
    worktree: &Path,
    patch: &str,
    index_path: Option<&Path>,
    changed_paths: &[String],
) -> Result<(), CandidateWorkspaceError> {
    let overrides = filter_driver_overrides(worktree, changed_paths)?;
    let mut command = sanitized_git_command();
    for (key, value) in &overrides {
        command.arg("-c").arg(format!("{key}={value}"));
    }
    command
        .args(["apply", "--cached", "--whitespace=nowarn", "-"])
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(index_path) = index_path {
        command.env("GIT_INDEX_FILE", index_path);
    }
    let mut child = command.spawn()?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        CandidateWorkspaceError::Git("git apply stdin was unavailable".to_string())
    })?;
    stdin.write_all(patch.as_bytes())?;
    drop(stdin);
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(CandidateWorkspaceError::Git(format!(
            "git apply --cached failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn filter_driver_overrides(
    worktree: &Path,
    changed_paths: &[String],
) -> Result<Vec<(String, String)>, CandidateWorkspaceError> {
    if changed_paths.is_empty() {
        return Ok(Vec::new());
    }
    let output = sanitized_git_command()
        .args(["check-attr", "-z", "filter", "--"])
        .args(changed_paths)
        .current_dir(worktree)
        .output()?;
    if !output.status.success() {
        return Err(CandidateWorkspaceError::Git(format!(
            "git check-attr filter failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let fields = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    if fields.len() % 3 != 0 {
        return Err(CandidateWorkspaceError::Git(
            "git check-attr returned malformed filter metadata".to_string(),
        ));
    }
    let mut drivers = Vec::new();
    for triple in fields.chunks_exact(3) {
        if triple[1] != b"filter" {
            return Err(CandidateWorkspaceError::Git(
                "git check-attr returned unexpected attribute metadata".to_string(),
            ));
        }
        let value = std::str::from_utf8(triple[2]).map_err(|_| {
            CandidateWorkspaceError::Unsafe("filter driver name is not UTF-8".to_string())
        })?;
        if matches!(value, "unspecified" | "unset" | "set") {
            continue;
        }
        if value.is_empty()
            || !value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
            })
        {
            return Err(CandidateWorkspaceError::Unsafe(
                "filter driver name is unsafe for isolated configuration".to_string(),
            ));
        }
        if !drivers.iter().any(|driver| driver == value) {
            drivers.push(value.to_string());
        }
    }
    let mut overrides = Vec::new();
    for driver in drivers {
        overrides.push((format!("filter.{driver}.clean"), String::new()));
        overrides.push((format!("filter.{driver}.smudge"), String::new()));
        overrides.push((format!("filter.{driver}.process"), String::new()));
        overrides.push((format!("filter.{driver}.required"), "false".to_string()));
    }
    Ok(overrides)
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
    match state
        .patch_transaction
        .as_ref()
        .map(|transaction| transaction.phase)
    {
        None | Some(CandidatePatchPhase::Applying)
            if state.candidate_tree == state.starting_tree
                && state.candidate_diff_digest == empty =>
        {
            Ok(())
        }
        Some(CandidatePatchPhase::Applied)
            if state.candidate_tree != state.starting_tree
                && state.candidate_diff_digest != empty =>
        {
            Ok(())
        }
        _ => Err(CandidateWorkspaceError::Mismatch(
            "candidate parent tree/diff evidence does not match its patch transaction phase"
                .to_string(),
        )),
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
    fn linked_worktree_authorities_share_the_git_common_directory_operation_lock() {
        let temp = tempfile::tempdir().expect("temp");
        let source = temp.path().join("source");
        let linked = temp.path().join("linked");
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
        test_git(
            &source,
            &["worktree", "add", "--detach", linked.to_str().unwrap()],
        );

        let source_workspace =
            LoopWorkspace::create(&temp.path().join("runs"), "source-authority").unwrap();
        let linked_workspace =
            LoopWorkspace::create(&temp.path().join("runs"), "linked-authority").unwrap();
        let source_identity = sha256_bytes(source.as_os_str().as_encoded_bytes());
        let linked_identity = sha256_bytes(linked.as_os_str().as_encoded_bytes());
        let source_plan =
            plan_candidate_workspace(source_workspace.run_directory(), &source, &source_identity)
                .expect("source plan");
        let linked_plan =
            plan_candidate_workspace(linked_workspace.run_directory(), &linked, &linked_identity)
                .expect("linked plan");

        assert_ne!(
            source_plan.repository_identity_digest,
            linked_plan.repository_identity_digest
        );
        assert_eq!(source_plan.git_common_dir, linked_plan.git_common_dir);
        assert_eq!(
            repository_operation_lock_path(Path::new(&source_plan.git_common_dir))
                .expect("source operation lock"),
            repository_operation_lock_path(Path::new(&linked_plan.git_common_dir))
                .expect("linked operation lock")
        );

        for (workspace, plan, identity) in [
            (&source_workspace, &source_plan, &source_identity),
            (&linked_workspace, &linked_plan, &linked_identity),
        ] {
            let mut run = crate::state::create_run(crate::state::NewLoopRun {
                run_id: workspace
                    .run_directory()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string(),
                ticket_id: "T-LINKED-LOCK".to_string(),
                goal_id: "production-use".to_string(),
                provider: "fake".to_string(),
                model: "fake-model".to_string(),
                input_digests: LoopInputDigests {
                    ticket: "1".repeat(64),
                    policy: "2".repeat(64),
                    config: "3".repeat(64),
                    repository: identity.clone(),
                },
            });
            run.execution_mode = LoopExecutionMode::IsolatedCandidate;
            run.candidate_workspace = Some(plan.clone());
            crate::state::save_run(workspace, &run).expect("planned run");
        }

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let source_thread = {
            let workspace = source_workspace.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                provision_candidate_workspace(&workspace)
            })
        };
        let linked_thread = {
            let workspace = linked_workspace.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                provision_candidate_workspace(&workspace)
            })
        };
        barrier.wait();
        let source_candidate = source_thread.join().unwrap().expect("source candidate");
        let linked_candidate = linked_thread.join().unwrap().expect("linked candidate");

        assert_ne!(source_candidate.path, linked_candidate.path);
        assert_eq!(
            source_candidate.lifecycle,
            CandidateWorkspaceLifecycle::Active
        );
        assert_eq!(
            linked_candidate.lifecycle,
            CandidateWorkspaceLifecycle::Active
        );
        validate_candidate_workspace(source_workspace.run_directory(), &source, &source_candidate)
            .expect("source candidate authority");
        validate_candidate_workspace(linked_workspace.run_directory(), &linked, &linked_candidate)
            .expect("linked candidate authority");
        assert!(worktree_registered(&source, Path::new(&source_candidate.path)).unwrap());
        assert!(worktree_registered(&linked, Path::new(&linked_candidate.path)).unwrap());

        for workspace in [&source_workspace, &linked_workspace] {
            let mut run = crate::state::load_run(workspace).expect("active run");
            run.status = LoopStatus::Completed;
            crate::state::save_run(workspace, &run).expect("terminal run");
        }
        assert_eq!(
            cleanup_candidate_workspace(&source_workspace, &source)
                .expect("source cleanup")
                .lifecycle,
            CandidateWorkspaceLifecycle::Cleaned
        );
        assert_eq!(
            cleanup_candidate_workspace(&linked_workspace, &linked)
                .expect("linked cleanup")
                .lifecycle,
            CandidateWorkspaceLifecycle::Cleaned
        );
        assert!(!worktree_registered(&source, Path::new(&source_candidate.path)).unwrap());
        assert!(!worktree_registered(&linked, Path::new(&linked_candidate.path)).unwrap());

        test_git(
            &source,
            &["worktree", "remove", "--force", linked.to_str().unwrap()],
        );
    }

    #[test]
    fn candidate_provisioning_recovers_real_create_and_publication_cuts() {
        for (run_id, cut) in [
            (
                "provision-before-create",
                CandidateProvisionPhase::BeforeWorktreeCreate,
            ),
            (
                "provision-after-create",
                CandidateProvisionPhase::WorktreeCreated,
            ),
            (
                "provision-after-active",
                CandidateProvisionPhase::ActivePersisted,
            ),
        ] {
            let (temp, source, workspace, planned) = provisioning_fixture(run_id);
            let error = provision_candidate_workspace_with_hook(&workspace, |phase| {
                if phase == cut {
                    Err(CandidateWorkspaceError::State(format!(
                        "injected {phase:?}"
                    )))
                } else {
                    Ok(())
                }
            })
            .expect_err("inject provisioning cut");
            assert!(error.to_string().contains("injected"), "{error}");
            let persisted = crate::state::load_run(&workspace).unwrap();
            match cut {
                CandidateProvisionPhase::BeforeWorktreeCreate => {
                    assert!(!Path::new(&planned.path).exists());
                    assert_eq!(
                        persisted.candidate_workspace.unwrap().lifecycle,
                        CandidateWorkspaceLifecycle::Provisioning
                    );
                }
                CandidateProvisionPhase::WorktreeCreated => {
                    assert!(Path::new(&planned.path).is_dir());
                    assert_eq!(
                        persisted.candidate_workspace.unwrap().lifecycle,
                        CandidateWorkspaceLifecycle::Provisioning
                    );
                }
                CandidateProvisionPhase::ActivePersisted => {
                    assert_eq!(
                        persisted.candidate_workspace.unwrap().lifecycle,
                        CandidateWorkspaceLifecycle::Active
                    );
                }
            }
            let active = provision_candidate_workspace(&workspace).expect("retry exact cut");
            assert_eq!(active.lifecycle, CandidateWorkspaceLifecycle::Active);
            test_git(
                &source,
                &["worktree", "remove", "--force", active.path.as_str()],
            );
            drop(temp);
        }
    }

    #[test]
    fn candidate_provisioning_stale_cas_leaves_an_exact_retryable_remnant() {
        let (_temp, source, workspace, planned) = provisioning_fixture("provision-stale-cas");
        let error = provision_candidate_workspace_with_hook(&workspace, |phase| {
            if phase == CandidateProvisionPhase::WorktreeCreated {
                let mut concurrent = crate::state::load_run(&workspace).unwrap();
                concurrent.updated_at = "concurrent-change".to_string();
                crate::state::save_run(&workspace, &concurrent).unwrap();
            }
            Ok(())
        })
        .expect_err("stale full-run CAS must fail");
        assert!(error.to_string().contains("changed"), "{error}");
        assert!(Path::new(&planned.path).is_dir());
        let active = provision_candidate_workspace(&workspace).expect("adopt exact remnant");
        assert_eq!(active.lifecycle, CandidateWorkspaceLifecycle::Active);
        test_git(
            &source,
            &["worktree", "remove", "--force", active.path.as_str()],
        );
    }

    fn provisioning_fixture(
        run_id: &str,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        LoopWorkspace,
        CandidateWorkspaceState,
    ) {
        let temp = tempfile::tempdir().expect("temp");
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "seaf@example.invalid"],
            vec!["config", "user.name", "SEAF Test"],
        ] {
            test_git(&source, &args);
        }
        fs::write(source.join("tracked.txt"), "source\n").unwrap();
        test_git(&source, &["add", "tracked.txt"]);
        test_git(&source, &["commit", "-qm", "initial"]);
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), run_id).unwrap();
        let repository = sha256_bytes(source.as_os_str().as_encoded_bytes());
        let planned =
            plan_candidate_workspace(workspace.run_directory(), &source, &repository).unwrap();
        let mut run = crate::state::create_run(crate::state::NewLoopRun {
            run_id: run_id.to_string(),
            ticket_id: "T-1".to_string(),
            goal_id: "goal".to_string(),
            provider: "fake".to_string(),
            model: "model".to_string(),
            input_digests: LoopInputDigests {
                ticket: "1".repeat(64),
                policy: "2".repeat(64),
                config: "3".repeat(64),
                repository,
            },
        });
        run.execution_mode = LoopExecutionMode::IsolatedCandidate;
        run.candidate_workspace = Some(planned.clone());
        crate::state::save_run(&workspace, &run).unwrap();
        (temp, source, workspace, planned)
    }

    #[test]
    fn cleanup_rejects_valid_provisioning_before_selecting_a_repository_lock() {
        let (_temp, source, workspace, planned) = provisioning_fixture("cleanup-provisioning");
        let repository_lock =
            repository_operation_lock_path(Path::new(&planned.git_common_dir)).unwrap();
        if repository_lock.exists() {
            fs::remove_file(&repository_lock).expect("remove prior repository lock");
        }
        let run_before = fs::read(workspace.run_file()).expect("run bytes");
        let source_head = git_text(&source, &["rev-parse", "HEAD"]).unwrap();
        let source_status = git_text(&source, &["status", "--porcelain=v1"]).unwrap();

        let error = cleanup_candidate_workspace(&workspace, &source)
            .expect_err("valid Provisioning authority cannot be cleaned");

        assert!(error.to_string().contains("active run"), "{error}");
        assert!(!repository_lock.exists());
        assert_eq!(fs::read(workspace.run_file()).unwrap(), run_before);
        assert_eq!(
            git_text(&source, &["rev-parse", "HEAD"]).unwrap(),
            source_head
        );
        assert_eq!(
            git_text(&source, &["status", "--porcelain=v1"]).unwrap(),
            source_status
        );
        assert!(!Path::new(&planned.path).exists());
    }

    #[test]
    fn cleanup_rejects_wrong_source_before_recreating_the_repository_lock() {
        let (temp, source, workspace, planned) = provisioning_fixture("cleanup-wrong-source");
        let candidate = provision_candidate_workspace(&workspace).expect("active candidate");
        let mut run = crate::state::load_run(&workspace).expect("active run");
        run.status = LoopStatus::Completed;
        crate::state::save_run(&workspace, &run).expect("terminal run");
        let repository_lock =
            repository_operation_lock_path(Path::new(&planned.git_common_dir)).unwrap();
        fs::remove_file(&repository_lock).expect("remove provision repository lock");

        let other = temp.path().join("other");
        fs::create_dir(&other).expect("other source");
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "seaf@example.invalid"],
            vec!["config", "user.name", "SEAF Test"],
        ] {
            test_git(&other, &args);
        }
        fs::write(other.join("tracked.txt"), "other\n").unwrap();
        test_git(&other, &["add", "tracked.txt"]);
        test_git(&other, &["commit", "-qm", "initial"]);
        let run_before = fs::read(workspace.run_file()).expect("run bytes");
        let source_head = git_text(&source, &["rev-parse", "HEAD"]).unwrap();
        let source_status = git_text(&source, &["status", "--porcelain=v1"]).unwrap();
        let candidate_tree = git_text(Path::new(&candidate.path), &["write-tree"]).unwrap();
        let candidate_status =
            git_text(Path::new(&candidate.path), &["status", "--porcelain=v1"]).unwrap();

        let error = cleanup_candidate_workspace(&workspace, &other)
            .expect_err("wrong caller source must fail before repository locking");

        let repository_lock_created = repository_lock.exists();
        if repository_lock_created {
            fs::remove_file(&repository_lock).expect("remove recreated test lock");
        }
        assert!(error.to_string().contains("source worktree"), "{error}");
        assert!(!repository_lock_created);
        assert_eq!(fs::read(workspace.run_file()).unwrap(), run_before);
        assert_eq!(
            git_text(&source, &["rev-parse", "HEAD"]).unwrap(),
            source_head
        );
        assert_eq!(
            git_text(&source, &["status", "--porcelain=v1"]).unwrap(),
            source_status
        );
        assert_eq!(
            git_text(Path::new(&candidate.path), &["write-tree"]).unwrap(),
            candidate_tree
        );
        assert_eq!(
            git_text(Path::new(&candidate.path), &["status", "--porcelain=v1"]).unwrap(),
            candidate_status
        );
        test_git(
            &source,
            &["worktree", "remove", "--force", candidate.path.as_str()],
        );
    }

    #[test]
    fn cleanup_wrong_source_never_selects_a_lock_for_cleaning_or_cleaned_authority() {
        for lifecycle in [
            CandidateWorkspaceLifecycle::Cleaning,
            CandidateWorkspaceLifecycle::Cleaned,
        ] {
            let run_id = match lifecycle {
                CandidateWorkspaceLifecycle::Cleaning => "cleanup-wrong-source-cleaning",
                CandidateWorkspaceLifecycle::Cleaned => "cleanup-wrong-source-cleaned",
                _ => unreachable!(),
            };
            let (temp, source, workspace, planned) = provisioning_fixture(run_id);
            let candidate = provision_candidate_workspace(&workspace).expect("active candidate");
            if lifecycle == CandidateWorkspaceLifecycle::Cleaned {
                test_git(
                    &source,
                    &["worktree", "remove", "--force", candidate.path.as_str()],
                );
            }
            let mut run = crate::state::load_run(&workspace).expect("active run");
            run.status = LoopStatus::Completed;
            let authority = run.candidate_workspace.as_mut().unwrap();
            authority.lifecycle = lifecycle;
            authority.cleanup_started_at = Some("cleanup-started".to_string());
            authority.cleaned_at = (lifecycle == CandidateWorkspaceLifecycle::Cleaned)
                .then(|| "cleanup-finished".to_string());
            crate::state::save_run(&workspace, &run).expect("cleanup lifecycle run");
            let repository_lock =
                repository_operation_lock_path(Path::new(&planned.git_common_dir)).unwrap();
            fs::remove_file(&repository_lock).expect("remove provision repository lock");

            let other = temp.path().join("other");
            fs::create_dir(&other).expect("other source");
            test_git(&other, &["init", "-q"]);
            let run_before = fs::read(workspace.run_file()).expect("run bytes");
            let source_head = git_text(&source, &["rev-parse", "HEAD"]).unwrap();
            let source_status = git_text(&source, &["status", "--porcelain=v1"]).unwrap();
            let candidate_before = Path::new(&candidate.path).exists().then(|| {
                (
                    git_text(Path::new(&candidate.path), &["write-tree"]).unwrap(),
                    git_text(Path::new(&candidate.path), &["status", "--porcelain=v1"]).unwrap(),
                )
            });

            let error = cleanup_candidate_workspace(&workspace, &other)
                .expect_err("wrong source must fail before repository locking");

            let repository_lock_created = repository_lock.exists();
            if repository_lock_created {
                fs::remove_file(&repository_lock).expect("remove recreated test lock");
            }
            assert!(error.to_string().contains("source worktree"), "{error}");
            assert!(!repository_lock_created, "{lifecycle:?}");
            assert_eq!(fs::read(workspace.run_file()).unwrap(), run_before);
            assert_eq!(
                git_text(&source, &["rev-parse", "HEAD"]).unwrap(),
                source_head
            );
            assert_eq!(
                git_text(&source, &["status", "--porcelain=v1"]).unwrap(),
                source_status
            );
            if let Some((tree, status)) = candidate_before {
                assert_eq!(
                    git_text(Path::new(&candidate.path), &["write-tree"]).unwrap(),
                    tree
                );
                assert_eq!(
                    git_text(Path::new(&candidate.path), &["status", "--porcelain=v1"]).unwrap(),
                    status
                );
                test_git(
                    &source,
                    &["worktree", "remove", "--force", candidate.path.as_str()],
                );
            }
        }
    }

    #[test]
    fn cleanup_rejects_tampered_common_dir_before_selecting_its_lock_namespace() {
        let (temp, source, workspace, _) = provisioning_fixture("cleanup-common-dir");
        let candidate = provision_candidate_workspace(&workspace).expect("active candidate");
        let malicious_common = temp.path().join("malicious-common");
        fs::create_dir(&malicious_common).expect("malicious common dir");
        let malicious_common = malicious_common.canonicalize().unwrap();
        let malicious_lock = repository_operation_lock_path(&malicious_common).unwrap();
        assert!(!malicious_lock.exists());
        let mut run = crate::state::load_run(&workspace).expect("active run");
        run.status = LoopStatus::Completed;
        run.candidate_workspace.as_mut().unwrap().git_common_dir =
            malicious_common.display().to_string();
        crate::state::save_run(&workspace, &run).expect("tampered terminal run");
        let run_before = fs::read(workspace.run_file()).expect("run bytes");
        let source_head = git_text(&source, &["rev-parse", "HEAD"]).unwrap();
        let source_status = git_text(&source, &["status", "--porcelain=v1"]).unwrap();
        let candidate_tree = git_text(Path::new(&candidate.path), &["write-tree"]).unwrap();
        let candidate_status =
            git_text(Path::new(&candidate.path), &["status", "--porcelain=v1"]).unwrap();

        let error = cleanup_candidate_workspace(&workspace, &source)
            .expect_err("tampered common dir must fail before repository locking");

        let malicious_lock_created = malicious_lock.exists();
        if malicious_lock_created {
            fs::remove_file(&malicious_lock).expect("remove malicious test lock");
        }
        assert!(
            error.to_string().contains("Git common directory"),
            "{error}"
        );
        assert!(!malicious_lock_created);
        assert_eq!(fs::read(workspace.run_file()).unwrap(), run_before);
        assert_eq!(
            git_text(&source, &["rev-parse", "HEAD"]).unwrap(),
            source_head
        );
        assert_eq!(
            git_text(&source, &["status", "--porcelain=v1"]).unwrap(),
            source_status
        );
        assert_eq!(
            git_text(Path::new(&candidate.path), &["write-tree"]).unwrap(),
            candidate_tree
        );
        assert_eq!(
            git_text(Path::new(&candidate.path), &["status", "--porcelain=v1"]).unwrap(),
            candidate_status
        );
        test_git(
            &source,
            &["worktree", "remove", "--force", candidate.path.as_str()],
        );
    }

    #[test]
    fn cleanup_revalidates_reloaded_run_authority_before_selecting_repository_lock() {
        let (temp, source, workspace, _) = provisioning_fixture("cleanup-authority-race");
        let candidate = provision_candidate_workspace(&workspace).expect("active candidate");
        let mut run = crate::state::load_run(&workspace).expect("active run");
        run.status = LoopStatus::Completed;
        crate::state::save_run(&workspace, &run).expect("terminal run");

        let malicious_common = temp.path().join("malicious-common");
        fs::create_dir(&malicious_common).expect("malicious common dir");
        let malicious_common = malicious_common.canonicalize().unwrap();
        let malicious_lock = repository_operation_lock_path(&malicious_common).unwrap();
        assert!(!malicious_lock.exists());
        let source_head = git_text(&source, &["rev-parse", "HEAD"]).unwrap();
        let source_status = git_text(&source, &["status", "--porcelain=v1"]).unwrap();
        let candidate_tree = git_text(Path::new(&candidate.path), &["write-tree"]).unwrap();
        let candidate_status =
            git_text(Path::new(&candidate.path), &["status", "--porcelain=v1"]).unwrap();
        let swapped_bytes = std::cell::RefCell::new(None);

        let error = cleanup_candidate_workspace_with_hook(&workspace, &source, |phase| {
            if phase == CandidateCleanupPhase::CandidateLockAcquired {
                let mut swapped = crate::state::load_run(&workspace)
                    .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
                let authority = swapped.candidate_workspace.as_mut().unwrap();
                authority.run_directory_digest = Some("f".repeat(64));
                authority.git_common_dir =
                    path_text(&malicious_common, "malicious common")?.to_string();
                crate::state::write_run_file(&workspace.run_file(), &swapped)
                    .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
                *swapped_bytes.borrow_mut() = Some(fs::read(workspace.run_file())?);
            }
            Ok(())
        })
        .expect_err("swapped run-directory authority must be rejected");

        let malicious_lock_created = malicious_lock.exists();
        if malicious_lock_created {
            fs::remove_file(&malicious_lock).expect("remove test lock");
        }
        let run_unchanged_after_swap = fs::read(workspace.run_file()).unwrap()
            == swapped_bytes.into_inner().expect("swapped bytes");
        let source_unchanged = git_text(&source, &["rev-parse", "HEAD"]).unwrap() == source_head
            && git_text(&source, &["status", "--porcelain=v1"]).unwrap() == source_status;
        let candidate_unchanged = git_text(Path::new(&candidate.path), &["write-tree"]).unwrap()
            == candidate_tree
            && git_text(Path::new(&candidate.path), &["status", "--porcelain=v1"]).unwrap()
                == candidate_status;
        test_git(
            &source,
            &["worktree", "remove", "--force", candidate.path.as_str()],
        );

        assert!(error.to_string().contains("run directory"), "{error}");
        assert!(
            !malicious_lock_created,
            "unauthoritative Git common directory selected a repository lock namespace"
        );
        assert!(run_unchanged_after_swap);
        assert!(source_unchanged);
        assert!(candidate_unchanged);
    }

    #[test]
    fn cleanup_revalidates_run_id_after_the_candidate_lock() {
        let (_temp, source, workspace, planned) = provisioning_fixture("cleanup-run-id-race");
        let candidate = provision_candidate_workspace(&workspace).expect("active candidate");
        let mut terminal = crate::state::load_run(&workspace).expect("active run");
        terminal.status = LoopStatus::Completed;
        crate::state::save_run(&workspace, &terminal).expect("terminal run");
        let repository_lock =
            repository_operation_lock_path(Path::new(&planned.git_common_dir)).unwrap();
        fs::remove_file(&repository_lock).expect("remove provision repository lock");
        let source_head = git_text(&source, &["rev-parse", "HEAD"]).unwrap();
        let source_status = git_text(&source, &["status", "--porcelain=v1"]).unwrap();
        let candidate_tree = git_text(Path::new(&candidate.path), &["write-tree"]).unwrap();
        let candidate_status =
            git_text(Path::new(&candidate.path), &["status", "--porcelain=v1"]).unwrap();
        let swapped_bytes = std::cell::RefCell::new(None);

        let error = cleanup_candidate_workspace_with_hook(&workspace, &source, |phase| {
            if phase == CandidateCleanupPhase::CandidateLockAcquired {
                let mut swapped = crate::state::load_run(&workspace)
                    .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
                swapped.run_id = "other-safe-run".to_string();
                crate::state::write_run_file(&workspace.run_file(), &swapped)
                    .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
                *swapped_bytes.borrow_mut() = Some(fs::read(workspace.run_file())?);
            }
            Ok(())
        })
        .expect_err("locked reload must bind persisted run ID to the directory");

        assert!(error.to_string().contains("run ID"), "{error}");
        assert!(!repository_lock.exists());
        assert_eq!(
            fs::read(workspace.run_file()).unwrap(),
            swapped_bytes.into_inner().expect("swapped run bytes")
        );
        assert_eq!(
            git_text(&source, &["rev-parse", "HEAD"]).unwrap(),
            source_head
        );
        assert_eq!(
            git_text(&source, &["status", "--porcelain=v1"]).unwrap(),
            source_status
        );
        assert_eq!(
            git_text(Path::new(&candidate.path), &["write-tree"]).unwrap(),
            candidate_tree
        );
        assert_eq!(
            git_text(Path::new(&candidate.path), &["status", "--porcelain=v1"]).unwrap(),
            candidate_status
        );
        crate::state::write_run_file(&workspace.run_file(), &terminal).expect("restore run");
        test_git(
            &source,
            &["worktree", "remove", "--force", candidate.path.as_str()],
        );
    }

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
        let planned = plan_candidate_workspace(
            workspace.run_directory(),
            &source,
            &repository_identity_digest,
        )
        .expect("candidate plan");
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
                repository: repository_identity_digest.clone(),
            },
        });
        run.candidate_workspace = Some(planned);
        run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
        crate::state::save_run(&workspace, &run).expect("candidate plan");
        let candidate = create_candidate_workspace(
            workspace.run_directory(),
            &source,
            &repository_identity_digest,
        )
        .expect("candidate");
        let mut run = crate::state::load_run(&workspace).expect("active run");
        run.status = LoopStatus::Completed;
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
    fn cleanup_outcome_uses_the_locked_snapshot_without_a_post_success_reread() {
        let (_temp, source, workspace, _) = provisioning_fixture("cleanup-locked-outcome");
        let candidate = provision_candidate_workspace(&workspace).expect("active candidate");
        let mut run = crate::state::load_run(&workspace).expect("active run");
        run.status = LoopStatus::Completed;
        crate::state::save_run(&workspace, &run).expect("terminal run");

        let outcome = cleanup_candidate_workspace_with_hook(&workspace, &source, |phase| {
            if phase == CandidateCleanupPhase::CleanedPersisted {
                fs::remove_file(workspace.run_file())?;
            }
            Ok(())
        })
        .expect("completed cleanup must not depend on an unlocked reread");

        assert_eq!(outcome.run_id, "cleanup-locked-outcome");
        assert_eq!(outcome.status, LoopStatus::Completed);
        assert_eq!(
            outcome.candidate.lifecycle,
            CandidateWorkspaceLifecycle::Cleaned
        );
        assert!(!Path::new(&candidate.path).exists());
        assert!(!workspace.run_file().exists());
        run.candidate_workspace = Some(outcome.candidate);
        crate::state::write_run_file(&workspace.run_file(), &run).expect("restore cleaned run");
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
        let planned = plan_candidate_workspace(
            workspace.run_directory(),
            &source,
            &repository_identity_digest,
        )
        .expect("candidate plan");
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
                repository: repository_identity_digest.clone(),
            },
        });
        run.candidate_workspace = Some(planned);
        run.execution_mode = seaf_core::LoopExecutionMode::IsolatedCandidate;
        crate::state::save_run(&workspace, &run).expect("candidate plan");
        let candidate = create_candidate_workspace(
            workspace.run_directory(),
            &source,
            &repository_identity_digest,
        )
        .expect("candidate");
        let mut run = crate::state::load_run(&workspace).expect("active run");
        run.status = LoopStatus::Completed;
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

    #[test]
    fn candidate_application_stale_pre_intent_cas_keeps_candidate_pristine() {
        let fixture = application_fixture("application-stale-cas");
        let error = apply_candidate_development_evidence_with_hook(
            &fixture.workspace,
            &fixture.source,
            |phase| {
                if phase == CandidatePatchApplicationPhase::BeforeApplyingPersisted {
                    let mut concurrent = crate::state::load_run(&fixture.workspace)
                        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
                    concurrent.updated_at = "concurrent-application-change".to_string();
                    crate::state::write_run_file(&fixture.workspace.run_file(), &concurrent)
                        .map_err(|error| CandidateWorkspaceError::State(error.to_string()))?;
                }
                Ok(())
            },
        )
        .expect_err("stale Applying CAS must fail");
        assert!(error.to_string().contains("compare-and-swap"), "{error}");
        let run = crate::state::load_run(&fixture.workspace).expect("concurrent run");
        assert!(run
            .candidate_workspace
            .as_ref()
            .unwrap()
            .patch_transaction
            .is_none());
        assert_eq!(
            git_text(Path::new(&fixture.candidate.path), &["write-tree"]).unwrap(),
            fixture.candidate.starting_tree
        );
        assert_eq!(
            fs::read(Path::new(&fixture.candidate.path).join("tracked.txt")).unwrap(),
            b"source\n"
        );
        assert!(fixture
            .workspace
            .run_directory()
            .join(PATCH_INTENT_PATH)
            .is_file());
        assert!(!fixture
            .workspace
            .run_directory()
            .join(PATCH_APPLIED_EVIDENCE_PATH)
            .exists());
        fixture.cleanup();
    }

    #[test]
    fn candidate_patch_planning_skips_an_orphaned_private_index_name() {
        let temp = tempfile::tempdir().expect("temp dir");
        let sequence = PATCH_PLAN_SEQUENCE.load(Ordering::Relaxed);
        let orphan = temp.path().join(format!(
            ".candidate-patch-plan.index-{}-{sequence}",
            std::process::id()
        ));
        fs::write(&orphan, b"orphan").expect("orphaned index");

        let reserved = unique_patch_plan_index(temp.path()).expect("unique planning index");

        assert_ne!(reserved, orphan);
        assert!(!reserved.exists());
        assert_eq!(fs::read(orphan).unwrap(), b"orphan");
    }

    #[test]
    fn raw_directory_transition_refuses_unrelated_contents() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::create_dir(temp.path().join("directory")).expect("directory");
        fs::write(temp.path().join("directory/changed.txt"), b"changed").expect("changed child");
        fs::write(temp.path().join("directory/unrelated.txt"), b"unrelated")
            .expect("unrelated child");

        let error = raw_rematerialize_changed_paths(
            temp.path(),
            &["directory/changed.txt".to_string(), "directory".to_string()],
        )
        .expect_err("unrelated directory contents must fail closed");

        assert!(error.to_string().contains("not empty"), "{error}");
        assert_eq!(
            fs::read(temp.path().join("directory/unrelated.txt")).unwrap(),
            b"unrelated"
        );
    }

    #[test]
    fn candidate_application_recovers_from_each_real_publication_cut() {
        let before_index = application_fixture("application-before-index");
        let error = apply_candidate_development_evidence_with_hook(
            &before_index.workspace,
            &before_index.source,
            |phase| {
                if phase == CandidatePatchApplicationPhase::ApplyingPersisted {
                    return Err(CandidateWorkspaceError::State(
                        "injected before index mutation".to_string(),
                    ));
                }
                Ok(())
            },
        )
        .expect_err("inject before index mutation");
        assert!(error.to_string().contains("injected"), "{error}");
        assert_applying_without_future_evidence(&before_index, false);
        let recovered =
            apply_candidate_development_evidence(&before_index.workspace, &before_index.source)
                .expect("recover pristine Applying");
        assert_eq!(
            recovered.patch_transaction.as_ref().unwrap().phase,
            CandidatePatchPhase::Applied
        );
        before_index.cleanup();

        let after_materialize = application_fixture("application-after-materialize");
        let error = apply_candidate_development_evidence_with_hook(
            &after_materialize.workspace,
            &after_materialize.source,
            |phase| {
                if phase == CandidatePatchApplicationPhase::Materialized {
                    return Err(CandidateWorkspaceError::State(
                        "injected after materialization".to_string(),
                    ));
                }
                Ok(())
            },
        )
        .expect_err("inject after materialization");
        assert!(error.to_string().contains("injected"), "{error}");
        assert_applying_without_future_evidence(&after_materialize, true);
        apply_candidate_development_evidence(
            &after_materialize.workspace,
            &after_materialize.source,
        )
        .expect("recover exact materialized Applying");
        after_materialize.cleanup();

        let after_applied = application_fixture("application-after-applied");
        let error = apply_candidate_development_evidence_with_hook(
            &after_applied.workspace,
            &after_applied.source,
            |phase| {
                if phase == CandidatePatchApplicationPhase::AppliedPersisted {
                    return Err(CandidateWorkspaceError::State(
                        "injected after Applied publication".to_string(),
                    ));
                }
                Ok(())
            },
        )
        .expect_err("inject after Applied publication");
        assert!(error.to_string().contains("injected"), "{error}");
        let applied_run = crate::state::load_run(&after_applied.workspace).expect("Applied run");
        assert_eq!(
            applied_run
                .candidate_workspace
                .as_ref()
                .unwrap()
                .patch_transaction
                .as_ref()
                .unwrap()
                .phase,
            CandidatePatchPhase::Applied
        );
        assert!(after_applied
            .workspace
            .run_directory()
            .join(PATCH_APPLIED_EVIDENCE_PATH)
            .is_file());
        apply_candidate_development_evidence(&after_applied.workspace, &after_applied.source)
            .expect("replay exact Applied publication");
        after_applied.cleanup();
    }

    #[test]
    fn isolated_resume_normalizes_completed_development_none_applying_and_applied_before_scaffold()
    {
        let none = application_fixture("resume-development-none");
        let none_run = crate::state::load_run(&none.workspace).expect("pre-B3 run");
        let none_initialized = crate::runner::InitializedLoopRun::resume_isolated(
            none.workspace.run_directory().parent().unwrap(),
            none_run,
        )
        .expect("pending no-history pre-B3 Development migrates");
        assert_eq!(
            none_initialized
                .run()
                .candidate_workspace
                .as_ref()
                .unwrap()
                .patch_transaction
                .as_ref()
                .unwrap()
                .phase,
            CandidatePatchPhase::Applied
        );
        none.cleanup();

        for (run_id, cut) in [
            (
                "resume-development-applying-pristine",
                CandidatePatchApplicationPhase::ApplyingPersisted,
            ),
            (
                "resume-development-applying-materialized",
                CandidatePatchApplicationPhase::Materialized,
            ),
        ] {
            let fixture = application_fixture(run_id);
            apply_candidate_development_evidence_with_hook(
                &fixture.workspace,
                &fixture.source,
                |phase| {
                    if phase == cut {
                        Err(CandidateWorkspaceError::State(
                            "injected resume cut".to_string(),
                        ))
                    } else {
                        Ok(())
                    }
                },
            )
            .expect_err("leave Applying authority");
            let applying = crate::state::load_run(&fixture.workspace).expect("Applying run");
            let initialized = crate::runner::InitializedLoopRun::resume_isolated(
                fixture.workspace.run_directory().parent().unwrap(),
                applying,
            )
            .expect("resume converges Applying transaction");
            assert_eq!(
                initialized
                    .run()
                    .candidate_workspace
                    .as_ref()
                    .unwrap()
                    .patch_transaction
                    .as_ref()
                    .unwrap()
                    .phase,
                CandidatePatchPhase::Applied
            );
            fixture.cleanup();
        }

        let applied = application_fixture("resume-development-applied");
        apply_candidate_development_evidence(&applied.workspace, &applied.source)
            .expect("Applied setup");
        let applied_run = crate::state::load_run(&applied.workspace).expect("Applied run");
        let initialized = crate::runner::InitializedLoopRun::resume_isolated(
            applied.workspace.run_directory().parent().unwrap(),
            applied_run.clone(),
        )
        .expect("Applied resume is read-only verified");
        assert_eq!(initialized.run(), &applied_run);
        applied.cleanup();
    }

    struct ApplicationFixture {
        _temp: tempfile::TempDir,
        source: PathBuf,
        workspace: LoopWorkspace,
        candidate: CandidateWorkspaceState,
    }

    impl ApplicationFixture {
        fn cleanup(self) {
            test_git(
                &self.source,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    self.candidate.path.as_str(),
                ],
            );
        }
    }

    fn application_fixture(run_id: &str) -> ApplicationFixture {
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
            LoopWorkspace::create(&temp.path().join("runs"), run_id).expect("workspace");
        let repository_identity_digest = sha256_bytes(source.as_os_str().as_encoded_bytes());
        let planned = plan_candidate_workspace(
            workspace.run_directory(),
            &source,
            &repository_identity_digest,
        )
        .expect("candidate plan");
        let mut run = crate::state::create_run(crate::state::NewLoopRun {
            run_id: run_id.to_string(),
            ticket_id: "T-1".to_string(),
            goal_id: "goal".to_string(),
            provider: "fake".to_string(),
            model: "model".to_string(),
            input_digests: LoopInputDigests {
                ticket: "1".repeat(64),
                policy: "2".repeat(64),
                config: "3".repeat(64),
                repository: repository_identity_digest.clone(),
            },
        });
        run.candidate_workspace = Some(planned);
        run.execution_mode = LoopExecutionMode::IsolatedCandidate;
        crate::state::save_run(&workspace, &run).expect("candidate plan");
        let candidate = create_candidate_workspace(
            workspace.run_directory(),
            &source,
            &repository_identity_digest,
        )
        .expect("candidate");
        let mut run = crate::state::load_run(&workspace).expect("active run");
        run.status = LoopStatus::Running;
        run.current_step = LoopStepName::Development;
        let patch = "diff --git a/tracked.txt b/tracked.txt\nindex 1f7391f..39c5733 100644\n--- a/tracked.txt\n+++ b/tracked.txt\n@@ -1 +1 @@\n-source\n+candidate\n";
        let decision = PolicyDecision {
            patch_id: run_id.to_string(),
            patch_sha256: crate::patch_digest(patch),
            changed_paths: vec!["tracked.txt".to_string()],
            decision: PatchDecisionKind::Allowed,
            reasons: Vec::new(),
            requires_human_review: false,
            apply_requested: false,
            applied: false,
        };
        let evidence = DevelopmentEvidence::new(
            run_id,
            crate::DeveloperResponse {
                role: crate::Role::Developer,
                status: crate::DeveloperStatus::PatchProposed,
                summary: "candidate patch".to_string(),
                changed_files: vec!["tracked.txt".to_string()],
                requires_human_review: false,
                patch: Some(patch.to_string()),
                context_request: None,
            },
            patch,
            decision.clone(),
        )
        .expect("evidence");
        fs::create_dir_all(workspace.run_directory().join(ARTIFACTS_DIR)).expect("artifacts");
        let evidence_path = "artifacts/05-development.json";
        fs::write(
            workspace.run_directory().join(evidence_path),
            evidence.canonical_bytes().expect("canonical evidence"),
        )
        .expect("evidence artifact");
        let development = run
            .steps
            .iter_mut()
            .find(|step| step.name == LoopStepName::Development)
            .unwrap();
        development.artifact_path = Some(evidence_path.to_string());
        development.artifact_digest = Some(evidence.artifact_digest().expect("evidence digest"));
        development.status = seaf_core::LoopStepStatus::Completed;
        run.current_step = LoopStepName::OutputReview;
        run.policy_decisions
            .push(serde_json::from_value(serde_json::to_value(decision).unwrap()).unwrap());
        crate::state::save_run(&workspace, &run).expect("run");
        ApplicationFixture {
            _temp: temp,
            source,
            workspace,
            candidate,
        }
    }

    fn assert_applying_without_future_evidence(fixture: &ApplicationFixture, materialized: bool) {
        let run = crate::state::load_run(&fixture.workspace).expect("Applying run");
        let candidate = run.candidate_workspace.as_ref().unwrap();
        assert_eq!(
            candidate.patch_transaction.as_ref().unwrap().phase,
            CandidatePatchPhase::Applying
        );
        assert_eq!(candidate.candidate_tree, candidate.starting_tree);
        assert_eq!(candidate.candidate_diff_digest, sha256_bytes(&[]));
        assert_eq!(
            git_text(Path::new(&candidate.path), &["write-tree"]).unwrap()
                != candidate.starting_tree,
            materialized
        );
        assert_eq!(
            fs::read(Path::new(&candidate.path).join("tracked.txt")).unwrap() == b"candidate\n",
            materialized
        );
        assert!(fixture
            .workspace
            .run_directory()
            .join(PATCH_INTENT_PATH)
            .is_file());
        assert!(fixture
            .workspace
            .run_directory()
            .join(PATCH_EXPECTED_DIFF_PATH)
            .is_file());
        assert!(!fixture
            .workspace
            .run_directory()
            .join(PATCH_APPLIED_DIFF_PATH)
            .exists());
        assert!(!fixture
            .workspace
            .run_directory()
            .join(PATCH_APPLIED_EVIDENCE_PATH)
            .exists());
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
