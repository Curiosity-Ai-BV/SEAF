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

M1-07c2 - Locked Approved evaluation transaction. Make `loop resume` recognize
only an exact current Approved authority and execute the canonical immutable
eval snapshot in that physical candidate without a model call. Acquire the
candidate lock first, authenticate the human approval, output-review/provider
chain, policy decision, source HEAD/tree, and physical candidate immediately
before commands, and plan every check plus both allowlists before executing any
command. Publish a create-only execution intent bound to the exact Approved run
digest before the first command; an existing incomplete intent must refuse
silent replay pending M1-09 recovery.

Execute only through the bounded controlled engine in the candidate. Publish
indexed create-only redacted stdout/stderr logs with digest pairs, canonical
Testing evidence, and the approval-bound EvalReport. Revalidate candidate,
source, approval, and artifacts after commands; physically verify the immutable
eval config, candidate diff, and log bytes. While retaining the candidate lock,
take the provider lock only for the final full-state compare-and-swap from the
exact Approved predecessor to `eval_passed` or the approval-bound reported
failure. Failed checks and timeouts publish rejecting evidence; prevalidation
failure executes zero commands; publication failure cannot claim a terminal
result. Preserve the M1-07c1 freeze rules, do not promote, and do not add an
audited adoption/invalidation path for incomplete attempts in this slice.
