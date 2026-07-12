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

M1-11b - Bounded artifact storage. Enforce per-artifact and aggregate run-tree
limits without changing immutable identities, M1-10 persistence semantics, or
the candidate/repository/run lock order.

Provider prompts/requests are capped at 2 MiB, canonical provider response
audits at 1 MiB, exchange records at 64 KiB, evaluation logs at 1 MiB, and all
other generated evidence/input artifacts at 2 MiB. The aggregate durable run
tree is capped at 32 MiB. Exact cap is valid and cap plus one is rejected.
Exact immutable retries cost zero; replacements account for both the observed
old size and intended new size without allowing concurrent oversubscription.

Reuse the permanent M1-10 per-run lock for aggregate accounting and reservation;
do not add a competing lock. Before an external provider call or evaluation
command, prove or reserve enough durable capacity for the authoritative audit
that must follow. A refused or failed reservation executes no external side
effect and publishes no partial or misleading authority. Preserve standalone
policy/evaluation behavior where no run root exists. Do not add secret
redaction, retention/purge, format migration, packaging, or release behavior in
this slice.
