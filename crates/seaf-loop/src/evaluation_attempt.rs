use std::{collections::BTreeMap, fs};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, ArtifactReference, EvalCheck, EvalCommandConfig,
    LoopInputDigests, LoopRun, RecoveryReference,
};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::{immutable_artifact::read_verified_regular_file, LoopWorkspace};

pub(crate) const FIXED_INTENT_PATH: &str = "artifacts/07-testing.execution-intent.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationAttemptPaths {
    pub intent: String,
    pub testing: String,
    pub report: String,
}

impl EvaluationAttemptPaths {
    pub fn indexed(attempt: u32) -> Result<Self, String> {
        if attempt == 0 {
            return Err("evaluation attempt must be positive".to_string());
        }
        Ok(Self {
            intent: format!("artifacts/07-testing.attempt-{attempt:03}.execution-intent.json"),
            testing: format!("artifacts/07-testing.attempt-{attempt:03}.json"),
            report: format!("artifacts/08-eval-report.attempt-{attempt:03}.json"),
        })
    }

    pub fn stdout(&self, check: usize) -> String {
        self.testing
            .replace(".json", &format!(".check-{check:03}.stdout.log"))
    }

    pub fn stderr(&self, check: usize) -> String {
        self.testing
            .replace(".json", &format!(".check-{check:03}.stderr.log"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spelling {
    Fixed,
    Indexed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Intent,
    Testing,
    Report,
    Stdout(u32),
    Stderr(u32),
}

#[derive(Debug, Default)]
struct AttemptInventory {
    spelling: Option<Spelling>,
    intent: Option<String>,
    testing: Option<String>,
    report: Option<String>,
    logs: BTreeMap<u32, (Option<String>, Option<String>)>,
}

#[derive(Debug)]
pub(crate) struct EvaluationAttemptInventory {
    attempts: BTreeMap<u32, AttemptInventory>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationRecoveryPrefixPaths {
    pub attempt: u32,
    pub spelling: Spelling,
    pub intent: String,
    pub testing: String,
    pub report: String,
    pub report_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationInvalidationPrefixPaths {
    pub attempt: u32,
    pub spelling: Spelling,
    pub paths: Vec<String>,
    pub testing_present: bool,
    pub report_present: bool,
    pub complete_log_pairs: u32,
    pub trailing_stdout: bool,
}

impl EvaluationAttemptInventory {
    pub fn load(workspace: &LoopWorkspace) -> Result<Self, String> {
        Self::load_with_mode(workspace, false)
    }

    pub(crate) fn load_for_invalidation(workspace: &LoopWorkspace) -> Result<Self, String> {
        Self::load_with_mode(workspace, true)
    }

    fn load_with_mode(
        workspace: &LoopWorkspace,
        allow_recovered_attempts_and_crash_prefixes: bool,
    ) -> Result<Self, String> {
        let mut attempts: BTreeMap<u32, AttemptInventory> = BTreeMap::new();
        for entry in fs::read_dir(workspace.run_directory().join("artifacts"))
            .map_err(|error| error.to_string())?
        {
            let entry = entry.map_err(|error| error.to_string())?;
            let raw = entry.file_name();
            let lossy = raw.to_string_lossy();
            let evaluation_name =
                lossy.starts_with("07-testing") || lossy.starts_with("08-eval-report");
            if !evaluation_name {
                continue;
            }
            let name = raw
                .into_string()
                .map_err(|_| "evaluation artifact filename is not valid UTF-8".to_string())?;
            let (attempt, spelling, kind) = parse_name(&name).ok_or_else(|| {
                format!("evaluation artifact filename is malformed or noncanonical: {name}")
            })?;
            if attempt != 1 && !allow_recovered_attempts_and_crash_prefixes {
                return Err(
                    "evaluation attempt is unauthorized before M1-09c3 recovery".to_string()
                );
            }
            let file_type = entry.file_type().map_err(|error| error.to_string())?;
            if file_type.is_symlink() || !file_type.is_file() {
                return Err(format!(
                    "evaluation artifact is not a real regular file: {name}"
                ));
            }
            let relative = format!("artifacts/{name}");
            let slot = attempts.entry(attempt).or_default();
            if slot.spelling.is_some_and(|current| current != spelling) {
                return Err(format!(
                    "mixed fixed and indexed evaluation attempt {attempt} authority"
                ));
            }
            slot.spelling = Some(spelling);
            let target = match kind {
                Kind::Intent => &mut slot.intent,
                Kind::Testing => &mut slot.testing,
                Kind::Report => &mut slot.report,
                Kind::Stdout(check) => &mut slot.logs.entry(check).or_default().0,
                Kind::Stderr(check) => &mut slot.logs.entry(check).or_default().1,
            };
            if target.replace(relative).is_some() {
                return Err(format!("duplicate evaluation artifact slot: {name}"));
            }
        }
        let mut expected = 1_u32;
        for (attempt, inventory) in &attempts {
            if *attempt != expected {
                return Err("evaluation attempt inventory contains a gap or future attempt".into());
            }
            if (inventory.testing.is_some()
                || inventory.report.is_some()
                || !inventory.logs.is_empty())
                && inventory.intent.is_none()
            {
                return Err("evaluation attempt contains artifacts without intent".into());
            }
            if inventory.report.is_some() && inventory.testing.is_none() {
                return Err(
                    "evaluation attempt contains EvalReport without Testing evidence".into(),
                );
            }
            let mut expected_check = 1_u32;
            for (check, (stdout, stderr)) in &inventory.logs {
                let trailing_stdout = allow_recovered_attempts_and_crash_prefixes
                    && *check == inventory.logs.keys().next_back().copied().unwrap_or(0)
                    && stdout.is_some()
                    && stderr.is_none()
                    && inventory.testing.is_none()
                    && inventory.report.is_none();
                if *check != expected_check
                    || stdout.is_none()
                    || (stderr.is_none() && !trailing_stdout)
                {
                    return Err(
                        "evaluation logs are unpaired or have noncontiguous check numbering".into(),
                    );
                }
                expected_check = expected_check
                    .checked_add(1)
                    .ok_or_else(|| "evaluation check sequence is exhausted".to_string())?;
            }
            expected = expected
                .checked_add(1)
                .ok_or_else(|| "evaluation attempt sequence is exhausted".to_string())?;
        }
        Ok(Self { attempts })
    }

    pub fn is_empty(&self) -> bool {
        self.attempts.is_empty()
    }

    pub fn require_selected(
        &self,
        attempt: u32,
        testing: &str,
        report: &str,
    ) -> Result<(), String> {
        let selected = self
            .attempts
            .get(&attempt)
            .ok_or_else(|| "selected evaluation attempt is absent".to_string())?;
        if self.attempts.keys().next_back().copied() != Some(attempt)
            || selected.testing.as_deref() != Some(testing)
            || selected.report.as_deref() != Some(report)
            || selected.intent.is_none()
        {
            return Err(
                "final evaluation references do not select one exact latest attempt".into(),
            );
        }
        Ok(())
    }

    pub fn intent_path(&self, attempt: u32) -> Option<&str> {
        self.attempts.get(&attempt)?.intent.as_deref()
    }

    pub(crate) fn recovery_prefix_paths(&self) -> Result<EvaluationRecoveryPrefixPaths, String> {
        let (&attempt, selected) = self
            .attempts
            .iter()
            .next_back()
            .ok_or_else(|| "adoption requires an exact evaluation attempt".to_string())?;
        let spelling = selected
            .spelling
            .ok_or_else(|| "adoption evaluation attempt lost path spelling".to_string())?;
        let intent = selected
            .intent
            .clone()
            .ok_or_else(|| "adoption evaluation prefix lost execution intent".to_string())?;
        let testing = selected
            .testing
            .clone()
            .ok_or_else(|| "adoption evaluation prefix lost Testing evidence".to_string())?;
        let report = match spelling {
            Spelling::Fixed => "artifacts/08-eval-report.json".to_string(),
            Spelling::Indexed => EvaluationAttemptPaths::indexed(attempt)?.report,
        };
        if selected.report.as_ref().is_some_and(|path| path != &report) {
            return Err("adoption evaluation prefix selects a noncanonical EvalReport".into());
        }
        Ok(EvaluationRecoveryPrefixPaths {
            attempt,
            spelling,
            intent,
            testing,
            report,
            report_present: selected.report.is_some(),
        })
    }

    pub(crate) fn invalidation_prefix_paths(
        &self,
    ) -> Result<EvaluationInvalidationPrefixPaths, String> {
        let attempt = self
            .attempts
            .keys()
            .next_back()
            .copied()
            .ok_or_else(|| "invalidation requires a factual evaluation prefix".to_string())?;
        self.invalidation_prefix_paths_for(attempt)
    }

    pub(crate) fn invalidation_prefix_paths_for(
        &self,
        attempt: u32,
    ) -> Result<EvaluationInvalidationPrefixPaths, String> {
        let selected = self
            .attempts
            .get(&attempt)
            .ok_or_else(|| "invalidation evaluation attempt is absent".to_string())?;
        let spelling = selected
            .spelling
            .ok_or_else(|| "invalidation evaluation attempt lost path spelling".to_string())?;
        let intent = selected
            .intent
            .clone()
            .ok_or_else(|| "invalidation evaluation prefix lost execution intent".to_string())?;
        let mut paths = vec![intent];
        let mut complete_log_pairs = 0_u32;
        let mut trailing_stdout = false;
        for (check, (stdout, stderr)) in &selected.logs {
            let stdout = stdout
                .clone()
                .ok_or_else(|| "invalidation evaluation prefix lost stdout log".to_string())?;
            paths.push(stdout);
            if let Some(stderr) = stderr.clone() {
                paths.push(stderr);
                complete_log_pairs = *check;
            } else {
                trailing_stdout = true;
            }
        }
        if let Some(testing) = selected.testing.clone() {
            paths.push(testing);
        }
        if let Some(report) = selected.report.clone() {
            paths.push(report);
        }
        Ok(EvaluationInvalidationPrefixPaths {
            attempt,
            spelling,
            paths,
            testing_present: selected.testing.is_some(),
            report_present: selected.report.is_some(),
            complete_log_pairs,
            trailing_stdout,
        })
    }

    pub fn validate_selected_logs(&self, attempt: u32, checks: &[EvalCheck]) -> Result<(), String> {
        let selected = self
            .attempts
            .get(&attempt)
            .ok_or_else(|| "selected evaluation attempt is absent".to_string())?;
        if selected.logs.len() != checks.len() {
            return Err("Testing evidence does not select every and only attempt log".into());
        }
        for (index, check) in checks.iter().enumerate() {
            let number = u32::try_from(index + 1)
                .map_err(|_| "evaluation check sequence is exhausted".to_string())?;
            let Some((stdout, stderr)) = selected.logs.get(&number) else {
                return Err("Testing evidence has a gapped log sequence".into());
            };
            if stdout.as_deref() != check.stdout_path.as_deref()
                || stderr.as_deref() != check.stderr_path.as_deref()
            {
                return Err("Testing evidence contains cross-attempt or surplus log paths".into());
            }
        }
        Ok(())
    }

    pub(crate) fn require_recovery_prefix(
        &self,
        attempt: u32,
        intent: &str,
        testing: &str,
        report: &str,
        allow_missing_report: bool,
        checks: &[EvalCheck],
    ) -> Result<bool, String> {
        let selected = self
            .attempts
            .get(&attempt)
            .ok_or_else(|| "recovery evaluation attempt is absent".to_string())?;
        let report_present = selected.report.as_deref() == Some(report);
        if selected.intent.as_deref() != Some(intent)
            || selected.testing.as_deref() != Some(testing)
            || (!report_present && (!allow_missing_report || selected.report.is_some()))
        {
            return Err("recovery prefix does not select one exact latest attempt".into());
        }
        self.validate_selected_logs(attempt, checks)?;
        Ok(report_present)
    }
}

pub(crate) fn selected_attempt(
    testing_path: &str,
    report_path: &str,
) -> Result<(u32, Spelling), String> {
    let testing_name = testing_path
        .strip_prefix("artifacts/")
        .ok_or_else(|| "Testing artifact path is not canonical".to_string())?;
    let report_name = report_path
        .strip_prefix("artifacts/")
        .ok_or_else(|| "EvalReport artifact path is not canonical".to_string())?;
    let (testing_attempt, testing_spelling, testing_kind) = parse_name(testing_name)
        .ok_or_else(|| "Testing artifact path is not canonical".to_string())?;
    let (report_attempt, report_spelling, report_kind) = parse_name(report_name)
        .ok_or_else(|| "EvalReport artifact path is not canonical".to_string())?;
    if testing_kind != Kind::Testing
        || report_kind != Kind::Report
        || testing_attempt != report_attempt
        || testing_spelling != report_spelling
    {
        return Err("Testing and EvalReport select different evaluation attempts".into());
    }
    Ok((testing_attempt, testing_spelling))
}

pub(crate) fn fixed_spelling(spelling: Spelling) -> bool {
    spelling == Spelling::Fixed
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovedEvaluationIntentV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub approved_run_digest: String,
    pub ticket: ArtifactReference,
    pub eval_config: ArtifactReference,
    pub candidate_diff: ArtifactReference,
    pub planned_checks: Vec<EvalCommandConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovedEvaluationIntentV2 {
    pub schema_version: u32,
    pub evaluation_attempt: u32,
    pub run_id: String,
    pub approved_run_digest: String,
    pub input_digests: LoopInputDigests,
    pub ticket: ArtifactReference,
    pub eval_config: ArtifactReference,
    pub candidate_state_digest: String,
    pub candidate_diff: ArtifactReference,
    pub source_worktree_state_digest: String,
    pub recovery: Option<RecoveryReference>,
    pub planned_checks: Vec<EvalCommandConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApprovedEvaluationIntent {
    V1(Box<ApprovedEvaluationIntentV1>),
    V2(Box<ApprovedEvaluationIntentV2>),
}

impl ApprovedEvaluationIntent {
    pub fn attempt(&self) -> u32 {
        match self {
            Self::V1(_) => 1,
            Self::V2(intent) => intent.evaluation_attempt,
        }
    }

    pub fn planned_checks(&self) -> &[EvalCommandConfig] {
        match self {
            Self::V1(intent) => &intent.planned_checks,
            Self::V2(intent) => &intent.planned_checks,
        }
    }

    pub fn recovery(&self) -> Option<&RecoveryReference> {
        match self {
            Self::V1(_) => None,
            Self::V2(intent) => intent.recovery.as_ref(),
        }
    }

    pub fn validate_against_with_recovery(
        &self,
        approved: &LoopRun,
        planned_checks: &[EvalCommandConfig],
        expected_recovery: Option<&RecoveryReference>,
    ) -> Result<(), String> {
        let approved_digest =
            canonical_sha256_digest(approved).map_err(|error| error.to_string())?;
        let approval = approved
            .human_approval
            .as_ref()
            .ok_or_else(|| "Approved evaluation intent lost human approval".to_string())?;
        let common_valid = |run_id: &str,
                            digest: &str,
                            ticket: &ArtifactReference,
                            eval: &ArtifactReference,
                            candidate: &ArtifactReference,
                            checks: &[EvalCommandConfig]| {
            run_id == approved.run_id
                && digest == approved_digest
                && ticket.path == "inputs/ticket.json"
                && ticket.digest == approved.input_digests.ticket
                && eval.path == "inputs/eval-config.json"
                && Some(&eval.digest) == approved.input_digests.eval_config.as_ref()
                && candidate == &approval.candidate_diff
                && checks == planned_checks
        };
        match self {
            Self::V1(intent)
                if intent.schema_version == 1
                    && expected_recovery.is_none()
                    && common_valid(
                        &intent.run_id,
                        &intent.approved_run_digest,
                        &intent.ticket,
                        &intent.eval_config,
                        &intent.candidate_diff,
                        &intent.planned_checks,
                    ) =>
            {
                Ok(())
            }
            Self::V2(intent)
                if intent.schema_version == 2
                    && intent.evaluation_attempt > 0
                    && intent.input_digests == approved.input_digests
                    && intent.recovery.as_ref() == expected_recovery
                    && approved
                        .candidate_workspace
                        .as_ref()
                        .is_some_and(|candidate| {
                            canonical_sha256_digest(candidate).ok().as_deref()
                                == Some(intent.candidate_state_digest.as_str())
                        })
                    && is_digest(&intent.source_worktree_state_digest)
                    && common_valid(
                        &intent.run_id,
                        &intent.approved_run_digest,
                        &intent.ticket,
                        &intent.eval_config,
                        &intent.candidate_diff,
                        &intent.planned_checks,
                    ) =>
            {
                Ok(())
            }
            _ => Err("Approved evaluation intent bindings do not match exact authority".into()),
        }
    }
}

pub(crate) fn load_intent(
    workspace: &LoopWorkspace,
    reference: &ArtifactReference,
) -> Result<ApprovedEvaluationIntent, String> {
    let bytes = read_verified_regular_file(
        workspace.run_directory(),
        &reference.path,
        "Approved evaluation intent",
    )
    .map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if canonical_json_bytes(&value).map_err(|error| error.to_string())? != bytes
        || canonical_sha256_digest(&value).map_err(|error| error.to_string())? != reference.digest
    {
        return Err("Approved evaluation intent bytes or digest mismatch".into());
    }
    let file_name = reference
        .path
        .strip_prefix("artifacts/")
        .ok_or_else(|| "Approved evaluation intent path is not canonical".to_string())?;
    let parsed = parse_name(file_name)
        .ok_or_else(|| "Approved evaluation intent path is not canonical".to_string())?;
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(1) if reference.path == FIXED_INTENT_PATH => serde_json::from_value(value)
            .map(Box::new)
            .map(ApprovedEvaluationIntent::V1)
            .map_err(|error| error.to_string()),
        Some(2) => {
            if value
                .as_object()
                .is_none_or(|object| !object.contains_key("recovery"))
            {
                return Err(
                    "Approved evaluation intent v2 requires an explicit recovery member".into(),
                );
            }
            let intent: ApprovedEvaluationIntentV2 =
                serde_json::from_value(value).map_err(|error| error.to_string())?;
            if parsed != (intent.evaluation_attempt, Spelling::Indexed, Kind::Intent) {
                return Err(
                    "Approved evaluation intent path does not match its exact attempt".into(),
                );
            }
            Ok(ApprovedEvaluationIntent::V2(Box::new(intent)))
        }
        _ => Err("unsupported Approved evaluation intent schema or path".into()),
    }
}

pub(crate) fn reference_for_path(
    workspace: &LoopWorkspace,
    path: &str,
) -> Result<ArtifactReference, String> {
    let bytes = read_verified_regular_file(workspace.run_directory(), path, "evaluation artifact")
        .map_err(|error| error.to_string())?;
    Ok(ArtifactReference {
        path: path.to_string(),
        digest: format!("{:x}", sha2::Sha256::digest(&bytes)),
    })
}

fn parse_name(name: &str) -> Option<(u32, Spelling, Kind)> {
    match name {
        "07-testing.execution-intent.json" => return Some((1, Spelling::Fixed, Kind::Intent)),
        "07-testing.json" => return Some((1, Spelling::Fixed, Kind::Testing)),
        "08-eval-report.json" => return Some((1, Spelling::Fixed, Kind::Report)),
        _ => {}
    }
    if let Some(rest) = name.strip_prefix("07-testing.check-") {
        let (check, stream) = rest.split_once('.')?;
        let check = canonical_number(check)?;
        let kind = match stream {
            "stdout.log" => Kind::Stdout(check),
            "stderr.log" => Kind::Stderr(check),
            _ => return None,
        };
        return Some((1, Spelling::Fixed, kind));
    }
    if let Some(rest) = name.strip_prefix("07-testing.attempt-") {
        let (attempt, suffix) = rest.split_once('.')?;
        let attempt = canonical_number(attempt)?;
        let kind = if suffix == "execution-intent.json" {
            Kind::Intent
        } else if suffix == "json" {
            Kind::Testing
        } else if let Some(log) = suffix.strip_prefix("check-") {
            let (check, stream) = log.split_once('.')?;
            let check = canonical_number(check)?;
            match stream {
                "stdout.log" => Kind::Stdout(check),
                "stderr.log" => Kind::Stderr(check),
                _ => return None,
            }
        } else {
            return None;
        };
        return Some((attempt, Spelling::Indexed, kind));
    }
    let rest = name.strip_prefix("08-eval-report.attempt-")?;
    let attempt = canonical_number(rest.strip_suffix(".json")?)?;
    Some((attempt, Spelling::Indexed, Kind::Report))
}

fn canonical_number(value: &str) -> Option<u32> {
    if value.len() < 3 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let parsed = value.parse::<u32>().ok()?;
    (parsed > 0 && format!("{parsed:03}") == value).then_some(parsed)
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use seaf_core::{CheckStatus, EvalCheck};

    type InventoryCase<'a> = (&'a str, &'a [(&'a str, &'a [u8])]);

    fn workspace(name: &str) -> (tempfile::TempDir, LoopWorkspace) {
        let temp = tempfile::tempdir().unwrap();
        let workspace = LoopWorkspace::create(&temp.path().join("runs"), name).unwrap();
        (temp, workspace)
    }

    fn write(workspace: &LoopWorkspace, name: &str, bytes: &[u8]) {
        fs::write(
            workspace.run_directory().join("artifacts").join(name),
            bytes,
        )
        .unwrap();
    }

    fn snapshot(workspace: &LoopWorkspace) -> Vec<(String, Vec<u8>)> {
        let mut files = fs::read_dir(workspace.run_directory().join("artifacts"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fs::read(entry.path()).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }

    fn v2_intent(attempt: u32) -> ApprovedEvaluationIntentV2 {
        ApprovedEvaluationIntentV2 {
            schema_version: 2,
            evaluation_attempt: attempt,
            run_id: "intent-attempt-substitution".into(),
            approved_run_digest: "a".repeat(64),
            input_digests: LoopInputDigests {
                ticket: "b".repeat(64),
                policy: "c".repeat(64),
                config: "d".repeat(64),
                repository: "e".repeat(64),
                eval_config: Some("f".repeat(64)),
            },
            ticket: ArtifactReference {
                path: "inputs/ticket.json".into(),
                digest: "b".repeat(64),
            },
            eval_config: ArtifactReference {
                path: "inputs/eval-config.json".into(),
                digest: "f".repeat(64),
            },
            candidate_state_digest: "1".repeat(64),
            candidate_diff: ArtifactReference {
                path: "artifacts/candidate.diff".into(),
                digest: "2".repeat(64),
            },
            source_worktree_state_digest: "3".repeat(64),
            recovery: None,
            planned_checks: Vec::new(),
        }
    }

    #[test]
    fn inventory_rejects_malformed_orphan_gapped_unpaired_and_future_attempts_inertly() {
        let cases: &[InventoryCase<'_>] = &[
            ("malformed", &[("07-testing.attempt-1.json", b"x")]),
            ("without-intent", &[("07-testing.attempt-001.json", b"x")]),
            (
                "unpaired",
                &[
                    ("07-testing.attempt-001.execution-intent.json", b"{}"),
                    ("07-testing.attempt-001.check-001.stdout.log", b"x"),
                ],
            ),
            (
                "gapped-check",
                &[
                    ("07-testing.attempt-001.execution-intent.json", b"{}"),
                    ("07-testing.attempt-001.check-002.stdout.log", b"x"),
                    ("07-testing.attempt-001.check-002.stderr.log", b"x"),
                ],
            ),
            (
                "future-attempt",
                &[("07-testing.attempt-002.execution-intent.json", b"{}")],
            ),
            (
                "contiguous-future-attempt",
                &[
                    ("07-testing.attempt-001.execution-intent.json", b"{}"),
                    ("07-testing.attempt-002.execution-intent.json", b"{}"),
                ],
            ),
        ];
        for (name, files) in cases {
            let (_temp, workspace) = workspace(name);
            for (path, bytes) in *files {
                write(&workspace, path, bytes);
            }
            let before = snapshot(&workspace);
            assert!(
                EvaluationAttemptInventory::load(&workspace).is_err(),
                "{name}"
            );
            assert_eq!(snapshot(&workspace), before, "{name}");
        }
    }

    #[test]
    fn invalidation_inventory_accepts_only_the_factual_trailing_stdout_crash_cut() {
        let (_temp, workspace) = workspace("invalidation-trailing-stdout");
        let intent = canonical_json_bytes(&v2_intent(1)).unwrap();
        write(
            &workspace,
            "07-testing.attempt-001.execution-intent.json",
            &intent,
        );
        write(
            &workspace,
            "07-testing.attempt-001.check-001.stdout.log",
            b"partial stdout",
        );

        assert!(EvaluationAttemptInventory::load(&workspace).is_err());
        let inventory = EvaluationAttemptInventory::load_for_invalidation(&workspace)
            .expect("stdout is published before stderr and is a factual crash cut");
        let prefix = inventory.invalidation_prefix_paths().unwrap();
        assert_eq!(prefix.attempt, 1);
        assert!(prefix.trailing_stdout);
        assert_eq!(prefix.complete_log_pairs, 0);

        fs::remove_file(
            workspace
                .run_directory()
                .join("artifacts/07-testing.attempt-001.check-001.stdout.log"),
        )
        .unwrap();
        write(
            &workspace,
            "07-testing.attempt-001.check-001.stderr.log",
            b"impossible stderr",
        );
        assert!(EvaluationAttemptInventory::load_for_invalidation(&workspace).is_err());
    }

    #[test]
    fn inventory_rejects_surplus_and_cross_attempt_testing_logs() {
        let (_temp, workspace) = workspace("surplus-logs");
        for name in [
            "07-testing.attempt-001.execution-intent.json",
            "07-testing.attempt-001.check-001.stdout.log",
            "07-testing.attempt-001.check-001.stderr.log",
            "07-testing.attempt-001.check-002.stdout.log",
            "07-testing.attempt-001.check-002.stderr.log",
            "07-testing.attempt-001.json",
            "08-eval-report.attempt-001.json",
        ] {
            write(&workspace, name, b"x");
        }
        let inventory = EvaluationAttemptInventory::load(&workspace).unwrap();
        let checks = vec![EvalCheck {
            name: "one".into(),
            status: CheckStatus::Passed,
            duration_ms: Some(1),
            stdout_path: Some("artifacts/07-testing.attempt-001.check-001.stdout.log".into()),
            stdout_digest: Some("a".repeat(64)),
            stderr_path: Some("artifacts/07-testing.attempt-001.check-001.stderr.log".into()),
            stderr_digest: Some("b".repeat(64)),
            summary: None,
        }];
        assert!(inventory.validate_selected_logs(1, &checks).is_err());
    }

    #[test]
    fn recovery_prefix_allows_create_missing_before_and_after_exact_report_publication() {
        let (_temp, workspace) = workspace("create-missing-prefix");
        for name in [
            "07-testing.attempt-001.execution-intent.json",
            "07-testing.attempt-001.check-001.stdout.log",
            "07-testing.attempt-001.check-001.stderr.log",
            "07-testing.attempt-001.json",
        ] {
            write(&workspace, name, b"x");
        }
        let checks = vec![EvalCheck {
            name: "one".into(),
            status: CheckStatus::Passed,
            duration_ms: Some(1),
            stdout_path: Some("artifacts/07-testing.attempt-001.check-001.stdout.log".into()),
            stdout_digest: Some("a".repeat(64)),
            stderr_path: Some("artifacts/07-testing.attempt-001.check-001.stderr.log".into()),
            stderr_digest: Some("b".repeat(64)),
            summary: None,
        }];
        let inventory = EvaluationAttemptInventory::load(&workspace).unwrap();
        assert!(!inventory
            .require_recovery_prefix(
                1,
                "artifacts/07-testing.attempt-001.execution-intent.json",
                "artifacts/07-testing.attempt-001.json",
                "artifacts/08-eval-report.attempt-001.json",
                true,
                &checks,
            )
            .unwrap());
        assert!(inventory
            .require_recovery_prefix(
                1,
                "artifacts/07-testing.attempt-001.execution-intent.json",
                "artifacts/07-testing.attempt-001.json",
                "artifacts/08-eval-report.attempt-001.json",
                false,
                &checks,
            )
            .is_err());

        write(&workspace, "08-eval-report.attempt-001.json", b"report");
        let inventory = EvaluationAttemptInventory::load(&workspace).unwrap();
        assert!(inventory
            .require_recovery_prefix(
                1,
                "artifacts/07-testing.attempt-001.execution-intent.json",
                "artifacts/07-testing.attempt-001.json",
                "artifacts/08-eval-report.attempt-001.json",
                true,
                &checks,
            )
            .unwrap());
    }

    #[test]
    fn v2_intent_path_must_match_its_payload_attempt() {
        let (_temp, workspace) = workspace("intent-attempt-substitution");
        let intent = v2_intent(2);
        let bytes = canonical_json_bytes(&intent).unwrap();
        let path = "artifacts/07-testing.attempt-001.execution-intent.json";
        write(
            &workspace,
            "07-testing.attempt-001.execution-intent.json",
            &bytes,
        );
        let reference = ArtifactReference {
            path: path.into(),
            digest: canonical_sha256_digest(&intent).unwrap(),
        };
        assert!(load_intent(&workspace, &reference).is_err());
    }

    #[test]
    fn v2_intent_requires_explicit_recovery_member() {
        let (_temp, workspace) = workspace("intent-recovery-presence");
        let mut value = serde_json::to_value(v2_intent(1)).unwrap();
        value.as_object_mut().unwrap().remove("recovery");
        let bytes = canonical_json_bytes(&value).unwrap();
        let path = "artifacts/07-testing.attempt-001.execution-intent.json";
        write(
            &workspace,
            "07-testing.attempt-001.execution-intent.json",
            &bytes,
        );
        let reference = ArtifactReference {
            path: path.into(),
            digest: canonical_sha256_digest(&value).unwrap(),
        };

        let error = load_intent(&workspace, &reference)
            .expect_err("omitted recovery member must not alias explicit null");

        assert!(error.contains("explicit recovery member"), "{error}");
    }
}
