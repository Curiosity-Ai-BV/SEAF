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
At this historical M1-04 boundary, `seaf loop resume --rerun-from <step>`
published an immutable rerun authorization and reset state in one
exchange-locked transaction. M1-09b now retires that writer in both the CLI and
public runner API; historical authorization remains readable, while all new
provider recovery uses actor/reason-bound `loop revise` followed by exact
`loop rerun --recovery <id>`.
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
complete. M1-06a now atomically stops an isolated run after an authenticated
OutputReview `ApproveForTests` outcome and records `awaiting_human_review`
without running Testing or EvalReport. The barrier is closed against provider,
rerun, reconciliation, cleanup, and public-writer bypasses; historical
unapproved Testing/Eval prefixes fail before ticket or provider work. M1-06b,
the exact human approval transaction, now publishes a versioned inline approval
record under candidate-to-provider locking and full-state compare-and-swap.
`seaf loop approve` requires the reviewer to confirm the exact staged diff and
target HEAD exposed by public run/status output; it reauthenticates policy,
review artifact, provider exchanges, source HEAD, and physical candidate state
inside the publication lock. Approved runs remain inert and non-cleanable.
M1-06 is complete. M1-07 is split into a reusable controlled engine, immutable
eval-configuration authority, and one approved Testing/EvalReport transaction.
The engine extraction is complete: shared typed configuration and controlled
planning/execution preserve valid standalone behavior, preflight all checks,
intersect both allowlists, confine candidate-relative execution paths, redact
before the persisted output cap, and reject ambiguous log identities without
side effects. Immutable eval authority is also complete: new provider runs
require a normalized repository-root config, bind its canonical typed bytes and
digest, and preflight an exact snapshot prefix before resume recovery. The
approved Testing/EvalReport work is split into inert evidence/terminal contracts
and the locked execution transaction. The contract slice is complete: canonical
Testing evidence and approval-bound final states now have one combined authority
validator, passing outcomes are immutable, reported failures permit only ordered
candidate cleanup, and all direct execution paths remain closed. The locked
execution transaction is also complete. Exact Approved resume now runs only
immutable ticket/eval checks in the candidate without a provider call, after
complete preflight and a create-only execution intent. It publishes indexed
redacted logs, canonical Testing evidence, and a bound EvalReport, then
atomically records `eval_passed` or approval-bound reported failure. Interrupted
attempts refuse silent replay; lasting source, candidate, command-identity, or
artifact drift blocks terminal publication. M1-08 promotion integrity is also
complete. `loop promote` requires a fresh exact candidate/EvalReport/target-HEAD
confirmation, publishes durable intent before mutation, applies only the frozen
evaluated patch to a clean target, and records immutable `promoted` authority.
Crash retry adopts only the exact already-applied patch; the result stays
unstaged and uncommitted, and the candidate is retained. M1-09a attempt-safe
history and factual inspection is complete. Structured role artifacts now bind
the exact durable attempt with create-only paths; ambiguous fixed-name reuse is
forensic-only and blocks rerun before reset. `loop inspect` is byte-inert,
authenticates the provider chain, safely verifies physical candidate authority
without executing repository helpers, and retains current/head evidence under
deterministic output caps. M1-09b audited provider recovery is complete.
`loop revise` publishes a create-only source snapshot and recovery decision
under candidate-to-provider full CAS without a provider call; only exact `loop
rerun --recovery <id>` may consume its first request. Attempts, provider ledger,
role artifacts, and policy artifacts remain immutable, while selected and
downstream current pointers are reset. The legacy rerun writer is retired.
M1-09c1 versioned evaluation authority is complete. New evaluations publish
strict attempt-001 v2 intent, logs, Testing, and EvalReport; fixed v1 final
evidence stays readable, while mixed, malformed, future, gapped, surplus, or
cross-attempt authority fails closed. Final validation and promotion select the
exact Testing-bound intent. M1-09c2 typed evaluation recovery and zero-command
adoption are also complete: mixed provider-v1/evaluation-v2 lineage is
authenticated, complete fixed-v1 or indexed-v2 prefixes can be adopted with an
exact existing or deterministically created EvalReport, and adopted finals
reconstruct their Approved source without weakening cleanup, promotion, or
frozen passing authority. M1-09c3 evaluation invalidation and fresh rerun is
also complete: dedicated v3 invalidation authority preserves incomplete or
failed evaluation history, reconstructs the exact Approved predecessor, and
authorizes one recovery-bound indexed attempt without replaying a partial
attempt. Repeated invalidation, zero-command adoption of complete recovered
prefixes, exact retry, promotion, drift, tamper, crash, and concurrent-winner
paths fail closed or converge as specified. M1-10 atomic state and run locking
is complete. One permanent per-run lock now serializes every cooperative
`run.json` mutation, while create-only publication, byte-exact retry, and
expected-to-intended compare-and-swap distinguish initialization, recovery,
and ordinary updates. Same-directory synced temporary files keep the old or
complete intended state across write, sync, link, rename, and directory-sync
cuts. Reads and replacements reject symlink or target-identity substitution;
bounded contention fails closed; exact retries reauthenticate and close
post-publication durability uncertainty. Provider history, candidate,
recovery, approval, evaluation, and promotion authority retain their narrower
transition guards and lock order. M1-11a private run artifacts is also
complete: supported Unix run trees are private from creation, existing broad
modes fail closed with remediation, and pinned directory-handle publication
protects run files, locks, temporaries, and final artifacts from pathname and
parent-substitution races. Source/candidate Git modes and standalone eval or
release files are unchanged. M1-11b1 serialized artifact limits is complete:
semantic caps and a pinned 32 MiB physical run-tree budget now cover every
cooperative publisher under the permanent run lock, while exact retry,
replacement peaks, concurrency, unsafe entries, bounded reads, and external Git
planning-temporary isolation are verified. Aggregate enumeration streams one
name at a time and is scanner-wide bounded to 4,096 non-dot entries and eight
descendant-directory levels from the depth-zero root, preventing zero-byte
files, hard-link names, or nested empty directories from causing unbounded
metadata retention or recursion. Under the permanent run lock, first-lock and
directory creation reserve one entry, new file publication reserves the
two-name temporary/final hard-link peak, replacement reserves one temporary
name, and exact existing retry reserves zero; runtime scaffolding shares these
checks. Entry-only lock and directory projections first validate that current
aggregate bytes remain within 32 MiB. The candidate-workspace lock is a guarded
permanent scaffold artifact; missing historical locks migrate only after
authority validation, with the run guard released before open-only candidate
acquisition to preserve candidate-before-run lock order. External
repository-operation locks remain outside this policy. M1-11b2 is split:
provider-side M1-11b2a and evaluation-side M1-11b2b are accepted. The
evaluation implementation
derives missing per-stream maxima, bounded Testing and EvalReport artifacts,
two bounded recovery artifacts, every future permanent name, and the full
future `run.json` replacement plus one transient name at the atomic coexistence
peak. It activates before intent publication, is reauthenticated under the run
guard before every command spawn, and transitions through adoption and
invalidation without a reservation file or holding the run guard across command
latency. Unauthenticated same-name logs and staged reports retain the unfilled
remainder of their cap until durable digest authority validates them. Reopening
the run guard reclaims only canonical private orphan `run.json` replacement
temporaries before commitment validation, so final and invalidation retry do
not double-count a dead prior writer's transient slot. Complete staged v2/v3
source and supersession verification, live command lock-release evidence, and a
literal production-used v2/v3 transition table were added after the first
independent review. Corrected focused and boundary gates pass, and final
independent specification, quality, and evidence re-reviews approve the slice.
M1-11b2 and M1-11b are complete. Second re-review additionally required every
staged v2/v3 source to select the
active latest factual attempt. That binding and end-to-end authentic
CreateMissing residual coverage are implemented, and the live command-lock
test is bounded and cleanup-safe. Corrected Rust owner gates pass, including library
225/225, provider/candidate boundary 65/65, and full-workspace CLI 138/138;
strict lint, formatting, and diff-hygiene gates pass as well.
M1-11c bounded secret redaction is accepted after independent
specification/security and quality review. Bounded corpus derivation and
byte-oriented screening now protect exact provider, evaluation, operator,
recovery, context, scaffold, log, and run-state envelopes before side effects;
versioned intent keeps configured values out of derived evidence, clean v1/v2
history remains readable, and unsafe legacy history fails closed. Fresh
isolated provisioning requires authoritative input snapshots and screens its
exact state and scaffold before creating the run leaf, candidate, or lock.
Full workspace, strict lint, formatting, SDK, and diff gates pass. M1-11 and
M1-12 interruption-recovery acceptance are complete. The corrected 14-test
focused gate separately proves complete input
snapshots, early and Development role dataflow, complete-prefix zero-command
adoption, crash-cut convergence, complete source/candidate snapshots at every
selected recovery cut, immutable Testing attempt history, and exact approved-
patch promotion.
Authoritative input changes still require a new run;
EvalPassed/Promoted and M1-08 promotion intent remain frozen.
Human approval authorizes local execution under the developer account: SEAF
validates command configuration and detects repository drift, but it does not
contain approved code from malicious same-user filesystem access.

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
   policy, eval, ticket, project configuration, documented CLI provider
   selection, and an ignore template.
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
| 1         | M1-01a - M1-12 | complete |
| 2         | M2-01 - M2-07  | active   |
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
  interruption at patch, review, Testing, EvalReport, and promotion boundaries.
  Complete evaluation prefixes adopt without provider or command calls;
  incomplete evaluation is never replayed in place and requires a new
  recovery-bound indexed attempt. Retries must not duplicate durable provider
  records, evidence, recovery entries, or unauthorized source changes; this is
  not an external-call exactly-once guarantee. Before live Ollama use, enforce
  private run-directory permissions, provider-response and storage caps, and
  prompt/response redaction.

Exit gate: focused tests prove authoritative inputs, role-to-role data flow,
candidate isolation, human approval, controlled evals, bound reports, promotion
integrity, and recovery. A failed run leaves the source checkout byte-for-byte
unchanged. Patch/review/evaluation interruption also leaves the worktree and
index exact. An interrupted authorized promotion may leave only the approved
patch unstaged and uncommitted; retry adopts it without further source
mutation. This gate runs SEAF from its Cargo source workspace only; packaging,
generic initialization, and external-project adoption belong to Milestone 2.

Accepted 2026-07-13: the named 14-test Milestone 1 gate proves the complete
source-workspace boundary above and runs in Ubuntu CI. Independent specification
and quality reviews approved the corrected fault coverage. The controller also
passed workspace check, strict Clippy, Rust and Prettier formatting, every
locked serial Rust workspace test, all pinned-pnpm SDK gates, and diff hygiene.
Milestone 1 and M2-01 through M2-04 are complete. Package identity is private,
exact, warning-free, and proven through pristine archives plus an external
installed-CLI smoke. Independent specification and quality reviews approved the
corrected package and release-artifact boundaries, and the full controller gates
passed. M2-05 and U7 were accepted on 2026-07-14. M2-06 and U8 are now also
accepted. M2-07 implementation and its reviewed remediation chain are accepted;
fresh sanitized live Ollama acceptance evidence remains pending.

### Milestone 2 - Make The Loop Consumable

Goal: a developer can adopt SEAF without understanding this monorepo.

- **U6 - Ship a generic project bootstrap.** Replace the Adaptive Notes-only
  default with stack-neutral initialization plus explicit optional examples.
  Generate a starter ticket, project policy, eval config, project config, and
  `.gitignore` entries; keep provider selection as documented CLI authority.
  Add `seaf doctor` for Git, model, configuration, candidate-workspace, and
  eval-command readiness.
- **U7 - Distribute a versioned CLI.** Add `seaf --version`, complete Cargo
  package metadata and versioned internal dependencies, a license and changelog,
  and tagged macOS/Linux binary releases with checksums. Test the packaged
  binary rather than only `cargo run` from the workspace.
- **U8 - Add an external golden path.** Maintain a small fixture repository
  outside the SEAF source tree. CI must install the packaged CLI, initialize the
  fixture, run a real candidate patch through the fake provider, execute its
  native tests, exercise interrupt/resume and rejection, and validate every
  referenced artifact. Rewrite the README and loop docs from that tested flow.

M2-01 accepted 2026-07-13: generic init emits the
five documented project files, executes the generated native checks in minimal
Rust, Node, hybrid, and Git-only fixtures, preserves Adaptive Notes only behind
explicit opt-in, and refuses target/ancestor conflicts without mutation.
Provider selection remains explicit CLI authority. Independent specification
and quality reviews approved the boundary, and the full controller gate passed.
M2-02 project doctor was accepted 2026-07-13 after independent specification
and quality reviews approved its typed eight-check report, read-only candidate/
eval planning, fake and explicit-live provider boundaries, complete failure
aggregation, and ready init-generated Rust, Node, hybrid, and Git-only plans.
The corrected transport uses one absolute deadline and a 1 MiB raw-response
cap; diagnostic candidate planning is source-name-independent; explicit ticket,
config, and policy behavior matches `loop run`; fake-only options fail offline;
and complete snapshots prove project, Git, worktree, and candidate namespaces
remain unchanged. The full Rust and SDK controller gates passed. M2-03 package
identity is accepted after exact archive/install smoke, adversarial boundary
guards, independent reviews, and the full controller matrix. M2-04 release
artifact workflow and M2-05 tagged prerelease are accepted. At that historical
M2-05 handoff, the external golden path had not yet been claimed.

M2-04 implementation evidence on 2026-07-13 records the required missing-script
and missing-workflow REDs plus a missing structure-only verifier RED. The final
local gate proves deterministic native archive bytes, normalized metadata,
bounded adversarial inventory and checksum refusal, non-executing aggregate
validation, external installed-binary identity, static read-only workflow
authority, and Git-status preservation. Formatting, stable strict Clippy,
package readiness, SDK, and diff gates pass. That first implementation evidence
was not acceptance or tagged publication; final acceptance follows the
correction and reviews below. At that boundary, M2-05 still awaited exact-SHA
authorization.

Quality review rejected M2-04 on 2026-07-14 for incomplete process identity,
incorrect Bash 3.2 file-limit units, nontransactional failed outputs, and
incomplete gzip/USTAR byte validation. The correction has dynamic RED evidence
for all four groups plus a static cross-step workflow-helper RED. The corrected
focused suite is GREEN for zero-status/exact-stdout/empty-stderr identity,
scoped 64 MiB identity-output and 128 MiB decompression boundaries, exact output
rollback, full normalized metadata mutations, and same-step workflow helper
authority. Fresh independent specification and quality/security reviews
approved the correction with no findings, and the complete controller gate
passed. M2-04 was accepted without M2-05 external authority; at that review
boundary, the tagged prerelease still awaited authorization.

M2-05 preflight on 2026-07-14 used prior authorization limited to exact commit
`29c2cba739bdbc75bf871220b498bf66d6d82c4d`. Ordinary-CI run
[`29312976772`](https://github.com/Curiosity-Ai-BV/SEAF/actions/runs/29312976772)
failed its Rust release-artifact gate on GNU tar USTAR device fields before any
tag or release existed. Corrections `7b895b5` and `f4d7c28` passed independent
reviews and final macOS plus exact `linux/amd64` Rust 1.97/GNU tar 1.34 gates.
The active
[`Protect v0.1.0`](https://github.com/Curiosity-Ai-BV/SEAF/rules/18918424)
tag ruleset and immutable releases with automatic attestation are live;
the preflight stopped with `v0.1.0` and all releases absent.

M2-05 and U7 were accepted on 2026-07-14 after fresh authorization named exact
commit `f4d7c28d27c345a8b0d7f6cc48c8c833b48f248a`. Only lightweight tag
`v0.1.0` was pushed directly to that commit; no branch was pushed. The single
initial [release workflow run](https://github.com/Curiosity-Ai-BV/SEAF/actions/runs/29318734239)
completed successfully on attempt 1 with both native builds and checksum
assembly successful. Its exact three nonexpired artifacts passed safe ZIP
inventory and aggregate checksum verification. The packaged macOS arm64 binary
then passed exact empty-stderr version/info, generic five-file init, and
eight-check fake-provider doctor smoke in a clean external repository. Linux
execution evidence is the successful Ubuntu workflow job, not the local smoke.

The verified assets were byte-checked while draft and after publication in the
[immutable `v0.1.0` prerelease](https://github.com/Curiosity-Ai-BV/SEAF/releases/tag/v0.1.0).
`gh release verify-asset` verified all three automatic GitHub release
attestations. Those attestations come from immutable GitHub Release publication;
the build workflow retains read-only contents permission and no write, OIDC, or
attestation authority.

M2-06 and U8 were accepted on 2026-07-14. Ordinary CI now builds the current
native CLI with locked offline Cargo, constructs and fully verifies its release
archive before extraction, and uses only the installed external binary in two
fresh Git repositories with external run and control roots. The passing path
proves exact identity, generic init, ticket/commit/doctor, wrong and exact human
approval, real evaluation interruption, refusal of ordinary replay, audited
attempt-2 invalidation/rerun without provider replay, stable inspection, and
exact approved-candidate promotion. The separate rejection path proves a
terminal rejecting EvalReport with exact exit-24 summary, empty referenced
stdout, and exact referenced rejection stderr. Explicit nonempty untracked file
and symlink sentinels retain their inventory, bytes, modes, and target through
failure and candidate cleanup. Recursive bounded inventory and digest traversal
validates the run artifact graph, while the harness compares the SEAF source
checkout on every exit. Local post-install adoption completed in 9 seconds, 8
seconds, and 8 seconds on the three pre-review runs. The expected SIGKILL shell
notification remains visible; successful eval cleanup may emit only a narrowly
bounded family of platform-rendered variants with an optional `/bin/` prefix and
optional parentheses around the same negative PID and `No such process`
semantics. Total stderr is capped at 4 KiB and no other stderr is permitted.
M2-07 implementation and its reviewed remediation chain are accepted, but no
successful sanitized live Ollama evidence has been published. Milestone 2
therefore remains active and Ollama acceptance is not claimed.

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
  **M3-01 is complete:** `seaf-core` owns the shared `PolicyDecision` contract,
  `LoopRun` stores typed decisions, and the five named Rust contracts have
  schema-drift coverage. The user authorized this slice ahead of the recorded
  M2-07 dependency; M2-07 live acceptance remains unexecuted, and neither U9
  nor Milestone 3 is complete. **M3-02a is complete:** after independent
  specification and quality
  approval plus the final controller gate, the five durable contracts emit
  explicit schema version 1, read legacy unversioned v0 and current v1, and
  reject explicit unsupported versions without mutating input files. Whole-run
  migration M3-02b is complete after independent specification and quality
  approval plus the final controller gate, so M3-02 is complete. **M3-03 is
  complete:** the explicit
  managed-byte policy, dry-run/apply CLI,
  protected active/locked/migrated state, intent-owned tombstones, repeatable
  interrupted purge, single-link refusal, immutable decision evidence,
  separately normalized convergence evidence, and bounded verified latest
  audit are present. Only Passed and Completed authority is purgeable;
  EvalPassed and Promoted remain protected because their supported candidate
  cleanup is unavailable. The review correction adds numeric timestamp
  ordering, partial-deletion recovery, relocatable audit identity, atomic
  result replacement, and compact one-run batches with proven 2 MiB intent and
  8 MiB cumulative-audit bounds. The batching re-review correction preserves
  continuation history across drift, separates the 4,096 operator-entry cap
  from four bounded SEAF controls, and uses fixed-length intent-validated
  tombstone digests. A later cross-slice review found that real pre-publication
  migration crash states could expose the unlocked source to purge. The focused
  correction authenticates and protects matching `source + intent` and
  `source + staged + intent` state while refusing malformed matching controls;
  unrelated dot siblings do not protect ordinary runs. Independent cross-slice
  re-review approved the correction with no remaining P0/P1/P2 findings, and
  the repeated full controller gate passed. U9 is complete. M2-07 remains
  unexecuted, and Milestones 2 and 3 remain incomplete; M3-04 is next.
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
- OS-level containment of approved commands. A sandbox, container, or isolated
  execution-repository backend requires a separate product-hardening roadmap.

## Roadmap Discipline

- Work in the listed order; U1 through U5 are one product milestone and should
  not be diluted by dashboard, signing, or telemetry work.
- Each ticket gets a failing regression or external acceptance test before the
  implementation when behavior changes.
- Update the README, local-loop guide, examples, and `.seaf/loops/current/`
  tracker in the same slice as the behavior they describe.
- Do not call the loop passed unless the exact reviewed candidate has passing
  deterministic checks and a bound EvalReport.
