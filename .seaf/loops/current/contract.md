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

M1-11c - Bounded secret redaction. M1-11b, including provider and evaluation
pre-side-effect commitments, is accepted. M1-11c is the only remaining M1-11
slice.

Configured evaluation environment values may remain only in the exact private
`inputs/eval-config.json`. Provider prompts, requests, responses, evaluation
logs, Testing evidence, EvalReport, and other derived artifacts must redact both
configured secret values and obvious credential forms before persistence.

Accept at most 64 configured secret values, each at most 4 KiB and at most
64 KiB in aggregate. Oversized redaction input or output fails closed. Preserve
clean provider results and M1-11b2 typed oversize failures exactly. A
secret-bearing provider response becomes a small safe non-retryable audited
failure without raw bytes or a raw digest, while request-only recovery semantics
remain unchanged.

Mandatory RED/GREEN covers configured values, obvious patterns, overlaps,
input/output caps, provider responses, request-only crash recovery, and
no-raw-leak assertions. Do not add retention/purge, format migration, packaging,
release, or M1-12 interruption-acceptance behavior in this slice.
