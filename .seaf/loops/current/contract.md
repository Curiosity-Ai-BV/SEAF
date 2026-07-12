# Current Contract

## Goal

Implement U1 through U11 from `docs/production-readiness-roadmap.md` as the
supervised local coding-loop product, using the dependency-ordered slices in
`docs/production-use-implementation-plan.md`.

## Success Criteria

- Each slice follows witnessed TDD and has separate spec and quality approval.
- Each accepted slice is one reviewable commit with roadmap/tracker updates.
- Model-modified code cannot execute before human approval.
- Candidate work cannot mutate the source checkout before verified promotion.
- The exact reviewed candidate, policy decision, EvalReport, target HEAD, and
  human confirmation are bound before promotion.
- The packaged external golden path and two approved real-project pilots pass.

## Scope Boundaries

In scope: authoritative configuration, role dataflow, bounded context requests,
candidate worktrees, approval/promotion, integrated evals, recovery/artifact
safety, generic bootstrap, CLI distribution, external acceptance, durable loop
contracts, pilots, and preview readiness.

Deferred: dashboard, cloud providers, autonomous PR/commit/merge/deploy,
production updater signing, supported telemetry SDK/runtime, and adversarial
same-user command containment. Human approval authorizes local execution under
the developer account; SEAF validates configuration and detects repository
drift but is not an OS sandbox for approved code.

## Review And Commit Gate

The controller dispatches one fresh implementer at a time. After self-review,
a fresh spec reviewer checks only the slice acceptance criteria. After approval,
a fresh quality reviewer checks correctness, maintainability, tests, and
security. Findings return to the implementer and are re-reviewed. The controller
runs final checks and commits only when both reviews approve.

After an accepted commit, the controller immediately advances to the next
dependency-ready slice. After interruption, it resumes from this contract,
progress, the roadmap, and the append-only log. It stops only for a recorded
failed gate, a genuine authority decision, or an external blocker.

## Current Slice

M1-09c3 - Evaluation invalidation and fresh rerun. Extend
`loop revise --from-step testing --eval-recovery invalidate --actor <actor>
--reason <reason>` without changing provider recovery or adoption-v2 behavior.
Eligible authority is either exact Approved/Testing with one factual latest
incomplete evaluation prefix, or active approval-bound final Failed whose exact
Approved predecessor and attempt evidence verify. EvalPassed, Promoted,
promotion intent, legacy missing-evaluation authority, pending recovery,
inactive candidate, malformed/mixed/gapped history, or any input, candidate,
source, provider, artifact, and namespace drift fails closed before mutation.

Invalidation executes zero commands and makes zero provider calls. Under the
candidate-then-provider lock order, preflight and publish a dedicated
create-only source snapshot and invalidation decision bound to every present
prefix byte, exact prior/final authority, next contiguous evaluation attempt,
actor/reason/time, and zero-digest reset projection. Preserve all prior
intent/log/Testing/EvalReport bytes and provider history; reset only current
Testing/EvalReport/final references to the reconstructed Approved authority and
advance `latest_recovery` by one CAS. Exact revise retry is byte-inert.

Only `loop rerun --recovery <id>` may consume the pending invalidation and start
its one authorized fresh indexed attempt. The new intent and Testing evidence
must bind that recovery reference; ordinary resume/evaluation rejects before
consumption. Once any artifact for that attempt exists, the same recovery may
not replay commands within the attempt: another audited invalidation is needed
to authorize the next contiguous attempt. Cover incomplete-prefix shapes,
final-Failed reset, every publication cut, competing callers, repeated
invalidate/rerun cycles, and frozen adoption/promotion regressions. Do not add
general M1-10 locking, artifact protection, or distribution work.
