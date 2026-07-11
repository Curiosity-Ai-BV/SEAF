# Production Use Implementation Plan

Date: 2026-07-11

Source roadmap: `docs/production-readiness-roadmap.md`

## Execution Protocol

Each slice is implemented by a fresh subagent using red-green-refactor. The
implementer must record the failing test and expected failure before production
code. A separate spec reviewer approves acceptance-criteria compliance, then a
separate quality reviewer approves correctness and maintainability. Open review
findings return to the implementer and are re-reviewed. The controller runs the
declared checks, updates the roadmap and `.seaf/loops/current/`, and creates one
logical commit only after both reviews approve.

No slice may weaken policy, eval, CI, context-exclusion, secret-handling, or
human-review controls. Model-modified code must not execute before human
approval. Commit, merge, release, and external-project promotion remain human
controlled.

After every accepted commit, the controller advances to the next
dependency-ready slice without waiting for another prompt. After interruption,
it reconstructs state from the roadmap, current progress, and append-only log.
Execution stops only for a failed required gate, a genuine authority decision,
or an external blocker recorded in all three tracking surfaces.

## Shared Definition Of Done

- The slice's acceptance criteria are encoded in tests or durable evidence.
- New behavior followed a witnessed RED -> GREEN -> REFACTOR cycle.
- The exact applicable commands from the gate matrix pass.
- Spec and quality reviews have no open findings.
- The roadmap status, current progress, and loop log describe exactly what was
  completed, verified, and remains pending.
- The commit contains one coherent slice and no unrelated cleanup.

## Verification Gate Matrix

Every command is run from the repository root. A skipped command is named with
its reason in the log and handoff; a mandatory unavailable command blocks the
slice rather than being silently waived.

| Gate           | Trigger                                                                   | Required commands                                                                                                                                                                                                                                                                             |
| -------------- | ------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Docs           | Any Markdown/JSON/YAML change                                             | `corepack pnpm format:check`; `git diff --check`                                                                                                                                                                                                                                              |
| Rust focused   | Any Rust behavior change                                                  | The slice-specific failing/passing test; `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `git diff --check`                                                                                                                                |
| Rust workspace | Shared contracts, state, loop runner, CLI, specs, or fixtures             | `cargo test --locked --workspace` plus Rust focused gate                                                                                                                                                                                                                                      |
| TypeScript     | `packages/**`, root JS package metadata, or lockfile                      | `corepack pnpm format:check`; `corepack pnpm lint:packages`; `corepack pnpm typecheck`; `corepack pnpm test`; `corepack pnpm build`; `git diff --check`                                                                                                                                       |
| Full repo      | Milestone exits, CI/release changes, external acceptance, or final review | `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --workspace`; `corepack pnpm format:check`; `corepack pnpm lint:packages`; `corepack pnpm typecheck`; `corepack pnpm test`; `corepack pnpm build`; `git diff --check` |
| Packaging      | Distribution or release slices                                            | `cargo package -p seaf-core --allow-dirty`; `cargo package -p seaf-models --allow-dirty`; `cargo package -p seaf-loop --allow-dirty`; `cargo package -p seaf-cli --allow-dirty`; packaged binary `--version`, `info`, and `doctor` smoke; Full repo gate                                      |

Release workflow slices additionally run their deterministic archive/checksum
contract tests and Prettier over the workflow. External tag, GitHub release,
Ollama, and pilot commands are mandatory only in the explicitly named evidence
slices; lack of authorization or environment keeps those slices pending.

## Dependency-Ordered Slices

### S0 - Establish The Execution Contract

Roadmap: program-wide.

Objective: make this file and the current loop tracker the shared source of
implementation context.

Acceptance criteria:

- U1-U11 are covered by bounded, ordered slices.
- Every slice has explicit tests/evidence, checks, docs updates, and a commit
  boundary.
- The roadmap contains live execution status.

Owned files: this plan, `docs/production-readiness-roadmap.md`, and
`.seaf/loops/current/`.

Verification: Prettier on changed docs and `git diff --check`.

Commit boundary: documentation and tracking only.

### M1-01a - Project Configuration And Input Digest Contracts

Roadmap: U1. Dependencies: S0.

Objective: define the typed configuration and LoopRun input-digest contracts
before changing CLI behavior.

Acceptance criteria:

- The smallest project configuration required now names a policy path, denies
  unknown fields, and rejects empty/unsafe values.
- LoopRun has typed required SHA-256 digests for effective ticket, policy, and
  config inputs; state creation and public schema/fixtures agree.
- Canonical serialization/digest helpers are deterministic and shared rather
  than CLI-local.

Likely seams: `seaf-core` models/validation/digest helpers, loop state creation,
public schemas/fixtures, and core/state tests.

RED: config validation/unknown-field tests, deterministic digest tests, and
LoopRun schema/fixture tests requiring all three digests.

Verification: focused core/state tests plus Rust workspace gate.

Docs/tracker: contract ownership and M1-01a status.

Commit boundary: contracts, schemas, fixtures, and state construction only; no
CLI discovery or snapshots.

### M1-01b - Authoritative Configuration Discovery And Snapshots

Roadmap: U1. Dependencies: M1-01a.

Objective: make explicit/discovered project configuration and policy drive new
provider runs instead of compiled defaults.

Acceptance criteria:

- Precedence is explicit policy override, then explicit/discovered config policy,
  then root `seaf.policy.json`; no authority fails closed.
- Explicit missing/invalid config, ambiguous input, unsafe path, or repository
  escape fails before workspace creation or provider calls.
- Config-relative paths resolve from the config directory and remain inside the
  Git repository.
- Canonical effective ticket, policy, and config snapshots are persisted under
  the run directory and their digests populate LoopRun.
- Custom project policy demonstrably changes patch gating for fake and mocked
  Ollama paths.

Likely seams: CLI loop args/preflight, provider-loop setup, input snapshots, and
CLI integration tests.

RED: custom-gating, zero-side-effect failure, relative/escape path, and
snapshot/digest-match tests.

Verification: focused CLI tests plus Rust workspace and Docs gates.

Docs/tracker: precedence/authority docs and M1-01b completion.

Commit boundary: new-run discovery/preflight/snapshots only; resume comparison
belongs to M1-02.

### M1-02 - Resume Configuration Integrity

Roadmap: U1. Dependencies: M1-01b.

Objective: bind resume to the exact authoritative inputs used at run creation.

Acceptance criteria:

- Resume verifies ticket, policy, config, repository identity, and digests.
- Same-path or same-ID replacement content cannot change a run contract.
- Mismatch fails before provider, candidate, or run-state mutation with an
  actionable start-a-new-run message.

Likely seams: resume preflight, run snapshots, validation, and CLI tests.

RED: mutated policy/config/repository-identity resume regressions.

Verification: focused resume tests plus workspace CLI tests, format, Clippy,
and diff check.

Docs/tracker: recovery contract and M1-02 status.

Commit boundary: resume integrity only.

### M1-03 - Validated Role Artifact Chain

Roadmap: U2. Dependencies: M1-02.

Objective: make each role consume the ticket and the validated outputs it
depends on.

Acceptance criteria:

- Role-specific request builders include the effective ticket/policy digests
  and only the necessary prior structured artifacts.
- Research -> analysis -> spec -> spec review -> development -> output review
  dataflow is explicit and persisted.
- Output review receives the exact normalized candidate patch and policy
  decision; missing or mismatched inputs fail closed.

Likely seams: role DTOs/schemas, provider runner, artifacts, runner state, and
provider-runner tests.

RED: per-role request assertions and a reviewer-exact-patch mismatch test.

Verification: role/provider/runner suites, workspace tests, format, Clippy, and
diff check.

Docs/tracker: artifact-flow documentation and M1-03 status.

Commit boundary: fixed role dataflow; no context expansion or eval execution.

### M1-04 - Bounded Additional Context

Roadmap: U2. Dependencies: M1-03.

Objective: allow a blocked role to request more repository context without
gaining direct tools or bypassing exclusions.

Acceptance criteria:

- A schema-validated request names additional paths and a reason.
- The orchestrator reuses existing path, secret, forbidden, symlink, and byte
  limits, records a new manifest, and caps request rounds.
- Unsafe, duplicate, or excessive requests fail or block deterministically.

Likely seams: role responses, context packer, provider runner, artifacts, and
focused tests.

RED: safe expansion, forbidden path, symlink escape, byte cap, and round-cap
tests.

Verification: context/role/provider suites, format, Clippy, and diff check.

Docs/tracker: context-expansion limits and M1-04 status.

Commit boundary: context-request protocol only.

### M1-05 - Isolated Candidate Workspace

Roadmap: U3. Dependencies: M1-04.

Objective: apply and inspect the candidate outside the user's source checkout.

Acceptance criteria:

- Provider runs create a dedicated candidate Git worktree bound to the starting
  repository and HEAD.
- Policy check and patch application target the candidate; the source checkout
  stays byte-for-byte unchanged on pass, block, failure, timeout, and resume.
- Candidate paths and HEAD/digests are persisted; cleanup is explicit and safe.

Likely seams: workspace/state, patch gate runner, CLI lifecycle, Git helpers,
and integration tests.

RED: real temporary-repository tests for source immutability and candidate
identity across failure/resume.

Verification: focused worktree/policy/CLI tests, full Rust workspace tests,
format, Clippy, and diff check.

Docs/tracker: candidate lifecycle and M1-05 status.

Commit boundary: isolated proposal/application only; no promotion or evals.

### M1-06 - Human Approval State

Roadmap: U3. Dependencies: M1-05.

Objective: require a human to approve the exact candidate before any
model-modified code executes.

Acceptance criteria:

- Run state explicitly represents `awaiting_human_review` and approved.
- Approval binds candidate patch digest, starting target HEAD, policy decision,
  and reviewer identity/time; stale or mismatched approval fails closed.
- Testing and promotion remain impossible in this slice.

Likely seams: core state models/schemas, CLI approval command, state machine,
and CLI/state tests.

RED: unapproved transition, stale HEAD, wrong digest, duplicate approval, and
successful exact approval tests.

Verification: core/state/CLI suites, full workspace tests, format, Clippy, and
diff check.

Docs/tracker: approval command/state and M1-06 status.

Commit boundary: approval evidence and state only.

### M1-07 - Integrated Testing And EvalReport

Roadmap: U4. Dependencies: M1-06.

Objective: make Testing and EvalReport deterministic loop steps over the exact
approved candidate.

Acceptance criteria:

- Testing consumes `ticket.eval.config` and both allowlists through the existing
  controlled runner inside the candidate workspace.
- Testing refuses missing human approval or mismatched candidate evidence.
- EvalReport persists logs and real policy evidence, binds run/ticket/patch,
  populates `LoopRun.eval_report_path`, and determines terminal eval state.
- Failed commands/evidence cannot produce `eval_passed` or promotion.

Likely seams: controlled command runner extraction, loop step runner, eval
builder, state/run contracts, and CLI integration tests.

RED: unapproved execution, candidate-only behavior, failed eval, missing policy
evidence, report binding, and no-op-removal tests.

Verification: eval/provider/CLI suites, full Rust tests, format, Clippy, and diff
check.

Docs/tracker: one-command flow and M1-07 status.

Commit boundary: Testing/EvalReport integration only.

### M1-08 - Promotion Integrity

Roadmap: U3. Dependencies: M1-07.

Objective: promote only the exact approved and evaluated candidate with a new
human confirmation.

Acceptance criteria:

- Run state distinguishes `eval_passed` and `promoted`.
- Promotion requires a fresh human confirmation bound to candidate digest,
  passing EvalReport, policy decision, current target HEAD, and clean target.
- Promotion applies the exact patch without committing; stale evidence fails
  before source mutation.

Likely seams: core state/schema, CLI promotion command, patch helpers, and
temporary-repository integration tests.

RED: missing fresh confirmation, failed eval, stale/dirty target, digest
mismatch, and successful exact promotion tests.

Verification: core/state/CLI suites, full workspace tests, format, Clippy, and
diff check.

Docs/tracker: promotion contract and M1-08 status.

Commit boundary: promotion only.

### M1-09 - Audited Recovery Operations

Roadmap: U5. Dependencies: M1-08.

Objective: inspect, revise, and rerun blocked/failed attempts without replacing
history.

Acceptance criteria:

- CLI inspect/revise/rerun-from-step operations preserve attempt artifacts.
  A revision creates a new immutable audited attempt, preserves prior input
  snapshots, rebinds every effective-input digest, and never edits history in
  place. Changing authoritative ticket/policy/config content still requires a
  new run under M1-02.
- Resetting a step clears all dependent approvals, evals, and promotion evidence.
- Invalid or unsafe reset targets fail before state mutation.

Likely seams: state transitions, runner, CLI recovery, and state/CLI tests.

RED: blocked rerun, downstream-evidence clearing, history preservation, and
invalid-reset tests.

Verification: state/CLI suites, full workspace tests, format, Clippy, and diff
check.

Docs/tracker: recovery commands and M1-09 status.

Commit boundary: recovery operations only.

### M1-10 - Atomic State And Run Locking

Roadmap: U5. Dependencies: M1-09.

Objective: prevent corrupt or concurrently mutated run state.

Acceptance criteria:

- Run state uses atomic replacement with durable same-directory temporary files.
- Exactly one mutating process can hold a per-run lock; stale-lock behavior is
  explicit and safe.
- Failed writes retain the last valid parseable state.

Likely seams: state/workspace persistence and fault-injection tests.

RED: concurrent mutation, partial-write, failed-rename, and stale-lock tests.

Verification: state/workspace suites, full workspace tests, format, Clippy, and
diff check.

Docs/tracker: persistence/lock behavior and M1-10 status.

Commit boundary: atomic persistence and locking only.

### M1-11 - Minimum Artifact Protection

Roadmap: U5. Dependencies: M1-10.

Objective: make local run artifacts safe enough for live provider use.

Acceptance criteria:

- Run directories/files use private permissions on supported platforms.
- Provider responses, prompts, and logs have enforced byte/storage caps.
- Configured and obvious secrets are redacted before persistence, with capped
  redaction output.

Likely seams: workspace/artifacts, provider persistence, shared redaction, and
focused tests.

RED: permission, oversize response, cumulative budget, and secret-leak tests.

Verification: workspace/provider/CLI suites, full workspace tests, format,
Clippy, and diff check.

Docs/tracker: artifact safety and M1-11 status.

Commit boundary: permissions, caps, and redaction only.

### M1-12 - Interruption Recovery Acceptance

Roadmap: U5 and Milestone 1 exit gate. Dependencies: M1-11.

Objective: prove safe restart across the complete reviewed lifecycle.

Acceptance criteria:

- Fault-injection tests interrupt patch, review, testing, report, and promotion
  boundaries and resume without duplication or source mutation.
- The focused Milestone 1 acceptance suite proves authoritative inputs, role
  dataflow, candidate isolation, approval, eval, promotion, and recovery.
- Roadmap/docs claim only the verified source-workspace path.

Likely seams: integration test harness, CLI tests, docs, and CI focused step.

RED: the new acceptance harness fails before boundary wiring/fixtures exist.

Verification: Milestone 1 acceptance suite, all Rust/TS gates, format, and diff
check.

Docs/tracker: Milestone 1 evidence and completion.

Commit boundary: integration/fault coverage and matching docs only.

### M2-01 - Generic Project Initialization

Roadmap: U6. Dependencies: M1-12.

Objective: bootstrap a stack-neutral external project.

Acceptance criteria:

- Default init generates editable policy, eval, ticket, provider/config, and
  ignore templates without SEAF-repo-specific commands.
- Named examples remain opt-in and existing-file refusal is atomic.

Likely seams: templates/core, init CLI, fixtures, and CLI tests.

RED: generic init in Rust and Node fixture repos plus atomic conflict tests.

Verification: CLI/core tests, template validation, full Rust tests, format,
Clippy, TS checks, and diff check.

Docs/tracker: bootstrap quickstart and M2-01 status.

Commit boundary: generic initialization only.

### M2-02 - Project Doctor

Roadmap: U6. Dependencies: M2-01.

Objective: diagnose project readiness without mutation.

Acceptance criteria:

- `seaf doctor` checks Git, config, candidate workspace, provider, and eval
  executables with human and JSON output.
- Checks are deterministic, actionable, and make no project/runtime changes.

Likely seams: doctor CLI/report models and CLI tests.

RED: doctor success/failure, JSON contract, and no-mutation tests.

Verification: CLI tests, full workspace tests, format, Clippy, and diff check.

Docs/tracker: doctor guide and M2-02 status.

Commit boundary: doctor only.

### M2-03 - Package Metadata And Version Identity

Roadmap: U7. Dependencies: M2-02.

Objective: make the CLI identifiable and Cargo-packagable.

Acceptance criteria:

- `seaf --version`, complete Cargo metadata, versioned internal dependencies,
  license, changelog, and supported-platform policy exist.
- Cargo package dry-runs and an installed-package smoke pass.

Likely seams: Cargo manifests, CLI metadata, license/changelog, and package
smoke tests.

RED/evidence: version and installed-package smoke fail before metadata changes.

Verification: Packaging and Full repo gates from the matrix.

Docs/tracker: install/version docs and M2-03 status.

Commit boundary: packaging metadata and identity only.

### M2-04 - Release Artifact Workflow

Roadmap: U7. Dependencies: M2-03.

Objective: reproducibly build checksummed macOS/Linux artifacts.

Acceptance criteria:

- A tag-gated, minimal-permission workflow builds supported binaries and
  checksums without publishing on ordinary CI.
- Deterministic scripts/tests validate archive naming, contents, and checksums.
- CI installs and smokes locally built release artifacts.

Likely seams: release workflow/scripts, package smoke tests, and release docs.

RED/evidence: deterministic artifact-contract tests fail before workflow/script
wiring.

Verification: deterministic artifact tests, Prettier on the workflow, and the
Full repo gate from the matrix.

Docs/tracker: release procedure and M2-04 status.

Commit boundary: release artifact automation only.

### M2-05 - Human-Authorized Tagged Prerelease

Roadmap: U7. Dependencies: M2-04.

Objective: prove the actual tagged, checksummed distribution path.

Acceptance criteria:

- With explicit user authorization, a preview tag produces downloadable
  macOS/Linux artifacts and checksums.
- Downloaded artifacts install and pass `--version`, `info`, and doctor smoke.
- Evidence and URLs are recorded; without authorization this slice stays
  pending and Milestone 2 cannot complete.

Likely seams: external GitHub release state and release evidence docs.

Evidence: successful tagged workflow and downloaded-artifact smoke.

Verification: checksums, install smoke, clean repo status, and workflow result.

Docs/tracker: release evidence and M2-05 status.

Commit boundary: evidence/docs after the separately authorized external action.

### M2-06 - Packaged External Golden Path

Roadmap: U8. Dependencies: M2-05.

Objective: continuously prove adoption outside the SEAF source tree.

Acceptance criteria:

- A minimal external fixture is initialized by the packaged CLI and exercises
  candidate creation, approval, controlled eval, rejection, interruption,
  resume, and promotion evidence with the fake provider.
- CI validates every artifact and proves failed runs leave the source unchanged.
- README and loop docs match the executed fake-provider commands.

Likely seams: external fixture, CI, acceptance scripts/tests, README, and loop
docs.

RED: acceptance job/script fails on the pre-U8 product path before fixture
wiring.

Verification: packaged golden path and Full repo gate from the matrix.

Docs/tracker: tested quickstart and M2-06 status.

Commit boundary: external acceptance fixture, CI, and matching docs.

### M2-07 - Executed Ollama Acceptance

Roadmap: U8 and Milestone 2 exit gate. Dependencies: M2-06.

Objective: execute the full packaged acceptance scenario with Ollama.

Acceptance criteria:

- The packaged CLI completes initialization, candidate creation, human
  approval, candidate-native eval, interruption/resume, and explicitly approved
  promotion locally with Ollama.
- Rejection/failure leaves the source unchanged; sanitized artifact evidence is
  retained.
- Milestone 2 remains pending if the model/environment is unavailable or any
  step is only a checklist.

Likely seams: local acceptance harness, sanitized evidence, and matching docs.

RED/evidence: the Ollama scenario is executed, not simulated; product defects
start as failing regressions in separately numbered remediation slices.

Verification: packaged Ollama acceptance plus Full repo gate from the matrix.

Docs/tracker: executed evidence and Milestone 2 completion.

Commit boundary: Ollama evidence and any docs only; defects get separate slices.

### M3-01 - Typed Durable Loop Contracts

Roadmap: U9. Dependencies: M2-07.

Objective: type the artifact set exposed by the supported loop.

Acceptance criteria:

- `PolicyDecision` is a shared typed contract and LoopRun no longer stores
  arbitrary policy maps.
- Ticket, Policy, LoopRun, PolicyDecision, and EvalReport have Rust/schema drift
  tests.

Likely seams: core models/validation, loop policy/eval, specs/fixtures, and drift
tests.

RED: typed policy-decision and schema-drift tests.

Verification: core/loop/CLI suites, schema fixtures, and Full repo gate from the
matrix.

Docs/tracker: typed contract ownership and M3-01 status.

Commit boundary: typed contracts and drift tests only.

### M3-02 - Artifact Format Versions And Migration

Roadmap: U9. Dependencies: M3-01.

Objective: version durable files and define compatible upgrade behavior.

Acceptance criteria:

- Supported artifacts carry explicit format versions.
- Older supported artifacts have tested reads/migrations; unsupported future
  versions fail closed without mutation.
- Migration is atomic, idempotent, and preserves an auditable backup/result.

Likely seams: core models/validation, state persistence, fixtures, and migration
CLI/helpers.

RED: old/current/future version, idempotence, and failed-migration tests.

Verification: core/state/CLI suites, fixture checks, and Full repo gate from the
matrix.

Docs/tracker: compatibility/migration policy and M3-02 status.

Commit boundary: artifact versioning and migration only.

### M3-03 - Retention And Audited Purge

Roadmap: U9. Dependencies: M3-02.

Objective: bound durable storage without deleting active evidence.

Acceptance criteria:

- Storage budgets and retention policy are explicit.
- Dry-run and purge preserve active/locked runs and emit an auditable deletion
  summary.
- Interrupted purge is safely repeatable.

Likely seams: workspace inventory, CLI maintenance commands, and purge tests.

RED: active-run protection, dry-run, budget, idempotence, and audit-summary tests.

Verification: workspace/CLI suites and Full repo gate from the matrix.

Docs/tracker: retention/purge guide and M3-03 status.

Commit boundary: retention and purge only.

### M3-04 - Two-Repository Pilot Evidence

Roadmap: U10. Dependencies: M3-03.

Objective: prove the complete acceptance scenario in two user-approved real
repositories and turn each observed product defect into a regression.

Acceptance criteria:

- Each repository independently completes install, generic init, approval,
  candidate-native eval, interruption/resume, and explicitly approved promotion.
- At least five aggregate tickets include approval, policy rejection, eval
  failure, and interruption/resume across two stacks.
- Upgrade/recovery behavior is executed and documented in both repositories.
- Setup time, applicability, corrections, eval reliability, recovery, and
  workarounds are recorded without committing target changes unless authorized.
- Every defect opens a new numbered remediation slice with its own failing test,
  reviews, commit, and tracker row before the pilot can complete.

Likely seams: user-approved pilot repositories and dynamic SEAF remediation
slices identified by evidence.

RED/evidence: sanitized per-repository scenario logs and failing regressions for
every defect.

Verification: Full repo gate from the matrix plus user-approved target-native
checks for every pilot ticket.

Docs/tracker: per-repository pilot report, dynamic remediation rows, and M3-04
status.

Commit boundary: final sanitized pilot evidence only; every defect is a separate
slice/commit. Target modification/promotion requires explicit user approval.

### M3-05 - Supported Preview Readiness

Roadmap: U11. Dependencies: M3-04.

Objective: make the reviewed branch ready for an explicit supported preview.

Acceptance criteria:

- Compatibility notes, security reporting, support boundary, release procedure,
  and honest experimental-surface labels are complete.
- A release-candidate build passes both pilot scenarios, packaged golden path,
  the Full repo gate, and clean-tree verification.
- A final independent cross-milestone review finds no open safety, data-loss,
  acceptance, or documentation issues.

Likely seams: governance/release docs, README, workflow metadata, roadmap, and
final acceptance evidence.

RED/evidence: documentation/package assertions fail before missing metadata is
added; final reviewer checks every roadmap exit gate.

Verification: all workspace checks, package/release dry-runs, golden path,
pilot evidence, format, diff check, and clean status.

Docs/tracker: mark U1-U10 complete with evidence and record U11 as awaiting
authorized publication.

Commit boundary: release-readiness docs/metadata only.

### M3-06 - Human-Authorized Preview Publication

Roadmap: U11 and Milestone 3 exit gate. Dependencies: M3-05.

Objective: publish and verify the supported preview through an explicit external
action.

Acceptance criteria:

- Explicit user authorization identifies the version/tag and release channel.
- The reviewed commit is clean, all final gates pass, and the tag triggers the
  approved checksummed artifacts without branch drift.
- Downloaded artifacts pass checksum, install, version, info, doctor, and
  packaged golden-path smoke.
- Release URLs, workflow evidence, compatibility/support boundaries, and any
  deviations are recorded before U11 and the roadmap are marked complete.

Likely seams: external GitHub tag/release state and final evidence docs.

Evidence: authorized tag, successful release workflow, downloaded-artifact
verification, and final clean status.

Verification: Packaging and Full repo gates from the matrix plus published
artifact checks.

Docs/tracker: publication evidence, U11 completion, and Milestone 3 completion.

Commit boundary: post-publication evidence only. Without explicit authorization
this slice and the goal remain pending rather than being reported complete.
