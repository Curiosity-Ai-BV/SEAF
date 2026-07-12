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

M1-09c2b - Zero-command adoption transaction. Add
`loop revise --from-step testing --eval-recovery adopt --actor <actor> --reason
<reason>` and no other evaluation action. Provider steps must reject the option;
Testing without it must reject before mutation. Fresh adoption accepts only an
exact Approved/Testing source with an active candidate, consumed prior provider
recovery, and one complete verified fixed-v1 or indexed-v2
intent/log/Testing prefix. EvalReport may be absent or exact. Partial, mixed,
future, substituted, pending-recovery, terminal, promotion-intent, input,
candidate, source, provider, or namespace drift fails closed.

Under the candidate lock, preflight every source/recovery/report collision
before the first write. Publish the exact prefix-bearing source snapshot,
evaluation-v2 recovery, and only a missing deterministic EvalReport; execute
zero evaluation commands and make zero provider calls. Reauthenticate all
authority, then use one dedicated provider-lock Approved-to-final CAS that
advances `latest_recovery` without weakening ordinary final relations. Cover
every crash cut and concurrent winner. An exact post-CAS retry is byte-inert
only when action, actor, reason, and adopted final authority all match; arbitrary
fresh Failed/EvalPassed/Promoted adoption remains forbidden. Do not add
invalidation, rerun, attempt 2, general M1-10 locking, or artifact protection.
