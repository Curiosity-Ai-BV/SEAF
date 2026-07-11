use seaf_core::{
    canonical_sha256_digest, LoopInputDigests, TicketAutonomy, TicketContext, TicketPriority,
    TicketSpec, TicketStatus,
};
use seaf_loop::{LoopRunner, LoopRunnerConfig, ProviderStepRunner};
use seaf_models::FakeProvider;

#[test]
fn production_provider_constructor_rejects_fresh_legacy_loop_without_side_effects() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    let ticket = ticket();
    let provider = FakeProvider::new(Vec::new());
    let mut runner =
        ProviderStepRunner::new(&provider, "fake-model", 30_000).with_ticket(ticket.clone());

    let error = LoopRunner::start(
        LoopRunnerConfig::for_ticket(
            &runs_root,
            "legacy-provider-bypass",
            &ticket,
            "fake",
            "fake-model",
            LoopInputDigests {
                ticket: canonical_sha256_digest(&ticket).expect("ticket digest"),
                policy: "b".repeat(64),
                config: "c".repeat(64),
                repository: "d".repeat(64),
            },
        ),
        &mut runner,
    )
    .expect_err("provider execution must use the isolated initializer");

    assert!(
        error.to_string().contains("start a new isolated run"),
        "{error}"
    );
    assert!(provider.requests().expect("provider requests").is_empty());
    assert!(!runs_root.join("legacy-provider-bypass").exists());
}

fn ticket() -> TicketSpec {
    TicketSpec {
        ticket_id: "T-ISOLATED".to_string(),
        goal_id: "production-use".to_string(),
        title: "Require candidate execution".to_string(),
        status: TicketStatus::Ready,
        priority: TicketPriority::P1,
        problem: "Provider runs must not execute against the source checkout.".to_string(),
        research_questions: Vec::new(),
        context: TicketContext {
            relevant_files: Vec::new(),
            forbidden_files: Vec::new(),
        },
        autonomy: TicketAutonomy {
            level: 1,
            apply_patch: false,
            allow_shell_commands: Vec::new(),
        },
        acceptance_criteria: vec!["Source checkout remains unchanged.".to_string()],
        eval: None,
    }
}
