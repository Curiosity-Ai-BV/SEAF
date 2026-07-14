//! Read-only, factual inspection of persisted loop authority.
//!
//! Invalid run schema, unsafe referenced paths, and symlink/directory authority are fatal.
//! Missing or digest-mismatched immutable bytes are instead reported as degraded evidence so an
//! operator can inspect corruption without any repair, migration, or recovery decision.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    process::Command,
};

use seaf_core::{
    canonical_sha256_digest, is_portable_artifact_path, validate_loop_run, CandidatePatchPhase,
    CandidateWorkspaceLifecycle, LoopRun, LoopStatus, LoopStepName, LoopStepStatus,
    ProviderExchangeKind, ProviderExchangeOutcome, ProviderExchangePhase,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    immutable_artifact::read_verified_regular_file,
    provider_exchange::{
        load_provider_exchange_record, validate_authoritative_provider_exchange_records,
    },
    state::{step_file_stem, LOOP_STEPS},
    workspace::{LoopWorkspace, WorkspaceError, ARTIFACTS_DIR, PROMPTS_DIR, RUN_FILE},
};

pub const MAX_PROVIDER_ATTEMPTS: usize = 64;
pub const MAX_PROVIDER_EXCHANGES_PER_ATTEMPT: usize = 32;
pub const MAX_ARTIFACT_HISTORY_PER_STEP: usize = 32;
pub const MAX_EVALUATION_PREFIX: usize = 64;
pub const MAX_INSPECTION_MESSAGES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionIntegrity {
    Verified,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClassification {
    Current,
    Historical,
    Ambiguous,
    Missing,
    Tampered,
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InputDigestInspection {
    pub digest: String,
    pub path: String,
    pub verification: EvidenceClassification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateInspection {
    pub lifecycle: CandidateWorkspaceLifecycle,
    pub path: String,
    pub starting_head: String,
    pub starting_tree: String,
    pub recorded_current_head: String,
    pub recorded_current_tree: String,
    pub recorded_diff_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_staged_diff_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_phase: Option<CandidatePatchPhase>,
    pub verification: EvidenceClassification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderExchangeInspection {
    pub exchange_index: u32,
    pub kind: ProviderExchangeKind,
    pub phase: ProviderExchangePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ProviderExchangeOutcome>,
    pub ledger_head: bool,
    pub verification: EvidenceClassification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderAttemptInspection {
    pub step: LoopStepName,
    pub attempt: u32,
    pub exchanges: Vec<ProviderExchangeInspection>,
}

struct ProviderAttemptsInventory {
    attempts: Vec<ProviderAttemptInspection>,
    attempts_total: usize,
    exchanges_total: usize,
    maxima: [Option<u32>; 8],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactHistoryInspection {
    pub attempt: u32,
    pub path: String,
    pub extension: String,
    pub classification: EvidenceClassification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepInspection {
    pub name: LoopStepName,
    pub status: LoopStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_digest: Option<String>,
    pub artifact_history: Vec<ArtifactHistoryInspection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvaluationPrefixInspection {
    pub path: String,
    pub classification: EvidenceClassification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoopInspection {
    pub command: &'static str,
    pub run_id: String,
    pub status: LoopStatus,
    pub current_step: LoopStepName,
    pub run_digest: String,
    pub run_directory: String,
    pub run_file: String,
    pub integrity: InspectionIntegrity,
    pub bounds: InspectionBounds,
    pub input_digests: BTreeMap<String, InputDigestInspection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<CandidateInspection>,
    pub steps: Vec<StepInspection>,
    pub provider_attempts: Vec<ProviderAttemptInspection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ledger_head: Option<String>,
    pub evaluation_prefix: Vec<EvaluationPrefixInspection>,
    pub integrity_messages: Vec<String>,
    pub ambiguity_messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectionBounds {
    pub provider_attempts_total: usize,
    pub provider_attempts_truncated: usize,
    pub provider_exchanges_total: usize,
    pub provider_exchanges_truncated: usize,
    pub artifact_history_total: usize,
    pub artifact_history_truncated: usize,
    pub evaluation_prefix_total: usize,
    pub evaluation_prefix_truncated: usize,
    pub integrity_messages_total: usize,
    pub integrity_messages_truncated: usize,
    pub ambiguity_messages_total: usize,
    pub ambiguity_messages_truncated: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinalEvaluationAuthority {
    NotRequired,
    Verified { attempt: u32 },
    Invalid,
}

pub fn inspect_loop_run(runs_root: &Path, run_id: &str) -> Result<LoopInspection, InspectError> {
    validate_run_id(run_id)?;
    let workspace = LoopWorkspace::open_minimal(runs_root, run_id)?;
    let run_bytes = read_verified_regular_file(workspace.run_directory(), RUN_FILE, "loop run")
        .map_err(|error| InspectError::Safety(error.to_string()))?;
    let run: LoopRun = serde_json::from_slice(&run_bytes)?;
    let errors = validate_loop_run(&run);
    if !errors.is_empty() {
        return Err(InspectError::Schema(
            errors
                .into_iter()
                .map(|error| format!("{}: {}", error.field, error.message))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    if run.run_id != run_id {
        return Err(InspectError::Schema(
            "persisted run_id does not match requested run ID".to_string(),
        ));
    }
    validate_referenced_paths(&run)?;

    let mut integrity_messages = Vec::new();
    let mut ambiguity_messages = Vec::new();
    let input_digests = inspect_inputs(&workspace, &run, &mut integrity_messages)?;
    let candidate = inspect_candidate(&run, &mut integrity_messages)?;
    let provider_inventory = inspect_provider_attempts(&workspace, &run, &mut integrity_messages)?;
    let ProviderAttemptsInventory {
        attempts: provider_attempts,
        attempts_total: provider_attempts_total,
        exchanges_total: provider_exchanges_total,
        maxima: provider_maxima,
    } = provider_inventory;
    let final_evaluation_authority = if matches!(
        run.status,
        LoopStatus::EvalPassed | LoopStatus::Promoted | LoopStatus::Failed
    ) && run.human_approval.is_some()
    {
        match crate::load_verified_final_evaluation_authority(&workspace, &run) {
            Ok(authority) => FinalEvaluationAuthority::Verified {
                attempt: authority.execution_intent().attempt(),
            },
            Err(_) => {
                integrity_messages.push("final evaluation authority is invalid".to_string());
                FinalEvaluationAuthority::Invalid
            }
        }
    } else {
        FinalEvaluationAuthority::NotRequired
    };
    let prompt_maxima = inspect_prompt_attempts(&workspace)?;
    let (steps, artifact_history_total) = inspect_steps(
        &workspace,
        &run,
        &provider_maxima,
        &prompt_maxima,
        final_evaluation_authority,
        &mut integrity_messages,
        &mut ambiguity_messages,
    )?;
    let (evaluation_prefix, evaluation_prefix_total) = inspect_evaluation_prefix(
        &workspace,
        &run,
        final_evaluation_authority,
        &mut integrity_messages,
    )?;
    let retained_provider_exchanges = provider_attempts
        .iter()
        .map(|attempt| attempt.exchanges.len())
        .sum::<usize>();
    let retained_artifact_history = steps
        .iter()
        .map(|step| step.artifact_history.len())
        .sum::<usize>();
    let artifact_history_total = artifact_history_total.max(retained_artifact_history);
    let integrity_messages_total = integrity_messages.len();
    integrity_messages.truncate(MAX_INSPECTION_MESSAGES);
    let ambiguity_messages_total = ambiguity_messages.len();
    ambiguity_messages.truncate(MAX_INSPECTION_MESSAGES);
    let integrity = if integrity_messages.is_empty() && ambiguity_messages.is_empty() {
        InspectionIntegrity::Verified
    } else {
        InspectionIntegrity::Degraded
    };

    Ok(LoopInspection {
        command: "inspect",
        run_id: run.run_id.clone(),
        status: run.status,
        current_step: run.current_step,
        run_digest: canonical_sha256_digest(&run)
            .map_err(|error| InspectError::Schema(error.to_string()))?,
        run_directory: workspace.run_directory().display().to_string(),
        run_file: workspace.run_file().display().to_string(),
        integrity,
        bounds: InspectionBounds {
            provider_attempts_total,
            provider_attempts_truncated: provider_attempts_total - provider_attempts.len(),
            provider_exchanges_total,
            provider_exchanges_truncated: provider_exchanges_total - retained_provider_exchanges,
            artifact_history_total,
            artifact_history_truncated: artifact_history_total - retained_artifact_history,
            evaluation_prefix_total,
            evaluation_prefix_truncated: evaluation_prefix_total - evaluation_prefix.len(),
            integrity_messages_total,
            integrity_messages_truncated: integrity_messages_total - integrity_messages.len(),
            ambiguity_messages_total,
            ambiguity_messages_truncated: ambiguity_messages_total - ambiguity_messages.len(),
        },
        input_digests,
        candidate,
        steps,
        provider_attempts,
        provider_ledger_head: run
            .provider_exchange_records
            .last()
            .map(|reference| reference.digest.clone()),
        evaluation_prefix,
        integrity_messages,
        ambiguity_messages,
    })
}

fn validate_run_id(run_id: &str) -> Result<(), InspectError> {
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
        Err(InspectError::Safety(
            "invalid run ID; use only ASCII letters, numbers, '-' or '_'".to_string(),
        ))
    }
}

fn validate_referenced_paths(run: &LoopRun) -> Result<(), InspectError> {
    let check = |path: &str| {
        if !is_portable_artifact_path(path) {
            return Err(InspectError::Safety(format!(
                "run contains unsafe referenced path: {path}"
            )));
        }
        Ok(())
    };
    for path in run
        .steps
        .iter()
        .filter_map(|record| record.artifact_path.as_deref())
        .chain(
            run.provider_exchange_records
                .iter()
                .map(|reference| reference.path.as_str()),
        )
        .chain(run.eval_report_path.as_deref())
    {
        check(path)?;
    }
    if let Some(transaction) = run
        .candidate_workspace
        .as_ref()
        .and_then(|candidate| candidate.patch_transaction.as_ref())
    {
        check(&transaction.intent.path)?;
        if let Some(evidence) = &transaction.applied_evidence {
            check(&evidence.path)?;
        }
    }
    if let Some(approval) = &run.human_approval {
        check(&approval.candidate_diff.path)?;
        check(&approval.output_review.path)?;
        check(&approval.output_review_request.path)?;
        check(&approval.output_review_response.path)?;
    }
    if let Some(promotion) = &run.promotion {
        for path in [
            &promotion.intent.path,
            &promotion.candidate_diff.path,
            &promotion.testing_evidence.path,
            &promotion.eval_report.path,
        ] {
            check(path)?;
        }
    }
    Ok(())
}

fn inspect_inputs(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    messages: &mut Vec<String>,
) -> Result<BTreeMap<String, InputDigestInspection>, InspectError> {
    let mut inputs = BTreeMap::new();
    let entries = [
        (
            "ticket",
            "inputs/ticket.json",
            Some(&run.input_digests.ticket),
        ),
        (
            "policy",
            "inputs/policy.json",
            Some(&run.input_digests.policy),
        ),
        (
            "config",
            "inputs/config.json",
            Some(&run.input_digests.config),
        ),
        (
            "repository",
            "inputs/repository.json",
            Some(&run.input_digests.repository),
        ),
        (
            "eval_config",
            "inputs/eval-config.json",
            run.input_digests.eval_config.as_ref(),
        ),
    ];
    for (name, path, digest) in entries {
        let Some(digest) = digest else {
            continue;
        };
        let verification = classify_digest(workspace, path, digest)?;
        if verification != EvidenceClassification::Verified {
            messages.push(format!("immutable input {name} is {verification:?}").to_lowercase());
        }
        inputs.insert(
            name.to_string(),
            InputDigestInspection {
                digest: digest.clone(),
                path: path.to_string(),
                verification,
            },
        );
    }
    Ok(inputs)
}

fn inspect_candidate(
    run: &LoopRun,
    messages: &mut Vec<String>,
) -> Result<Option<CandidateInspection>, InspectError> {
    let Some(candidate) = run.candidate_workspace.as_ref() else {
        return Ok(None);
    };
    let path = Path::new(&candidate.path);
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let verification = if candidate.lifecycle == CandidateWorkspaceLifecycle::Cleaned {
                EvidenceClassification::Verified
            } else {
                messages.push("candidate Git authority is missing".to_string());
                EvidenceClassification::Missing
            };
            return Ok(Some(CandidateInspection {
                lifecycle: candidate.lifecycle,
                path: candidate.path.clone(),
                starting_head: candidate.starting_head.clone(),
                starting_tree: candidate.starting_tree.clone(),
                recorded_current_head: candidate.candidate_head.clone(),
                recorded_current_tree: candidate.candidate_tree.clone(),
                recorded_diff_digest: candidate.candidate_diff_digest.clone(),
                observed_head: None,
                observed_staged_diff_digest: None,
                patch_phase: candidate
                    .patch_transaction
                    .as_ref()
                    .map(|transaction| transaction.phase),
                verification,
            }));
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(InspectError::Safety(
            "candidate path authority is not a real directory".to_string(),
        ));
    }
    let canonical_path = path.canonicalize()?;
    let git_toplevel = PathBuf::from(git_output(path, &["rev-parse", "--show-toplevel"])?);
    if git_toplevel.canonicalize()? != canonical_path {
        return Err(InspectError::Safety(
            "candidate path is not the exact Git worktree root".to_string(),
        ));
    }
    let observed_head = git_output(path, &["rev-parse", "HEAD"])?;
    let observed_diff = git_output_bytes(
        path,
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
    let observed_diff_digest = digest_bytes(&observed_diff);
    let worktree_matches_index =
        match crate::candidate_workspace::verify_worktree_matches_index(path) {
            Ok(()) => true,
            Err(crate::CandidateWorkspaceError::Mismatch(_)) => false,
            Err(error) => {
                return Err(InspectError::Safety(format!(
                    "candidate physical worktree could not be verified safely: {error}"
                )))
            }
        };
    let untracked = git_output_bytes(path, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let verification = if observed_head == candidate.candidate_head
        && observed_diff_digest == candidate.candidate_diff_digest
        && worktree_matches_index
        && untracked.is_empty()
    {
        EvidenceClassification::Verified
    } else {
        messages.push("candidate Git authority is tampered".to_string());
        EvidenceClassification::Tampered
    };
    Ok(Some(CandidateInspection {
        lifecycle: candidate.lifecycle,
        path: candidate.path.clone(),
        starting_head: candidate.starting_head.clone(),
        starting_tree: candidate.starting_tree.clone(),
        recorded_current_head: candidate.candidate_head.clone(),
        recorded_current_tree: candidate.candidate_tree.clone(),
        recorded_diff_digest: candidate.candidate_diff_digest.clone(),
        observed_head: Some(observed_head),
        observed_staged_diff_digest: Some(observed_diff_digest),
        patch_phase: candidate
            .patch_transaction
            .as_ref()
            .map(|transaction| transaction.phase),
        verification,
    }))
}

fn inspect_provider_attempts(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    messages: &mut Vec<String>,
) -> Result<ProviderAttemptsInventory, InspectError> {
    let chain_classification =
        match validate_authoritative_provider_exchange_records(workspace, run) {
            Ok(()) => EvidenceClassification::Verified,
            Err(error) => {
                let classification = classify_provider_error(&error)?;
                messages.push(
                    "authoritative provider record chain is tampered or incomplete".to_string(),
                );
                classification
            }
        };
    let mut attempts: Vec<ProviderAttemptInspection> = Vec::new();
    let mut attempts_total = 0;
    let mut exchanges_total = 0;
    let mut previous_by_step = [None; 8];
    let mut maxima = [None; 8];
    let mut current_group = None;
    let ledger_head_group = run
        .provider_exchange_records
        .last()
        .map(|reference| (reference.step, reference.step_attempt));
    for reference in &run.provider_exchange_records {
        let group = (reference.step, reference.step_attempt);
        if current_group != Some(group) {
            attempts_total += 1;
            current_group = Some(group);
            let index = step_index(reference.step);
            maxima[index] = Some(
                maxima[index].map_or(reference.step_attempt, |current: u32| {
                    current.max(reference.step_attempt)
                }),
            );
            let previous = previous_by_step[index];
            if previous.map_or(reference.step_attempt != 1, |value: u32| {
                value.checked_add(1) != Some(reference.step_attempt)
            }) {
                messages.push(format!(
                    "provider attempt sequence for {:?} has a factual gap before {}",
                    reference.step, reference.step_attempt
                ));
            }
            previous_by_step[index] = Some(reference.step_attempt);
            if attempts.len() < MAX_PROVIDER_ATTEMPTS {
                attempts.push(ProviderAttemptInspection {
                    step: reference.step,
                    attempt: reference.step_attempt,
                    exchanges: Vec::new(),
                });
            } else if Some(group) == ledger_head_group {
                attempts.pop();
                attempts.push(ProviderAttemptInspection {
                    step: reference.step,
                    attempt: reference.step_attempt,
                    exchanges: Vec::new(),
                });
            }
        }
        exchanges_total += 1;
        if let Some(summary) = attempts.last_mut().filter(|summary| {
            summary.step == reference.step && summary.attempt == reference.step_attempt
        }) {
            let loaded = load_provider_exchange_record(workspace.run_directory(), reference);
            let (outcome, verification) = match loaded {
                Ok(record) => (record.outcome, chain_classification.clone()),
                Err(error) => {
                    let classification = classify_provider_error(&error)?;
                    if classification == EvidenceClassification::Missing {
                        messages.push(format!(
                            "provider ledger record {} is missing",
                            reference.path
                        ));
                    } else {
                        messages.push(format!(
                            "provider ledger record {} is tampered",
                            reference.path
                        ));
                    }
                    (None, classification)
                }
            };
            let exchange = ProviderExchangeInspection {
                exchange_index: reference.exchange_index,
                kind: reference.kind,
                phase: reference.phase,
                outcome,
                ledger_head: run.provider_exchange_records.last() == Some(reference),
                verification,
            };
            if summary.exchanges.len() < MAX_PROVIDER_EXCHANGES_PER_ATTEMPT {
                summary.exchanges.push(exchange);
            } else if let Some(last) = summary.exchanges.last_mut() {
                *last = exchange;
            }
        }
    }
    Ok(ProviderAttemptsInventory {
        attempts,
        attempts_total,
        exchanges_total,
        maxima,
    })
}

fn classify_provider_error(
    error: &crate::ProviderExchangeError,
) -> Result<EvidenceClassification, InspectError> {
    match error {
        crate::ProviderExchangeError::Artifact(message)
            if message.contains("could not be inspected") =>
        {
            Ok(EvidenceClassification::Missing)
        }
        crate::ProviderExchangeError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(EvidenceClassification::Missing)
        }
        crate::ProviderExchangeError::Artifact(message)
            if message.contains("safety")
                || message.contains("regular file")
                || message.contains("symlink") =>
        {
            Err(InspectError::Safety(error.to_string()))
        }
        _ => Ok(EvidenceClassification::Tampered),
    }
}

fn inspect_steps(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    provider_maxima: &[Option<u32>; 8],
    prompt_maxima: &[Option<u32>; 8],
    final_evaluation_authority: FinalEvaluationAuthority,
    messages: &mut Vec<String>,
    ambiguities: &mut Vec<String>,
) -> Result<(Vec<StepInspection>, usize), InspectError> {
    let mut steps = Vec::new();
    let mut total_history = 0;
    for record in &run.steps {
        let stem = step_file_stem(record.name);
        let priority = record
            .artifact_path
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let (artifacts, total) = list_regular_artifacts(
            workspace,
            MAX_ARTIFACT_HISTORY_PER_STEP,
            &priority,
            |path| role_artifact_identity(&stem, path).is_some(),
        )?;
        total_history += total;
        let index = step_index(record.name);
        let is_evaluation_step = matches!(
            record.name,
            LoopStepName::Testing | LoopStepName::EvalReport
        );
        let invalid_final_authority =
            is_evaluation_step && final_evaluation_authority == FinalEvaluationAuthority::Invalid;
        let durable_attempt = match (is_evaluation_step, final_evaluation_authority) {
            (true, FinalEvaluationAuthority::Verified { attempt }) => Some(attempt),
            (true, FinalEvaluationAuthority::Invalid) => None,
            _ => provider_maxima[index]
                .into_iter()
                .chain(prompt_maxima[index])
                .max()
                .or(Some(1)),
        };
        let mut history = Vec::new();
        for path in &artifacts {
            let (attempt, extension) =
                role_artifact_identity(&stem, path).expect("filtered role artifact identity");
            let mut classification = if record.artifact_path.as_deref() == Some(path) {
                match record.artifact_digest.as_deref() {
                    Some(digest) => classify_digest(workspace, path, digest)?,
                    None => EvidenceClassification::Tampered,
                }
            } else {
                EvidenceClassification::Historical
            };
            if classification == EvidenceClassification::Verified {
                classification = EvidenceClassification::Current;
            }
            if classification == EvidenceClassification::Current && invalid_final_authority {
                ambiguities.push(format!(
                    "{:?} selected artifact cannot be current because final evaluation authority is invalid",
                    record.name
                ));
                classification = EvidenceClassification::Ambiguous;
            }
            if record.artifact_path.as_deref() == Some(path)
                && durable_attempt.is_some_and(|durable_attempt| attempt != durable_attempt)
            {
                let durable_attempt = durable_attempt.expect("checked durable attempt");
                let message = if attempt == 1 && durable_attempt >= 2 {
                    format!(
                        "{:?} durable attempt {durable_attempt} still selects the historical fixed-name artifact",
                        record.name
                    )
                } else {
                    format!(
                        "{:?} selects artifact attempt {attempt} but durable authority proves attempt {durable_attempt}",
                        record.name
                    )
                };
                ambiguities.push(message);
                if classification == EvidenceClassification::Current {
                    classification = EvidenceClassification::Ambiguous;
                }
            }
            if classification == EvidenceClassification::Tampered {
                messages.push(format!(
                    "current {:?} role artifact is tampered",
                    record.name
                ));
            }
            history.push(ArtifactHistoryInspection {
                attempt,
                path: path.clone(),
                extension,
                classification,
            });
        }
        if let Some(path) = record.artifact_path.as_ref() {
            if !history.iter().any(|artifact| artifact.path == *path) {
                let verification = classify_digest(
                    workspace,
                    path,
                    record.artifact_digest.as_deref().unwrap_or_default(),
                )?;
                let identity = role_artifact_identity(&stem, path);
                let (selected_attempt, extension, mut classification) =
                    match (identity, verification) {
                        (Some((attempt, extension)), EvidenceClassification::Verified) => {
                            (attempt, extension, EvidenceClassification::Current)
                        }
                        (Some((attempt, extension)), EvidenceClassification::Missing) => {
                            (attempt, extension, EvidenceClassification::Missing)
                        }
                        (identity, _) => (
                            identity
                                .as_ref()
                                .map_or(durable_attempt.unwrap_or(1), |(attempt, _)| *attempt),
                            identity.map(|(_, extension)| extension).unwrap_or_else(|| {
                                Path::new(path)
                                    .extension()
                                    .and_then(|value| value.to_str())
                                    .unwrap_or("")
                                    .to_string()
                            }),
                            EvidenceClassification::Tampered,
                        ),
                    };
                if classification == EvidenceClassification::Current && invalid_final_authority {
                    ambiguities.push(format!(
                        "{:?} selected artifact cannot be current because final evaluation authority is invalid",
                        record.name
                    ));
                    classification = EvidenceClassification::Ambiguous;
                }
                if durable_attempt
                    .is_some_and(|durable_attempt| selected_attempt != durable_attempt)
                {
                    let durable_attempt = durable_attempt.expect("checked durable attempt");
                    ambiguities.push(format!(
                        "{:?} selects artifact attempt {selected_attempt} but durable authority proves attempt {durable_attempt}",
                        record.name
                    ));
                    if classification == EvidenceClassification::Current {
                        classification = EvidenceClassification::Ambiguous;
                    }
                }
                if classification != EvidenceClassification::Current {
                    messages.push(format!(
                        "current {:?} role artifact is missing, unrecognized, or inconsistent",
                        record.name
                    ));
                }
                if role_artifact_identity(&stem, path).is_none()
                    || classification == EvidenceClassification::Missing
                {
                    total_history += 1;
                }
                history.push(ArtifactHistoryInspection {
                    attempt: selected_attempt,
                    path: path.clone(),
                    extension,
                    classification,
                });
            }
        }
        history.sort_by(|left, right| {
            left.attempt
                .cmp(&right.attempt)
                .then_with(|| left.path.cmp(&right.path))
        });
        if history.len() > MAX_ARTIFACT_HISTORY_PER_STEP {
            let selected = record.artifact_path.as_deref();
            if let Some(position) = history
                .iter()
                .rposition(|entry| Some(entry.path.as_str()) != selected)
            {
                history.remove(position);
            } else {
                history.truncate(MAX_ARTIFACT_HISTORY_PER_STEP);
            }
        }
        steps.push(StepInspection {
            name: record.name,
            status: record.status,
            artifact_path: record.artifact_path.clone(),
            artifact_digest: record.artifact_digest.clone(),
            artifact_history: history,
        });
    }
    Ok((steps, total_history))
}

fn inspect_prompt_attempts(workspace: &LoopWorkspace) -> Result<[Option<u32>; 8], InspectError> {
    let directory = workspace.run_directory().join(PROMPTS_DIR);
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok([None; 8]),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(InspectError::Safety(
            "prompt attempt authority is not a real directory".to_string(),
        ));
    }
    let mut maxima = [None; 8];
    let mut counts = [0_u64; 8];
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(InspectError::Safety(format!(
                "prompt inventory contains unsafe entry: {}",
                entry.path().display()
            )));
        }
        let name = entry.file_name().into_string().map_err(|_| {
            InspectError::Safety("prompt inventory contains a non-UTF-8 name".to_string())
        })?;
        for step in LOOP_STEPS {
            let stem = step_file_stem(step);
            match prompt_attempt_identity(&stem, &name) {
                Some(attempt) => {
                    let index = step_index(step);
                    counts[index] += 1;
                    maxima[index] =
                        Some(maxima[index].map_or(attempt, |current: u32| current.max(attempt)));
                }
                None if name.starts_with(&stem)
                    && name.contains(".attempt-")
                    && name.ends_with(".prompt.md") =>
                {
                    return Err(InspectError::Safety(format!(
                        "prompt attempt name is malformed, non-canonical, or exhausted: {name}"
                    )));
                }
                None => {}
            }
        }
    }
    for index in 0..8 {
        if maxima[index].is_some_and(|maximum| counts[index] != u64::from(maximum)) {
            return Err(InspectError::Safety(format!(
                "prompt attempt sequence for {:?} contains a skipped attempt",
                LOOP_STEPS[index]
            )));
        }
    }
    Ok(maxima)
}

fn prompt_attempt_identity(stem: &str, name: &str) -> Option<u32> {
    if name == format!("{stem}.prompt.md") {
        return Some(1);
    }
    let attempt = name
        .strip_prefix(&format!("{stem}.attempt-"))?
        .strip_suffix(".prompt.md")?;
    if attempt.len() < 3 || !attempt.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let parsed = attempt.parse::<u32>().ok()?;
    (parsed >= 2 && format!("{parsed:03}") == attempt).then_some(parsed)
}

fn step_index(step: LoopStepName) -> usize {
    LOOP_STEPS
        .iter()
        .position(|candidate| *candidate == step)
        .expect("known loop step")
}

fn inspect_evaluation_prefix(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    final_evaluation_authority: FinalEvaluationAuthority,
    messages: &mut Vec<String>,
) -> Result<(Vec<EvaluationPrefixInspection>, usize), InspectError> {
    let is_evaluation_prefix = |path: &str| {
        path.strip_prefix("artifacts/").is_some_and(|name| {
            name.starts_with("07-testing") || name.starts_with("08-eval-report")
        })
    };
    let current_paths = run
        .steps
        .iter()
        .filter(|record| {
            matches!(
                record.name,
                LoopStepName::Testing | LoopStepName::EvalReport
            )
        })
        .filter_map(|record| record.artifact_path.clone())
        .chain(run.eval_report_path.clone())
        .filter(|path| is_evaluation_prefix(path))
        .collect::<BTreeSet<_>>();
    let (artifacts, existing_total) = list_regular_artifacts(
        workspace,
        MAX_EVALUATION_PREFIX,
        &current_paths,
        is_evaluation_prefix,
    )?;
    let mut inventory = Vec::new();
    for path in &artifacts {
        let current = run
            .steps
            .iter()
            .find(|record| record.artifact_path.as_deref() == Some(path));
        let classification = if let Some(record) = current {
            match classify_digest(
                workspace,
                path,
                record.artifact_digest.as_deref().unwrap_or_default(),
            )? {
                EvidenceClassification::Verified
                    if final_evaluation_authority == FinalEvaluationAuthority::Invalid =>
                {
                    EvidenceClassification::Ambiguous
                }
                EvidenceClassification::Verified => EvidenceClassification::Current,
                other => {
                    messages.push(format!("evaluation prefix {path} is {other:?}").to_lowercase());
                    other
                }
            }
        } else {
            EvidenceClassification::Historical
        };
        inventory.push(EvaluationPrefixInspection {
            path: path.clone(),
            classification,
        });
    }
    let mut missing_current = 0;
    for path in current_paths {
        if !inventory.iter().any(|entry| entry.path == path) {
            missing_current += 1;
            messages.push(format!("evaluation prefix {path} is missing"));
            inventory.push(EvaluationPrefixInspection {
                path,
                classification: EvidenceClassification::Missing,
            });
        }
    }
    inventory.sort_by(|left, right| left.path.cmp(&right.path));
    inventory.truncate(MAX_EVALUATION_PREFIX);
    let total = existing_total + missing_current;
    Ok((inventory, total))
}

fn list_regular_artifacts<F>(
    workspace: &LoopWorkspace,
    retain_limit: usize,
    priority: &BTreeSet<String>,
    include: F,
) -> Result<(Vec<String>, usize), InspectError>
where
    F: Fn(&str) -> bool,
{
    let directory = workspace.run_directory().join(ARTIFACTS_DIR);
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(InspectError::Safety(
            "artifacts authority is not a real directory".to_string(),
        ));
    }
    let mut paths = BTreeSet::new();
    let mut retained_priority = BTreeSet::new();
    let historical_limit = retain_limit.saturating_sub(priority.len().min(retain_limit));
    let mut total = 0;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let metadata = entry.file_type()?;
        if metadata.is_symlink() || !metadata.is_file() {
            return Err(InspectError::Safety(format!(
                "artifact inventory contains unsafe entry: {}",
                entry.path().display()
            )));
        }
        let name = entry.file_name().into_string().map_err(|_| {
            InspectError::Safety("artifact inventory contains a non-UTF-8 name".to_string())
        })?;
        let path = format!("{ARTIFACTS_DIR}/{name}");
        if include(&path) {
            total += 1;
            if priority.contains(&path) {
                retained_priority.insert(path);
            } else {
                paths.insert(path);
                if paths.len() > historical_limit {
                    paths.pop_last();
                }
            }
        }
    }
    paths.extend(retained_priority);
    Ok((paths.into_iter().collect(), total))
}

fn role_artifact_identity(stem: &str, path: &str) -> Option<(u32, String)> {
    let name = path.strip_prefix("artifacts/")?;
    let suffix = name.strip_prefix(stem)?;
    if let Some(extension) = suffix.strip_prefix('.') {
        if !extension.contains('.') && valid_extension(extension) {
            return Some((1, extension.to_string()));
        }
    }
    let suffix = suffix.strip_prefix(".attempt-")?;
    let (attempt, extension) = suffix.split_once('.')?;
    if attempt.len() < 3 || !attempt.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let parsed_attempt = attempt.parse::<u32>().ok()?;
    (parsed_attempt >= 2
        && format!("{parsed_attempt:03}") == attempt
        && !extension.contains('.')
        && valid_extension(extension))
    .then(|| (parsed_attempt, extension.to_string()))
}

fn valid_extension(extension: &str) -> bool {
    !extension.is_empty()
        && extension
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

fn classify_digest(
    workspace: &LoopWorkspace,
    path: &str,
    expected: &str,
) -> Result<EvidenceClassification, InspectError> {
    match read_verified_regular_file(workspace.run_directory(), path, "inspect evidence") {
        Ok(bytes) if digest_bytes(&bytes) == expected => Ok(EvidenceClassification::Verified),
        Ok(_) => Ok(EvidenceClassification::Tampered),
        Err(crate::immutable_artifact::ImmutableArtifactError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(EvidenceClassification::Missing)
        }
        Err(crate::immutable_artifact::ImmutableArtifactError::Safety(message))
            if message.contains("No such file") || message.contains("could not be inspected") =>
        {
            Ok(EvidenceClassification::Missing)
        }
        Err(error) => Err(InspectError::Safety(error.to_string())),
    }
}

fn git_output(path: &Path, args: &[&str]) -> Result<String, InspectError> {
    let bytes = git_output_bytes(path, args)?;
    String::from_utf8(bytes)
        .map(|value| value.trim_end().to_string())
        .map_err(|_| InspectError::Safety("Git inspection output was not UTF-8".to_string()))
}

fn git_output_bytes(path: &Path, args: &[&str]) -> Result<Vec<u8>, InspectError> {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let output = Command::new("git")
        .env_clear()
        .env("PATH", path_env)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_device())
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-c")
        .arg(format!("core.hooksPath={}", null_device()))
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(path)
        .output()?;
    if !output.status.success() {
        return Err(InspectError::Safety(
            "candidate Git authority could not be inspected".to_string(),
        ));
    }
    Ok(output.stdout)
}

#[cfg(windows)]
fn null_device() -> &'static str {
    "NUL"
}

#[cfg(not(windows))]
fn null_device() -> &'static str {
    "/dev/null"
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Debug)]
pub enum InspectError {
    Safety(String),
    Schema(String),
    Workspace(WorkspaceError),
    Json(serde_json::Error),
    Io(std::io::Error),
}

impl fmt::Display for InspectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Safety(message) => {
                write!(formatter, "unsafe loop inspection authority: {message}")
            }
            Self::Schema(message) => write!(formatter, "invalid loop inspection schema: {message}"),
            Self::Workspace(error) => write!(formatter, "could not open loop run: {error}"),
            Self::Json(error) => write!(formatter, "invalid loop run JSON: {error}"),
            Self::Io(error) => write!(formatter, "loop inspection I/O error: {error}"),
        }
    }
}

impl Error for InspectError {}

impl From<WorkspaceError> for InspectError {
    fn from(error: WorkspaceError) -> Self {
        Self::Workspace(error)
    }
}

impl From<serde_json::Error> for InspectError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<std::io::Error> for InspectError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{create_run, NewLoopRun};
    use seaf_core::LoopInputDigests;

    fn run(run_id: &str) -> LoopRun {
        create_run(NewLoopRun {
            run_id: run_id.to_string(),
            ticket_id: "ticket".to_string(),
            goal_id: "goal".to_string(),
            provider: "provider".to_string(),
            model: "model".to_string(),
            input_digests: LoopInputDigests {
                ticket: "0".repeat(64),
                policy: "1".repeat(64),
                config: "2".repeat(64),
                repository: "3".repeat(64),
                eval_config: None,
            },
        })
    }

    #[test]
    fn later_durable_attempt_with_fixed_role_artifact_is_ambiguous_forensic_evidence() {
        let temp = tempfile::tempdir().expect("temp dir");
        let workspace =
            LoopWorkspace::create(&temp.path().join("runs"), "ambiguous").expect("workspace");
        let bytes = b"historical role artifact";
        crate::artifact_safety::write_private_fixture(
            workspace.run_directory().join("artifacts/01-research.json"),
            bytes,
        )
        .expect("artifact");
        let mut run = run("ambiguous");
        let research = run
            .steps
            .iter_mut()
            .find(|record| record.name == LoopStepName::Research)
            .expect("research");
        research.artifact_path = Some("artifacts/01-research.json".to_string());
        research.artifact_digest = Some(digest_bytes(bytes));
        let provider_maxima = [None; 8];
        let mut prompt_maxima = [None; 8];
        prompt_maxima[0] = Some(2);
        let mut messages = Vec::new();
        let mut ambiguities = Vec::new();

        let (steps, _) = inspect_steps(
            &workspace,
            &run,
            &provider_maxima,
            &prompt_maxima,
            FinalEvaluationAuthority::NotRequired,
            &mut messages,
            &mut ambiguities,
        )
        .expect("inspect historical ambiguity");

        assert_eq!(
            steps[0].artifact_history[0].classification,
            EvidenceClassification::Ambiguous
        );
        assert_eq!(ambiguities.len(), 1);
        assert!(messages.is_empty());
    }

    #[test]
    fn unsafe_referenced_path_is_fatal_before_inspection_can_follow_it() {
        let mut run = run("unsafe-reference");
        run.steps[0].artifact_path = Some("../outside.json".to_string());
        run.steps[0].artifact_digest = Some("a".repeat(64));

        let error = validate_referenced_paths(&run).expect_err("unsafe path authority");

        assert!(error.to_string().contains("unsafe referenced path"));
    }

    #[test]
    fn role_artifact_attempt_name_uses_canonical_minimum_width_without_a_three_digit_cap() {
        assert_eq!(
            role_artifact_identity("01-research", "artifacts/01-research.attempt-1000.yaml"),
            Some((1000, "yaml".to_string()))
        );
        assert_eq!(
            role_artifact_identity("01-research", "artifacts/01-research.attempt-02.yaml"),
            None
        );
        assert_eq!(
            role_artifact_identity("01-research", "artifacts/01-research.attempt-0002.yaml"),
            None
        );
    }

    #[test]
    fn selected_indexed_artifact_is_not_current_when_durable_authority_stops_earlier() {
        let temp = tempfile::tempdir().expect("temp dir");
        let workspace =
            LoopWorkspace::create(&temp.path().join("runs"), "mismatched").expect("workspace");
        let fixed = b"attempt one";
        let selected = b"untrusted attempt three";
        crate::artifact_safety::write_private_fixture(
            workspace.run_directory().join("artifacts/01-research.json"),
            fixed,
        )
        .unwrap();
        crate::artifact_safety::write_private_fixture(
            workspace
                .run_directory()
                .join("artifacts/01-research.attempt-003.json"),
            selected,
        )
        .unwrap();
        let mut run = run("mismatched");
        run.steps[0].artifact_path = Some("artifacts/01-research.attempt-003.json".to_string());
        run.steps[0].artifact_digest = Some(digest_bytes(selected));
        let mut messages = Vec::new();
        let mut ambiguities = Vec::new();

        let provider_maxima = [None; 8];
        let mut prompt_maxima = [None; 8];
        prompt_maxima[0] = Some(2);
        let (steps, _) = inspect_steps(
            &workspace,
            &run,
            &provider_maxima,
            &prompt_maxima,
            FinalEvaluationAuthority::NotRequired,
            &mut messages,
            &mut ambiguities,
        )
        .expect("inspect mismatch");

        assert_eq!(
            steps[0]
                .artifact_history
                .iter()
                .find(|entry| entry.attempt == 3)
                .unwrap()
                .classification,
            EvidenceClassification::Ambiguous
        );
        assert_eq!(
            steps[0]
                .artifact_history
                .iter()
                .find(|entry| entry.attempt == 1)
                .unwrap()
                .classification,
            EvidenceClassification::Historical,
            "once an indexed artifact is selected, persisted authority cannot prove the fixed file was reused"
        );
        assert!(!ambiguities.is_empty());
    }

    #[test]
    fn provider_attempt_output_is_bounded_while_totals_remain_factual() {
        use seaf_core::{
            ProviderExchangeKind, ProviderExchangePhase, ProviderExchangeRecordReference,
            ProviderRole,
        };

        let temp = tempfile::tempdir().unwrap();
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), "ledger-cap").unwrap();
        let mut run = run("ledger-cap");
        for attempt in 1..=100 {
            run.provider_exchange_records
                .push(ProviderExchangeRecordReference {
                    run_id: run.run_id.clone(),
                    step: LoopStepName::Research,
                    role: ProviderRole::Researcher,
                    step_attempt: attempt,
                    exchange_index: 1,
                    kind: ProviderExchangeKind::Initial,
                    context_round: None,
                    phase: ProviderExchangePhase::Request,
                    path: format!(
                        "artifacts/01-research.attempt-{attempt:03}.exchange-001.researcher.initial.request.record.json"
                    ),
                    digest: "0".repeat(64),
                });
        }
        let mut messages = Vec::new();

        let inventory = inspect_provider_attempts(&workspace, &run, &mut messages).unwrap();

        assert_eq!(inventory.attempts.len(), MAX_PROVIDER_ATTEMPTS);
        assert_eq!(inventory.attempts_total, 100);
        assert_eq!(inventory.exchanges_total, 100);
        assert_eq!(inventory.attempts.first().unwrap().attempt, 1);
        assert_eq!(inventory.attempts.last().unwrap().attempt, 100);
        assert!(
            inventory
                .attempts
                .last()
                .unwrap()
                .exchanges
                .last()
                .unwrap()
                .ledger_head
        );
        assert_eq!(inventory.maxima[0], Some(100));
    }

    #[test]
    fn artifact_history_cap_retains_selected_current_path_after_lexical_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), "history-cap").unwrap();
        for attempt in 1..=40 {
            let name = if attempt == 1 {
                "01-research.json".to_string()
            } else {
                format!("01-research.attempt-{attempt:03}.json")
            };
            crate::artifact_safety::write_private_fixture(
                workspace.run_directory().join("artifacts").join(name),
                format!("attempt {attempt}").as_bytes(),
            )
            .unwrap();
        }
        let mut run = run("history-cap");
        let selected = "artifacts/01-research.attempt-040.json";
        run.steps[0].artifact_path = Some(selected.to_string());
        run.steps[0].artifact_digest = Some(digest_bytes(b"attempt 40"));
        let mut prompt_maxima = [None; 8];
        prompt_maxima[0] = Some(40);

        let (steps, total) = inspect_steps(
            &workspace,
            &run,
            &[None; 8],
            &prompt_maxima,
            FinalEvaluationAuthority::NotRequired,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();

        assert_eq!(total, 40);
        assert_eq!(
            steps[0].artifact_history.len(),
            MAX_ARTIFACT_HISTORY_PER_STEP
        );
        assert_eq!(
            steps[0]
                .artifact_history
                .iter()
                .find(|entry| entry.path == selected)
                .unwrap()
                .classification,
            EvidenceClassification::Current
        );
    }
}
