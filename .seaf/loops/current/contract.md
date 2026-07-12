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

M1-10 - Atomic state and run locking. Generalize the narrow provider-ledger
lock and atomic replacement guarantees to every remaining run-state mutation
and recovery operation without weakening or replacing the existing
ledger-specific guard.

Every run-state writer must use durable same-directory temporary publication
and atomic replacement. Exactly one cooperative SEAF process may mutate a run
at a time; stale-lock behavior must be explicit, bounded, and safe. Concurrent
writers must either serialize against the latest authenticated state or fail
closed before mutation, never publish a hybrid of two intended states. Failed
write, sync, or rename cuts must retain the last valid parseable `run.json` and
leave a deterministic retry or cleanup path.

Start with an inventory of every state/workspace writer and the existing lock
orders. Add focused fault-injection and competing-writer tests before changing
production code. Preserve candidate, repository-operation, and provider lock
ordering and all M1-09 recovery CAS semantics. Do not add M1-11 artifact
permissions, retention, distribution, or release work.
