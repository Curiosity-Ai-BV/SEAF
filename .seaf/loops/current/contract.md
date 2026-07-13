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

M1-11b2b - Evaluation-side pre-side-effect storage commitments. Provider-side
M1-11b2a is accepted. M1-11b2b remains active, so parent M1-11b2 is incomplete.

An authenticated active evaluation intent reserves every missing normalized
stdout/stderr maximum, bounded Testing evidence, bounded EvalReport, two bounded
recovery artifacts, the full future `run.json` replacement bytes at the atomic
coexistence peak, missing permanent names, and one transient name. The shared
output-limit normalizer accepts 1 through 1 MiB and defaults to 64 KiB.

Before publishing a fresh intent or spawning each approved command, prove the
current physical operation and complete future commitment fit within the 32 MiB
and 4,096-entry run budgets. Testing/report publication, direct finalization,
invalidation, and zero-command adoption consume only their authenticated slots
and reconstruct the remaining commitment after interruption. Release the run
guard before command latency. Do not add secret redaction, retention/purge,
format migration, packaging, or release behavior in this slice.
