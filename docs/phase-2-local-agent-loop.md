# Phase 2 Local Agent Loop

Source of truth: `/Users/adrian/Downloads/SEAF_next_phase_local_agent_loop_plan.md`

Date authored: 2026-07-01

Branch: `codex/seaf-foundation-agent-loop`

## Overview

Phase 2 turns the MVP's manual SEAF artifact chain into a supervised local
agent-loop runner for macOS. The target outcome is a runnable local loop that
can take a ticket file, gather bounded repository context, ask a local model for
structured outputs, validate the outputs, gate any patch deterministically, run
tests/evals, and write durable run artifacts.

Target flow:

```text
Ticket / Feature request
  -> local repo research
  -> analysis
  -> implementation spec
  -> spec review gate
  -> local model patch proposal
  -> policy-gated patch application
  -> output review gate
  -> deterministic tests/evals
  -> EvalReport + loop trace
```

Phase 2 is Autonomy Level 1. Local agents may propose and apply patches in a
working tree through deterministic policy gates, but humans still review and
merge.

## Scope Boundary

In scope:

- Ticket and loop-run contracts.
- Provider-neutral local model calls with a deterministic fake provider.
- Ollama support for local Mac smoke tests.
- Bounded repository context packing.
- Restartable loop workspace and durable run artifacts.
- Role prompts with schema-validated structured responses.
- Patch parsing, forbidden-path detection, and policy gates.
- CLI commands for model checking, ticket validation, loop run/status/resume,
  smoke testing, and benchmarking.
- EvalReport integration and deterministic CI coverage that does not require
  Ollama.
- Developer documentation for local Mac setup and agent-loop recovery.

Out of scope:

- Automatic merge to `main`.
- Automatic GitHub PR creation.
- Production update shipping.
- Real signing keys.
- Cloud agent execution.
- Network research by agents.
- Direct terminal access by the model.
- Direct filesystem writes by the model.
- Model access to secrets, private telemetry, or unrestricted logs.
- Dependency updates without human review.
- Self-modification of evals, CI, signing, updater, auth, billing, or policy
  paths.

Safety boundary:

- The model has no direct tools. The orchestrator reads, validates, applies, and
  runs commands.
- Repository content, tickets, logs, and telemetry excerpts are untrusted
  context unless they are SEAF system instructions.
- Forbidden files and generated/dependency folders must be excluded from model
  context.
- Output review is not authoritative. Deterministic gates remain authoritative.

## Release And Checking Protocol

- Ticket specs are authored by one agent and reviewed by another agent before
  development begins.
- Development agents work on one P2 slice at a time and must follow that slice's
  objective, allowed files, dependencies, acceptance criteria, and verification
  commands.
- Every development slice requires two independent reviews before commit:
  spec-compliance review and code-quality review.
- Implementers do not self-approve their own work.
- The commit/merge agent stages, commits, or merges only after the required
  slice checks pass and both reviews are complete.
- Skipped checks must be reported explicitly. "Tests pass" is invalid if any
  required check was skipped.
- Patches that touch forbidden paths, weaken evals/tests/CI/policies, or add
  dependencies require explicit human review or must be blocked by policy.

## Current Status

`P2-001`, `P2-002`, `P2-003`, `P2-004`, `P2-005`, `P2-006`, `P2-007`,
`P2-008`, and `P2-010` are complete. `P2-009` is the next implementation slice
so local model behavior can be evaluated with AgentBench-lite.

| Ticket | Title                                            | Status   | First Slice |
| ------ | ------------------------------------------------ | -------- | ----------- |
| P2-001 | Add TicketSpec and LoopRun contracts             | complete | done        |
| P2-002 | Add model provider abstraction and fake provider | complete | done        |
| P2-003 | Add Ollama provider                              | complete | done        |
| P2-004 | Add local context packer                         | complete | done        |
| P2-005 | Add loop workspace and state machine             | complete | done        |
| P2-006 | Add role prompts and structured response schemas | complete | done        |
| P2-007 | Add patch parser and deterministic policy gate   | complete | done        |
| P2-008 | Add CLI commands for model, ticket, and loop     | complete | done        |
| P2-009 | Build AgentBench-lite                            | pending  | next        |
| P2-010 | Integrate evals with existing EvalReport         | complete | done        |
| P2-011 | Documentation and Mac setup guide                | pending  | no          |
| P2-012 | CI hardening                                     | pending  | no          |

Recommended order from the plan:

```text
Sprint 1: P2-001, P2-002, P2-004
Sprint 2: P2-005, P2-006, P2-007
Sprint 3: P2-003, P2-008, P2-010
Sprint 4: P2-009, P2-011, P2-012
```

## Ticket Specs

### P2-001 - Add TicketSpec and LoopRun contracts

Status: complete in `65fc489`

Objective: Add typed contracts for local-loop tickets and loop runs.

Allowed files:

- `crates/seaf-core/src/models.rs`
- `crates/seaf-core/src/validation.rs`
- `crates/seaf-core/src/lib.rs`
- `specs/ticket.schema.json`
- `specs/loop-run.schema.json`
- `examples/local-loop/tickets/add-health-command.yaml`
- Focused valid/invalid fixtures needed to test these contracts.

Dependencies: none.

Acceptance criteria:

- Valid ticket fixture loads.
- Invalid ticket fixture fails with useful field errors.
- Valid loop-run fixture loads.
- Unknown fields fail closed.
- Schemas mirror Rust models.
- Models include `TicketStatus`, `TicketPriority`, `TicketSpec`,
  `TicketContext`, `TicketAutonomy`, `TicketEval`, `LoopStatus`,
  `LoopStepStatus`, `LoopStepName`, `LoopRun`, and `LoopStepRecord`.
- Validation includes `validate_ticket_spec`, `validate_loop_run`,
  `load_ticket_file`, and `load_loop_run_file`.

Verification commands:

```bash
cargo fmt --all -- --check
cargo test -p seaf-core
pnpm format:check
```

### P2-002 - Add model provider abstraction and fake provider

Status: complete in `946aa4d`

Objective: Create a provider-neutral interface for model calls and deterministic
tests.

Allowed files:

- `Cargo.toml`
- `Cargo.lock` when changed mechanically by adding the local workspace crate.
- `crates/seaf-models/Cargo.toml`
- `crates/seaf-models/src/lib.rs`
- `crates/seaf-models/src/provider.rs`
- `crates/seaf-models/src/fake.rs`
- Focused fixtures/tests for scripted fake-provider responses.

Dependencies: none.

Acceptance criteria:

- Fake provider can script a sequence of responses.
- Tests can run the entire loop without network.
- Provider errors are serializable into loop artifacts.
- Core API supports model, system prompt, messages, optional response schema,
  temperature, timeout, response content, latency, and raw metadata.

Verification commands:

```bash
cargo test -p seaf-models
cargo clippy --all-targets --all-features -- -D warnings
```

Completed in `946aa4d`. Review follow-ups for future slices: add nested
`ModelMessage` unknown-field coverage if the DTO tests are expanded, and
replace `BTreeMap<String, Value>` policy decisions with a typed model in P2-007
once the artifact shape is known.

### P2-003 - Add Ollama provider

Status: complete in `3fe0744`

Objective: Implement local Ollama API support behind the model-provider
abstraction.

Allowed files:

- `crates/seaf-models/src/ollama.rs`
- `crates/seaf-models/src/lib.rs`
- `crates/seaf-cli/Cargo.toml` when changed mechanically by adding the local
  `seaf-models` dependency.
- `crates/seaf-cli/src/main.rs`
- `Cargo.lock` when changed mechanically by adding the local CLI dependency.
- Focused tests or HTTP request-builder fixtures for Ollama behavior.

Dependencies: P2-002.

Acceptance criteria:

- Unit tests mock the HTTP client or request builder.
- Manual smoke works against local Ollama.
- CI does not depend on live Ollama.
- Default base URL is `http://localhost:11434/api`.
- `/api/chat` is used with `stream: false`.
- Structured response schemas are sent via `format` when supplied.
- Low temperature is used by default for structured steps.
- Errors are actionable for Ollama not running, model not installed, timeout,
  and non-JSON model response.

Verification commands:

```bash
cargo test -p seaf-models
cargo run -p seaf-cli -- model check --provider ollama --model gemma4:e4b-mlx
```

Manual smoke note: the smoke command compiled and reached local Ollama during
review, but `gemma4:e4b-mlx` was not installed in the test environment. The CLI
returned an actionable `ollama pull gemma4:e4b-mlx` hint.

Review follow-ups for future provider work: the std HTTP client intentionally
tries all resolved localhost addresses before failing, and generic HTTP 404
responses are reported as base URL/API-root issues unless the provider message
actually indicates a missing model.

### P2-004 - Add local context packer

Status: complete in `5f36eba`

Objective: Gather safe, bounded repository context for model prompts.

Allowed files:

- `Cargo.toml` when changed mechanically by adding the local workspace crate.
- `Cargo.lock` when changed mechanically by adding the local workspace crate.
- `crates/seaf-loop/Cargo.toml`
- `crates/seaf-loop/src/lib.rs`
- `crates/seaf-loop/src/context.rs`
- `crates/seaf-loop/src/policy.rs`
- `crates/seaf-loop/src/workspace.rs`
- Focused context-packing tests and fixtures.

Dependencies: P2-001.

Acceptance criteria:

- Excludes secrets and generated folders.
- Enforces max context size.
- Includes file digests for traceability.
- Writes `context-manifest.json` to the run directory.
- Inputs include ticket relevant files, policy forbidden paths, default exclude
  globs, max bytes per file, and total context bytes.
- Prompts treat all included files as untrusted context.

Verification commands:

```bash
cargo test -p seaf-loop context
```

Review follow-up for future context hardening: add direct regression tests for
absolute/traversal path rejection and symlink escape blocking.

### P2-005 - Add loop workspace and state machine

Status: complete in `af7a2fa`

Objective: Make loop runs restartable and auditable.

Allowed files:

- `crates/seaf-loop/src/runner.rs`
- `crates/seaf-loop/src/lib.rs`
- `crates/seaf-loop/src/state.rs`
- `crates/seaf-loop/src/workspace.rs`
- `crates/seaf-loop/src/artifacts.rs`
- Focused state-machine tests and workspace fixtures.

Dependencies: P2-001, P2-002.

Acceptance criteria:

- A run can be resumed after interruption.
- Completed steps are not repeated unless `--rerun-from <step>` is supplied.
- Every model request and response is stored.
- Run status is updated after each step.
- Run workspace includes `run.json`, `context-manifest.json`, prompts,
  responses, artifacts, and `log.md`.

Verification commands:

```bash
cargo test -p seaf-loop state
```

Review follow-ups before CLI wiring: validate user-facing `run_id` values before
joining them into workspace paths, and consider a focused retry test where a
prompt exists without a response.

### P2-006 - Add role prompts and structured response schemas

Status: complete in `bbc5665`

Objective: Implement local agent roles and schema-validated structured outputs.

Allowed files:

- Role prompt and response-schema modules under `crates/seaf-loop/src/`.
- `fixtures/model-responses/*`
- Focused tests for role response validation and repair.

Dependencies: P2-001, P2-002, P2-005.

Acceptance criteria:

- Roles exist for Researcher, Analyzer, Spec Writer, Spec Reviewer, Developer,
  and Output Reviewer.
- Each role has a schema.
- Each role has unit tests with valid/invalid fixtures.
- Markdown-only model responses are rejected.
- Repair prompt is attempted once for invalid JSON.
- Developer responses put unified diff content only in the `patch` field.
- Reviewer responses include explicit blocking and non-blocking issue arrays.

Verification commands:

```bash
cargo test -p seaf-loop role_response
```

Review follow-up before policy-gate integration: consider rejecting unified diff
content in the developer `patch` field when the developer status is `blocked`
or `needs_context`. Current behavior still satisfies P2-006 because diff content
is restricted to `patch`.

### P2-007 - Add patch parser and deterministic policy gate

Status: complete in `0e5f9e5`

Objective: Ensure model-generated code changes are safe to apply.

Allowed files:

- `crates/seaf-loop/src/lib.rs`
- `crates/seaf-loop/src/patch.rs`
- `crates/seaf-loop/src/policy_gate.rs`
- `fixtures/patches/*`
- Focused patch parser and policy-gate tests.

Dependencies: P2-004.

Acceptance criteria:

- Forbidden patches never apply.
- Bad patches leave the working tree unchanged.
- Allowed patch applies with `--apply-patch`.
- Without `--apply-patch`, patch is only written to disk.
- Unified diff paths are parsed.
- `git apply --check` runs before applying.
- Forbidden path, eval/CI/policy/dependency/updater/signing/auth/billing,
  binary patch, and path traversal changes are blocked or escalated according
  to policy.
- A `PolicyDecision` artifact is emitted.

Verification commands:

```bash
cargo test -p seaf-loop patch
cargo test -p seaf-loop policy_gate
```

Review notes for future integration: category-style `requires_human_review`
entries are treated as canonical policy keys, while path-like entries are
matched as review patterns. Git command diagnostics are serialized in
`PolicyDecisionReason.details`; `pattern` is reserved for matched policy keys or
path patterns.

### P2-008 - Add CLI commands for model, ticket, and loop

Status: complete in `e7f04a2`

Objective: Expose the local loop through `seaf-cli`.

Allowed files:

- `crates/seaf-cli/src/main.rs`
- `crates/seaf-cli/tests/cli.rs`
- `crates/seaf-cli/Cargo.toml` when changed mechanically by adding the local
  `seaf-loop` dependency.
- `Cargo.lock` when changed mechanically by adding that local dependency.
- No core loop behavior changes except through already reviewed public APIs.

Dependencies: P2-001, P2-002, P2-004, P2-005, P2-006, P2-007.

Acceptance criteria:

- Commands exist for `model check`, `ticket validate`, `loop run`,
  `loop status`, `loop resume`, and `loop smoke`.
- CLI returns nonzero on validation failures.
- JSON output is available for automation.
- Human-readable output summarizes next action.
- `loop run` refuses dirty working trees unless `--allow-dirty` is provided.

Verification commands:

```bash
cargo test -p seaf-cli
cargo run -p seaf-cli -- ticket validate examples/local-loop/tickets/add-health-command.yaml
```

Review notes for future CLI work: keep command wiring on reviewed public APIs
instead of duplicating loop internals in `seaf-cli`. User-provided and persisted
run IDs are validated before path use, and `loop resume` preflights `run.json`
before creating or mutating a workspace.

### P2-009 - Build AgentBench-lite

Status: pending

Objective: Create a repeatable local model eval for the loop.

Allowed files:

- `examples/agent-bench-lite/README.md`
- `examples/agent-bench-lite/repo-fixture/`
- `examples/agent-bench-lite/tickets/`
- `examples/agent-bench-lite/evals/`
- `examples/agent-bench-lite/expected/`
- CLI bench wiring only if needed to expose the planned benchmark command.

Dependencies: P2-005, P2-006, P2-007, P2-008.

Acceptance criteria:

- Benchmark can run with fake provider in CI.
- Benchmark can run with Ollama locally.
- Outputs a JSON summary.
- Forbidden/eval-weakening violations are zero-tolerance failures.
- Initial tickets cover CLI health, validation test, docs-only change,
  forbidden CI change rejection, and eval-weakening rejection.
- Metrics include schema-valid rate, repair-success rate, patch-apply rate,
  eval-pass rate, forbidden violation count, eval-weakening accepted count, and
  median latency.

Verification commands:

```bash
cargo run -p seaf-cli -- loop bench --provider fake --fixture examples/agent-bench-lite
cargo run -p seaf-cli -- loop bench --provider ollama --model gemma4:e4b-mlx --fixture examples/agent-bench-lite
```

### P2-010 - Integrate evals with existing EvalReport

Status: complete in `1e86622`

Objective: Keep the local loop compatible with SEAF's existing eval report
system.

Allowed files:

- EvalReport integration modules under `crates/seaf-loop/src/`.
- `crates/seaf-cli/src/main.rs` only for command wiring.
- `examples/local-loop/seaf.evals.yaml`
- Existing `seaf-core` EvalReport model or validation files only if required
  for backward-compatible loop-check representation.
- Focused eval-report integration tests.

Dependencies: P2-005, P2-007, P2-008.

Acceptance criteria:

- Existing `seaf eval run` remains backward compatible.
- Loop-level checks are represented as EvalCheck objects.
- A failing patch gate produces a rejected report.
- Generated EvalReports use `patch_id = run_id`, `goal_id = ticket.goal_id`,
  `approve_for_human_review` on success, and `reject` on failure.
- Check names include `schema_validation`, `patch_policy_gate`,
  `spec_review`, `output_review`, and configured command checks.

Verification commands:

```bash
cargo test -p seaf-loop eval_report
cargo run -p seaf-cli -- eval run examples/local-loop/seaf.evals.yaml --goal-id local_agent_loop_mvp --patch-id test --json
```

Review notes for future eval work: loop eval mode validates the loop run and
ticket before creating logs, output files, or running configured shell checks.
Policy evidence is bound to the loop `run_id`; missing, malformed, mismatched,
or rejected policy decisions fail closed. Deterministic CLI loop runs record an
empty-patch no-op policy decision with `apply_requested = false` and
`applied = false`.

### P2-011 - Documentation and Mac setup guide

Status: pending

Objective: Make the local loop easy to run by a developer and easy to dispatch
to agents.

Allowed files:

- `docs/local-agent-loop.md`
- `docs/local-models/ollama-gemma4.md`
- `docs/loop-evals.md`
- `docs/security/local-agent-boundaries.md`
- `examples/local-loop/README.md`

Dependencies: P2-003, P2-008, P2-009.

Acceptance criteria:

- Docs contain one complete demo path from ticket to eval report.
- Docs explain what is local-only.
- Docs explain that model output is untrusted.
- Docs explain how to recover from failed runs.
- Mac setup includes `brew install ollama`, model pulls for
  `gemma4:e2b-mlx` and `gemma4:e4b-mlx`, `ollama serve`, and model check.

Verification commands:

```bash
pnpm format:check
```

### P2-012 - CI hardening

Status: pending

Objective: Keep the repo trustworthy as local agents start editing code.

Allowed files:

- `.github/workflows/ci.yml`
- `package.json`
- `pnpm-lock.yaml` only if package-manager behavior changes.
- Focused schema fixture checks, fake-loop integration tests, and forbidden
  patch fixture tests.
- No release signing, updater, auth, billing, or production deployment files.

Dependencies: P2-001, P2-002, P2-007, P2-008, P2-009.

Acceptance criteria:

- CI does not need Ollama.
- Fake loop e2e test runs in CI.
- Forbidden patch fixtures are tested.
- Generated examples are validated.
- CI covers Cargo tests, Rust formatting, Clippy, frozen pnpm install,
  Prettier format check, pnpm lint/typecheck/test/build, schema fixture checks,
  fake loop integration test, and forbidden patch fixture test.

Verification commands:

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
pnpm install --frozen-lockfile
pnpm format:check
pnpm lint
pnpm typecheck
pnpm test
pnpm build
```

## Context For Future Agents

Current branch:

```bash
git branch --show-current
# codex/seaf-foundation-agent-loop
```

Key current commands:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
pnpm format:check
pnpm lint
pnpm typecheck
pnpm test
pnpm build
```

Target Phase 2 demo commands:

```bash
ollama pull gemma4:e4b-mlx
cargo run -p seaf-cli -- model check --provider ollama --model gemma4:e4b-mlx
cargo run -p seaf-cli -- ticket validate examples/local-loop/tickets/add-health-command.yaml
cargo run -p seaf-cli -- loop run \
  --ticket examples/local-loop/tickets/add-health-command.yaml \
  --provider ollama \
  --model gemma4:e4b-mlx \
  --apply-patch \
  --max-iterations 1
cargo run -p seaf-cli -- loop status .seaf/loops/runs/<run_id>
cargo run -p seaf-cli -- eval run examples/local-loop/seaf.evals.yaml \
  --goal-id local_agent_loop_mvp \
  --patch-id <run_id> \
  --json
```

Existing docs to read before implementation:

- [Architecture](architecture.md)
- [Agent Loop](agent-loop.md)
- [MVP Backlog](mvp-backlog.md)
- [Development Roadmap](development-roadmap.md)
- [Threat Model](threat-model.md)
- [Forbidden Shortcuts](security/forbidden-shortcuts.md)

Development caution:

- Keep each slice narrow.
- Avoid parallel edits to `crates/seaf-cli/src/main.rs`.
- Do not weaken tests, evals, CI, policies, or forbidden-shortcut docs.
- Do not add dependencies unless the slice explicitly requires them and the
  reason is documented.
- Treat failed runs as useful artifacts; they should still leave durable
  diagnostics.
