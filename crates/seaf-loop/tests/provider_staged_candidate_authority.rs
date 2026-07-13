use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, LoopInputDigests, LoopRun, LoopStepName, Policy,
    ProviderExchangeKind, ProviderExchangeOutcome, ProviderExchangePhase, ProviderExchangeRecord,
    ProviderRole, TicketAutonomy, TicketContext, TicketPriority, TicketSpec, TicketStatus,
};
use seaf_loop::{
    artifacts::write_step_request, pack_live_context, persist_provider_exchange_record_reference,
    stage_provider_exchange_record, write_provider_exchange_request,
    write_provider_exchange_response, AuthoritativeRunInputSnapshots, CandidateContextAuthority,
    CandidateContextAuthorityKind, CommandOutput, ContextBundle, ContextLimits, ContextPackRequest,
    InitializedLoopRun, LoopRunner, LoopRunnerConfig, PatchCommand, PatchCommandRunner,
    PatchGateError, ProviderExchangeCoordinates, ProviderExchangeResponseAudit,
    ProviderPatchGateConfig, ProviderStepRunner, Role, PROVIDER_EXCHANGE_SCHEMA_VERSION,
};
use seaf_models::{FakeProvider, ModelMessage, ModelMessageRole, ModelRequest, ModelResponse};
use sha2::{Digest, Sha256};

#[test]
fn isolated_resume_rejects_staged_initial_history_for_another_candidate_without_mutation() {
    assert_resume_rejected_without_mutation(Fixture::new(HistoryCase::FirstInitialWrong));
}

#[test]
fn isolated_resume_rejects_later_staged_initial_hidden_by_valid_durable_initial() {
    assert_resume_rejected_without_mutation(Fixture::new(
        HistoryCase::ValidResearchThenWrongAnalysis,
    ));
}

fn assert_resume_rejected_without_mutation(mut fixture: Fixture) {
    let run_before = fs::read(fixture.run_directory.join("run.json")).expect("run bytes");
    let tree_before = snapshot_tree(&fixture.run_directory);
    let source_before = git_evidence(&fixture.source);
    let candidate_before = git_evidence(&fixture.candidate);
    let provider = FakeProvider::new(Vec::new());
    let provider_calls_before = provider.requests().expect("provider requests");

    let verified_run: LoopRun = serde_json::from_slice(&run_before).expect("verified run");
    let initialized = InitializedLoopRun::resume_isolated(&fixture.runs_root, verified_run)
        .expect("candidate recovery before provider preparation");
    let prepared = initialized
        .scaffold()
        .expect("idempotent scaffold")
        .publish_authoritative_inputs(fixture.snapshots.clone())
        .expect("idempotent snapshots");
    let context_request = ContextPackRequest::for_ticket(
        &fixture.candidate,
        &fixture.run_directory,
        &fixture.ticket,
        &fixture.policy.forbidden_paths,
        fixture.context_limits,
    );
    let mut patch_runner = RecordingPatchRunner::default();
    let mut step_runner = ProviderStepRunner::new(&provider, "fake-model", 30_000)
        .with_ticket(fixture.ticket.clone())
        .with_context_pack_request(context_request)
        .with_patch_gate(
            ProviderPatchGateConfig::for_ticket(
                &fixture.candidate,
                &fixture.ticket,
                fixture.policy.clone(),
                true,
            ),
            &mut patch_runner,
        );

    let error = LoopRunner::resume_initialized(prepared, &mut step_runner)
        .expect_err("staged history must not substitute candidate authority");

    assert!(
        error
            .to_string()
            .contains("audited initial repository context has no exact candidate authority"),
        "{error}"
    );
    assert_eq!(
        provider.requests().expect("provider requests"),
        provider_calls_before
    );
    assert!(patch_runner.calls.is_empty());
    assert_eq!(
        fs::read(fixture.run_directory.join("run.json")).unwrap(),
        run_before
    );
    assert_eq!(snapshot_tree(&fixture.run_directory), tree_before);
    assert_eq!(git_evidence(&fixture.source), source_before);
    assert_eq!(git_evidence(&fixture.candidate), candidate_before);
    fixture.cleanup_candidate();
}

#[derive(Clone, Copy)]
enum HistoryCase {
    FirstInitialWrong,
    ValidResearchThenWrongAnalysis,
}

struct Fixture {
    _temp: tempfile::TempDir,
    runs_root: PathBuf,
    run_directory: PathBuf,
    source: PathBuf,
    candidate: PathBuf,
    ticket: TicketSpec,
    policy: Policy,
    snapshots: AuthoritativeRunInputSnapshots,
    context_limits: ContextLimits,
    candidate_cleaned: bool,
}

impl Fixture {
    fn new(history_case: HistoryCase) -> Self {
        let temp = tempfile::tempdir().expect("temp");
        let source = temp.path().join("source");
        fs::create_dir(&source).expect("source");
        git_ok(&source, &["init", "-q"]);
        git_ok(&source, &["config", "user.email", "test@example.com"]);
        git_ok(&source, &["config", "user.name", "SEAF Test"]);
        fs::create_dir(source.join("src")).expect("source tree");
        fs::write(source.join("src/lib.rs"), "pub fn authority() {}\n").expect("source file");
        git_ok(&source, &["add", "."]);
        git_ok(&source, &["commit", "-qm", "initial"]);

        let ticket = ticket();
        let policy = policy();
        let config = serde_json::json!({"policy_path":"seaf.policy.json"});
        let repository = serde_json::json!({"source":source.canonicalize().unwrap()});
        let ticket_bytes = canonical_json_bytes(&ticket).expect("ticket bytes");
        let policy_bytes = canonical_json_bytes(&policy).expect("policy bytes");
        let config_bytes = canonical_json_bytes(&config).expect("config bytes");
        let repository_bytes = canonical_json_bytes(&repository).expect("repository bytes");
        let eval_config = seaf_core::parse_eval_config(
            "evals:\n  allow_commands: []\n  required:\n    - name: tests\n      command: cargo test\n",
        )
        .expect("eval config");
        let eval_config_bytes = canonical_json_bytes(&eval_config).expect("eval config bytes");
        let snapshots = AuthoritativeRunInputSnapshots {
            ticket: ticket_bytes.clone(),
            provider_ticket: ticket_bytes,
            policy: policy_bytes,
            config: config_bytes,
            repository: repository_bytes,
            eval_config: eval_config_bytes,
        };
        let runs_root = temp.path().join("runs");
        let initialized = InitializedLoopRun::create_isolated(
            LoopRunnerConfig::for_ticket(
                &runs_root,
                "staged-wrong-candidate-authority",
                &ticket,
                "fake",
                "fake-model",
                LoopInputDigests {
                    ticket: canonical_sha256_digest(&ticket).expect("ticket digest"),
                    policy: canonical_sha256_digest(&policy).expect("policy digest"),
                    config: canonical_sha256_digest(&config).expect("config digest"),
                    repository: canonical_sha256_digest(&repository).expect("repository digest"),
                    eval_config: Some(
                        canonical_sha256_digest(&eval_config).expect("eval config digest"),
                    ),
                },
            ),
            &source,
            &snapshots,
        )
        .expect("isolated initialization");
        let candidate_state = initialized
            .run()
            .candidate_workspace
            .as_ref()
            .expect("candidate authority")
            .clone();
        let candidate = PathBuf::from(&candidate_state.path);
        let prepared = initialized
            .scaffold()
            .expect("scaffold")
            .publish_authoritative_inputs(snapshots.clone())
            .expect("snapshots");
        let workspace = prepared.workspace().clone();
        let run = prepared.run().clone();
        let run_directory = workspace.run_directory().to_path_buf();
        let context_limits = ContextLimits {
            max_bytes_per_file: 4_096,
            max_total_bytes: 8_192,
        };
        let context_request = ContextPackRequest::for_ticket(
            &candidate,
            &run_directory,
            &ticket,
            &policy.forbidden_paths,
            context_limits,
        );
        let context = pack_live_context(&context_request).expect("candidate context");
        let exact_authority = CandidateContextAuthority {
            kind: CandidateContextAuthorityKind::IsolatedCandidate,
            repository_identity_digest: candidate_state.repository_identity_digest,
            candidate_path_digest: sha256(candidate_state.path.as_bytes()),
            starting_head: candidate_state.starting_head,
            starting_tree: candidate_state.starting_tree,
        };
        let mut wrong_authority = exact_authority.clone();
        wrong_authority.candidate_path_digest = "0".repeat(64);
        let initial_authority = match history_case {
            HistoryCase::FirstInitialWrong => wrong_authority.clone(),
            HistoryCase::ValidResearchThenWrongAnalysis => exact_authority,
        };
        let research = stage_initial_request(
            &workspace,
            &run,
            &ticket,
            &context_request,
            &context,
            LoopStepName::Research,
            ProviderRole::Researcher,
            Role::Researcher,
            initial_authority,
            None,
        );
        if matches!(history_case, HistoryCase::ValidResearchThenWrongAnalysis) {
            let durable_research =
                persist_provider_exchange_record_reference(&workspace, research.clone())
                    .expect("durable Research initial");
            let coordinates = ProviderExchangeCoordinates {
                run_id: run.run_id.clone(),
                step: LoopStepName::Research,
                role: ProviderRole::Researcher,
                step_attempt: 1,
                exchange_index: 1,
                kind: ProviderExchangeKind::Initial,
                context_round: None,
            };
            let request_record =
                seaf_loop::load_provider_exchange_record(&run_directory, &research)
                    .expect("Research request record");
            let response = write_provider_exchange_response(
                &run_directory,
                &coordinates,
                &ProviderExchangeResponseAudit::ModelResponse {
                    response: ModelResponse {
                        content: serde_json::json!({
                            "role": "researcher",
                            "status": "passed",
                            "summary": "done",
                            "findings": [],
                            "risks": [],
                            "next_step_recommendation": "continue"
                        })
                        .to_string(),
                        latency_ms: 1,
                        raw_provider_metadata: serde_json::Value::Null,
                    },
                },
            )
            .expect("Research response");
            let response_record = ProviderExchangeRecord {
                schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
                run_id: run.run_id.clone(),
                step: LoopStepName::Research,
                role: ProviderRole::Researcher,
                step_attempt: 1,
                exchange_index: 1,
                kind: ProviderExchangeKind::Initial,
                context_round: None,
                phase: ProviderExchangePhase::Response,
                previous_record_digest: Some(
                    durable_research
                        .provider_exchange_records
                        .last()
                        .expect("Research head")
                        .digest
                        .clone(),
                ),
                request: request_record.request,
                response: Some(response),
                expansion: None,
                outcome: Some(ProviderExchangeOutcome::Passed),
            };
            let response_reference =
                stage_provider_exchange_record(&run_directory, &response_record)
                    .expect("staged Research response");
            let durable_response =
                persist_provider_exchange_record_reference(&workspace, response_reference)
                    .expect("durable Research response");
            let previous = durable_response
                .provider_exchange_records
                .last()
                .expect("Research response head")
                .digest
                .clone();
            stage_initial_request(
                &workspace,
                &durable_response,
                &ticket,
                &context_request,
                &context,
                LoopStepName::Analysis,
                ProviderRole::Analyzer,
                Role::Analyzer,
                wrong_authority,
                Some(previous),
            );
        }

        Self {
            _temp: temp,
            runs_root,
            run_directory,
            source,
            candidate,
            ticket,
            policy,
            snapshots,
            context_limits,
            candidate_cleaned: false,
        }
    }

    fn cleanup_candidate(&mut self) {
        if self.candidate_cleaned {
            return;
        }
        git_ok(
            &self.source,
            &[
                "worktree",
                "remove",
                "--force",
                self.candidate.to_str().expect("candidate path"),
            ],
        );
        git_ok(&self.source, &["worktree", "prune"]);
        assert!(
            !self.candidate.exists(),
            "candidate must be removed exactly"
        );
        let worktrees = git(&self.source, &["worktree", "list", "--porcelain"]);
        assert!(!worktrees.contains(self.candidate.to_str().unwrap()));
        self.candidate_cleaned = true;
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.candidate_cleaned && self.source.exists() {
            let _ = Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&self.candidate)
                .current_dir(&self.source)
                .output();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stage_initial_request(
    workspace: &seaf_loop::LoopWorkspace,
    run: &LoopRun,
    ticket: &TicketSpec,
    context_request: &ContextPackRequest,
    context: &ContextBundle,
    step: LoopStepName,
    provider_role: ProviderRole,
    role: Role,
    candidate_authority: CandidateContextAuthority,
    previous_record_digest: Option<String>,
) -> seaf_core::ProviderExchangeRecordReference {
    let audited_context = serde_json::json!({
        "candidate_authority": candidate_authority,
        "untrusted_context_marker": context.untrusted_context_marker,
        "total_context_bytes": context.total_context_bytes,
        "files": context.files.iter().map(|file| serde_json::json!({
            "path": file.path,
            "source_sha256": file.sha256,
            "included_sha256": sha256(file.content.as_bytes()),
            "source_bytes": file.source_bytes,
            "included_bytes": file.included_bytes,
            "truncated": file.truncated,
        })).collect::<Vec<_>>(),
        "warnings": context.warnings,
        "limits": context_request.limits,
        "default_exclude_globs": context_request.default_exclude_globs,
        "ticket_forbidden_files": context_request.ticket_forbidden_files,
        "policy_forbidden_paths": context_request.policy_forbidden_paths,
    });
    let role_input = serde_json::json!({
        "instructions": format!("Run the {step:?} loop step. Return only JSON matching the response schema."),
        "run_id": run.run_id,
        "input_digests": run.input_digests,
        "ticket": ticket,
        "prerequisites": {},
        "repository_context": render_context(context),
        "repository_context_authority": audited_context,
    });
    let request = ModelRequest {
        model: "fake-model".to_string(),
        system: role.system_prompt().to_string(),
        messages: vec![ModelMessage {
            role: ModelMessageRole::User,
            content: serde_json::to_string(&role_input).expect("role input"),
        }],
        response_schema: Some(role.response_schema()),
        temperature: 0.0,
        timeout_ms: 30_000,
    };
    let request_bytes = serde_json::to_vec_pretty(&request).expect("request bytes");
    write_step_request(
        workspace,
        step,
        1,
        std::str::from_utf8(&request_bytes).expect("request text"),
    )
    .expect("conventional prompt");
    let coordinates = ProviderExchangeCoordinates {
        run_id: run.run_id.clone(),
        step,
        role: provider_role,
        step_attempt: 1,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
    };
    let request_reference =
        write_provider_exchange_request(workspace.run_directory(), &coordinates, &request_bytes)
            .expect("audited request");
    stage_provider_exchange_record(
        workspace.run_directory(),
        &ProviderExchangeRecord {
            schema_version: PROVIDER_EXCHANGE_SCHEMA_VERSION,
            run_id: run.run_id.clone(),
            step,
            role: provider_role,
            step_attempt: 1,
            exchange_index: 1,
            kind: ProviderExchangeKind::Initial,
            context_round: None,
            phase: ProviderExchangePhase::Request,
            previous_record_digest,
            request: request_reference,
            response: None,
            expansion: None,
            outcome: None,
        },
    )
    .expect("staged initial request")
}

fn ticket() -> TicketSpec {
    TicketSpec {
        ticket_id: "T-STAGED-CANDIDATE".to_string(),
        goal_id: "production-use".to_string(),
        title: "Bind staged provider history to its candidate".to_string(),
        status: TicketStatus::Ready,
        priority: TicketPriority::P1,
        problem: "Provider history must not cross candidate authorities.".to_string(),
        research_questions: Vec::new(),
        context: TicketContext {
            relevant_files: vec!["src/lib.rs".to_string()],
            forbidden_files: Vec::new(),
        },
        autonomy: TicketAutonomy {
            level: 1,
            apply_patch: false,
            allow_shell_commands: Vec::new(),
        },
        acceptance_criteria: vec!["Mismatched history is rejected without mutation.".to_string()],
        eval: None,
    }
}

fn policy() -> Policy {
    Policy {
        policy_id: "test".to_string(),
        default_autonomy_level: 1,
        forbidden_paths: vec!["secrets/**".to_string()],
        requires_human_review: vec!["dependency_changes".to_string()],
        allowed_without_review: vec!["source_changes".to_string()],
    }
}

fn render_context(context: &ContextBundle) -> String {
    let mut rendered = format!(
        "\n\nRepository context:\n{}\n",
        context.untrusted_context_marker
    );
    for file in &context.files {
        rendered.push_str(&format!(
            "\ncontext file\npath: {}\nsha256: {}\ncontent:\n{}",
            file.path, file.sha256, file.content
        ));
        if !file.content.ends_with('\n') {
            rendered.push('\n');
        }
    }
    if !context.warnings.is_empty() {
        rendered.push_str("\ncontext warnings:\n");
        for warning in &context.warnings {
            rendered.push_str(&format!("- {warning}\n"));
        }
    }
    rendered
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitEvidence {
    head: Vec<u8>,
    tree: Vec<u8>,
    status: Vec<u8>,
    unstaged_diff: Vec<u8>,
    staged_diff: Vec<u8>,
    tracked_file: Vec<u8>,
}

fn git_evidence(root: &Path) -> GitEvidence {
    GitEvidence {
        head: git_bytes(root, &["rev-parse", "HEAD"]),
        tree: git_bytes(root, &["rev-parse", "HEAD^{tree}"]),
        status: git_bytes(root, &["status", "--porcelain=v1", "--untracked-files=all"]),
        unstaged_diff: git_bytes(root, &["diff", "--binary"]),
        staged_diff: git_bytes(root, &["diff", "--cached", "--binary"]),
        tracked_file: fs::read(root.join("src/lib.rs")).expect("tracked file"),
    }
}

fn snapshot_tree(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn visit(root: &Path, directory: &Path, snapshot: &mut BTreeMap<String, Option<Vec<u8>>>) {
        let mut entries = fs::read_dir(directory)
            .expect("tree directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("tree entries");
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("tree relative path")
                .to_string_lossy()
                .into_owned();
            let metadata = fs::symlink_metadata(&path).expect("tree metadata");
            assert!(
                !metadata.file_type().is_symlink(),
                "unexpected symlink {relative}"
            );
            if metadata.is_dir() {
                snapshot.insert(relative, None);
                visit(root, &path, snapshot);
            } else {
                assert!(metadata.is_file(), "unexpected tree entry {relative}");
                snapshot.insert(relative, Some(fs::read(path).expect("tree file")));
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

#[derive(Default)]
struct RecordingPatchRunner {
    calls: Vec<PatchCommand>,
}

impl PatchCommandRunner for RecordingPatchRunner {
    fn run(
        &mut self,
        _root: &Path,
        command: PatchCommand,
        _patch: &str,
    ) -> Result<CommandOutput, PatchGateError> {
        self.calls.push(command);
        Ok(CommandOutput::success())
    }
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn git(root: &Path, args: &[&str]) -> String {
    String::from_utf8(git_bytes(root, args))
        .expect("git utf-8")
        .trim()
        .to_string()
}

fn git_bytes(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git_ok(root: &Path, args: &[&str]) {
    let _ = git_bytes(root, args);
}
