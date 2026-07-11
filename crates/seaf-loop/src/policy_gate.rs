use std::{
    error::Error,
    fmt, fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use seaf_core::Policy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    patch::{parse_unified_diff, PatchParseError},
    policy::matching_pattern,
};

#[derive(Debug, Clone, PartialEq)]
pub struct PatchGateRequest<'a> {
    pub repo_root: &'a Path,
    pub artifact_dir: &'a Path,
    pub patch_id: &'a str,
    pub patch: &'a str,
    pub policy: &'a Policy,
    pub apply_patch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDecision {
    pub patch_id: String,
    pub patch_sha256: String,
    pub changed_paths: Vec<String>,
    pub decision: PatchDecisionKind,
    pub reasons: Vec<PolicyDecisionReason>,
    pub requires_human_review: bool,
    pub apply_requested: bool,
    pub applied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchDecisionKind {
    Allowed,
    RequiresHumanReview,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDecisionReason {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchCommand {
    GitApplyCheck,
    GitApply,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success() -> Self {
        Self {
            success: true,
            stderr: String::new(),
        }
    }

    pub fn failure(stderr: impl Into<String>) -> Self {
        Self {
            success: false,
            stderr: stderr.into(),
        }
    }
}

pub trait PatchCommandRunner {
    fn run(
        &mut self,
        repo_root: &Path,
        command: PatchCommand,
        patch: &str,
    ) -> Result<CommandOutput, PatchGateError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct GitCommandRunner;

impl PatchCommandRunner for GitCommandRunner {
    fn run(
        &mut self,
        repo_root: &Path,
        command: PatchCommand,
        patch: &str,
    ) -> Result<CommandOutput, PatchGateError> {
        let args = match command {
            PatchCommand::GitApplyCheck => ["apply", "--check", "-"],
            PatchCommand::GitApply => ["apply", "-", ""],
        };
        let args = args.into_iter().filter(|arg| !arg.is_empty());
        let mut child = Command::new("git")
            .args(args)
            .current_dir(repo_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(PatchGateError::CommandIo)?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| PatchGateError::Command("git stdin was unavailable".to_string()))?;
        stdin
            .write_all(patch.as_bytes())
            .map_err(PatchGateError::CommandIo)?;
        drop(stdin);

        let output = child
            .wait_with_output()
            .map_err(PatchGateError::CommandIo)?;
        Ok(CommandOutput {
            success: output.status.success(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

pub fn gate_patch<R: PatchCommandRunner + ?Sized>(
    request: PatchGateRequest<'_>,
    runner: &mut R,
) -> Result<PolicyDecision, PatchGateError> {
    gate_patch_with_execution(request, runner, true)
}

pub(crate) fn gate_patch_proposal<R: PatchCommandRunner + ?Sized>(
    request: PatchGateRequest<'_>,
    runner: &mut R,
) -> Result<PolicyDecision, PatchGateError> {
    gate_patch_with_execution(request, runner, false)
}

fn gate_patch_with_execution<R: PatchCommandRunner + ?Sized>(
    request: PatchGateRequest<'_>,
    runner: &mut R,
    execute_apply: bool,
) -> Result<PolicyDecision, PatchGateError> {
    fs::create_dir_all(request.artifact_dir).map_err(PatchGateError::Io)?;

    let artifact_stem = safe_artifact_stem(request.patch_id);
    fs::write(
        request.artifact_dir.join(format!("{artifact_stem}.diff")),
        request.patch,
    )
    .map_err(PatchGateError::Io)?;

    let mut decision = PolicyDecision {
        patch_id: request.patch_id.to_string(),
        patch_sha256: patch_digest(request.patch),
        changed_paths: Vec::new(),
        decision: PatchDecisionKind::Allowed,
        reasons: Vec::new(),
        requires_human_review: false,
        apply_requested: request.apply_patch,
        applied: false,
    };

    match parse_unified_diff(request.patch) {
        Ok(parsed) => {
            decision.changed_paths = parsed.changed_paths.clone();

            if parsed.contains_binary_patch {
                decision.reasons.push(reason(
                    "binary_patch",
                    "binary patches cannot be safely reviewed or applied by the local agent",
                    None,
                    None,
                    None,
                ));
            }

            classify_paths(request.policy, &parsed.changed_paths, &mut decision.reasons);
            refresh_decision_kind(&mut decision);

            if request.apply_patch && decision.decision == PatchDecisionKind::Allowed {
                let check = runner.run(
                    request.repo_root,
                    PatchCommand::GitApplyCheck,
                    request.patch,
                )?;
                if !check.success {
                    decision.reasons.push(reason(
                        "git_apply_check_failed",
                        "git apply --check failed; patch was not applied",
                        None,
                        None,
                        Some(trim_stderr(&check.stderr)),
                    ));
                    refresh_decision_kind(&mut decision);
                } else if execute_apply {
                    let apply =
                        runner.run(request.repo_root, PatchCommand::GitApply, request.patch)?;
                    if apply.success {
                        decision.applied = true;
                    } else {
                        decision.reasons.push(reason(
                            "git_apply_failed",
                            "git apply failed after a successful check",
                            None,
                            None,
                            Some(trim_stderr(&apply.stderr)),
                        ));
                        refresh_decision_kind(&mut decision);
                    }
                }
            }
        }
        Err(error) => {
            decision.reasons.push(parse_error_reason(&error));
            refresh_decision_kind(&mut decision);
        }
    }

    write_decision_artifact(request.artifact_dir, &artifact_stem, &decision)?;
    Ok(decision)
}

fn classify_paths(policy: &Policy, paths: &[String], reasons: &mut Vec<PolicyDecisionReason>) {
    let review_path_patterns: Vec<String> = policy
        .requires_human_review
        .iter()
        .filter(|entry| !is_known_review_category_key(entry))
        .cloned()
        .collect();

    for path in paths {
        if let Some(pattern) = matching_pattern(path, &policy.forbidden_paths) {
            push_reason(
                reasons,
                reason(
                    "forbidden_path",
                    "path matches a policy forbidden path",
                    Some(path.clone()),
                    Some(pattern.to_string()),
                    None,
                ),
            );
        }

        if let Some(pattern) = matching_pattern(path, &review_path_patterns) {
            push_reason(
                reasons,
                reason(
                    "policy_requires_human_review",
                    "path matches a policy human-review pattern",
                    Some(path.clone()),
                    Some(pattern.to_string()),
                    None,
                ),
            );
        }

        for category in review_categories(path) {
            if policy_requires_category(policy, category.policy_key) {
                push_reason(
                    reasons,
                    reason(
                        category.code,
                        category.message,
                        Some(path.clone()),
                        Some(category.policy_key.to_string()),
                        None,
                    ),
                );
            }
        }
    }
}

fn review_categories(path: &str) -> Vec<ReviewCategory> {
    let mut categories = Vec::new();
    let lower = path.to_ascii_lowercase();
    let file_name = lower.rsplit('/').next().unwrap_or(&lower);
    let components: Vec<&str> = lower.split('/').collect();

    if matches!(
        file_name,
        "cargo.toml"
            | "cargo.lock"
            | "package.json"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "bun.lockb"
            | "requirements.txt"
            | "pyproject.toml"
            | "poetry.lock"
            | "pipfile"
            | "pipfile.lock"
            | "go.mod"
            | "go.sum"
            | "gemfile"
            | "gemfile.lock"
    ) {
        categories.push(ReviewCategory {
            code: "review_required_dependency",
            message: "dependency or lockfile changes require human review",
            policy_key: "dependency_changes",
        });
    }

    if components
        .iter()
        .any(|component| matches!(*component, "migration" | "migrations"))
    {
        categories.push(ReviewCategory {
            code: "review_required_database_migration",
            message: "database migration changes require human review",
            policy_key: "database_migrations",
        });
    }

    if lower.starts_with(".github/workflows/")
        || file_name == ".gitlab-ci.yml"
        || file_name == "azure-pipelines.yml"
        || components.contains(&".circleci")
        || components.contains(&"ci")
    {
        categories.push(ReviewCategory {
            code: "review_required_ci",
            message: "CI changes require human review",
            policy_key: "ci_changes",
        });
    }

    if components
        .iter()
        .any(|component| matches!(*component, "eval" | "evals" | "evaluation" | "evaluations"))
    {
        categories.push(ReviewCategory {
            code: "review_required_eval",
            message: "eval changes require human review",
            policy_key: "eval_changes",
        });
    }

    if components
        .iter()
        .any(|component| matches!(*component, "policy" | "policies" | ".seaf"))
        || file_name.ends_with(".policy.json")
        || lower == "docs/security/forbidden-shortcuts.md"
    {
        categories.push(ReviewCategory {
            code: "review_required_policy",
            message: "policy changes require human review",
            policy_key: "policy_changes",
        });
    }

    if components
        .iter()
        .any(|component| matches!(*component, "update" | "updater" | "updates"))
    {
        categories.push(ReviewCategory {
            code: "review_required_updater",
            message: "updater changes require human review",
            policy_key: "updater_changes",
        });
    }

    if components.contains(&"signing")
        || matches!(file_name, "release-key.pem" | "release-key.key")
        || file_name.ends_with(".pem")
        || file_name.ends_with(".key")
        || file_name.ends_with(".p12")
    {
        categories.push(ReviewCategory {
            code: "review_required_signing",
            message: "signing changes require human review",
            policy_key: "signing_changes",
        });
    }

    if components
        .iter()
        .any(|component| component.contains("auth") || *component == "login")
    {
        categories.push(ReviewCategory {
            code: "review_required_auth",
            message: "auth changes require human review",
            policy_key: "auth_code",
        });
    }

    if components.iter().any(|component| {
        component.contains("billing")
            || component.contains("payment")
            || *component == "payments"
            || *component == "stripe"
    }) {
        categories.push(ReviewCategory {
            code: "review_required_payment",
            message: "billing or payment changes require human review",
            policy_key: "payment_code",
        });
    }

    if components.iter().any(|component| {
        matches!(*component, "privacy" | "pii" | "telemetry") || component.contains("personal")
    }) {
        categories.push(ReviewCategory {
            code: "review_required_privacy",
            message: "privacy-sensitive changes require human review",
            policy_key: "privacy_sensitive_code",
        });
    }

    if lower.contains("network") && lower.contains("permission") {
        categories.push(ReviewCategory {
            code: "review_required_network_permission",
            message: "network permission changes require human review",
            policy_key: "network_permission_changes",
        });
    }

    categories
}

fn policy_requires_category(policy: &Policy, category_key: &str) -> bool {
    policy
        .requires_human_review
        .iter()
        .any(|entry| entry.trim() == category_key)
}

fn is_known_review_category_key(entry: &str) -> bool {
    REVIEW_CATEGORY_KEYS.contains(&entry.trim())
}

fn refresh_decision_kind(decision: &mut PolicyDecision) {
    if decision
        .reasons
        .iter()
        .any(|reason| is_rejection_reason(&reason.code))
    {
        decision.decision = PatchDecisionKind::Rejected;
        decision.requires_human_review = false;
    } else if decision.reasons.iter().any(|reason| {
        reason.code.starts_with("review_required_") || reason.code == "policy_requires_human_review"
    }) {
        decision.decision = PatchDecisionKind::RequiresHumanReview;
        decision.requires_human_review = true;
    } else {
        decision.decision = PatchDecisionKind::Allowed;
        decision.requires_human_review = false;
    }
}

fn is_rejection_reason(code: &str) -> bool {
    matches!(
        code,
        "binary_patch"
            | "forbidden_path"
            | "invalid_patch"
            | "invalid_patch_path"
            | "git_apply_check_failed"
            | "git_apply_failed"
    )
}

fn parse_error_reason(error: &PatchParseError) -> PolicyDecisionReason {
    match error {
        PatchParseError::UnsafePath(path) => reason(
            "invalid_patch_path",
            "patch contains an unsafe path",
            Some(path.clone()),
            None,
            None,
        ),
        _ => reason(
            "invalid_patch",
            "patch could not be parsed as a safe unified diff",
            None,
            None,
            Some(error.to_string()),
        ),
    }
}

fn write_decision_artifact(
    artifact_dir: &Path,
    artifact_stem: &str,
    decision: &PolicyDecision,
) -> Result<(), PatchGateError> {
    let mut json = serde_json::to_vec_pretty(decision).map_err(PatchGateError::Json)?;
    json.push(b'\n');
    fs::write(
        artifact_dir.join(format!("{artifact_stem}.policy-decision.json")),
        json,
    )
    .map_err(PatchGateError::Io)
}

fn reason(
    code: impl Into<String>,
    message: impl Into<String>,
    path: Option<String>,
    pattern: Option<String>,
    details: Option<String>,
) -> PolicyDecisionReason {
    PolicyDecisionReason {
        code: code.into(),
        message: message.into(),
        path,
        pattern,
        details,
    }
}

fn push_reason(reasons: &mut Vec<PolicyDecisionReason>, reason: PolicyDecisionReason) {
    if !reasons.iter().any(|existing| existing == &reason) {
        reasons.push(reason);
    }
}

pub fn patch_digest(patch: &str) -> String {
    let digest = Sha256::digest(patch.as_bytes());
    format!("sha256:{}", hex::encode(digest))
}

fn safe_artifact_stem(patch_id: &str) -> String {
    let stem: String = patch_id
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(*character, '.' | '_' | '-')
        })
        .collect();
    if stem.is_empty() {
        "patch".to_string()
    } else {
        stem
    }
}

fn trim_stderr(stderr: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        "no stderr".to_string()
    } else {
        stderr.to_string()
    }
}

#[derive(Debug, Clone, Copy)]
struct ReviewCategory {
    code: &'static str,
    message: &'static str,
    policy_key: &'static str,
}

const REVIEW_CATEGORY_KEYS: &[&str] = &[
    "dependency_changes",
    "database_migrations",
    "auth_code",
    "payment_code",
    "privacy_sensitive_code",
    "network_permission_changes",
    "ci_changes",
    "eval_changes",
    "policy_changes",
    "updater_changes",
    "signing_changes",
];

#[derive(Debug)]
pub enum PatchGateError {
    Io(std::io::Error),
    Json(serde_json::Error),
    CommandIo(std::io::Error),
    Command(String),
}

impl fmt::Display for PatchGateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "patch gate I/O error: {error}"),
            Self::Json(error) => write!(formatter, "patch gate JSON error: {error}"),
            Self::CommandIo(error) => write!(formatter, "patch command I/O error: {error}"),
            Self::Command(message) => write!(formatter, "patch command error: {message}"),
        }
    }
}

impl Error for PatchGateError {}
