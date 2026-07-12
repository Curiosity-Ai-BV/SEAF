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
- [ ] M1-08: Promotion integrity (active).
- [ ] M1-09: Audited recovery operations.
- [ ] M1-10: Atomic state and run locking.
- [ ] M1-11: Minimum artifact protection.
- [ ] M1-12: Interruption recovery acceptance.

## Milestone 2 - Consumable Loop

- [ ] M2-01: Generic project initialization.
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

M1-06 and M1-07 are complete; M1-08 is active. Public run/status output supplies
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
containment. Passing authority remains frozen and cannot promote until M1-08.
