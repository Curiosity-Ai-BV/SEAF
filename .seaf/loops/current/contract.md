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

M1-09c - Approved-evaluation recovery. Add attempt-indexed create-only
evaluation intent, log, Testing, and EvalReport paths. ApprovedEvaluationIntent
v2 must bind its evaluation attempt and recovery reference; TestingEvidence v2
must bind the exact intent and invalidation authority. Historical v1 fixed paths
remain readable and final validation selects only the bound attempt.

`seaf loop revise --from-step testing --eval-recovery adopt|invalidate` must
publish audited authority under the candidate-to-provider recovery CAS.
Adoption accepts only a complete verified intent/check/log/Testing prefix,
executes zero commands, and may deterministically create only a missing
EvalReport. Invalidation preserves every byte and exact candidate, approval,
policy, input, and provider authority; clears only current Testing/EvalReport
and final-eval references; and gates one fresh attempt behind `loop rerun
--recovery N`. Active Approved incomplete prefixes and active approval-bound
final Failed are eligible. EvalPassed, Promoted, historical missing-eval
authority, partial adoptable evidence, gaps, substitution, or physical drift
fail closed. Do not weaken M1-08 promotion-intent or final-state relations.
