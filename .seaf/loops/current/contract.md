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

M1-07c - Approved Testing and EvalReport transaction. `seaf loop resume` on an
exact Approved run must execute only the canonical ticket and eval snapshots in
the exact candidate, without a model call. Preflight every check and both
allowlists before intent or execution. Under candidate authority, reauthenticate
the human approval, current OutputReview/provider chain, exact policy decision,
source HEAD/tree, candidate staged diff, and physical workspace immediately
before commands and again before final publication. Publish a create-only
execution intent before the first command so an interrupted attempt never
silently replays side effects. Publish indexed create-only redacted logs,
canonical Testing evidence, and a backward-compatible EvalReport binding the
run, ticket/config digests, candidate diff, approval, policy decision, and
Testing artifact. After those artifacts are durable, use candidate-to-provider
lock order and full-state compare-and-swap to publish the Testing/EvalReport
step results, `eval_report_path`, and terminal `eval_passed` or reported `failed`
together. Direct provider Testing/EvalReport,
unapproved or historical runs, substitutions, failed checks, timeout, source or
candidate drift, and partial prior attempts must never claim eval success.
Preserve the source checkout; do not promote, commit, merge, deploy, or contact
a model in Testing/EvalReport.
