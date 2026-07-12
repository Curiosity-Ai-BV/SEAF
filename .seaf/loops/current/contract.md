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

M1-09c2 - Zero-command evaluation adoption. Add a schema-v2 evaluation recovery
variant to the existing sequential recovery chain without changing provider
RecoveryAttemptV1. `loop revise --from-step testing --eval-recovery adopt` must
accept only one exact complete v1 or v2 intent/check-log/Testing prefix for the
active Approved authority. EvalReport may be absent. Gaps, substitutions,
partial Testing, mixed attempts, physical drift, final Failed, EvalPassed,
Promoted, or an active recovery fail closed.

Adoption publishes a create-only source-run snapshot and recovery artifact,
executes zero commands and makes zero provider calls, reconstructs the exact
execution-time Approved predecessor, and deterministically creates only a
missing EvalReport before one candidate-to-provider final CAS advances recovery
authority. Existing complete report bytes must verify exactly. Preserve every
prior byte and the M1-08 promotion/final-state relations. Do not add evaluation
invalidation, command rerun, attempt 2 execution, general M1-10 locking, or
artifact protection in this checkpoint.
