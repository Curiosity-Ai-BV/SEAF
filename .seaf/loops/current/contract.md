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

M1-09 - Audited recovery operations. Add explicit inspect, revise, and rerun
commands for blocked, failed, and interrupted attempts without editing durable
history in place. Every revision must create a new versioned immutable attempt,
preserve prior prompts/responses/artifacts and original input snapshots, and
bind its reason, actor, source attempt, candidate authority, and effective-input
digests. A change to authoritative ticket, policy, project config, repository
identity, or eval config still requires a new run.

Rerun from a named eligible step under the run/candidate authority locks. Clear
and invalidate every downstream role result, provider exchange head, human
approval, Testing/EvalReport, promotion intent/evidence, and terminal status
that depended on the replaced attempt while preserving their bytes as history.
Add an audited decision for incomplete Approved-evaluation intent: adopt only a
fully verifiable already-completed artifact prefix, or explicitly invalidate it
before a new execution attempt; never silently replay commands. Invalid targets,
stale authority, exhausted attempts, and unsafe reset boundaries must fail
before mutation. Keep recovery local and supervised; do not add automatic
commit, merge, push, deploy, or history deletion.
