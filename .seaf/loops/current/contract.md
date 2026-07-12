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

M1-07b - Immutable eval configuration authority. For every new provider run,
resolve `ticket.eval.config` before run-directory, candidate, or provider work
as a real regular file contained by the authoritative repository root. Reject
absolute, traversal, backslash-ambiguous, missing, malformed, and symlink-escape
authority without side effects. Parse the shared typed config once,
canonicalize it to JSON, publish it create-only as `inputs/eval-config.json`,
and bind its digest in the run input contract. Keep the new digest optional only
for historical deserialization; new provider runs require it. Incomplete resume
must compare live authority with the bound digest. Historical Approved runs
without this authority remain byte-identical, execute no command, and instruct
the user to start a new run; never backfill from mutable live or candidate YAML.
Do not execute checks, add eval terminal states, publish Testing/EvalReport
evidence, promote, or contact a model beyond the existing pre-eval provider
workflow in this slice.

Next, M1-07c executes one approval-bound Testing/EvalReport transaction from the
canonical ticket and eval snapshots inside the exact candidate.
