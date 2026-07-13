use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, LoopInputDigests, LoopStatus, Policy,
    TicketAutonomy, TicketContext, TicketPriority, TicketStatus,
};
use seaf_models::{FakeProvider, ModelResponse};

use super::*;
use crate::{
    policy_gate::{
        CommandOutput, GitCommandRunner, PatchCommand, PatchCommandRunner, PatchGateError,
    },
    runner::{AuthoritativeRunInputSnapshots, InitializedLoopRun, LoopRunner, LoopRunnerConfig},
    verify_candidate_patch_evidence,
};

#[derive(Debug, PartialEq, Eq)]
enum RepositoryEntry {
    File(Vec<u8>),
    Symlink(PathBuf),
}

#[derive(Debug, PartialEq, Eq)]
struct RepositorySnapshot {
    head: Vec<u8>,
    status: Vec<u8>,
    staged_diff: Vec<u8>,
    unstaged_diff: Vec<u8>,
    entries: BTreeMap<PathBuf, RepositoryEntry>,
}

#[test]
fn output_review_response_cut_adopts_without_provider_replay_or_source_mutation() {
    let temp = tempfile::tempdir().expect("temp directory");
    let runs_root = temp.path().join("runs");
    let source = temp.path().join("source");
    fs::create_dir(&source).expect("source directory");
    git_ok(&source, &["init", "-q"]);
    git_ok(&source, &["config", "user.email", "test@example.com"]);
    git_ok(&source, &["config", "user.name", "SEAF Test"]);
    fs::create_dir(source.join("src")).expect("source tree");
    fs::write(source.join("src/lib.rs"), "pub fn existing() {}\n").expect("source file");
    git_ok(&source, &["add", "."]);
    git_ok(&source, &["commit", "-qm", "initial"]);

    let ticket = ticket();
    let policy = policy();
    let config = serde_json::json!({"policy_path":"seaf.policy.json"});
    let repository = serde_json::json!({"source":source.canonicalize().unwrap()});
    let eval_config = seaf_core::parse_eval_config(
        "evals:\n  allow_commands: []\n  required:\n    - name: tests\n      command: cargo test\n",
    )
    .expect("eval config");
    let snapshots = AuthoritativeRunInputSnapshots {
        ticket: canonical_json_bytes(&ticket).unwrap(),
        provider_ticket: canonical_json_bytes(&ticket).unwrap(),
        policy: canonical_json_bytes(&policy).unwrap(),
        config: canonical_json_bytes(&config).unwrap(),
        repository: canonical_json_bytes(&repository).unwrap(),
        eval_config: canonical_json_bytes(&eval_config).unwrap(),
    };
    let initialized = InitializedLoopRun::create_isolated(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "output-review-response-cut",
            &ticket,
            "fake",
            "fake-model",
            LoopInputDigests {
                ticket: canonical_sha256_digest(&ticket).unwrap(),
                policy: canonical_sha256_digest(&policy).unwrap(),
                config: canonical_sha256_digest(&config).unwrap(),
                repository: canonical_sha256_digest(&repository).unwrap(),
                eval_config: Some(canonical_sha256_digest(&eval_config).unwrap()),
            },
        ),
        &source,
        &snapshots,
    )
    .expect("initialize isolated run");
    let candidate = PathBuf::from(
        &initialized
            .run()
            .candidate_workspace
            .as_ref()
            .expect("candidate authority")
            .path,
    );
    let prepared = initialized
        .scaffold()
        .expect("scaffold")
        .publish_authoritative_inputs(snapshots.clone())
        .expect("authoritative inputs");

    let provider = FakeProvider::new(provider_responses());
    let cut_seen = std::cell::Cell::new(false);
    let observer = |_workspace: &LoopWorkspace,
                    durable: &LoopRun,
                    coordinates: &ProviderExchangeCoordinates| {
        if coordinates.step == LoopStepName::OutputReview {
            assert_eq!(
                durable
                    .provider_exchange_records
                    .last()
                    .expect("durable response reference")
                    .phase,
                ProviderExchangePhase::Response
            );
            cut_seen.set(true);
            panic!("injected interruption after durable OutputReview response");
        }
    };
    let mut patch_runner = RecordingGitCommandRunner::default();
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&candidate, &ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(&candidate, &ticket, policy.clone(), true),
            &mut patch_runner,
        )
        .with_after_response_persist_observer(&observer);
    let mut loop_runner =
        LoopRunner::start_initialized(prepared, &mut step_runner).expect("start loop");
    for _ in 0..5 {
        assert!(loop_runner.run_next_step().expect("through Development"));
    }
    let workspace = LoopWorkspace::open(&runs_root, "output-review-response-cut").unwrap();
    let exact_subject = verify_candidate_patch_evidence(&workspace, &source).unwrap();
    let source_before = repository_snapshot(&source);
    let candidate_before = repository_snapshot(&candidate);
    let earlier_files = run_files(workspace.run_directory())
        .into_iter()
        .filter(|(path, _)| path != Path::new("run.json") && path != Path::new("log.md"))
        .collect::<BTreeMap<_, _>>();

    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = loop_runner.run_next_step();
    }));
    assert!(interrupted.is_err(), "the response-persist cut must interrupt");
    assert!(cut_seen.get());
    drop(loop_runner);
    drop(step_runner);
    assert_eq!(
        repository_snapshot(&source),
        source_before,
        "the durable response cut must leave the complete source repository unchanged before resume"
    );
    assert_eq!(
        repository_snapshot(&candidate),
        candidate_before,
        "the durable response cut must leave the complete candidate repository unchanged before resume"
    );
    assert_eq!(provider.requests().unwrap().len(), 6);
    assert_eq!(patch_runner.calls, vec![PatchCommand::GitApplyCheck]);

    let interrupted = crate::state::load_run(&workspace).expect("interrupted run");
    let output_review = interrupted
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::OutputReview)
        .expect("OutputReview step");
    assert_eq!(output_review.status, LoopStepStatus::Running);
    assert!(output_review.artifact_path.is_none());
    assert!(output_review.artifact_digest.is_none());
    assert_eq!(
        interrupted
            .provider_exchange_records
            .iter()
            .filter(|record| record.step == LoopStepName::OutputReview)
            .count(),
        2
    );
    assert!(!workspace
        .run_directory()
        .join("artifacts/06-output-review.json")
        .exists());

    let resumed_initialized = InitializedLoopRun::resume_isolated_with_inputs(
        &runs_root,
        interrupted,
        &snapshots,
    )
    .expect("resume isolated authority with retained inputs");
    let resumed_prepared = resumed_initialized
        .scaffold()
        .expect("resume scaffold")
        .publish_authoritative_inputs(snapshots)
        .expect("resume authoritative input retry");
    let resume_provider = FakeProvider::new(Vec::new());
    let mut resume_patch_runner = RecordingGitCommandRunner::default();
    let mut resume_step_runner = ProviderStepRunner::new(&resume_provider, "fake-model", 30_000)
        .with_ticket(ticket.clone())
        .with_context_pack_request(context_request(&candidate, &ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(&candidate, &ticket, policy, true),
            &mut resume_patch_runner,
        );
    let mut resumed = LoopRunner::resume_initialized(resumed_prepared, &mut resume_step_runner)
        .expect("resume durable OutputReview response");
    assert!(resumed
        .run_next_step()
        .expect("adopt exact OutputReview response"));
    assert_eq!(resumed.run().status, LoopStatus::AwaitingHumanReview);
    drop(resumed);
    drop(resume_step_runner);

    assert!(resume_provider.requests().unwrap().is_empty());
    assert!(
        resume_patch_runner.calls.is_empty(),
        "OutputReview adoption must not rerun patch policy/apply commands"
    );
    let completed = crate::state::load_run(&workspace).expect("completed review");
    let review_records = completed
        .provider_exchange_records
        .iter()
        .filter(|record| record.step == LoopStepName::OutputReview)
        .collect::<Vec<_>>();
    assert_eq!(review_records.len(), 2);
    assert_eq!(review_records[0].phase, ProviderExchangePhase::Request);
    assert_eq!(review_records[1].phase, ProviderExchangePhase::Response);
    let output_review = completed
        .steps
        .iter()
        .find(|step| step.name == LoopStepName::OutputReview)
        .expect("completed OutputReview");
    assert_eq!(output_review.status, LoopStepStatus::Passed);
    let review_artifact = output_review.artifact_path.as_ref().expect("review artifact");
    let completed_files = run_files(workspace.run_directory());
    assert_eq!(
        completed_files
            .keys()
            .filter(|path| {
                path.parent() == Some(Path::new("artifacts"))
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name == "06-output-review.json"
                                || (name.starts_with("06-output-review.attempt-")
                                    && !name.contains(".exchange-")
                                    && !name.contains(".rerun-authorization"))
                        })
            })
            .count(),
        1,
        "resume must publish exactly one OutputReview role artifact"
    );
    assert!(completed_files.contains_key(Path::new(review_artifact)));

    let request_record = load_provider_exchange_record(workspace.run_directory(), review_records[0])
        .expect("request record");
    let request = load_provider_exchange_request(workspace.run_directory(), &request_record.request)
        .expect("request audit");
    let request: ModelRequest = serde_json::from_slice(&request).expect("typed request");
    let role_input: serde_json::Value =
        serde_json::from_str(&request.messages[0].content).expect("role input");
    assert_eq!(
        serde_json::from_value::<crate::VerifiedCandidatePatchEvidence>(
            role_input["verified_candidate_patch"].clone(),
        )
        .expect("verified candidate subject"),
        exact_subject
    );
    for (path, bytes) in earlier_files {
        assert_eq!(completed_files.get(&path), Some(&bytes), "{}", path.display());
    }
    assert_eq!(repository_snapshot(&source), source_before);
    assert_eq!(repository_snapshot(&candidate), candidate_before);

    git_ok(&source, &["worktree", "remove", "--force", candidate.to_str().unwrap()]);
}

#[derive(Default)]
struct RecordingGitCommandRunner {
    calls: Vec<PatchCommand>,
}

impl PatchCommandRunner for RecordingGitCommandRunner {
    fn run(
        &mut self,
        repo_root: &Path,
        command: PatchCommand,
        patch: &str,
    ) -> Result<CommandOutput, PatchGateError> {
        self.calls.push(command);
        GitCommandRunner.run(repo_root, command, patch)
    }
}

fn ticket() -> TicketSpec {
    TicketSpec {
        ticket_id: "T-M1-12".to_string(),
        goal_id: "milestone-one".to_string(),
        title: "Recover OutputReview response".to_string(),
        status: TicketStatus::Ready,
        priority: TicketPriority::P1,
        problem: "A durable review response must not be requested twice.".to_string(),
        research_questions: Vec::new(),
        context: TicketContext {
            relevant_files: vec!["src/lib.rs".to_string()],
            forbidden_files: Vec::new(),
        },
        autonomy: TicketAutonomy {
            level: 1,
            apply_patch: true,
            allow_shell_commands: Vec::new(),
        },
        acceptance_criteria: vec!["Resume the durable exact response.".to_string()],
        eval: None,
    }
}

fn policy() -> Policy {
    Policy {
        policy_id: "test-policy".to_string(),
        default_autonomy_level: 1,
        forbidden_paths: vec!["secrets/**".to_string()],
        requires_human_review: vec!["dependency_changes".to_string()],
        allowed_without_review: vec!["source_changes".to_string()],
    }
}

fn context_request(repository_root: &Path, ticket: &TicketSpec) -> ContextPackRequest {
    ContextPackRequest::for_ticket(
        repository_root,
        Path::new("unused"),
        ticket,
        &[],
        ContextLimits {
            max_bytes_per_file: 4096,
            max_total_bytes: 8192,
        },
    )
}

fn provider_responses() -> Vec<Result<ModelResponse, seaf_models::ModelError>> {
    [
        include_str!("../../../../fixtures/model-responses/research.valid.json").to_string(),
        include_str!("../../../../fixtures/model-responses/analyzer.valid.json").to_string(),
        serde_json::json!({
            "role": "spec_writer",
            "status": "passed",
            "summary": "Add the requested file.",
            "findings": [],
            "risks": [],
            "next_step_recommendation": "Review the narrow implementation."
        })
        .to_string(),
        serde_json::json!({
            "role": "spec_reviewer",
            "decision": "approve_spec",
            "summary": "The spec is narrow and testable.",
            "blocking_issues": [],
            "non_blocking_issues": []
        })
        .to_string(),
        serde_json::json!({
            "role": "developer",
            "status": "patch_proposed",
            "summary": "Add one source file.",
            "changed_files": ["src/new.rs"],
            "requires_human_review": false,
            "patch": "diff --git a/src/new.rs b/src/new.rs\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/src/new.rs\n@@ -0,0 +1 @@\n+pub fn added() {}\n"
        })
        .to_string(),
        serde_json::json!({
            "role": "output_reviewer",
            "decision": "approve_for_tests",
            "summary": "The applied candidate matches the approved spec.",
            "blocking_issues": [],
            "non_blocking_issues": []
        })
        .to_string(),
    ]
    .into_iter()
    .map(|content| {
        Ok(ModelResponse {
            content,
            latency_ms: 1,
            raw_provider_metadata: serde_json::Value::Null,
        })
    })
    .collect()
}

fn repository_snapshot(root: &Path) -> RepositorySnapshot {
    RepositorySnapshot {
        head: git_output(root, &["rev-parse", "HEAD"]),
        status: git_output(root, &["status", "--porcelain=v1", "-z"]),
        staged_diff: git_output(root, &["diff", "--binary", "--cached", "--no-ext-diff"]),
        unstaged_diff: git_output(root, &["diff", "--binary", "--no-ext-diff"]),
        entries: repository_entries(root),
    }
}

fn repository_entries(root: &Path) -> BTreeMap<PathBuf, RepositoryEntry> {
    fn visit(root: &Path, directory: &Path, entries: &mut BTreeMap<PathBuf, RepositoryEntry>) {
        let mut children = fs::read_dir(directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            if child.file_name() == ".git" {
                continue;
            }
            let path = child.path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.file_type().is_symlink() {
                entries.insert(relative, RepositoryEntry::Symlink(fs::read_link(path).unwrap()));
            } else if metadata.is_dir() {
                visit(root, &path, entries);
            } else if metadata.is_file() {
                entries.insert(relative, RepositoryEntry::File(fs::read(path).unwrap()));
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn run_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let mut children = fs::read_dir(directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let path = child.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                visit(root, &path, files);
            } else if metadata.is_file() {
                files.insert(path.strip_prefix(root).unwrap().to_path_buf(), fs::read(path).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn git_ok(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git command");
    assert!(output.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&output.stderr));
}

fn git_output(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git command");
    assert!(output.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&output.stderr));
    output.stdout
}
