# Production Use Roadmap

Date: 2026-07-11

Assessed branch: `codex/seaf-foundation-agent-loop` at `1013207` (already
merged into `origin/main` by PR #3)

## Product Decision

The first supported SEAF product should be the **supervised local coding
loop**: a developer installs the CLI in an existing repository, supplies a
ticket and project policy, receives an isolated candidate patch, runs bounded
checks, reviews the evidence, and explicitly promotes the change.

This is narrower than the full vision in `README.md`. Telemetry ingestion, a
dashboard, signed updates, cloud agents, and autonomous merge/deploy remain
experimental or deferred until the local loop works reliably in real projects.
Calling those surfaces production-ready now would spread effort across several
unfinished products.

## Current Verdict

SEAF is a production-conscious prototype, not yet a tool that should modify a
real project unattended.

The previous roadmap delivered its baseline and provider-loop mechanics:

- P3-001 through P3-005 are complete: current tracking, policy categories,
  generated-artifact hygiene, and deterministic CI were hardened.
- P3-006 through P3-010 are mechanically present: fake and Ollama providers use
  `ProviderStepRunner`; context and provider exchanges are persisted; developer
  patches pass through the policy gate; eval commands are bounded; and recovery
  failures have focused coverage.
- The latest `main` CI run passed. Local Rust formatting, Clippy, the full
  180-test Rust suite, and all TypeScript format/lint/typecheck/test/build checks
  also pass at this assessment baseline.

The remaining blocker is workflow integrity, not another broad set of platform
features:

1. Each model role receives a generic instruction and the same initial file
   context. It does not receive the complete ticket, effective project policy,
   or validated outputs from earlier roles. The output reviewer therefore does
   not review the exact proposed patch.
2. Provider-backed Development is now proposal-only: ticket apply intent is
   audited, but the policy gate never applies the patch to the developer's
   checkout. Evals therefore still test the unchanged checkout because there
   is no isolated candidate workspace yet.
3. The loop's Testing and EvalReport steps are explicit no-ops. The real eval
   runner is a separate manual command, `ticket.eval.config` is not consumed by
   `loop run`, and the report is not attached to the `LoopRun`.
4. Blocked and failed runs are terminal. `loop resume` cannot revise them even
   though the docs say it can, and there is no audited CLI rerun-from-step flow.
5. External adoption is unsupported: no released CLI binary or documented
   install path, no `--version`, Adaptive Notes-specific initialization, and no
   end-to-end test in a repository outside SEAF.
6. The SDK defaults to an HTTP endpoint for which SEAF ships no server. Release
   capsules are unsigned development metadata. Both surfaces currently
   overstate their usable lifecycle.

M1-01b and M1-02 resolved the former project-authority gap: provider runs use
explicit or Git-root project authority, persist canonical effective inputs and
repository identity, and verify them before an incomplete resume can mutate
run state or contact the provider.

M1-03a and M1-03b resolved workflow-integrity item 1: every model role now
consumes its exact validated prerequisites and persists canonically verified
run-bound artifacts. Development is bound to the approved spec and the bounded
source context needed to construct a patch. Output review receives only the
persisted policy-gated DevelopmentEvidence, approved-spec identities, and run
input digests. M1-04a now defines and validates bounded typed context requests;
M1-04b1 now safely materializes each additive request as an immutable,
canonical, cumulative-budgeted artifact whose prior accepted bytes do not
depend on the changed live repository. Its expected initial path/byte metadata
is bound but remains caller supplied. M1-04b2a now defines the authoritative
ordered exchange ledger: immutable request/response records and typed LoopRun
references bind every audited call, its exact role outcome, the run-wide prior
record, and any trusted M1-04b1 expansion identity. Its narrow stable lock and
atomic synced state replacement protect the ledger append that must precede a
later provider call; M1-10 still generalizes those guarantees to all other
state mutations. This is cooperative SEAF-process locking, not protection from
a hostile same-user process replacing run-directory entries; M1-10 and M1-11
own that stronger boundary. Outcomes are derived from canonical full provider
response/failure audits through the exact role parser rather than trusted from
callers; only malformed JSON is repair-eligible, while schema, role, reviewer,
and context-contract invalidity is terminal. M1-04b2b now uses that ledger for
fresh provider execution: each request is durable before its call, each full
response/failure audit is durable before interpretation, and accepted
same-role context retries rebuild the prompt only from the verified initial
exchange request and ordered immutable expansion artifacts. The exact ticket
remains in the original role input even when its metadata names a context path;
each content-bearing initial or additive file entry appears once in the
combined repository-context and expansion payload. Two expansions per logical
step and eight per run are enforced across attempts and roles; initial and
JSON-repair calls consume neither cap. Denied context requests end blocked with
evidence, trusted-audit and post-response interpretation failures end failed
with evidence when safe state publication remains possible, and immutable
publication failures stop without another call or false terminal claim. Typed
response audits are published before their classification is derived inside
the response-record seam, and that record is durable before orchestration acts.
Every post-creation `LoopRunner` state publication uses the same narrow lock and
an exact exchange-vector compare, including when the intended vector is empty,
so it cannot erase a concurrent cooperative first request or later suffix. The
compare protects provider-exchange history only; general state coordination
remains M1-10 scope.
M1-04b2c now applies the same bounded protocol to resume and explicit rerun.
Recovery verifies the authoritative chain, adopts only one uniquely linked
canonical staged-record suffix, and rejects raw orphans, missing bytes,
reordering, substitution, digest failure, unsafe file types, and skipped
attempts before another provider call. Conventional prompt crash cuts reuse
only the exact byte-identical authorized attempt. Structured metadata inside
the immutable initial request binds the one content-bearing repository-context
payload, so resumed rounds reconstruct accepted bytes and original limits
without rereading changed sources. Context-free roles remain recoverable.
`seaf loop resume --rerun-from <step>` publishes an immutable rerun
authorization and reset state in one exchange-locked transaction, then uses a
fresh exact attempt while preserving the two-per-step and eight-per-run caps.
Recovery validates the exact conventional prompt before staged-head adoption.
Terminal legacy `needs_context` history stays inert until that explicit rerun.
Real CLI tests cover default resume, repository changes, cap exhaustion, and
request, response, expansion, and record tampering.

M1-05a defines the candidate lifecycle boundary independently of provider
integration. A detached worktree lives at a deterministic absolute path outside
the source checkout and is bound to the exact source/common-directory identity,
starting HEAD/tree, empty pre-apply candidate tree/diff, and lifecycle in closed
typed LoopRun state. Creation bypasses hooks and configured content filters and
can adopt only the exact clean registered detached crash remnant. Validation
streams raw index objects through one bounded batch process, ignores replace
refs, and fails closed on path, repository, registration, HEAD/tree, mode,
index/worktree, extra bytes, digest, lifecycle, or helper/config injection
drift. Creation is serialized and Unix authority directories are private.
Cleanup uses full-LoopRun compare-and-swap, durably records intent, and
reconciles post-removal interruption. Independent spec and quality review plus
the full repository gate accepted M1-05a on 2026-07-11.

M1-05b1 now adds the candidate-only indexed patch transaction. Explicit
isolated execution binds canonical Development and policy evidence to a planned
tree and staged diff, persists Applying before index mutation, raw-materializes
the exact resulting index objects, and persists Applied evidence afterward.
Recovery recomputes the authoritative plan and accepts only pristine or exact
planned state; coherent tampering, partial indexes, unrelated drift, configured
filters, ident expansion, and source-checkout mutation are covered.

M1-05b2 now makes every new provider CLI run isolated. Provisioning authority
is persisted before worktree creation; Active candidate validation precedes
scaffold, complete create-only input snapshots, context, reconciliation,
provider calls, and semantic logs. Resume repairs exact missing snapshot
prefixes only after candidate validation. Initial and additive context bind the
candidate identity, patch gating runs check-only in the candidate, and the
source checkout remains unchanged even when apply intent is true or source
bytes are dirty. Legacy provider execution and rerun fail closed while the
deterministic non-provider runner remains compatible.

M1-05b3 now durably publishes completed Development and policy evidence before
applying it only to the isolated candidate. OutputReview is built from a
locked, verified projection of the exact Applied tree and staged diff. Resume
recovers None/Applying/Applied transaction cuts; only OutputReview may rerun
after Applied, and forbidden reruns fail before mutation. M1-05b4a binds every
candidate operation to the canonical original run directory and makes legacy
authority forensic-only, so copied or moved run state cannot operate on or
remove the real candidate. M1-05b4b now exposes only explicit, run-targeted
cleanup, binds the caller repository and persisted run identity before
repository-lock selection, returns one locked Cleaned outcome, and preserves
the source checkout across success, refusal, retry, and timeout. M1-05 is
complete and M1-06 human approval is active. The candidate is still not
executable or promotable before the later human-approval and eval milestones.

The B1 boundary also preserves pre-B1 candidate runs through a narrow
missing-mode migration, atomically publishes and directory-syncs immutable
artifacts, skips orphaned private planning indexes, requires completed
Development on a running run, and proves real interruption recovery at every
Applying/Applied publication cut. Exact directory/file transitions are raw-
materialized safely; unrelated contents remain a hard stop.

## Production-Use Acceptance Scenario

This scenario is the release gate for the roadmap. A developer must be able to:

1. Install a versioned `seaf` binary without cloning this repository.
2. Initialize a generic, clean external Git repository and obtain editable
   policy, eval, ticket, provider, and ignore templates.
3. Run a small test-covered ticket with Ollama in an isolated candidate
   worktree. The original checkout must not be mutated.
4. Inspect evidence showing that each role received the ticket and preceding
   validated outputs, and that the reviewer evaluated the exact candidate diff.
5. Approve the exact candidate diff before executing model-modified code, then
   run the ticket-configured checks against that candidate and receive a passing
   or failing EvalReport linked from the LoopRun.
6. Interrupt and resume once, then revise and rerun a reviewer-blocked step
   without losing or silently replacing audit history.
7. Explicitly promote a passing candidate after human review. Any rejected,
   blocked, timed-out, or failing run must leave the original checkout
   unchanged.

All context digests, prompts, responses, patch evidence, policy decisions,
command logs, and the EvalReport must remain inspectable and redact obvious
secrets.

## Lean Roadmap

### Execution Status

Detailed slice contracts and review gates are maintained in
`docs/production-use-implementation-plan.md`.

| Milestone | Slices         | Status   |
| --------- | -------------- | -------- |
| Contract  | S0             | complete |
| 1         | M1-01a - M1-12 | active   |
| 2         | M2-01 - M2-07  | pending  |
| 3         | M3-01 - M3-06  | pending  |

### Milestone 1 - Make One Loop Coherent And Safe

Goal: one command produces a reviewable candidate and trustworthy evidence.

- **U1 - Make project inputs authoritative.** Add explicit project config and
  policy discovery with documented precedence. Snapshot the complete ticket,
  effective policy, config, repository identity, and their digests into the
  run. Resume must fail closed if those inputs have changed unless the user
  starts a new audited attempt.
- **U2 - Pass validated state between roles.** Research receives the ticket;
  analysis receives research; spec creation receives both; spec review receives
  the proposed spec; development receives the approved spec; output review
  receives the exact normalized patch and policy decision. Persist each
  structured role artifact separately. Add a bounded request-more-context flow
  so success does not depend on perfect upfront file enumeration.
- **U3 - Isolate the candidate lifecycle.** Create a dedicated temporary Git
  worktree or equivalent candidate workspace. Generate, policy-check, apply,
  review, and test the same patch there. Do not mutate the user's checkout or
  commit/merge automatically. Add explicit `awaiting_human_review`,
  `eval_passed`, and `promoted` states. Human-review decisions must block tests
  and promotion, not be represented as a passed run. Promotion must verify the
  candidate patch digest, bound EvalReport, target HEAD and cleanliness, and a
  fresh human confirmation before applying to the original checkout.
- **U4 - Integrate Testing and EvalReport.** Replace both no-op steps with the
  existing controlled command runner and EvalReport builder. Consume
  `ticket.eval.config`, run both ticket and eval allowlists in the candidate
  workspace, persist logs, bind real policy evidence, set
  `LoopRun.eval_report_path`, and fail the loop when checks or evidence fail.
  Do not execute model-modified code before the exact diff is approved by a
  human.
- **U5 - Make recovery real.** Add audited CLI operations to inspect, revise,
  and rerun from a named step. Preserve attempt history, bind the candidate and
  config snapshots, use atomic state replacement and a per-run lock, and cover
  interruption at patch, review, testing, and report boundaries. Before live
  Ollama use, enforce private run-directory permissions, provider-response and
  storage caps, and prompt/response redaction.

Exit gate: focused tests prove authoritative inputs, role-to-role data flow,
candidate isolation, human approval, controlled evals, bound reports, promotion
integrity, and recovery. A failed run leaves the source checkout byte-for-byte
unchanged. This gate may still run SEAF from its source workspace; packaging and
external initialization belong to Milestone 2.

### Milestone 2 - Make The Loop Consumable

Goal: a developer can adopt SEAF without understanding this monorepo.

- **U6 - Ship a generic project bootstrap.** Replace the Adaptive Notes-only
  default with stack-neutral initialization plus explicit optional examples.
  Generate a starter ticket, project policy, eval config, provider config, and
  `.gitignore` entries. Add `seaf doctor` for Git, model, configuration,
  candidate-workspace, and eval-command readiness.
- **U7 - Distribute a versioned CLI.** Add `seaf --version`, complete Cargo
  package metadata and versioned internal dependencies, a license and changelog,
  and tagged macOS/Linux binary releases with checksums. Test the packaged
  binary rather than only `cargo run` from the workspace.
- **U8 - Add an external golden path.** Maintain a small fixture repository
  outside the SEAF source tree. CI must install the packaged CLI, initialize the
  fixture, run a real candidate patch through the fake provider, execute its
  native tests, exercise interrupt/resume and rejection, and validate every
  referenced artifact. Rewrite the README and loop docs from that tested flow.

Exit gate: a new developer can complete the fake-provider path from the public
quickstart in under 15 minutes. The full production-use acceptance scenario
passes with the packaged fake provider in CI and locally with Ollama.

### Milestone 3 - Stabilize Through Real Project Pilots

Goal: turn observed usage failures into a small, supportable `0.x` contract.

- **U9 - Version and protect durable artifacts.** Type `PolicyDecision` in the
  shared contract layer; add schema drift tests for Ticket, Policy, LoopRun,
  PolicyDecision, and EvalReport; version on-disk formats; and define compatible
  read/migration behavior. Enforce private state-directory permissions, storage
  budgets, and retention/purge controls.
- **U10 - Dogfood two real repositories.** Complete at least five bounded
  tickets across two different stacks, including an approved patch, a policy
  rejection, an eval failure, and an interrupted/resumed run. Track setup time,
  patch applicability, review corrections, eval reliability, recovery success,
  and manual workarounds. Every safety or data-loss failure becomes a
  regression before release.
- **U11 - Cut the first supported preview.** Publish the tested binaries,
  compatibility notes, security reporting path, support boundary, and release
  procedure. The release notes must state that promotion remains manual and
  telemetry, release capsules, and update delivery are experimental.

Exit gate: both pilot repositories complete the acceptance scenario without
editing SEAF artifacts by hand, no unresolved safety/data-loss defects remain,
and upgrade/recovery behavior is documented and tested.

## Explicitly Deferred

- Dashboard or multi-user service.
- Cloud model providers and credential storage.
- Automatic PR creation, commit, merge, deployment, or rollback.
- Production signing and verified updater infrastructure.
- Treating `@seaf/sdk` or `seaf-local-runtime` as supported. If telemetry is
  required by pilot feedback, give it a separate roadmap covering a loopback
  service, bounded SDK timeouts/failure semantics, database migrations,
  retention, privacy handling, and one event-to-signal integration test.
- Generating every public contract across Rust, TypeScript, and JSON Schema
  before the smaller loop artifact set has stabilized.

## Roadmap Discipline

- Work in the listed order; U1 through U5 are one product milestone and should
  not be diluted by dashboard, signing, or telemetry work.
- Each ticket gets a failing regression or external acceptance test before the
  implementation when behavior changes.
- Update the README, local-loop guide, examples, and `.seaf/loops/current/`
  tracker in the same slice as the behavior they describe.
- Do not call the loop passed unless the exact reviewed candidate has passing
  deterministic checks and a bound EvalReport.
