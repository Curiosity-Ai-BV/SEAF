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

M1-08 - Promotion integrity. Add an explicit human-authorized promotion
transaction for a frozen `eval_passed` run. Require a fresh bounded reviewer
identity and exact confirmations for the approved candidate diff, bound
EvalReport digest, and current target HEAD. Under candidate authority, reload
and physically verify the immutable human approval, policy decision, Testing
evidence, EvalReport, command logs, candidate diff, source repository identity,
and clean target worktree before any source mutation.

Use candidate-to-repository-operation lock order and a full-state compare-and-
swap. Apply only the exact already-approved staged patch to the original
checkout without committing, merging, pushing, deploying, or deleting the
candidate. Reverify target HEAD and cleanliness immediately before application,
fail closed on stale/substituted authority or patch conflict, and publish a
closed `promoted` record bound to the fresh confirmation. Exact retry must be
byte-identical; failed, incomplete, cleaned, or non-passing runs must never
mutate the target. Preserve the M1-07 terminal evidence and add focused
temporary-repository regressions before implementation.
