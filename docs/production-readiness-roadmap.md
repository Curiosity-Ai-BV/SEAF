# Production Readiness Roadmap

Date: 2026-07-07

Branch assessed: `codex/seaf-foundation-agent-loop`

## Assumptions

Production-ready means SEAF can safely run a supervised local agent loop against
real provider output, produce auditable artifacts, publish dependable SDK/schema
contracts, and prepare verified release metadata without weakening the existing
safety boundaries. It does not mean production apps can rewrite, merge, sign, or
deploy changes without human review.

## Current State

Phase 2 is complete. `docs/phase-2-local-agent-loop.md` remains the historical
source for P2-001 through P2-012, and `.seaf/loops/current/contract.md` now
tracks the Phase 0 production-readiness baseline. The implemented foundation is
substantial:

- Rust workspace crates exist for core contracts, CLI, loop orchestration,
  model providers, and local runtime.
- `@seaf/sdk` can emit events, metrics, and feedback through HTTP or memory
  transports.
- Public schemas exist under `specs/`.
- The CLI validates goals, policies, tickets, runs deterministic loop
  executions, runs evals, prepares/verifies release capsules, and checks local
  Ollama availability.
- Phase 2 added ticket and loop-run contracts, fake and Ollama providers,
  context packing, role response schemas, a deterministic policy gate,
  AgentBench-lite, EvalReport integration, documentation, and CI guardrails.

The repo is production-conscious, but not production-ready. The highest-impact
gap is that `seaf loop run --provider ollama` still records provider/model
metadata while executing the deterministic local runner. Live provider-backed
role execution and policy-gated patch proposals remain future work
(`docs/local-agent-loop.md`, `docs/phase-2-local-agent-loop.md`). Release
capsules are digest/provenance metadata only, with no production signing or
verified updater path (`docs/artifact-chain.md`, `crates/seaf-cli/src/main.rs`).

## Production Readiness Criteria

SEAF should not be called production-ready until these criteria are met:

1. A real provider-backed loop can run from ticket to EvalReport through
   structured role outputs, bounded context, patch extraction, policy gating,
   command checks, and durable artifacts.
2. Synthetic empty-patch policy evidence is limited to explicit smoke paths.
   Real loop evals fail closed on missing, malformed, placeholder, mismatched,
   or rejected policy decisions.
3. Ticket autonomy is enforced for patch application and shell commands.
4. SDK, Rust models, and JSON schemas have one declared source of truth and
   drift tests.
5. Local telemetry ingestion has privacy, retention, redaction, migration, and
   operational controls before ingesting sensitive production data.
6. Release capsules include signing/provenance controls strong enough for a
   verified update chain.
7. CI, dependency, packaging, and governance controls are explicit and
   repeatable.

## Roadmap

### Phase 0 - Stabilize The Current Baseline

Goal: remove stale planning signals and close cheap guardrail gaps before
enabling live model-driven changes.

- P3-001: Create a new post-Phase-2 loop contract in `.seaf/loops/current/`.
  The contract should make live provider-backed execution, command sandboxing,
  real policy evidence, and schema drift tests the next acceptance criteria.
- P3-002: Reconcile stale docs. `docs/mvp-backlog.md` still lists some work as
  next slices even though the Phase 2 tracker says the primitives are complete.
  Treat `docs/phase-2-local-agent-loop.md` as authoritative and update backlog
  wording to distinguish implemented primitives from missing integration.
- P3-003: Fix default policy drift. The policy gate recognizes `ci_changes`,
  `eval_changes`, `policy_changes`, `updater_changes`, and `signing_changes`,
  but the default/example policies only list dependency, database, auth,
  payment, privacy, and network categories. Add the missing categories to
  templates/examples and lock them with tests.
- P3-004: Fix generated artifact hygiene. The CLI writes loop runs under
  `.seaf/loops/runs`, while `.gitignore` ignores `.seaf/runs`. Align the ignore
  rules and add `.seaf/**` to default context exclusions so generated run
  artifacts are not accidentally packed back into model context.
- P3-005: Harden CI determinism. Use locked Cargo commands, pin the Rust
  toolchain or document why stable is acceptable, set workflow permissions,
  timeouts and concurrency, and split Rust clippy from TypeScript package lint
  so the TypeScript job does not depend on an unconfigured Rust environment.

Exit gate: `cargo fmt --all -- --check`, `cargo clippy --locked
--all-targets --all-features -- -D warnings`, `cargo test --locked
--workspace`, `pnpm format:check`, `pnpm lint`, `pnpm typecheck`, `pnpm test`,
`pnpm build`, and `git diff --check` pass on a clean tree.

### Phase 1 - Wire The Live Local Agent Loop

Goal: make `seaf loop run` use the loop primitives already built in Phase 2.

- P3-006: Add a provider-backed `StepRunner` that maps loop steps to role
  prompts, sends `ModelRequest`s through `ModelProvider`, persists every request
  and response, parses structured responses, and attempts the existing one-time
  repair path only for invalid JSON.
- P3-007: Connect context packing to live role prompts. The run should write
  `context-manifest.json`, include file digests, preserve the untrusted-context
  marker, and fail closed on unsafe or forbidden context paths.
- P3-008: Wire developer patch output to `gate_patch`. Policy decisions must
  include the real patch digest, changed paths, decision kind, human-review
  requirement, apply request, and applied status. Patch application must remain
  opt-in through ticket autonomy and a clean worktree guard.
- P3-009: Enforce command controls. Replace raw `sh -c` eval execution with a
  command runner that validates ticket/eval allowlists, working directory, env,
  timeout, output size, and redaction rules before writing logs.
- P3-010: Add live-loop recovery tests. Cover malformed role output, repair
  success/failure, model timeout, blocked reviewer decisions, rejected forbidden
  patches, resume after interruption, and eval failure from missing policy
  evidence.

Exit gate: `loop run --provider fake` exercises the same provider-backed path
as live runs, `loop run --provider ollama` can complete a local smoke against an
installed model, and `eval run --loop-run ... --ticket ...` refuses runs without
real policy evidence.

### Phase 2 - Make Contracts And SDK Publishable

Goal: make SEAF's public contracts dependable for Rust and TypeScript
consumers.

- P3-011: Declare one contract source of truth. Either generate Rust/TS from
  JSON Schema or generate schemas/TS from Rust models. Add drift tests for every
  public contract.
- P3-012: Tighten schema parity. Current schemas allow shapes that Rust
  validation rejects, such as empty goal guardrails, empty policy lists, and
  empty EvalReport checks. Make schema and Rust behavior agree.
- P3-013: Move `PolicyDecision` into `seaf-core` or another shared contract
  surface and make `LoopRun.policy_decisions` typed instead of arbitrary object
  maps.
- P3-014: Expand `@seaf/sdk` beyond event emission. Export contract types and
  validation helpers for Event, Signal, GoalSpec, Policy, TicketSpec, LoopRun,
  EvalReport, and ReleaseCapsule.
- P3-015: Make package release checks real. Include schemas intentionally in
  `@seaf/sdk` or publish `@seaf/schemas`, add license/repository/engines and
  publish metadata, run `npm pack --dry-run`, and import the packed artifact in
  CI.

Exit gate: SDK package contents are verified from the packed artifact, schema
fixtures pass in Rust and TypeScript, and a schema drift test fails on any
contract mismatch.

### Phase 3 - Harden Runtime, Telemetry, And Product Surface

Goal: turn the local runtime from an MVP event store into a safe operational
component and expose enough product surface to inspect the loop.

- P3-016: Add runtime migrations, retention policy, payload redaction,
  sensitive/private event handling, log-size caps, and optional encryption for
  local storage.
- P3-017: Add a supported ingestion surface. Either expose a local HTTP service
  matching the SDK default endpoint or change the SDK/runtime contract so the
  default integration is demonstrably runnable.
- P3-018: Build the read-only dashboard described in the roadmap: goals,
  signals, tasks, loop runs, eval reports, policy decisions, release capsules,
  and generated artifacts. Keep it read-only until policy and release controls
  mature.
- P3-019: Promote Adaptive Notes from example data to a complete demo shell that
  emits SDK events, produces local signals, runs a ticket through the loop, and
  shows the resulting EvalReport and ReleaseCapsule.

Exit gate: a new developer can run one documented demo from app event emission
through signal, ticket, loop run, eval report, and release capsule without
manual fixture editing.

### Phase 4 - Production Release And Governance

Goal: make release artifacts, operational ownership, and supply-chain controls
credible.

- P3-020: Implement signing and verified-update metadata. Release capsules
  should require signatures for production channels, include build recipe or
  provenance hashes, and verify artifact/eval digests before trust.
- P3-021: Add provenance and commit checks. Verify source commit, dirty tree
  state, patch digest, eval report digest, and release capsule digest before
  commit/merge or release preparation.
- P3-022: Add supply-chain controls: `cargo-deny` or equivalent, npm audit
  policy, license allowlist, SBOM generation, Dependabot/Renovate, and release
  artifact attestation.
- P3-023: Add governance docs and ownership. Include `SECURITY.md`,
  `CODEOWNERS`, release procedure, incident process, support policy, changelog
  rules, and coverage expectations.

Exit gate: a release candidate can be built, checked, signed, verified, and
rolled back through documented commands with CI evidence and human review gates.

## Recommended Order

Start with Phase 0. It is small, removes stale instructions, and closes gaps
that become dangerous once live agents are allowed to propose patches. Phase 1
is the core product milestone: until live provider-backed loop execution is
real, production signing, dashboards, and package publishing are supporting
work rather than proof that SEAF works end to end. Phase 2 and Phase 3 can run
partly in parallel after Phase 1 interfaces settle. Phase 4 should wait until
real loop artifacts and contract surfaces have stopped moving.

## Non-Goals For The Next Phase

- Automatic merge to `main`.
- Automatic production deployment.
- Real signing keys stored in the repo, CI general jobs, or agent-readable
  context.
- Cloud agent execution.
- Allowing model output to bypass schema validation, policy gates, evals, or
  human review.
