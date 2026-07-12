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

M1-09b - Audited provider revise and rerun. Add one versioned, create-only
`RecoveryAttemptV1` authorization plus its exact source-run snapshot. Bind the
sequential recovery ID, actor/reason/time, selected provider step, source and
next attempts, immutable input and candidate authority, prior provider/recovery
heads, and expected reset-state digest. Existing ticket, policy, config,
repository, eval config, provider/model, candidate, and artifact bytes cannot be
revised; changing any of them requires a new run.

`seaf loop revise` must publish the recovery evidence and pure reset under the
candidate-to-provider compare-and-swap boundary without calling a provider.
Only `seaf loop rerun --recovery N` may consume a pending recovery before its
first durable request. Ordinary resume rejects that cut, then may recover after
the exact request is durable. Preserve every historical file and provider
record, clear only the selected/downstream current pointers and their dependent
policy/approval/eval references, retire new `resume --rerun-from` use with
migration guidance, and keep evaluation/promotion recovery out of this slice.
