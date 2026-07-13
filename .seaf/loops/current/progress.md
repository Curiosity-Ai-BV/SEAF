# Progress

## Baseline

- [x] Fresh production-use roadmap authored and independently reviewed (`4a20922`).
- [x] S0: Establish shared execution contract and tracker.

## Milestone 1 - Coherent And Safe Loop

- [x] M1-01a: Project configuration and input digest contracts.
- [x] M1-01b: Authoritative configuration discovery and snapshots.
- [x] M1-02: Resume configuration integrity.
- [x] M1-03a: Validated early role artifact chain.
- [x] M1-03b: Development and exact output-review evidence.
- [x] M1-04a: Context request contract.
- [x] M1-R01: Stabilize descendant pipe cleanup regression.
- [x] M1-04b1: Additive context expansion artifact.
- [x] M1-04b2a: Durable context exchange contract.
- [x] M1-04b2b: Bounded live context orchestration.
- [x] M1-04b2c: Context round recovery and CLI integration.
- [x] M1-05a: Candidate workspace lifecycle contract.
- [x] M1-05b1: Indexed candidate patch transaction.
- [x] M1-05b2: Provider start/resume candidate authority.
- [x] M1-05b3: Development/output-review candidate integration.
- [x] M1-05b4a: Authoritative run-directory binding.
- [x] M1-05b4b: Explicit candidate cleanup CLI.
- [x] M1-06a: Stop isolated runs for human review.
- [x] M1-06b: Record exact human candidate approvals.
- [x] M1-07a: Reusable controlled eval engine.
- [x] M1-07b: Immutable eval configuration authority.
- [x] M1-07c1: Evaluation evidence and terminal contracts.
- [x] M1-07c2: Locked Approved Testing and EvalReport transaction.
- [x] M1-08: Promotion integrity.
- [x] M1-09a: Attempt-safe role artifacts and factual inspect.
- [x] M1-09b: Audited provider revise and rerun.
- [x] M1-09c1: Versioned evaluation attempt authority.
- [x] M1-09c2a: Versioned evaluation recovery authority.
- [x] M1-09c2b: Zero-command adoption transaction.
- [x] M1-09c3: Evaluation invalidation and rerun.
- [x] M1-10: Atomic state and run locking.
- [x] M1-11: Minimum artifact protection.
  - [x] M1-11a: Private run artifacts.
  - [x] M1-11b: Bounded artifact storage.
    - [x] M1-11b1: Serialized artifact limits.
    - [x] M1-11b2: Pre-side-effect storage commitments.
      - [x] M1-11b2a: Provider commitments.
      - [x] M1-11b2b: Evaluation commitments.
  - [x] M1-11c: Bounded secret redaction.
- [x] M1-12: Interruption recovery acceptance.

## Milestone 2 - Consumable Loop

- [ ] M2-01: Generic project initialization (active).
- [ ] M2-02: Project doctor.
- [ ] M2-03: Package metadata and version identity.
- [ ] M2-04: Release artifact workflow.
- [ ] M2-05: Human-authorized tagged prerelease.
- [ ] M2-06: Packaged external golden path.
- [ ] M2-07: Executed Ollama acceptance.

## Milestone 3 - Piloted Preview

- [ ] M3-01: Typed durable loop contracts.
- [ ] M3-02: Artifact format versions and migration.
- [ ] M3-03: Retention and audited purge.
- [ ] M3-04: Two-repository pilot evidence.
- [ ] M3-05: Supported preview readiness.
- [ ] M3-06: Human-authorized preview publication.

## Current Gate

Milestone 1, including M1-12 interruption recovery acceptance, is complete.
M2-01 generic project initialization is active.
Public run/status output supplies
the exact staged-diff digest and target HEAD required by `seaf loop approve`.
Approval publishes versioned inline evidence only after candidate-to-provider
locked revalidation of physical candidate/source state, policy, approving role
artifact, and authenticated latest provider exchange. Exact retries preserve
bytes; stale, substituted, concurrent, non-Awaiting, cleanup, rerun, provider,
and direct-writer paths fail closed. M1-07a extracted
typed eval configuration into core and the controlled planner/executor into the
loop crate while preserving valid standalone behavior. Every check is planned
before execution; both allowlists are intersected; candidate-relative cwd and
executables stay inside the canonical root; output is redacted before its
persisted cap; and ambiguous log identities fail before filesystem or process
side effects. M1-07b now requires a normalized repository-root eval path before
workspace or provider work, reads it through a verified no-follow handle,
persists canonical typed JSON with a bound digest, and preflights exact snapshot
prefixes before any resume recovery. Historical runs without this authority
stay inert. M1-07c1 defines canonical Testing evidence, approval-bound final
states, combined final authority validation, and immutable passing/cleanup-only
failed outcomes. M1-07c2 makes exact Approved `loop resume` run immutable
ticket/eval checks locally in the candidate with no provider call. It publishes
create-only intent, indexed redacted logs, Testing evidence, and a bound
EvalReport before the exact Approved-to-final compare-and-swap. Prevalidation
executes zero commands, partial attempts refuse replay pending M1-09, ignored
candidate build output is permitted without weakening the approved diff, and
lasting source/candidate/artifact drift blocks final publication. Human approval
authorizes local execution under the developer account; it is not OS
containment. M1-08 adds a fresh `loop promote` confirmation bound to the exact
candidate diff, Testing/EvalReport, policy decision, EvalPassed predecessor, and
clean target HEAD. A create-only intent precedes mutation; candidate,
repository-operation, then provider locking supports exact crash adoption and
full-state publication. Raw index/worktree verification bypasses hooks, filters,
and replace refs; the applied patch remains unstaged, uncommitted, and exactly
reviewable while the frozen candidate is retained. Promoted authority is
immutable. M1-09a now binds structured role artifacts to their exact attempts
with create-only publication and preserves the historical attempt-1 path. Its
read-only `loop inspect` authenticates the full provider chain, reports
run/input/candidate/current artifact authority without raw model bodies, retains
current/head evidence under deterministic output caps, and classifies missing,
tampered, unsafe, and ambiguous history without executing repository filters or
changing any byte. Ambiguous fixed-name reuse blocks recovery before reset can
erase the evidence. M1-09b now publishes versioned, actor-bound provider
recovery with a create-only source snapshot, exact
candidate/input/provider/recovery bindings, and one candidate-to-provider CAS.
`revise` is provider-free; exact `rerun --recovery` owns the first request, then
ordinary resume may continue it. Every prior role, policy, and provider artifact
remains byte-identical, downstream current pointers are reset, and the old
public/CLI rerun writer is retired while its historical reader remains. M1-09c1
now publishes strict attempt-001 v2 evaluation authority, keeps fixed v1 final
evidence readable, and makes final validation and promotion consume the exact
Testing-bound intent and logs. Future or mixed attempts, malformed names,
unsafe file types, gaps, surplus logs, and cross-attempt references fail closed
before execution or mutation. The pre-preview 0.1.0 public `TestingEvidence`
shape gained additive v2 fields; downstream Rust struct literals must be
updated, while persisted v1 JSON remains readable. Carry this
source-compatibility note into M1-12 and preview release notes. M1-09c2a now
adds evaluation-v2 recovery/source/prefix contracts, a stack-safe mixed-v1/v2
reader, and source-snapshot Approved reconstruction for pass/fail, frozen
EvalPassed/Promoted, and approval-bound Failed cleanup. Pending provider
recovery grafts and prior evaluation-v2 recovery fail closed. M1-09c2b now
adopts only an exact complete fixed-v1 or indexed-v2 prefix through an audited,
zero-command transaction. It verifies all six authoritative input snapshots,
candidate/source/provider authority, deterministic report bytes, crash orphans,
and a dedicated recovery-advancing final CAS; exact retry is byte-inert and
competing callers converge on one audit winner. M1-09c3 now adds dedicated v3
invalidation authority for exact incomplete or active approval-bound Failed
evaluation history. It preserves every prior byte, reconstructs the exact
Approved predecessor, and resets only current final-evaluation pointers under
candidate-to-provider CAS. Exact rerun owns one recovery-bound indexed attempt;
partial attempts cannot replay and require another invalidation, while complete
prefixes use zero-command adoption. Iterative mixed v1/v2/v3 lineage, all-six
inputs, candidate/source/provider state, prior final references, crash cuts,
tamper, competition, repeated attempts, pre-spawn drift abort, final failure,
passing promotion, and frozen states are covered. M1-09 is complete. M1-10 now
serializes every cooperative run-state mutation through the permanent bounded
per-run lock. Create-only state, exact canonical retry, and authenticated
expected-to-intended CAS share durable same-directory publication; write,
sync, rename/link, parent-sync, contention, symlink, lock replacement, target
replacement, concurrency, and post-publication retry cuts retain or converge
to one complete valid state. Narrow provider/candidate/recovery/final authority
and lock-order constraints remain intact. Resume validates the human-review
barrier before resync, authenticates frozen terminal authority before returning
inert, and still subjects ordinary terminal provider history to recovery
validation. Full Rust, strict Clippy, formatting, package, SDK, and diff gates
pass. M1-11a now creates supported Unix run directories as `0700` and every
run-owned file as `0600`, rejects existing broader modes with explicit repair
guidance, and publishes through pinned directory handles with identity-checked
create/link/rename/unlink operations. Parent, path, symlink, lock, target, and
temporary-file substitution fail closed without touching the substituted tree;
valid existing scaffold content remains intact. Source/candidate Git modes and
standalone eval/release artifacts are unchanged. M1-11b1 now enforces semantic
per-file caps and a pinned 32 MiB physical aggregate across every cooperative
run publisher under the permanent lock. Unique-inode accounting includes locks
and orphan temps; unsafe entries and existing oversize files fail before
mutation or unbounded reads; exact immutable retry costs zero; atomic
replacement budgets its coexisting temp; concurrent publishers cannot
oversubscribe. The aggregate scanner streams pinned-directory entries and
accepts at most 4,096 non-dot entries across the tree and eight descendant
directory levels from the depth-zero root; hard-link names consume entry budget
while their bytes remain unique-inode-counted. Prospective checks reserve +1
entry for the first permanent lock or a child directory, +2 for new-file
temporary/final-name coexistence, +1 for replacement temporaries, and +0 for
exact existing retry before mutation. Runtime scaffolding uses the same guarded
projections, and entry-only lock/directory creation first rejects existing
aggregate-byte overage. The candidate-workspace lock is now a permanent guarded
scaffold artifact; authenticated missing-lock migration releases the run guard
before open-only candidate acquisition, preserving candidate-before-run order.
Git patch-planning indexes are
isolated in pinned external operation directories, and promotion crash tests now
synchronize through the repository-to-provider lock order. Provider-side
M1-11b2a and M1-11b2b are accepted after independent specification, quality,
and evidence review. M1-11b2 and M1-11b are complete. M1-11c bounded secret
redaction is accepted after independent specification/security and quality
re-reviews. The exact provider, evaluation, operator, recovery, context,
scaffold, log, and run-state envelopes are screened before side effects and at
authority-changing compare-and-swap boundaries. V3 intent omits configured
values; clean v1/v2 history remains readable and unsafe legacy history fails
closed. Fresh isolated provisioning requires authoritative input snapshots and
screens its exact state/scaffold before the run leaf, candidate, or lock exists.
The full workspace passes, including CLI 142/142, provider/candidate boundary
75/75, state 44/44, and provider exchange 22/22; strict Clippy, workspace check,
Rust/Prettier formatting, pinned-pnpm lint/typecheck/test/build with 8 SDK tests,
and diff hygiene pass. M1-11 is complete. M1-12 implements the integrated
fault-injection acceptance harness and both pre-preview Rust compatibility notes.
M1-12 now has one stable source-workspace gate at
`scripts/test-milestone-one-acceptance.sh`. It rejects Cargo's zero-test success
and runs 14 exact locked tests serially; the final-tree implementation run passed
in 2m14s. Separate selections prove the complete canonical input snapshot set,
full provider run, early-role chain, Development's exact approved-spec input,
zero-command complete-prefix adoption, and crash-cut convergence.
The new natural OutputReview fault cut interrupts after the response and response
record are durable but before the step artifact/final state, then resumes through
isolated authority with retained snapshots. Recovery makes zero provider calls,
keeps one request/response pair and one review artifact, preserves every prior
artifact and the exact candidate subject, reruns no patch operation, and reaches
`awaiting_human_review` with exact source/candidate Git and filesystem snapshots.
The interrupted state is asserted immediately before any resume entrypoint as
well as after recovery. Promotion crash injection waits for two consecutive
identical complete authorized source snapshots while final publication is
blocked, avoiding a torn or partially applied cut.
CLI failure/promotion coverage now snapshots regular bytes and symlink targets
outside `.git` plus HEAD, status, staged binary diff, and unstaged binary diff.
Candidate Applying/Applied cuts compare complete source and candidate snapshots
before and after every cut and resume. Testing invalidation/rerun preserves the
source plus every attempt-1/history byte, adding only the recovery pair and
recovery-bound attempt 2. Failed evaluation and pre-promotion cuts are exact;
promotion interruption and success require the source's canonical binary
worktree patch and complete entries to equal the approved candidate, and retry
is source-byte inert. Complete evaluation prefixes adopt with zero provider or
command calls; incomplete prefixes require a new recovery-bound indexed attempt
and are never replayed in place. This is not an external-call exactly-once
claim. Independent specification and quality re-reviews approve M1-12 with no
open findings. The controller's final gate passed all 14 exact acceptance
tests, workspace check, strict Clippy, Rust and Prettier formatting, every
locked serial Rust workspace test, all pinned-pnpm SDK gates, and diff hygiene.
M2-01 is active. Compatibility handoff is recorded in
`docs/preview-compatibility-handoff.md` for M2-03 and M3-05.
The documented complete promotion path requires a clean checkout/worktree. The
source-workspace gate currently supports macOS/Linux only: CI runs it on Ubuntu
and current local verification is macOS; Windows is not claimed.
