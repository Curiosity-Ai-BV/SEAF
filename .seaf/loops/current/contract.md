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

M1-11b2 - Pre-side-effect storage commitments. Derive logical capacity from
authority that already defines recovery; do not add a reservation file or hold
the permanent run lock across provider latency or approved commands.

An authenticated provider request-phase ledger tail reserves the missing 1 MiB
response audit, 64 KiB response record, and exact future run-state growth. An
active authenticated evaluation intent reserves missing configured stdout and
stderr maxima plus bounded Testing, EvalReport, and final run-state bytes.
Before creating a fresh request or evaluation intent, prove that its physical
publication and complete future commitment fit within the 32 MiB run budget.
Insufficient capacity performs zero provider calls or command spawns.

Every cooperative writer continues using the M1-10 lock and authorizes physical
bytes plus the one derivable active commitment. Verified canonical prefix files
consume their reserved slots. Request-only and staged-response recovery,
evaluation crash prefixes, invalidation, zero-command adoption, and exact retry
must reconstruct the same remainder without durable reservation metadata. A
provider result whose exact canonical audit exceeds 1 MiB becomes a small typed
non-retryable oversize failure with no raw bytes or raw-result digest. Preserve
request-only replay decisions. Do not add secret redaction, retention/purge,
format migration, packaging, or release behavior in this slice.
