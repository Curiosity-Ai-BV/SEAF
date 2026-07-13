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

### M1-03a - Validated Early Role Artifact Chain

Roadmap: U2. Dependencies: M1-02.

Status: complete on 2026-07-11. M1-03b is also complete; M1-04 is active.

Objective: make research, analysis, spec creation, and spec review consume and
persist their exact validated prerequisites.

Acceptance criteria:

- Role-specific request builders include the effective ticket/policy digests
  and only the necessary prior structured artifacts.
- Research receives the effective TicketSpec; analysis receives validated
  research; spec creation receives validated research and analysis; spec review
  receives the exact proposed spec.
- Every parsed structured response is canonically persisted as a role artifact
  with a digest before the next role request is built.
- Resume loads and verifies required prior role artifacts; missing, tampered,
  wrong-role, or wrong-run artifacts fail before provider calls or mutation.

Likely seams: role DTOs/schemas, provider runner, artifacts, runner state, and
provider-runner tests.

RED: per-role request/artifact assertions plus missing/tampered/wrong-role resume
tests.

Verification: role/provider/runner suites, workspace tests, format, Clippy, and
diff check.

Docs/tracker: early artifact-flow documentation and M1-03a status.

Commit boundary: research through spec-review dataflow only; developer/output
review, context expansion, and eval execution remain excluded.

Implemented flow: each early request carries the exact effective ticket, run
ID, and all effective input digests. Its `prerequisites` object is limited to
research for analysis, research plus analysis for spec creation, and the
proposed spec for spec review. Canonical validated response envelopes bind the
run, step, role, response digest, artifact path, and artifact digest. Resume
revalidates canonical bytes, both digest layers, identity fields, and the
role-specific response schema before context packing or any durable mutation.

### M1-03b - Development And Exact Output Review Evidence

Roadmap: U2. Dependencies: M1-03a.

Status: complete on 2026-07-11. M1-04a is complete and M1-04b is active.

Objective: bind development and output review to the approved specification and
the exact normalized policy-gated patch.

Acceptance criteria:

- Development receives the exact approved spec artifact and its digest.
- The developer response is normalized and policy-gated once; the resulting
  candidate patch, patch digest, and policy decision are canonically persisted.
- Output review receives those exact persisted values, not initial repository
  context or reparsed model text.
- Missing, tampered, wrong-run, digest-mismatched, or substituted patch/policy
  evidence fails before output-review provider calls or run mutation.
- Resume verifies and reuses the same persisted development evidence.

Likely seams: developer/reviewer DTOs, provider runner, patch gate artifacts,
runner state, and provider/CLI tests.

RED: approved-spec request, reviewer exact-patch/policy request, substitution,
tamper, and resume regressions.

Verification: role/provider/policy/runner suites plus Rust workspace and Docs
gates.

Docs/tracker: exact output-review evidence and M1-03b completion.

Commit boundary: development/output-review dataflow only; no context expansion,
candidate worktree, approval, or eval execution.

Implemented flow: Development receives the exact canonical SpecCreation and
approving SpecReview envelopes, paths, and digests plus the bounded initial
repository context it still needs to construct a patch. It does not receive
Research or Analysis bodies. Every validated DeveloperResponse is persisted;
patch proposals additionally produce typed canonical DevelopmentEvidence that
binds the exact gated patch, digest, normalized changed paths, and exact
PolicyDecision. OutputReview is built only from verified DevelopmentEvidence,
approved-spec identities, the run ID, and all input digests. Resume verifies
both downstream artifact envelopes and the run's exact policy decision before
mutation or provider calls, and never reruns the patch gate at OutputReview.
DevelopmentEvidence independently reparses the exact patch and binds its
normalized paths to both evidence and policy decision fields. Provider-backed
gating is proposal-only: `apply_requested` preserves ticket intent, while
`applied` remains false and the source checkout is never modified before the
M1-05 isolated candidate workspace exists.

### M1-04a - Context Request Contract

Roadmap: U2. Dependencies: M1-03b.

Status: complete on 2026-07-11. M1-04b is active.

Objective: define a strict structured request for additional repository context
without granting model tools.

Acceptance criteria:

- A typed ContextRequest names a bounded nonempty list of safe relative paths
  and a nonempty reason, denying unknown fields. Requests contain 1-8 unique
  normalized repository-relative paths, and reasons are capped at 1,024
  Unicode scalar values.
- Agent and Developer responses require exactly one ContextRequest when status
  is `needs_context` and forbid it for passed/blocked/patch-proposed statuses.
- Role response JSON schemas and runtime validation agree on presence,
  cardinality, duplicate, absolute/traversal/backslash/control-character, and
  empty-reason rules.

Likely seams: role response DTOs/parsers/schemas, fixtures, and focused tests.

RED: valid needs-context plus missing/unexpected request, unsafe/duplicate/path
count, empty reason, and schema parity tests.

Verification: role response suites plus Rust workspace and Docs gates.

Docs/tracker: request contract and M1-04a status.

Commit boundary: response contract/schema only; no repacking, retry, provider
round, or manifest behavior.

Implemented flow: Researcher, Analyzer, SpecWriter, and Developer responses now
carry an optional typed ContextRequest. Runtime validation and their handcrafted
schemas require it only for `needs_context`, reject missing or unexpected
requests, and agree on path count, uniqueness, normalized relative path,
control-character, reason, and unknown-field constraints. Reviewer responses
remain unchanged. The ProviderStepRunner retains the existing single-round
semantics: a validated `needs_context` response still blocks without repacking
or retrying until M1-04b.

### M1-R01 - Stabilize Descendant Pipe Cleanup Regression

Roadmap: execution-gate remediation. Dependencies: M1-04a.

Status: complete on 2026-07-11. M1-04b is active.

Objective: make the existing descendant-pipe cleanup regression reliably prove
the intended behavior without racing its own eval timeout.

Acceptance criteria:

- The test still fails if SEAF waits for the detached pipe-owning descendant.
- The eval command timeout is comfortably above normal direct-child completion,
  while the assertion threshold remains comfortably below descendant lifetime.
- At least 20 consecutive focused executions pass locally.
- No production timing constant changes unless a new failing behavioral test
  proves the implementation itself is wrong.

Likely seams: `crates/seaf-cli/tests/cli.rs` helper/test only.

RED/evidence: record repeated full-gate failures near 1,000ms and demonstrate
the old test can fail under harmless scheduling delay.

Verification: 20 focused repetitions plus Rust workspace and Docs gates.

Docs/tracker: remediation evidence and M1-R01 completion.

Commit boundary: timing-regression design only; no unrelated eval behavior.

Implemented flow: the regression now gives the direct child a bounded 1,200ms
scheduling delay after its detached pipe-owning descendant is ready. The eval
timeout is 4,000ms and the elapsed ceiling is 3 seconds, while the detached
descendant has an 8-second safety lifetime. A stop sentinel and required exit
marker terminate and verify descendant cleanup after every execution. The
production 250ms drain grace and eval behavior remain unchanged.

### M1-04b - Bounded Context Expansion Orchestration

Roadmap: U2. Dependencies: M1-R01.

Status: complete on 2026-07-11. This work is split into M1-04b1 and M1-04b2a through M1-04b2c
so context data, durable exchange state, live orchestration, and recovery can be
reviewed independently.

Objective: satisfy validated ContextRequests through the orchestrator without
direct model tools or safety-boundary bypass.

#### M1-04b1 - Additive Context Expansion Artifact

Status: complete on 2026-07-11. M1-04b2a through M1-04b2c are also complete;
M1-05 is active.

Objective: create one safe, canonical, immutable expansion from a validated
ContextRequest without changing provider retry or LoopRun behavior.

Acceptance criteria:

- Reuse the existing normalized-path, default-exclude, ticket-forbidden,
  policy-forbidden, repository-boundary, symlink, UTF-8, per-file, and
  total-byte controls.
- Treat expansion as all-or-nothing for unsafe or unavailable new paths.
  Mixed already-loaded and safe new paths may succeed with only the new files;
  duplicate-only, missing, directory, binary, or zero-useful-byte requests fail
  deterministically without writing an artifact.
- Enforce the cumulative byte budget across the initial context and all prior
  expansions. Per-file or remaining-budget truncation stays explicit, but an
  expansion that would omit a requested new file entirely fails atomically.
- Persist exact included content plus normalized paths, source/included sizes,
  truncation flags, and source digests. Canonical bytes bind schema version,
  run, step, role, step attempt, context round, the validated request/reason,
  the immutable initial audited provider-request path/digest,
  previous-expansion digest, limits, excluded loaded paths, and prior/resulting
  cumulative totals. The mutable content-free initial context manifest is not
  the byte authority.
- Canonical path ordering makes semantically equivalent requests deterministic.
  A flat artifact name includes step, attempt, and round; creation is
  create-only. Replaying byte-identical canonical content returns the existing
  identity for idempotent recovery; an existing different file is tampering,
  never an overwrite. Previously accepted expansion bytes are reconstructed
  from these artifacts, never reread from a changed live repository.
- The existing mutable initial `context-manifest.json`, LoopRun schema, provider
  call count, and single-round blocking behavior remain unchanged in this
  slice.

Likely seams: `context.rs`, a focused canonical expansion artifact/codec,
workspace create-only writing, exports, and context tests.

RED: safe single/multi-file expansion; ordering determinism; mixed loaded/new;
duplicate-only; unsafe, forbidden, symlink escape, missing, directory, binary,
and atomic mixed failure; UTF-8/per-file/cumulative byte edges; create-only
collision; digest/content/identity tamper; and live-repository substitution.
Include byte-identical idempotent replay and different-content collision cases.

Verification: focused context/artifact tests plus Rust workspace and Docs gates.

Docs/tracker: artifact contract and M1-04b1 completion.

Commit boundary: additive expansion primitive and immutable artifact codec only;
no provider retry, run-state field, CLI change, candidate workspace, approval,
eval, or promotion.

Implemented flow: a validated request is normalized into deterministic path
order, checked atomically against the same repository path, exclusion,
forbidden-pattern, symlink-boundary, UTF-8, per-file, and cumulative-byte
controls, then persisted as one canonical version-1 artifact. The artifact
contains the exact accepted bytes and file metadata, immutable initial request
audit identity, prior expansion link, excluded already-loaded paths, limits,
and prior/result totals. Prior artifacts are verified recursively and supply
their accepted bytes without rereading changed repository files. Trusted
recovery requires `load_context_expansion` with a caller-supplied artifact path
and digest; M1-04b1 never adopts an unreferenced existing final artifact or
self-derives its authority. Creation always rebuilds canonical bytes from the
current live inputs, then publishes from a synced same-directory temporary file
through an atomic no-replace link. Concurrent identical publishers converge;
changed live bytes or a canonical existing forgery collide, and partial
temporary files are never final artifacts. Source files are opened once after
component checks, rebound to the current in-repository file identity, then
streamed through full-file hashing and UTF-8 validation while retaining only
the bounded included prefix. Symlinked parents/targets, noncanonical JSON, or
identity/link/digest mutations fail.
Provider call count, LoopRun, CLI, and the initial context manifest are
unchanged. This slice derives and verifies the exact initial prompt audit path
and binds the complete caller-supplied initial loaded-path/byte metadata;
M1-04b2a now provides authoritative reconciliation of those expected values to
a structured provider-request audit record and persistence of the trusted
expansion identity used for recovery.

#### M1-04b2a - Durable Context Exchange Contract

Status: complete on 2026-07-11. M1-04b2b and M1-04b2c are also complete;
M1-05 is active.

Objective: define authoritative, backward-compatible state and immutable audit
records for ordered provider exchanges without adding retry behavior.

Acceptance criteria:

- Add a versioned canonical exchange record binding run, step, role, step
  attempt, exchange kind/index, previous-record digest, request path/digest,
  response path/digest when present, optional expansion path/digest, and parsed
  outcome. Flat immutable names always include step attempt and exchange index.
- LoopRun stores authoritative ordered record path/digest references and counts;
  filesystem scanning alone is not authoritative. Runtime and schema validation
  reject gaps, reorderings, identity mismatches, invalid pairings, and malformed
  digests.
- New LoopRun fields use safe empty defaults so completed legacy runs still
  load. Existing fixtures and public schema remain in parity; unknown fields
  still fail closed.
- Provide create-only, byte-identical-idempotent request, response, expansion,
  and exchange-record writers. Different existing bytes are tampering. A staged
  record not yet referenced by LoopRun has an explicit inspectable state for
  later reconciliation; this slice does not adopt it automatically.
- Add the smallest persistence protocol needed for a caller to durably append
  one verified reference before continuing. Do not call the provider or change
  single-round behavior in this slice.

Likely seams: shared LoopRun models/schema/fixtures, artifact naming/writers,
state validation and append protocol, exports, and state/artifact tests.

RED: canonical record/digest; every identity/link/pairing mutation; ordered
append; duplicate index; byte-identical replay; different-content collision;
staged-but-unreferenced classification; safe legacy defaults; schema/runtime
parity; and no change to provider call count.

Verification: core contract, state, artifact, and provider-regression suites plus
Rust workspace and Docs gates.

Docs/tracker: durable exchange contract and M1-04b2a completion.

Commit boundary: exchange/state contract only; no context retry, cap enforcement,
resume reconciliation, CLI change, or later-milestone behavior.

Implemented flow: the ledger contract and API can represent each logical
provider call as an immutable request record followed by an immutable response
record; the live provider runner intentionally remains ledger-empty in this
slice. Version-1 canonical records bind the run, step, exact provider role,
step attempt, logical exchange index and kind, optional distinct context round,
run-wide previous-record digest, request and response identities, the trusted
M1-04b1 expansion identity when required, and the exact parsed role outcome.
Response files are canonical typed audits containing the complete
`ModelResponse` or `ModelError`; stage and load derive the outcome by running
model content through the existing exact role parser, so callers cannot choose
the recorded outcome. Parse/schema failures become `invalid_response` and
provider failure envelopes become `provider_failure`. Only
`RoleResponseError::InvalidJson` is JSON-repair eligible; schema, role,
reviewer-decision, and context-contract invalidity are terminal. Role/outcome
compatibility and phase invariants fail closed. `needs_context` may advance only
to the next context retry round, malformed JSON permits exactly one JSON repair,
and successful role-specific outcomes alone may advance to the exact next
provider step. Blocked, failed, invalid, change-requested, and rejected results
cannot bypass their chain by starting another step or attempt.

`LoopRun.provider_exchange_records` is the ordered authority and defaults to an
empty list for legacy runs; no duplicate count is stored. The closed JSON
Schema enforces structural identity conditionals across step/role/path stem,
kind/path, phase/path, and context-round presence. Rust runtime and state
validation enforce ordered gaps, reorderings, global links, role outcomes, and
bound-file equality; those sequence guarantees are not attributed to JSON
Schema. Request, response, and record names include step attempt, exchange
index, kind, and phase where applicable, so they cannot collide with the
ordinary single-round files. The M1-04b1 publisher was extracted as the shared
synced atomic create-only primitive; byte-identical replay converges and
different bytes, symlinks, or non-files are tampering. Expansion bytes are
never duplicated: context records reference the exact existing M1-04b1
path/digest and explicit context round. JSON repair inherits either the exact
round and expansion of the invalid context response or neither from an initial
exchange, and it never consumes another context round.

A verified unreferenced record is classified as staged, while a conflicting
authoritative identity is rejected. Because another provider call depends on a
durable ledger head, this slice pulls forward a narrow provider-exchange lock
and atomic run-state publication: the stable real lock file is held while state
is reloaded and verified, the new state is written and synced to a unique
same-parent temporary file, the file is atomically replaced on macOS/Linux, and
the parent is synced. Concurrent stale-head appenders reject without losing an
update, and pre-publication failure leaves the old `run.json` valid. M1-10 still
generalizes atomic replacement and per-run locking to every other state
mutation. Loading a run re-verifies its entire authoritative chain. This slice
does not scan or auto-adopt staged records and makes no provider call or retry;
M1-04b2b owns live orchestration and cap enforcement.

The narrow lock is a cooperative concurrency guarantee between SEAF processes,
not an adversarial same-user filesystem boundary. Preexisting symlink and
non-file lock paths fail closed, and lock identity is rechecked immediately
before publication as defense in depth, but a hostile process with permission
to unlink or replace run-directory entries remains outside this slice. M1-10
will generalize locking, and M1-11 private artifact permissions will strengthen
that threat boundary.

#### M1-04b2b - Bounded Live Context Orchestration

Status: complete on 2026-07-11. M1-04b2c is also complete and M1-05 is active.
Dependencies: M1-04b2a.

Objective: execute bounded same-role context retries with every exchange durable
before the next provider call.

Acceptance criteria:

- A validated `needs_context` response durably records its provider response,
  canonical expansion artifact, state reference, and next provider request
  before the next call. Every repair request/response in every round follows the
  same ordering.
- Retry the same role with the original audited input and ordered expansion
  chain. Preserve the complete authoritative ticket even when its metadata
  names a context path; exact-once means each content-bearing initial or
  additive file entry appears once in the combined repository-context and
  expansion payload. Initial bytes come from the verified provider-request
  audit and expansion bytes from verified artifacts, never a fresh repository
  read.
- Permit at most two accepted expansions per logical step across all attempts
  and eight per run across all steps and attempts. Initial role calls and JSON
  repairs are audited exchanges but consume zero expansion rounds. Unit tests
  cover exact, one-over, cross-role, and cross-attempt boundaries.
- Unsafe, unavailable, duplicate-only, excessive, and cap-exhausted requests
  become terminal `Blocked` with denial evidence. Provider/audit failures become
  terminal `Failed` with failure evidence when the run-state store remains
  writable. If a durable write itself fails, return a clear error, perform no
  further provider call or mutation, and leave the staged state for M1-04b2c
  reconciliation; do not claim evidence that could not be written.
- A terminal valid role response completes the logical step exactly once. No
  live outcome remains unexplained as `Running` when durable state is writable.

Likely seams: ProviderStepRunner, a small StepRunner/LoopRunner exchange hook,
context expansion integration, state transitions, and provider/state tests.

RED: fake-provider callbacks inspect on-disk ordering before each call; same-role
prompt chain; repair plus context; write/provider failure boundaries; denial
outcomes; exact/over step and run caps including another attempt; and final
completion once.

Verification: focused context/provider/state suites plus Rust workspace and Docs
gates.

Docs/tracker: live round behavior/caps and M1-04b2b completion.

Commit boundary: fresh-run bounded orchestration only; no resume/rerun/CLI
recovery, candidate workspace, approval, eval, or promotion.

Implemented flow: `LoopRunner` now hands the exact step attempt to the provider
runner and imports only the verified append-only exchange-reference suffix
before it can finish or fail the step, so its older in-memory `LoopRun` cannot
erase durable exchange references and the step runner cannot replace unrelated
run state. Fresh provider calls publish and append the immutable
request record before invocation, then publish the canonical full
response/failure audit, derive its classification inside the bound response
record seam, and persist that response record before returning the
classification to orchestration. Callers never supply the outcome. The same
ordering applies to the one allowed malformed-JSON repair in the initial or any
context round.

A valid `needs_context` response creates the canonical M1-04b1 expansion, then
reconstructs the retry from the exact verified initial exchange request plus
the ordered verified expansion chain. The complete authoritative ticket stays
in the original role input; each content-bearing initial or added file entry
appears once in the combined repository-context and expansion payload, and no
accepted expansion is reread from the live repository. Legacy M1-04b1 prompt
identities remain loadable, while fresh b2b expansion artifacts bind the
initial exchange request identity. Context retry request records carry the
exact expansion identity before another provider call.

Only durable context-retry request records count as accepted expansions. The
runner enforces two per logical step across attempts and eight per run across
roles; initial and repair exchanges consume neither cap. Unsafe, unavailable,
duplicate-only, byte-exhausted, and cap-exhausted requests finish `Blocked`
with canonical denial evidence. Provider failures and invalid response/audit
outcomes finish `Failed` with canonical failure evidence when normal state
writes remain available, including post-response interpretation or patch-gate
failures. Source-unavailable errors are distinct from trusted-audit safety
failures and immutable publication safety failures, collisions, and I/O. A
trusted-audit failure never becomes a user context denial. A durable write
failure returns immediately, makes no later provider call or terminal claim,
and leaves the authoritative prefix plus any staged artifact for M1-04b2c.

After initial workspace/run creation, every ordinary `LoopRunner` step-state
publication uses the same narrow exchange lock and atomic writer. It reloads
the current run under lock and requires the intended exchange vector, including
an expected empty vector, to match exactly before publishing. A concurrent
cooperative first request or later suffix therefore cannot be overwritten by
an older in-memory `LoopRun`. M1-10 still owns comparison and coordination for
general non-ledger state changes; this seam preserves only provider-exchange
history.

M1-04b2c extends this behavior through verified resume and explicitly
authorized rerun. It reconciles staged exchange chains before provider
preparation and restores the same bounded orchestration without resetting caps.

#### M1-04b2c - Context Round Recovery And CLI Integration

Status: complete on 2026-07-11. M1-05 is active. Dependencies: M1-04b2b.

Objective: verify or reconcile interrupted exchange chains and preserve caps
through resume, rerun, and real CLI entrypoints.

Acceptance criteria:

- Before another provider call, resume verifies authoritative ordered references
  and reconciles or rejects every crash cut: initial response, expansion
  artifact, retry request, retry response, repair exchange, staged record, and
  run-state head update. Only byte-identical, correctly linked staged content may
  be adopted; missing, orphaned, reordered, substituted, or digest-invalid data
  fails before provider invocation or mutation.
- Resume and `rerun_from` preserve both caps: two accepted expansions per logical
  step across all attempts and eight per run. Earlier attempts are immutable and
  new attempts never overwrite their names or reset counts.
- Legacy M1-04a runs already terminal on `needs_context` do not silently enter
  the protocol. They require an explicit audited rerun; existing calls remain
  immutable history but consume zero expansion rounds.
- CLI start/resume/rerun tests prove the same guarantees, including repository
  changes after the first exchange, exact-byte reconstruction, cap exhaustion,
  and request/response/expansion/record tampering.
- Recovery failures use the M1-04b2b outcome rules and never make another
  provider call from ambiguous durable state.

Likely seams: state/workspace reconciliation, ProviderStepRunner preparation,
rerun handling, CLI provider flow, and provider/state/CLI integration tests.

RED: each crash cut; every identity/digest/link mutation; repository-byte
substitution; resume and rerun at both cap boundaries; legacy blocked runs;
immutable attempt naming; and CLI start/resume/rerun paths.

Verification: provider/state/CLI suites plus Rust workspace and Docs gates.

Docs/tracker: recovery behavior and complete M1-04b status.

Commit boundary: M1-04b recovery/CLI integration only; no candidate workspace,
approval, eval, promotion, or general M1-09 recovery operations.

Implemented flow: resume first validates every authoritative request, response,
expansion, and record byte. Under the narrow exchange lock it scans flat
exchange-family files, computes at most one uniquely linked staged-record
suffix, validates the complete prospective chain and all bound files, rejects
any raw orphan or ambiguity, then publishes the reconciled vector in one atomic
run-state replacement. A standalone expansion has no trusted digest and is
rejected; a staged retry record may bind and adopt its exact expansion digest.

The runner resumes at the durable request or response phase. It never repeats a
durable response, and a request phase reuses the exact audited ModelRequest.
Malformed-JSON repair requests and staged repair responses follow the same
path. A durable terminal response closes the step without another provider
call. Provider failures, invalid responses, context denials, audit failures,
and unwritable state retain the M1-04b2b taxonomy and audit-before-control
ordering.

Fresh initial requests contain a closed metadata-only
`repository_context_authority` next to the single human-readable
content-bearing context payload. The request digest therefore binds paths,
source and included digests and byte counts, truncation, limits, exclusions,
warnings, and exact included content without duplicating that content. Recovery
cross-checks the readable payload against this authority and reconstructs the
original bundle. Later rounds use only this bundle and referenced expansion
artifacts; changed live initial or accepted expansion sources are never reread.
Context-free initial roles such as OutputReview legitimately recover with no
context authority.

Conventional prompt cuts before the first exchange request reuse only a
byte-identical exact attempt. Skipped, stale, substituted, symlinked, or
unauthorized prompts fail before an exchange write. Every new exchange group
uses the exact next durable attempt. Reconciliation checks every initial
exchange against its exact conventional prompt before publishing a staged
suffix. Explicit rerun writes the canonical previous-head authorization and
the reset run state in one exchange-locked transaction; an interrupted
pre-publication attempt can retry the identical authorization without a stale
collision. Replay rechecks that authorization, including context-free or
first-ledger attempt-two cases. Earlier attempt files are create-only or byte-
identical and are never overwritten.

Both caps are recomputed from all durable context-retry request records, so
resume and rerun cannot reset them. Empty-ledger incomplete runs enter audited
execution, while terminal legacy M1-04a `needs_context` runs remain inert until
an explicit rerun. The CLI exposes that narrow operation as
`seaf loop resume --rerun-from <provider-step>`; broader inspect/revise recovery
remains M1-09.

### M1-05 - Isolated Candidate Workspace

Status: complete on 2026-07-12. Split into M1-05a lifecycle contract and
M1-05b provider/CLI integration so the durable identity boundary could be
reviewed independently from patch-gate mutation. M1-06 is active.

### M1-05a - Candidate Workspace Lifecycle Contract

Status: complete on 2026-07-11; independently approved by spec and quality
review and accepted by the full repository gate.

Objective: define and prove the candidate worktree identity and lifecycle
before any provider path can use it.

Acceptance criteria:

- A detached candidate is created at a deterministic absolute path outside the
  source checkout, at the exact authoritative HEAD and tree, with repository
  checkout hooks disabled.
- Closed typed state and the LoopRun schema bind the source root, Git common
  directory, repository identity digest, starting/candidate HEAD and tree,
  empty pre-apply diff digest, and active/cleaning/cleaned lifecycle. Applied
  patch identity and the candidate tree transition belong to M1-05b.
- Creation is crash-idempotent only for the exact registered, clean candidate;
  resume validation rejects missing, substituted, symlinked, wrong-repository,
  attached-HEAD, wrong-HEAD/tree, staged, unstaged, ordinary/ignored untracked,
  executable-mode, or digest-tampered candidates. Repository hooks, filters,
  fsmonitor, diff helpers, and Git redirection/config injection cannot execute
  during creation or validation; Git replace refs cannot substitute the bound
  commit, tree, or blobs.
- Cleanup reads the authoritative persisted LoopRun, durably records intent,
  refuses active or mismatched state, removes only the verified registered
  worktree, and reconciles interrupted removal to retained cleaned evidence.
- Provider exchange reconciliation requires full persisted LoopRun equality
  with its verified authority; ordinary provider state publication cannot
  replace a newer candidate lifecycle state.

Verification: real temporary-Git-repository lifecycle tests, core contract
tests, format, Clippy, full workspace tests, and diff check.

Commit boundary: lifecycle primitives and typed contract only; no provider,
policy-gate, CLI, approval, eval, or promotion integration.

Compatibility: materialization streams exact index blobs directly, bypassing
checkout filters and built-in ident, encoding, and line-ending transforms.
Regular non-executable/executable files are supported everywhere (Git modes
100644 and 100755); raw symbolic links (120000), including non-UTF-8 targets,
are supported on Unix and fail closed elsewhere. Gitlinks/submodules (160000)
fail closed until a supported materialization contract is defined. Symlink
targets are capped at 4096 bytes; regular blobs stream with bounded buffers.
Unix candidate authority directories are private 0700. Candidate locks and
opened-file identity checks coordinate SEAF processes and fail closed; hostile
same-user directory-entry races remain M1-10/M1-11 hardening scope.

### M1-05b - Candidate Provider And CLI Integration

Status: complete on 2026-07-12. Dependencies: M1-05a (complete). Split into
four reviewable boundaries: M1-05b1 through M1-05b3 plus the M1-05b4a safety
prerequisite and M1-05b4b CLI are complete. M1-06 is active.

Roadmap: U3. Dependencies: M1-04b.

Objective: apply and inspect the candidate outside the user's source checkout.

#### M1-05b1 - Indexed Candidate Patch Transaction

Status: complete on 2026-07-12. Dependencies: M1-05a.

- `LoopExecutionMode` defaults old runs to `legacy_proposal_only`; only explicit
  `isolated_candidate` runs may carry candidate authority. A narrow decode
  migration recognizes pre-B1 M1-05a runs that already carried candidate state
  without the new mode and reserializes them explicitly as isolated.
- A closed, versioned candidate patch transaction binds immutable canonical
  Development evidence, policy digest, changed paths, planned index tree, and
  expected staged-diff bytes. `Applying` is durably published by full-LoopRun
  compare-and-swap before the real candidate index changes; `Applied` binds the
  exact observed tree and create-only staged-diff evidence.
- Recovery accepts only the pristine pre-apply state or the exact planned
  staged state. It recomputes the plan from authoritative Development evidence,
  rejects partial/coherent substitution, and validates exact Applied evidence
  on replay.
- Indexed application uses a private planning index and the real candidate
  index, then raw-rematerializes only changed paths from exact index objects.
  This preserves executable, delete, symlink, ident, and filter-independent raw
  semantics without touching the source checkout.
- Candidate artifacts use the shared atomic create-only publisher with file and
  parent-directory durability. Unique private planning indexes ensure a crash
  orphan cannot block retry. Real fault cuts cover stale pre-intent CAS,
  durable Applying before index mutation, materialized Applying before Applied
  evidence, and post-Applied replay.
- Materialization requires completed Development evidence on a running LoopRun.
  Exact file-to-directory and directory-to-file transitions are supported;
  unrelated directory contents fail closed.
- Allowed and RequiresHumanReview decisions may materialize in the isolated
  candidate. Rejected or already-applied policy evidence cannot mutate it;
  `apply_requested` remains audit-only.

Verification: 31 candidate lifecycle/transaction integration tests, 6 focused
candidate fault/unit tests, 33 core tests, 22 provider-exchange tests, 38
provider-step tests, and the full locked Rust workspace. Existing full-CAS
fault tests continue to prove stale candidate publication cannot replace newer
authoritative run state.

Commit boundary: typed execution/transaction authority and candidate-only
indexed materialization; no provider, CLI, approval, eval, or promotion wiring.

#### M1-05b2 - Provider Start And Resume Candidate Authority

Status: complete on 2026-07-12. Dependencies: M1-05b1.

Create and atomically persist the exact candidate before context, provider, or
log side effects. Resume must validate that authority before mutation and route
initial/additive repository context through the candidate.

- `Provisioning` is a closed pristine candidate lifecycle. Planning snapshots
  the canonical source/common-directory identity, repository digest, exact
  HEAD/tree, and deterministic candidate path without creating the worktree.
  Provisioning loads only that persisted authority, creates or adopts the exact
  detached worktree, raw-validates it, and full-state-CAS publishes `Active`.
- Provider startup is typed and two-stage: a minimal run directory atomically
  publishes the Provisioning run, provisions Active, creates a retry-safe
  synced runtime scaffold, publishes the complete canonical input snapshot set,
  and only then prepares context/provider execution and appends the semantic
  start log. Exact crash prefixes converge; collisions fail before new files.
- Resume compares current input digests read-only, validates or provisions the
  candidate before snapshot repair or provider reconciliation, repairs only
  missing exact snapshots, then derives both context and patch-gate roots from
  the candidate. Staged provider history is audited read-only for exact
  candidate authority before reconciliation may publish it.
- Initial and additive context artifacts bind the repository digest, candidate
  path digest, and starting HEAD/tree. Every predecessor in an expansion chain
  must carry the same authority. Candidate-native tests cover dirty source-only
  bytes, NeedsContext, replay, and cross-candidate substitution.
- Provider patch gating preserves `apply_requested` but is check-only. A real
  command spy proves one candidate-cwd `git apply --check`, no direct apply, and
  unchanged source and candidate trees. Context and patch roots are rejected
  independently when either differs from the candidate.
- All provider use of legacy execution fails with an explicit start-new-run
  error, including fresh library construction, incomplete resume, and terminal
  rerun. Deterministic non-provider `LoopRunner::start` remains unchanged.
- Real fault cuts cover pre-create, post-create/pre-CAS, post-Active, stale CAS,
  scaffold prefixes, and snapshot prefixes/collisions. Shared Git worktree
  mutations use a no-follow, identity-checked repository operation lock for
  both provisioning and cleanup.

The former provider integration suites were moved under `cfg(test)` unit
modules so their explicitly legacy historical harness is compiled only inside
the crate test build. A separate integration target uses the normal dependency
build and public constructor to prove no test harness or source-root provider
bypass ships.

Verification: full locked Rust workspace passes with 85 CLI tests, 33 core
tests, 94 seaf-loop library tests, 34 candidate integration tests, 22 context
expansion tests, 22 provider-exchange tests, 28 state tests, and focused
candidate/provider authority integration tests. Clippy with warnings denied,
Rust/Prettier formatting, and diff checks pass.

#### M1-05b3 - Development And Output-Review Integration

Status: complete on 2026-07-12. Dependencies: M1-05b2.

Wire policy-gated Development evidence into the candidate transaction and make
OutputReview consume the verified candidate tree/diff evidence.

- A completed isolated Development response, canonical Development artifact,
  unique policy decision, and completed step state are durable before candidate
  application begins. The runner requires exact Applied authority before the
  semantic finish log or OutputReview; rejected, blocked, provider-failed, or
  application-failed Development never reaches OutputReview.
- A read-only, candidate-locked verifier rechecks the Development reference,
  exact policy and digest, B2 candidate authority, immutable intent and applied
  evidence, candidate tree, and exact staged-diff reference, digest, and bytes.
  OutputReview receives only that closed projection, approved-spec identities,
  run identity, and input digests.
- Resume normalizes only the narrow pre-B3 no-transaction state with pending
  OutputReview and no review history, recovers pristine or materialized
  Applying cuts through the B1 transaction, and verifies Applied read-only.
  Proposal-only review history and inconsistent transaction/step combinations
  fail with start-new-run guidance.
- Every staged, durable, and fresh OutputReview Initial request is checked as a
  complete subject and provider envelope. Recovery and fresh publication
  validate the prospective ledger while the provider lock is held and bind the
  authoritative run model. The exported raw append rejects this authenticated
  record identity.
- Applied candidates permit only OutputReview reruns. Earlier reruns fail before
  ticket handling, scaffold, snapshot repair, provider reconciliation, or log
  mutation. OutputReview attempt two preserves candidate, Development/policy
  authority, and attempt-one audit history.
- The source checkout remains unchanged across pass, block, provider failure,
  application failure, resume, and rerun. Historical provider harness behavior
  is retained only in structurally `cfg(test)` code.

Verification: the final locked workspace passes with 86 CLI tests, 33 core
tests, 98 seaf-loop library tests, 34 candidate integration tests, 11 provider
candidate boundary tests, 22 provider-exchange integration tests, 28 state
tests, all remaining integration and doc tests, and 8 SDK tests. Clippy with all
targets/features and warnings denied, SDK lint/typecheck/build, Rust/Prettier
formatting, and diff checks pass. Independent spec and quality re-reviews
approved the final boundary after the forbidden-rerun, authoritative-model,
locked-append, and public raw-append findings were closed.

#### M1-05b4a - Authoritative Run-Directory Binding

Status: complete on 2026-07-12. Dependencies: M1-05b3.

Bind candidate authority to the canonical original run directory before
exposing destructive cleanup through the CLI.

- Candidate authority schema version 2 carries the lowercase SHA-256 digest of
  the canonical real absolute run-directory OS bytes. The runtime model,
  validation, and public JSON Schema admit only closed versions 1 and 2:
  version 1 must omit the digest and is forensic-only, while version 2 requires
  a non-null valid digest.
- Every candidate operation rejects legacy, copied, moved, symlinked, or
  tampered run-directory authority before candidate locks, Git operations,
  artifacts, state publication, source mutation, or candidate mutation.
  Operational recovery for version 1 requires a new run or manually verified
  worktree recovery; no copied state can bless itself through migration.
- Public candidate creation requires already-persisted matching authority and
  delegates to provisioning. Provisioning/adoption, application, verification,
  and Active/Cleaning/Cleaned cleanup revalidate authority under the candidate
  lock before later mutation.
- A cleanup race regression swaps both the digest and Git common-directory
  authority after the candidate lock. The locked reload rejects it before
  selecting or creating a repository-operation lock and leaves run, source,
  and candidate state unchanged.

Verification: 33 core tests, 39 candidate integration tests, the focused
cleanup race regression, Clippy for core and loop with all targets/features and
warnings denied, Rust formatting, and diff check. Independent spec and quality
re-reviews approved the correction after the repository-lock ordering finding
was closed.

Commit boundary: candidate run-directory authority only; no CLI, approval,
promotion, or eval behavior.

#### M1-05b4b - Explicit Candidate Cleanup CLI

Status: complete on 2026-07-12. Dependencies: M1-05b4a (complete).

Expose explicit cleanup through the existing authoritative
Active-to-Cleaning-to-Cleaned primitive and close end-to-end source immutability
coverage.

- `seaf loop cleanup --run-id ID [--runs-root PATH] [--json]` is the only
  cleanup trigger. It validates the run ID, minimally opens the named existing
  run, resolves the current repository through a Git-redirection-sanitized
  command, and delegates to the authoritative cleanup transaction.
- Persisted run identity is bound to the run-directory basename before the
  candidate lock and again on the locked reload. Active, Cleaning, and Cleaned
  authority validates the caller source, Git common directory, and candidate
  path read-only before selecting the persistent repository lock, then repeats
  physical/static validation under that lock.
- Cleanup returns a typed locked outcome containing the exact run ID, terminal
  status, and Cleaned candidate authority. The CLI renders only that snapshot;
  it never rereads and combines state after the destructive transaction.
- Exact terminal Active cleanup removes only the verified candidate path and
  Git registration, leaves the source checkout unchanged, and persists
  Cleaned. Repeating cleanup is byte-for-byte idempotent. Active,
  Provisioning, legacy, copied, wrong-repository, tampered, invalid, and missing
  authority fail without candidate removal or false success output.
- Normal-build isolated provider coverage now explicitly includes timeout in
  the non-completed Development matrix and proves source/candidate immutability,
  no patch transaction, and no OutputReview.

The first independent review found repository-lock mutation before source
validation, missing persisted run-ID binding, inherited Git-environment
redirection, a mixed post-cleanup report, and missing normal-build timeout
coverage. Each received a focused regression and both spec and quality
re-reviews approved the corrected boundary.

Verification: 94 CLI tests, 105 loop library tests, 39 candidate integration
tests, 11 provider-candidate boundary tests, the full locked Rust workspace,
Clippy with all targets/features and warnings denied, Rust/Prettier formatting,
SDK/package gates, and diff check.

Docs/tracker: candidate lifecycle and M1-05 status.

Commit boundary: explicit cleanup and its lifecycle safety only; no approval,
promotion, or eval execution.

### M1-06 - Human Approval State

Status: complete on 2026-07-12. Split into M1-06a stop barrier and M1-06b exact
approval transaction so execution safety was independently reviewable
from approval evidence and CLI confirmation.

Roadmap: U3. Dependencies: M1-05.

Objective: require a human to approve the exact candidate before any
model-modified code executes.

### M1-06a - Stop Before Human Review

Status: complete on 2026-07-12; independently approved by spec and quality
review after two correction rounds.

Acceptance criteria delivered:

- Isolated OutputReview can publish `awaiting_human_review` only through the
  locked workspace-aware state seam after the terminal immutable review record
  canonically derives `ApproveForTests` and matches the latest review attempt.
- The transition advances the current step to Testing without starting it;
  Testing and EvalReport remain pending and publish no artifacts.
- Resume, rerun, provider append/reconciliation, cleanup, and public state
  writers cannot cross, mint, replace, or remove the barrier. Exact valid
  public-writer retries are byte-preserving no-ops.
- Historical isolated Testing/EvalReport prefixes without approval fail before
  ticket, repository, provider, scaffold, or log work. Exact pre-M1-06
  Completed runs remain loadable and cleanable.
- No approved state, human evidence, approval CLI, eval execution, promotion,
  or source-checkout mutation is introduced.

The first review round found schema duplicate-step parity, misleading lock
coverage, unauthenticated barrier publication, late CLI preflight, and direct
barrier replacement. The second found that authenticated `RequestChanges`
could be relabelled passed and that public writers could mint the barrier.
Focused regressions close each path; the concurrent public-writer TOCTOU and
hostile artifact replacement remain M1-10 and M1-11 scope.

Verification: core/state/provider-candidate/CLI suites, full workspace tests,
format, Clippy, package lint/typecheck/test/build, and diff check.

Commit boundary: authenticated stop barrier only.

### M1-06b - Exact Human Approval Transaction

Status: complete on 2026-07-12; independently approved by spec and quality
review after two correction rounds. Dependencies: M1-06a.

Acceptance criteria delivered:

- Run state explicitly represents approved without weakening or replacing the
  durable awaiting barrier.
- Approval binds candidate patch digest, starting target HEAD, policy decision,
  current OutputReview artifact and its authenticated provider exchanges, and
  reviewer identity/time; stale or mismatched approval fails closed.
- The CLI requires explicit human confirmation and writes compact versioned
  approval evidence under the candidate/run locking order with a full-state
  compare-and-swap. Duplicate approval is byte-identical or rejected.
- Testing and promotion remain impossible in this slice.

`seaf loop approve` requires a bounded reviewer identity plus exact
`--confirm-candidate-diff` and `--confirm-target-head` values. Awaiting and
Approved run/status reports expose those values through JSON and human output,
so the supported flow does not require parsing `run.json`. Approval reuses the
candidate-locked Applied verifier, selects exactly one typed Development policy
decision, loads the approving OutputReview artifact, and binds the complete
initial and latest terminal provider record references. The inline versioned
evidence and Approved status publish together; exact retries revalidate and
return without rewriting bytes.

The first quality review found that physical source/candidate verification
occurred before waiting for the provider lock, allowing stale physical
authority to publish despite an unchanged LoopRun. A validator now runs after
provider-lock acquisition while the candidate lock remains held and re-derives
the complete physical and evidence authority immediately before atomic write.
It also found that required confirmation values were absent from public CLI
output. Both received focused regressions. Re-review rejected timing-based
race tests; the final deterministic in-crate hook injects run, candidate, and
source changes at the exact post-verification/pre-provider boundary without a
public test API. Spec and quality approved the final frozen result.

Likely seams: core state models/schemas, CLI approval command, state machine,
candidate authority, provider exchange evidence, and CLI/state tests.

RED: unapproved transition, stale HEAD, wrong digest, duplicate approval, and
successful exact approval tests, plus OutputReview artifact/exchange
substitution and concurrent state change.

Verification: core/state/CLI suites, full workspace tests, format, Clippy, and
diff check.

Docs/tracker: approval command/state and M1-06 status.

Commit boundary: approval evidence and state only.

### M1-07 - Integrated Testing And EvalReport

Status: active. Split into M1-07a reusable controlled engine, M1-07b immutable
eval authority, and M1-07c approved Testing/EvalReport transaction.
Dependencies: M1-06 (complete).

Roadmap: U4. Dependencies: M1-06.

Objective: make Testing and EvalReport deterministic loop steps over the exact
approved candidate without trusting mutable eval configuration.

### M1-07a - Reusable Controlled Eval Engine

Status: complete on 2026-07-12. Dependencies: M1-06 (complete). M1-07b is
active.

Objective: extract the existing standalone controlled eval implementation into
shared typed configuration and reusable planning/execution modules while
preserving valid `seaf eval run` behavior and failing closed on newly exposed
unsafe invalid configurations.

Acceptance criteria:

- Shared typed eval configuration rejects unknown fields and the reusable
  engine returns redacted, bounded output while callers own log persistence.
- Both command allowlists are intersected, every check is planned before the
  first command, and empty either allowlist denies execution.
- Working directories and candidate-relative executables stay inside the
  supplied root; trusted system executables retain the existing rules.
- Shell-free parsing, cleared environment, timeout/process-group cleanup,
  output draining, redaction-before-truncation, CLI flags, report semantics,
  exit codes, and standalone log paths remain compatible for valid
  configurations. Duplicate, sanitized-name, and ASCII case-folded log
  identities fail before directory creation or command execution.

RED: independent allowlist denial, invalid later check preventing an earlier
marker, redacted/capped returned output, root escape, ambiguous log identities
with zero side effects, and existing standalone CLI behavior tests.

Verification: shared engine tests, standalone eval CLI coverage, full Rust
tests, format, Clippy, and diff check.

Commit boundary: controlled engine extraction and standalone compatibility
only. No run snapshots, new states, or Approved execution.

### M1-07b - Immutable Eval Configuration Authority

Status: complete on 2026-07-12. Dependencies: M1-07a (complete). M1-07c is
active.

Objective: bind the exact eval program before candidate or provider work so
later Approved execution never rereads mutable live or candidate YAML.

Acceptance criteria:

- New provider runs require `ticket.eval.config` to resolve to a real,
  repository-root-contained regular file; absolute, traversal, ambiguous,
  symlink-escaping, missing, and malformed paths fail before run creation or a
  provider call.
- Parse once, canonicalize to JSON, publish create-only as
  `inputs/eval-config.json`, and bind its digest in the authoritative run input
  contract. Historical state remains readable through an optional field.
- Incomplete resume compares live authority with the bound digest. Approved
  evaluation will consume only the canonical snapshot.
- Historical Approved runs without this authority stay byte-identical and
  instruct the user to start a new run; they are never backfilled.

RED: unsafe paths, missing/malformed config, canonical key-order parity, live
mutation on resume, digest substitution, and inert historical approval.

Verification: core/schema/snapshot/CLI suites, full Rust tests, format, Clippy,
and diff check.

Commit boundary: immutable eval input authority only. No command execution.

### M1-07c - Approved Testing And EvalReport Transaction

Status: complete on 2026-07-12. Split into M1-07c1 inert evidence/terminal contracts and
M1-07c2 locked Approved execution. Dependencies: M1-07b (complete).

Objective: execute the canonical checks only in the exact Approved candidate
and durably publish one approval-bound Testing/EvalReport terminal transaction.

### M1-07c1 - Evaluation Evidence And Terminal Contracts

Status: complete on 2026-07-12. Dependencies: M1-07b (complete).

Objective: define the closed durable Testing/EvalReport evidence and terminal
state shapes without making any Approved run executable.

Acceptance criteria:

- Add backward-compatible Testing evidence and optional EvalReport loop
  bindings for run/ticket/config, exact candidate diff and starting HEAD,
  approval, policy decision, command log digests, and Testing artifact.
- Add `eval_passed` with a closed final shape: human approval unchanged,
  Testing and EvalReport passed with artifact path/digest pairs,
  `eval_report_path` equal to the EvalReport step artifact, and a non-rejecting
  report. Define the corresponding approval-bound reported-failure shape.
- Historical LoopRun and standalone EvalReport artifacts remain readable;
  integrated checks require stdout/stderr path-digest pairs.
- Direct state writers, provider execution/append/reconciliation, rerun, and
  cleanup cannot mint, replace, or remove passing eval authority. Direct
  ProviderStepRunner Testing/EvalReport fails closed instead of returning
  no-op success.
- No CLI path executes checks, publishes integrated evidence, or transitions an
  Approved run in this slice.

RED: legacy fixture compatibility, malformed/mismatched binding, duplicate
steps, report-path mismatch, provider no-op removal, public-writer minting, and
non-cleanable eval-passed state tests.

Verification: core/schema/state/report/provider/candidate suites, full Rust
tests, format, Clippy, and diff check.

Commit boundary: inert durable contracts and freeze rules only.

### M1-07c2 - Locked Approved Evaluation Transaction

Status: complete on 2026-07-12. Dependencies: M1-07c1 (complete).

Acceptance criteria:

- `loop resume` recognizes exact Approved authority and uses the canonical
  ticket and eval snapshots without a model call. Direct provider Testing and
  EvalReport execution fails closed instead of reporting no-op success.
- Preflight all checks before mutation or execution. Reauthenticate approval,
  provider/review/policy evidence, source identity, and physical candidate
  before commands and before final publication under candidate-to-provider
  lock order.
- Publish create-only redacted command logs, canonical Testing evidence, and a
  backward-compatible EvalReport binding the run, ticket, config, candidate
  diff, approval, policy decision, and Testing artifact.
- Publish Testing, EvalReport, `LoopRun.eval_report_path`, and terminal
  `eval_passed` or reported `failed` state with a full-state compare-and-swap.
  Failed checks/evidence cannot claim eval success or promotion.
- An execution intent prevents silent command replay after interruption;
  M1-09 owns audited adoption or invalidation of an incomplete attempt.

Likely seams: approved-eval controller, candidate/provider locks, create-only
artifact publisher, eval builder, state/run contracts, and CLI resume tests.

RED: unapproved or historical execution, substituted authority, independent
allowlist denial with zero commands, candidate-only side effects, no provider
calls, failed/timeout report binding, mutation before final publication,
artifact substitution, and no-op-removal tests.

Verification: eval/provider/CLI suites, full Rust tests, format, Clippy, and diff
check.

Docs/tracker: one-command flow, supervised local-execution boundary, and M1-07
status.

Commit boundary: Approved Testing/EvalReport transaction only.

### M1-08 - Promotion Integrity

Roadmap: U3. Dependencies: M1-07.

Status: complete on 2026-07-12. Dependencies: M1-07 (complete).

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

Status: complete. M1-09a attempt-safe history/inspect, M1-09b provider
revise/rerun, and M1-09c Approved-evaluation recovery are complete.
Dependencies: M1-08 (complete).

Objective: inspect, revise, and rerun blocked/failed attempts without replacing
history. Authoritative ticket, policy, project config, repository identity, eval
config, provider/model, and candidate changes require a new run. EvalPassed and
Promoted remain immutable; M1-08 retains promotion-intent recovery.

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

### M1-09a - Attempt-Safe Role Artifacts And Factual Inspect

Status: complete. Dependencies: M1-08 (complete).

Objective: close the current structured-artifact overwrite seam and make durable
attempt history inspectable before any new recovery mutation exists.

Acceptance criteria:

- Structured role artifacts receive the real step attempt. Attempt 1 preserves
  the historical fixed name; attempts 2+ use create-only
  `artifacts/<step>.attempt-NNN.<validated-extension>` paths.
- Preserve the exact validated `ArtifactContent` extension. Exact retry is
  idempotent; collision, symlink, directory, different bytes, skipped/exhausted
  attempts, or ambiguous historical reuse fails closed without rewriting files.
- `loop inspect` reports only factual authority: canonical run digest,
  status/current step, input digests, candidate lifecycle/head/diff, current step
  references, authenticated provider attempt summaries, and evaluation-prefix
  inventory. It emits no raw bodies, eligibility decision, or reset preview.
- Inspection performs no writes, log append, provider/model call, candidate or
  source mutation. Historical fixed-name attempt 1 remains readable; ambiguous
  later reuse is forensic-only.

RED: OutputReview attempt 2 preserves attempt-1 bytes and selects the new
artifact; exact retry/collision/symlink/directory/exhaustion; historical
compatibility/ambiguity; tampered authority classification; byte-identical
human/JSON inspect.

Verification: artifact/provider/state/CLI suites, full workspace tests, format,
Clippy, and diff check.

Commit boundary: attempt-safe role artifacts and read-only inspect only. No
recovery schema or state mutation.

### M1-09b - Audited Provider Revise And Rerun

Status: complete. Dependencies: M1-09a (complete).

Objective: publish one actor/reason-bound recovery decision, reset current
provider pointers without deleting history, then consume exactly that decision
for a new provider attempt.

Acceptance criteria:

- Add versioned create-only source-run snapshot and `RecoveryAttemptV1` bound to
  sequential recovery ID, action/step, actor/reason/time, exact source-run and
  input/candidate digests, source/next step attempts, previous recovery/provider
  heads, and expected reset-state digest.
- `loop revise` publishes recovery plus pure reset under candidate-to-provider
  full CAS and performs no provider call. `loop rerun --recovery N` alone may
  consume the active recovery before its first durable request; ordinary resume
  rejects pending recovery, then may resume after that request exists.
- Eligible: active schema-v2 isolated Blocked/unapproved Failed with pristine
  candidate through Development; exact Applied candidate only OutputReview;
  AwaitingHumanReview or Approved with no eval prefix only OutputReview. Pending,
  Running, Applying, non-Active, approval-bound final Failed, EvalPassed,
  Promoted, legacy/ambiguous, or exhausted history is ineligible.
- Preserve every file and the complete provider ledger. Clear only selected and
  downstream current pointers, matching policy decisions, and OutputReview
  approval/eval refs. New recovery uses one authorization contract; historical
  provider rerun authorization remains readable but is not newly published.
- New use of `resume --rerun-from` returns migration guidance. Inputs,
  provider/model, candidate bytes, and promotion authority cannot be revised.

RED: eligible Blocked/Failed steps and Applied OutputReview; active-recovery
resume gate; note exactly once; mutation/substitution/gap/exhaustion; downstream
clearing with old bytes/ledger preserved; CAS race; ineligible terminal/lifecycle
matrix; source checkout unchanged.

Verification: recovery/provider/candidate/CLI suites, full workspace tests,
format, Clippy, and diff check.

Commit boundary: provider revise/rerun only. No eval or promotion recovery.

### M1-09c - Approved-Evaluation Recovery

Status: complete. M1-09c1 versioned evaluation authority, M1-09c2 zero-command
adoption, and M1-09c3 invalidation/rerun are complete. Dependencies: M1-09b
(complete).

Objective: adopt complete verified interrupted evaluation evidence with zero
commands, or explicitly invalidate it before one fresh attempt.

Acceptance criteria:

- Add attempt-indexed create-only evaluation intent/log/Testing/EvalReport paths.
  ApprovedEvaluationIntent v2 binds evaluation attempt and recovery reference;
  TestingEvidence v2 binds exact intent and invalidation authorization. V1 fixed
  paths remain readable, and promotion/final validation select the bound intent.
- `loop revise --from-step testing --eval-recovery adopt|invalidate` publishes
  audited authority. Adoption requires exact intent, complete planned checks,
  canonical TestingEvidence and every log; it executes zero commands and may
  deterministically create only a missing EvalReport. Intent/log-only prefixes
  never adopt.
- Invalidation preserves every prior byte and exact candidate/approval/policy/
  input authority, resets only current Testing/EvalReport/final-eval refs, and
  gates the fresh attempt behind `loop rerun --recovery N`. Active Approved with
  an incomplete prefix and active approval-bound final Failed are eligible;
  EvalPassed/Promoted and historical missing-eval authority are not.
- Candidate-to-provider recovery CAS reconstructs the exact execution-time
  Approved predecessor from the recovery source snapshot; it does not weaken
  final EvalPassed/Promoted or M1-08 promotion-intent relations.

RED: every interruption prefix, zero-command adopt, passing/failing evidence,
attempt-2 byte preservation, cross-run/tampered/gapped authority, candidate/
source/input drift, CAS race, explicit-rerun gate, final-failed retry, and frozen
EvalPassed/Promoted/M1-08 regression.

Verification: recovery/eval/promotion/provider/CLI suites, full workspace tests,
format, Clippy, and diff check.

Commit boundary: Approved-evaluation adoption/invalidation/rerun only.

### M1-09c1 - Versioned Evaluation Attempt Authority

Status: complete. Dependencies: M1-09b (complete).

Objective: make every new evaluation artifact attempt-indexed and make final
validation/promotion select the exact bound attempt before recovery can adopt or
invalidate it.

Acceptance criteria:

- New evaluations publish `ApprovedEvaluationIntent` v2, indexed logs,
  `TestingEvidence` v2, and indexed EvalReport at canonical attempt-001 paths.
  The intent binds evaluation attempt, exact Approved/input/candidate/source
  authority, optional recovery (`null` for a fresh attempt), and complete plan.
  Testing binds that exact intent, attempt, optional recovery, and every log.
- Strict inventory rejects mixed fixed/indexed attempt 1, gaps, malformed names,
  unsafe file types, duplicate logs, orphan future attempts, or cross-attempt
  references. Exact retry is create-only and collision-safe.
- Historical fixed-path v1 intent/log/Testing/EvalReport remains readable.
  Final authority and promotion use one typed v1/v2 loader and select only the
  intent bound by Testing; no hardcoded v1 promotion path remains.
- No adopt, invalidate, rerun, final-state relaxation, or provider behavior is
  added in this checkpoint. Existing incomplete-prefix refusal remains.

RED: fresh attempt-001 v2 path/bindings, v1 final compatibility, mixed/gapped/
tampered inventory, v2 final pass/fail, exact retry/collision, promotion selects
the Testing-bound intent and rejects attempt substitution.

Verification: approved-eval/Testing/final/promotion/CLI suites, full workspace,
format, Clippy, and diff check.

Commit boundary: evaluation artifact/version readers only.

Compatibility note: this pre-preview 0.1.0 checkpoint adds public v2 fields to
`TestingEvidence`, so downstream Rust struct literals must add those fields.
Persisted fixed-path v1 JSON remains readable. M1-12 must carry this source API
change into the preview release notes.

### M1-09c2 - Zero-Command Evaluation Adoption

Status: complete. Split into M1-09c2a recovery authority and M1-09c2b adoption
transaction. Dependencies: M1-09c1 (complete).

Objective: publish audited recovery and finalize one complete interrupted v1 or
v2 evaluation prefix without executing commands.

Acceptance criteria: M1-09c adoption criteria above, plus schema-v2 evaluation
recovery in the existing sequential recovery chain, exact source-snapshot
Approved reconstruction, deterministic missing EvalReport creation, and
Approved-to-final recovery CAS without weakening ordinary final relations.

Commit boundary: adoption only; no invalidation or command execution.

### M1-09c2a - Versioned Evaluation Recovery Authority

Status: complete. Dependencies: M1-09c1 (complete).

Objective: add typed evaluation recovery lineage and recovery-aware final
reconstruction before exposing an adoption operation.

Acceptance criteria:

- Provider recovery v1 remains byte-, API-, and behavior-compatible. A private
  discriminated loader authenticates provider v1 or evaluation v2 in the same
  contiguous recovery chain.
- Evaluation v2 uses a prefix-bearing source snapshot and binds exact Approved,
  input, candidate, source, provider, intent, Testing, expected EvalReport,
  report disposition, prior recovery, and zero-digest final projection
  authority.
- Final evaluation authority reconstructs the exact Approved predecessor from
  the v2 source snapshot and validates pass/fail, Failed-cleanup, and Promoted
  descendants without weakening direct c1 or provider-v1 final relations.
- No writer, CLI option, report creation, adoption CAS, invalidation, rerun, or
  attempt-2 behavior is added.

RED: v1 compatibility; v2 pass/fail reconstruction; mixed lineage; source,
prefix, disposition, projection, reference, and descendant substitution.

Verification: recovery/final/provider/promotion suites, full workspace, format,
Clippy, and diff check.

Commit boundary: typed evaluation recovery reader and final reconstruction only.

### M1-09c2b - Zero-Command Adoption Transaction

Status: complete. Dependencies: M1-09c2a (complete).

Objective: expose exact complete-prefix adoption with no command or provider
execution.

Acceptance criteria: M1-09c2 criteria above, including preflight-before-write,
deterministic report verification/creation, candidate-to-provider adoption CAS,
all crash cuts, and inert exact post-CAS retry for the same action, actor, and
reason. Arbitrary fresh terminal adoption remains forbidden.

Delivered: the CLI and public adoption API accept only exact Approved/Testing
authority with a complete fixed-v1 or indexed-v2 attempt-1 prefix. All six
authoritative input snapshots, physical candidate/source state, provider
ledger, report bytes, recovery namespace, and crash orphans are authenticated
under candidate-to-provider lock order. Fixed/indexed, present/missing report,
pass/fail, crash, retry, drift, and concurrent-winner matrices are covered.

Commit boundary: adoption writer and CLI only; no invalidation or rerun.

### M1-09c3 - Evaluation Invalidation And Rerun

Status: complete. Dependencies: M1-09c2 (complete).

Objective: preserve and invalidate one incomplete or failed evaluation attempt,
then authorize exactly one fresh indexed attempt.

Acceptance criteria: M1-09c invalidation/rerun criteria above. A dedicated
create-only invalidation authority binds every present byte of the latest
incomplete prefix or exact active approval-bound final Failed, the reconstructed
Approved source, next contiguous attempt, audit fields, prior recovery/provider
heads, and reset projection. Invalidation preserves all history, executes zero
commands/provider calls, and advances only audited recovery/reset authority.
Only exact `loop rerun --recovery N` may consume it; new intent and Testing bind
the recovery reference. Once any artifact for the authorized attempt exists,
commands may not replay within that attempt. A partial prefix requires a later
audited invalidation; a complete pre-CAS prefix uses zero-command adoption. An
exact post-final-CAS rerun may return byte-inert success only when the final
authority binds that recovery. Cover repeated cycles, every crash cut and race,
malformed/mixed/gapped inventory, drift, final-Failed reset, and frozen
EvalPassed/Promoted/promotion-intent regressions.

Delivered: a dedicated evaluation-invalidation v3 source and decision contract
binds the exact incomplete or active approval-bound Failed authority, every
present prefix byte, reconstructed Approved predecessor, next attempt, input,
candidate, source, provider and prior-recovery authority, audit fields, and
zero-digest reset projection. `loop revise ... invalidate` is command- and
provider-free; only exact `loop rerun --recovery N` publishes the recovery-bound
indexed intent and Testing evidence. A typed pre-spawn rejection aborts on
authority drift without manufacturing final failure evidence. Mixed v1/v2/v3
lineage validation is iterative and exact; partial attempts require a new
invalidation, complete prefixes use zero-command adoption, and exact final
retries are inert. Repeated cycles, V2-adopted failure, tamper, crash, all-input
drift, replay, concurrency, final failure, passing promotion, and frozen-state
coverage pass.

Commit boundary: evaluation invalidation and fresh rerun only.

### M1-10 - Atomic State And Run Locking

Roadmap: U5. Dependencies: M1-09.

Status: complete on 2026-07-12. Dependencies: M1-09 (complete). M1-11 is
active.

Objective: prevent corrupt or concurrently mutated run state.

M1-04b2a already provides a narrow stable lock and atomic replacement for the
provider-exchange append that must complete before another provider call. This
slice generalizes the guarantee to every other run-state mutation and recovery
operation; it does not replace or weaken the earlier ledger-specific guard.

Acceptance criteria:

- Run state uses atomic replacement with durable same-directory temporary files.
- Exactly one mutating process can hold a per-run lock; stale-lock behavior is
  explicit and safe.
- Failed writes retain the last valid parseable state.

Likely seams: state/workspace persistence and fault-injection tests.

RED: concurrent mutation, partial-write, failed-rename, and stale-lock tests.

Verification: state/workspace suites, full workspace tests, format, Clippy, and
diff check.

Delivered: the existing permanent `provider-exchange.lock` is now the single
bounded per-run mutation lock rather than a second competing lock. Initial
state uses create-only hard-link publication; public direct writes permit only
byte-identical canonical retry; legitimate changes use complete
expected-to-intended compare-and-swap with the provider, candidate, recovery,
human-review, evaluation, and promotion transition guards preserved. Every
replacement writes and syncs a unique same-directory temporary file,
revalidates the stable lock and opened `run.json` identity immediately before
atomic rename, and syncs the parent directory. Pre-publication faults retain
old or absent state; post-publication directory-sync uncertainty leaves the
complete intended bytes, and exact retry reauthenticates and resyncs them.
Symlink, non-file, replaced-lock, replaced-target, competing-writer, partial
write, sync, publish, unlink, and parent-sync regressions fail closed or
converge to one legal state.

Every ordinary runner, provider append/reconcile, candidate lifecycle,
approval, evaluation v1/v2/v3 recovery, cleanup, and promotion retry now uses
the shared seam without changing candidate-to-repository-to-run lock order.
Resume preserves the historical human-review fail-before-mutation barrier,
then exact-resyncs durable authority; frozen review/final states return inert,
while ordinary terminal provider history still passes recovery validation.
The generic ordinary CAS and exact-resync operations remain crate-private.

Verification: independent contract and quality reviews approved with no
remaining P0/P1/P2 findings. Controller gates pass the full Rust workspace,
including CLI 137, loop library 147, provider-candidate 53, candidate 39, state
35, and provider-exchange 22 tests, plus all remaining integration and doc
suites. Strict all-target/all-feature Clippy, Rust and Prettier formatting,
package lint/typecheck, 8 SDK tests, SDK build, and diff checks pass through the
pinned pnpm runtime.

Docs/tracker: persistence/lock behavior and M1-10 status.

Commit boundary: atomic persistence and locking only.

### M1-11 - Minimum Artifact Protection

Roadmap: U5. Dependencies: M1-10.

Status: accepted on 2026-07-13. M1-11a private run artifacts, M1-11b bounded
artifact storage, and M1-11c bounded secret redaction are complete.

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

#### M1-11a - Private Run Artifacts

Status: complete on 2026-07-12. Dependencies: M1-10 (complete). M1-11b is
active.

Objective: make every run-owned directory and file private from its first
published byte without changing source or candidate Git modes.

Delivered: supported Unix run roots and subdirectories are created as `0700`;
run state, locks, prompts, responses, logs, immutable evidence, temporary
files, and final files are created as `0600`. Existing broader modes fail
closed with an explicit `chmod` remedy and are never silently migrated.
Standalone evaluation and release artifacts remain outside this boundary.

Publication no longer depends on a previously checked pathname. A pinned
private directory handle owns create, link, rename, unlink, identity, and sync
operations. Parent substitution, symlinks, non-regular files, name traversal,
lock replacement, target replacement, and operation-owned temporary-file races
fail closed. Workspace scaffolding revalidates retained existing entries and
directories before creating any missing default, preserves valid populated
files, and does not mutate either the original or substituted tree after a
race. Non-Unix loop workspaces return unsupported before creating a run leaf.

Focused regressions cover zero-umask creation, broad existing modes, exact
retry, immutable and state publication cuts, workspace preflight ordering,
candidate and repository lock substitution, and parent-directory replacement.
Independent specification and quality reviews approved the final slice with no
remaining P0/P1/P2 findings. The complete Rust workspace, strict all-target and
all-feature Clippy, Rust and Prettier formatting, package lint/typecheck, eight
SDK tests, SDK build, and diff checks pass.

Commit boundary: private permissions and pinned-directory publication only.

#### M1-11b - Bounded Artifact Storage

Status: complete on 2026-07-13. M1-11b1 serialized artifact limits and M1-11b2
pre-side-effect storage commitments are accepted.
Dependencies: M1-11a (complete).

Objective: reject oversized run artifacts before partial or misleading
authority, provider calls, or evaluation commands can consume an unrecordable
storage commitment.

Acceptance criteria:

- Provider prompts/requests are capped at 2 MiB, canonical provider response
  audits at 1 MiB, exchange records at 64 KiB, evaluation logs at 1 MiB, and
  other generated evidence/input artifacts at 2 MiB.
- The durable run tree has a 32 MiB aggregate cap. Exact-cap publication is
  accepted; one byte over is rejected. Exact immutable retries consume no new
  budget, while replacement accounts for the old and intended sizes safely.
- Aggregate accounting and reservation reuse the M1-10 per-run lock and do not
  introduce a competing lock or change candidate/repository/run lock order.
- Every external provider call and evaluation command either has sufficient
  durable capacity reserved for its authoritative result or is refused before
  the side effect.

RED: per-artifact exact-cap and cap-plus-one, aggregate exhaustion, exact
retry, replacement, concurrency, crash, provider pre-call, and evaluation
pre-spawn tests.

Commit boundary: per-artifact and aggregate storage limits only.

##### M1-11b1 - Serialized Artifact Limits

Status: complete on 2026-07-12. Dependencies: M1-11a (complete). M1-11b2 is
active.

Objective: enforce semantic per-artifact and physical aggregate limits for
every cooperative run-tree publisher.

Delivered: provider exchange requests/prompts are capped at 2 MiB, canonical
provider response audits at 1 MiB, exchange records at 64 KiB, evaluation
stdout/stderr and root `log.md` at 1 MiB, and all other run-owned state,
inputs, and evidence at 2 MiB. Exact cap succeeds and cap plus one fails before
creation, truncation, or publication. Existing oversized artifacts fail closed
before allocation through metadata checks and bounded reads.

The existing M1-10 lock now serializes run state, scaffold, logs, mutable and
immutable artifacts, inputs, policy evidence, provider exchanges, candidate
evidence, evaluation, recovery, and promotion publishers. A pinned recursive
scan authenticates private directories and real regular files, counts logical
bytes once per device/inode, includes locks and crash-orphan temporaries, and
rejects symlinks, FIFOs, special entries, unsafe modes, and arithmetic overflow.
Enumeration streams one child name at a time and accepts at most 4,096 total
non-dot entries across the run tree. The run root is depth zero; descendant
directories through depth eight, including files directly inside a depth-eight
directory, are accepted, while a depth-nine directory is rejected before it is
opened or recursed into. Directory entries and every hard-link name consume the
entry budget even though hard-linked regular-file bytes remain counted once.
Prospective entry peaks are authorized under the same permanent run lock:
first-lock and child-directory creation reserve one name, create-only and
mutable-create publication reserve two names while the temporary and final
hard-link names coexist, replacement reserves one temporary name beside the old
target, and an exact existing retry reserves zero. Runtime scaffolding uses the
same directory and file projections before each creation. Entry-only
projections also reject current aggregate bytes over 32 MiB before first-lock or
child-directory creation, so a zero-byte name cannot mutate an already invalid
tree.
The zero-byte candidate-workspace lock is a permanent scaffold artifact. Fresh
scaffolding creates it through guarded immutable publication; an authenticated
existing-run operation may migrate a missing lock under a temporary run guard,
revalidates run-directory authority after acquiring that guard, and releases it
before candidate locking. Candidate lock acquisition is then authenticated
open-only. Its existing-file fast path never acquires the run lock, preserving
candidate-before-run operational order. External repository-operation locks
retain their separate creation policy outside the run tree.
Exact immutable retry adds no bytes but still rejects an already-over-cap tree.
Atomic replacement reserves the new synced inode while the old target still
exists; competing writers cannot both consume the final capacity.

Git's temporary patch-planning index now lives in a unique pinned `0700`
operation directory under the external candidate authority, never in the run,
source, or candidate tree. Parent/child identity is checked around every Git
call; cleanup accepts Git's private-parent-contained mode but unlinks only
no-follow single-link regular files through the pinned directory. Rebound
paths and collisions remain untouched. The permanent zero-byte candidate lock
is included by every later scan.

Prior specification, writer-inventory, and quality/security reviews approved
the slice. A subsequent quality follow-up found that the byte-bounded scanner
still collected unbounded directory names and retained unbounded inode
metadata; streaming enumeration plus the global entry and depth limits close
that gap. A further quality follow-up found that entry use was scanned but not
prospectively reserved; the peak projections above close that path before any
temporary file, final name, first permanent lock, or child directory is created.
A specification follow-up then found that entry-only lock/directory projections
did not reject existing aggregate-byte overage; both seams now validate current
bytes before entry headroom. A final quality follow-up found direct candidate
lock creation bypassed accounting; the permanent scaffold/migration contract
above closes it. The loop library passes 193/193, candidate integration 40/40,
state 38/38, and provider-candidate boundary 53/53. Both absolute-final
independent reviews approve with no P0/P1/P2 findings. The definitive controller
workspace run passes with CLI 138, core 51, local runtime 5, loop library 193,
candidate 40, context expansion 22, eval engine 7, eval report 3, final authority
2, patch 7, policy 13, provider-candidate 53, provider-exchange 22, isolation 1,
staged authority 2, role response 13, state 38, Testing evidence 8, models 7,
Ollama 8, and all doc tests. Native workspace check, strict
all-target/all-feature Clippy, Rust and Markdown formatting, and diff checks
pass.

Commit boundary: per-artifact limits, serialized physical aggregate accounting,
and directly required bounded reads only.

##### M1-11b2 - Pre-Side-Effect Storage Commitments

Status: complete on 2026-07-13. Provider-side M1-11b2a and evaluation-side
M1-11b2b are accepted.
Dependencies: M1-11b1 (complete).

Objective: guarantee logical capacity for the authoritative result before a
provider call or approved evaluation command starts, without a new reservation
file or holding the run lock across external latency.

Acceptance criteria:

- An authenticated request-phase provider ledger tail reserves the missing
  response-audit cap, response-record cap, and full future `run.json`
  replacement bytes at the atomic coexistence peak.
  Insufficient capacity creates no new authoritative request and performs zero
  provider calls.
- An authenticated active evaluation intent reserves every missing stdout and
  stderr maximum separately plus bounded Testing evidence, EvalReport, two
  recovery artifacts, and the full future `run.json` replacement at the atomic
  coexistence peak. The commitment includes every missing permanent name and
  one transient replacement name. An over-budget plan publishes no intent and
  spawns zero commands.
- Every cooperative publisher accounts physical bytes plus the one derivable
  active provider/evaluation commitment. Canonical prefix publication consumes
  its slot; crash/reopen, staged adoption, invalidation, and exact retry
  reconstruct the same remaining commitment without accumulating metadata.
- A provider result whose canonical exact audit exceeds 1 MiB publishes a small
  typed non-retryable oversize failure without raw result bytes or a raw-result
  digest. Existing request-only crash recovery semantics remain unchanged.

RED: insufficient/exact provider headroom and call count; request-only,
response-audit, and staged-record crash cuts; eval worst-case plan and spawn
count; trailing-log/invalidation/adoption commitments; concurrent unrelated
publisher; typed oversize provider failure with no raw leak.

Commit boundary: derived provider/evaluation commitments and bounded oversize
provider failure only.

###### M1-11b2a - Provider-Side Commitments

Status: complete and independently accepted on 2026-07-13.

Fresh and uniquely adoptable staged request records activate one authenticated
byte-and-entry commitment under the permanent run guard. It reserves missing
1 MiB response-audit and 64 KiB response-record slots, full future `run.json`
replacement bytes at the atomic coexistence peak, future permanent names, and
one transient name.
Physical operation peaks and post-operation steady commitment are checked
separately. Every provider call reauthenticates the exact request tail and
commitment under the guard, releases it, then invokes the provider.

Response audit, response record, and response-tail run publishers consume only
their authenticated slot; unrelated publishers retain the complete remainder.
Request-only replay, staged adoption, exact retry, and concurrent publishers
reconstruct the same commitment without metadata. Canonical audit size is
measured through a cap-plus-one writer before raw hashing or persistence.
Oversized success or error results become one fixed non-retryable failure with
no raw bytes or digest; exact 1 MiB and under-cap results remain unchanged.

RED/GREEN covers exact and one-short byte and entry headroom, fresh denial with
zero calls, request-only replay with one or zero calls, staged request
activation, concurrent unrelated denial, exact 1 MiB, oversized success/error,
and raw marker/digest absence.

###### M1-11b2b - Evaluation-Side Commitments

Status: accepted on 2026-07-13. Dependencies: M1-11b2a accepted. Parent
M1-11b2 and M1-11b are complete; M1-11 remains active for M1-11c.

Delivered implementation: one shared output-limit normalizer gives each
stdout and stderr stream its configured maximum (`64 KiB` by default, valid
range `1..=1 MiB`). A fresh authenticated intent atomically activates capacity
for every missing stream, bounded Testing and EvalReport artifacts, both
2 MiB recovery source and recovery decision fallbacks, the full 2 MiB future
`run.json` replacement at its atomic coexistence peak, every missing permanent
name, and one transient replacement name. Physical publication peaks remain
separate from post-publication steady usage plus the derived commitment.

The exact intent/log/Testing/report prefix reconstructs and consumes only
authenticated typed slots. Before every command spawn, SEAF acquires the run
guard after the candidate lock, reauthenticates the durable run, candidate,
source, inputs, intent, completed log prefix, and remaining capacity, then
releases the run guard before spawning. Unrelated cooperative publishers retain
the full evaluation commitment. Existing-winner retries consume no slot;
different bytes and surplus same-name artifacts fail closed.

Before Testing is durable, a present stdout/stderr name does not create free
slack: physical bytes plus a non-consumable `normalized_limit - physical_size`
residual still equal the full stream maximum, while the already-present name
needs no future entry. Testing validates the exact intent, attempt, selected log
paths, and log digests before releasing those residuals. A staged adoption
report likewise retains `2 MiB - physical_size` until a fully verified recovery
decision binds its exact digest. Wrong same-name and hard-linked artifacts
therefore cannot finance an unrelated cooperative publication.

Normal intent authority retains the two recovery slots after Testing and
EvalReport publication. Adoption and invalidation then transition from source
to decision to optional report to the final run replacement without a durable
reservation file. Direct final publication consumes the full replacement and
releases unused recovery fallback. Fixed-v1, indexed-v2, invalidation-v3,
provider recovery, crash-cut retry, and promotion/final authority behavior are
preserved. Pinned bounded evaluation inventory replaces raw directory
enumeration. A new run-guard owner removes only canonical, private,
single-link root `run.json` replacement temporaries left by a dead prior lock
owner before commitment validation; lookalike names remain untouched. Real
adoption/final and invalidation/reset reopen tests prove that the stale physical
replacement does not double-count the future transient slot.

RED/GREEN covers the shared normalizer, a literal mixed-limit prefix
table, trailing stdout and exact slot consumption, missing-log rejection,
exact/one-short byte and entry activation boundaries, checked overflow,
recovery publication crash cuts, fresh denial with no intent or child marker,
capacity loss after check one with no second marker or Testing authority,
pinned intent-parent substitution, wrong same-name residual protection, both
run-temp reopen transitions, and the complete provider/candidate boundary. The
first independent review rejected whole-slot log consumption, partial staged
v2/v3 authentication, permissive temp spelling, missing live lock-release and
hard-link/Testing/report regressions, and the absence of a literal recovery
transition table. Those findings are corrected. The production-used table now
asserts literal paths, bytes, and entries for v2 CreateMissing, v2
VerifyExisting, and v3 source/decision/report cuts; real adoption and
invalidation reauthentication require final/reset authority to derive no active
commitment. Corrected gates pass for evaluation-storage units (4), the full
`seaf-loop` library (225), provider/candidate boundary (65), eval engine (7),
eval report (3), final authority (2), provider exchange (22), state (38), and
Testing evidence (8). The full workspace, strict Clippy, Rust formatting,
pinned-pnpm format/typecheck/test/build, and diff-hygiene gates pass.

Second re-review correction: staged v2/v3 recovery sources now select both the
exact active commitment prefix and the factual inventory's latest attempt. A
contiguous newer attempt makes an older otherwise-valid v3 source fail closed
without releasing capacity. End-to-end coverage now reaches the authentic
CreateMissing `PresentUnauthenticated` branch: with source durable, decision and
report absent, and physical usage plus the 6 MiB transition commitment exactly
at cap, a one-byte unbound same-name report retains its remaining report cap and
cannot finance an unrelated prompt. The live command-lock test uses a bounded
10-second command and releases/joins the worker on every assertion path. The
Rust owner gates pass: `seaf-loop` library 225/225, provider/candidate boundary
65/65, and the full workspace including CLI 138/138. Strict Clippy, Rust
formatting, pinned-pnpm formatting, and diff hygiene also pass. Final
independent specification, quality, and evidence re-reviews approve the slice.

#### M1-11c - Bounded Secret Redaction

Status: accepted on 2026-07-13. Dependencies: M1-11b (complete). M1-11 is
complete; M1-12 is accepted.

Objective: prevent configured and obvious credentials from escaping their one
private authoritative input snapshot into provider requests or derived run
evidence while keeping redaction output bounded.

Acceptance criteria:

- Sensitive evaluation environment values may remain only in the exact private
  `inputs/eval-config.json`; derived prompts, requests, responses, logs, and
  evidence redact configured and obvious credential forms before persistence.
- At most 64 configured secret values are accepted, each at most 4 KiB and
  together at most 64 KiB; oversized redaction input or output fails closed.
- A clean provider result and M1-11b2's typed oversize failure remain exact. A
  secret-bearing response becomes a small safe non-retryable audited failure
  without raw bytes or a raw digest, preserving existing request-only recovery
  semantics.

RED: configured-value, obvious-pattern, overlap, cap, provider-response,
request-only crash recovery, and no-raw-leak tests.

Commit boundary: bounded secret derivation and redaction only.

Accepted implementation: a shared byte-oriented redactor enforces the
64-occurrence, 4 KiB-per-value, 64 KiB aggregate, and 2 MiB input/output bounds.
Evaluation output uses the complete authenticated corpus before lossy
conversion or truncation and keeps markers atomic. Indexed evaluation intent
v3 persists only structural check projections and sorted env names;
secret-free v1/v2 history stays readable while secret-bearing legacy intent
fails closed.

Provider requests are sanitized before audit and call. Exact request,
response, provider-record, prospective-run, replay, and historical-run bytes
are screened before provider invocation or durable publication. Under-cap
secret-bearing provider results become the fixed non-retryable safe failure
only after exact raw audit-size measurement. Approval, promotion, recovery,
adoption, invalidation, evaluation, context expansion, scaffold, log, and
generic run-state paths screen their exact prospective envelopes before the
paired mutation and again at authority-changing compare-and-swap boundaries.
Full context-source screening is bounded at the first byte beyond 2 MiB rather
than reading an unbounded tail.

Fresh isolated provisioning now requires `AuthoritativeRunInputSnapshots` at
`InitializedLoopRun::create_isolated`, screens the exact Provisioning and
Active state plus scaffold bytes before creating the run leaf, candidate, or
lock, and rederives the corpus from authenticated inputs on resume. This is a
pre-preview Rust source compatibility change that M1-12 and the preview release
notes must carry. Arbitrary configured values that collide with fixed
structural literals fail closed before side effects; supporting those values
successfully would require a future encoded/versioned representation and is
outside this slice.

Independent specification/security and quality re-reviews approve the final
boundary. Final gates pass: `cargo test --workspace --no-fail-fast --
--test-threads=1`, including CLI 142/142, provider/candidate boundary 75/75,
state 44/44, provider exchange 22/22, and every remaining Rust integration and
doc-test suite; strict all-target/all-feature Clippy; workspace check; Rust and
Prettier formatting; pinned-pnpm lint/typecheck/test/build with 8 SDK tests;
and diff hygiene.

### M1-12 - Interruption Recovery Acceptance

Status: accepted on 2026-07-13. Dependencies: M1-11 (complete). Milestone 1 is
complete and M2-01 is active.

Roadmap: U5 and Milestone 1 exit gate. Dependencies: M1-11.

Objective: prove safe restart across the complete reviewed lifecycle.

Acceptance criteria:

- Retries do not duplicate durable provider records, role/evaluation evidence,
  recovery entries, or unauthorized source changes. Provider calls are not
  claimed to be exactly once: request-only recovery may repeat an external
  call when the durable contract permits it.
- A complete evaluation prefix is adopted with zero provider or command calls.
  An incomplete evaluation prefix is never replayed in place; it can continue
  only through a new recovery-bound indexed attempt.
- Patch, review, Testing, and EvalReport interruption leaves the source
  worktree and index byte-exact. Authorized promotion interruption may leave
  only the exact approved patch unstaged and uncommitted; retry adopts those
  bytes without another source mutation.
- The focused Milestone 1 acceptance suite proves authoritative inputs, role
  dataflow, candidate isolation, approval, eval, promotion, and recovery.
- The preview handoff records that M1-09c1 added public v2 `TestingEvidence`
  fields: downstream Rust struct literals require an update, while persisted v1
  JSON remains readable. It also records that M1-11c made
  `InitializedLoopRun::create_isolated` require
  `AuthoritativeRunInputSnapshots`. Both notes are carried into preview release
  notes through `docs/preview-compatibility-handoff.md`.
- Roadmap/docs claim only the verified source-workspace path.

Likely seams: integration test harness, CLI tests, docs, and CI focused step.

RED: `scripts/test-milestone-one-acceptance.sh` passed the first three exact
tests, then selected the absent OutputReview response-cut regression and failed
loudly after Cargo reported `running 0 tests` / `0 passed`.

Verification: Milestone 1 acceptance suite, all Rust/TS gates, format, and diff
check.

Docs/tracker: accepted Milestone 1 evidence and M2-01 activation.

Commit boundary: integration/fault coverage and matching docs only.

Implemented evidence: the stable script uses `cargo test --locked`, exact test
selection, and `--test-threads=1`; it rejects zero-test Cargo success and ran 14
exact tests in 2m14s on the final tree. Separate exact selections prove the complete
canonical input snapshot set and full provider run, the Research-through-
SpecReview chain and Development's approved-spec/repository-context boundary,
complete-prefix zero-command adoption, and crash-cut convergence. A test-only
post-response-persistence observer
interrupts OutputReview after its exact response and response record are
durable but before its step artifact/final state. Isolated resume reuses retained
authoritative snapshots, makes zero provider calls, publishes one request and
response pair plus one OutputReview artifact, preserves the reviewed candidate
subject and every earlier artifact, reruns no patch operation, leaves source and
candidate Git/filesystem snapshots exact, and stops at `awaiting_human_review`.
The cut snapshots are asserted before entering resume and again after recovery.
The promotion crash test blocks final publication and requires two consecutive
identical complete authorized source snapshots before killing the process, so it
cannot select a torn or partially applied state. No production API changed.

Selected CLI failure and promotion tests now snapshot every regular file byte
and symlink target outside `.git`, plus HEAD, NUL-delimited status, staged
binary diff, and unstaged binary diff. Candidate Applying/Applied tests compare
that complete evidence before and after every publication cut and resume.
Testing invalidation/rerun preserves the source and every existing attempt-1
history byte, adding only its recovery pair and recovery-bound attempt 2. Failed
evaluation and every pre-promotion cut remain byte-exact. Promotion source bytes,
filesystem entries, and canonical binary worktree patch must equal the approved
candidate exactly; a killed promotion may leave only that authorized unstaged
patch, and its retry is source-byte inert.

The complete documented approve/evaluate/promote path requires a clean checkout
or worktree and omits `--allow-dirty`. Current source-workspace acceptance is
limited to macOS/Linux: the workflow runs on Ubuntu and local evidence is from
macOS; Windows or generic platform support is not claimed.

Independent specification and quality re-reviews approve the corrected
boundary with no remaining findings. The controller's final gate passed all 14
exact acceptance tests; workspace check; strict all-target/all-feature Clippy;
Rust and Prettier formatting; pinned-pnpm lint, typecheck, 8-test SDK suite, and
build; diff hygiene; and the complete locked serial Rust workspace suite,
including CLI 142/142, loop unit 286/286, provider/candidate 75/75, state 44/44,
provider exchange 22/22, and every remaining integration and doc-test suite.

### M2-01 - Generic Project Initialization

Status: active on 2026-07-13. Dependencies: M1-12 (accepted).

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
- Package/release notes consume the Rust source-compatibility entries in
  `docs/preview-compatibility-handoff.md`.

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
- The preview notes carry every entry from
  `docs/preview-compatibility-handoff.md`, including both pre-preview Rust
  source-compatibility changes.
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
