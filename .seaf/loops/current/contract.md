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

M1-11 - Minimum artifact protection. Make local run evidence safe enough for
live provider use without changing the M1-10 state transaction or broadening
into retention, distribution, or release work.

On supported Unix platforms, every run directory, mutation lock, state file,
prompt, provider response, log, and generated evidence file must be private at
creation and remain private through retry or replacement. Provider response,
prompt, exchange, log, and aggregate run storage must have explicit byte caps;
oversize input must fail closed before partial or misleading authority is
published. Configured secrets and obvious credential patterns must be redacted
before persistence, and redaction itself must stay bounded.

Start with an inventory of every run-directory and artifact creation seam,
existing output/redaction limits, and the order in which immutable provider
audits become authoritative. Witness permission, oversize response, cumulative
budget, retry, and secret-leak failures before implementation. Preserve
M1-10's one-lock publication semantics, all immutable artifact identities, and
the candidate/repository/run lock order. Do not add purge/retention policy,
format migration, packaging, or release behavior.
