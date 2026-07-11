use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, LoopInputDigests, Policy, TicketAutonomy,
    TicketContext, TicketPriority, TicketSpec, TicketStatus,
};
use seaf_loop::{
    AuthoritativeRunInputSnapshots, CommandOutput, ContextLimits, ContextPackRequest,
    InitializedLoopRun, LoopRunner, LoopRunnerConfig, PatchCommand, PatchCommandRunner,
    PatchGateError, PreparedLoopRun, ProviderPatchGateConfig, ProviderStepRunner,
};
use seaf_models::{FakeProvider, ModelResponse};

#[test]
fn provider_rejects_context_and_patch_roots_that_are_not_the_candidate() {
    for wrong_context in [true, false] {
        let fixture = fixture(if wrong_context {
            "wrong-context"
        } else {
            "wrong-patch"
        });
        let candidate = fixture.candidate.clone();
        let context_root = if wrong_context {
            &fixture.source
        } else {
            &candidate
        };
        let patch_root = if wrong_context {
            &candidate
        } else {
            &fixture.source
        };
        let provider = FakeProvider::new(Vec::new());
        let mut patch_runner = RecordingPatchRunner::default();
        let mut runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
            .with_ticket(fixture.ticket.clone())
            .with_context_pack_request(context_request(context_root, &fixture.ticket))
            .with_patch_gate(
                ProviderPatchGateConfig::for_ticket(
                    patch_root,
                    &fixture.ticket,
                    fixture.policy.clone(),
                    true,
                ),
                &mut patch_runner,
            );
        let error = LoopRunner::start_initialized(fixture.prepared, &mut runner)
            .expect_err("each configured root must independently equal candidate");
        assert!(error.to_string().contains("must both equal"), "{error}");
        assert!(provider.requests().unwrap().is_empty());
        remove_candidate(&fixture.source, &candidate);
    }
}

#[test]
fn apply_requested_patch_is_checked_in_candidate_without_mutating_either_checkout() {
    let fixture = fixture("candidate-patch-check");
    let source_before = source_evidence(&fixture.source);
    let candidate_before = source_evidence(&fixture.candidate);
    let responses = vec![
        response(include_str!(
            "../../../fixtures/model-responses/research.valid.json"
        )),
        response(include_str!(
            "../../../fixtures/model-responses/analyzer.valid.json"
        )),
        response(include_str!(
            "../../../fixtures/model-responses/spec_writer.valid.json"
        )),
        response(include_str!(
            "../../../fixtures/model-responses/spec_reviewer.valid.json"
        )),
        response(
            r#"{"role":"developer","status":"patch_proposed","summary":"Add file","changed_files":["src/new.rs"],"requires_human_review":false,"patch":"diff --git a/src/new.rs b/src/new.rs\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/src/new.rs\n@@ -0,0 +1 @@\n+pub fn added() {}\n"}"#,
        ),
    ];
    let provider = FakeProvider::new(responses);
    let mut patch_runner = RecordingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(fixture.ticket.clone())
        .with_context_pack_request(context_request(&fixture.candidate, &fixture.ticket))
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(
                &fixture.candidate,
                &fixture.ticket,
                fixture.policy.clone(),
                true,
            ),
            &mut patch_runner,
        );
    let mut loop_runner =
        LoopRunner::start_initialized(fixture.prepared, &mut step_runner).expect("start");
    for _ in 0..5 {
        assert!(loop_runner.run_next_step().expect("provider step"));
    }
    let completed = loop_runner.run().clone();
    drop(loop_runner);
    drop(step_runner);
    assert_eq!(patch_runner.calls.len(), 1);
    assert_eq!(
        patch_runner.calls[0].0,
        fixture.candidate.canonicalize().unwrap()
    );
    assert_eq!(patch_runner.calls[0].1, PatchCommand::GitApplyCheck);
    let decision = &completed.policy_decisions[0];
    assert_eq!(decision.get("apply_requested").unwrap(), true);
    assert_eq!(decision.get("applied").unwrap(), false);
    assert_eq!(source_evidence(&fixture.source), source_before);
    assert_eq!(source_evidence(&fixture.candidate), candidate_before);
    remove_candidate(&fixture.source, &fixture.candidate);
}

struct Fixture {
    _temp: tempfile::TempDir,
    source: PathBuf,
    candidate: PathBuf,
    ticket: TicketSpec,
    policy: Policy,
    prepared: PreparedLoopRun,
}

fn fixture(run_id: &str) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    git_ok(&source, &["init", "-q"]);
    git_ok(&source, &["config", "user.email", "test@example.com"]);
    git_ok(&source, &["config", "user.name", "SEAF Test"]);
    fs::create_dir(source.join("src")).unwrap();
    fs::write(source.join("src/lib.rs"), "pub fn existing() {}\n").unwrap();
    git_ok(&source, &["add", "."]);
    git_ok(&source, &["commit", "-qm", "initial"]);
    let ticket = ticket();
    let policy = policy();
    let config = serde_json::json!({"policy_path":"seaf.policy.json"});
    let repository = serde_json::json!({"source":source.canonicalize().unwrap()});
    let ticket_bytes = canonical_json_bytes(&ticket).unwrap();
    let policy_bytes = canonical_json_bytes(&policy).unwrap();
    let config_bytes = canonical_json_bytes(&config).unwrap();
    let repository_bytes = canonical_json_bytes(&repository).unwrap();
    let initialized = InitializedLoopRun::create_isolated(
        LoopRunnerConfig::for_ticket(
            temp.path().join("runs"),
            run_id,
            &ticket,
            "fake",
            "fake-model",
            LoopInputDigests {
                ticket: canonical_sha256_digest(&ticket).unwrap(),
                policy: canonical_sha256_digest(&policy).unwrap(),
                config: canonical_sha256_digest(&config).unwrap(),
                repository: canonical_sha256_digest(&repository).unwrap(),
            },
        ),
        &source,
    )
    .unwrap();
    let candidate = PathBuf::from(&initialized.run().candidate_workspace.as_ref().unwrap().path);
    let prepared = initialized
        .scaffold()
        .unwrap()
        .publish_authoritative_inputs(AuthoritativeRunInputSnapshots {
            ticket: ticket_bytes.clone(),
            provider_ticket: ticket_bytes,
            policy: policy_bytes,
            config: config_bytes,
            repository: repository_bytes,
        })
        .unwrap();
    Fixture {
        _temp: temp,
        source,
        candidate,
        ticket,
        policy,
        prepared,
    }
}

fn ticket() -> TicketSpec {
    TicketSpec {
        ticket_id: "T-CANDIDATE".into(),
        goal_id: "production-use".into(),
        title: "Check candidate patch".into(),
        status: TicketStatus::Ready,
        priority: TicketPriority::P1,
        problem: "Patch must stay isolated.".into(),
        research_questions: vec![],
        context: TicketContext {
            relevant_files: vec!["src/lib.rs".into()],
            forbidden_files: vec![],
        },
        autonomy: TicketAutonomy {
            level: 1,
            apply_patch: true,
            allow_shell_commands: vec![],
        },
        acceptance_criteria: vec!["Source remains unchanged.".into()],
        eval: None,
    }
}

fn policy() -> Policy {
    Policy {
        policy_id: "test".into(),
        default_autonomy_level: 1,
        forbidden_paths: vec!["secrets/**".into()],
        requires_human_review: vec!["dependency_changes".into()],
        allowed_without_review: vec!["source_changes".into()],
    }
}

fn context_request(root: &Path, ticket: &TicketSpec) -> ContextPackRequest {
    ContextPackRequest::for_ticket(
        root,
        Path::new("unused"),
        ticket,
        &[],
        ContextLimits {
            max_bytes_per_file: 4096,
            max_total_bytes: 8192,
        },
    )
}

#[derive(Default)]
struct RecordingPatchRunner {
    calls: Vec<(PathBuf, PatchCommand)>,
}
impl PatchCommandRunner for RecordingPatchRunner {
    fn run(
        &mut self,
        root: &Path,
        command: PatchCommand,
        _patch: &str,
    ) -> Result<CommandOutput, PatchGateError> {
        self.calls.push((root.canonicalize().unwrap(), command));
        Ok(CommandOutput::success())
    }
}

fn response(content: &str) -> Result<ModelResponse, seaf_models::ModelError> {
    Ok(ModelResponse {
        content: content.to_string(),
        latency_ms: 1,
        raw_provider_metadata: serde_json::Value::Null,
    })
}
fn source_evidence(root: &Path) -> (String, String, Vec<u8>) {
    (
        git(root, &["rev-parse", "HEAD"]),
        git(root, &["status", "--porcelain=v1"]),
        fs::read(root.join("src/lib.rs")).unwrap(),
    )
}
fn remove_candidate(source: &Path, candidate: &Path) {
    git_ok(
        source,
        &["worktree", "remove", "--force", candidate.to_str().unwrap()],
    );
}
fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}
fn git_ok(root: &Path, args: &[&str]) {
    let _ = git(root, args);
}
