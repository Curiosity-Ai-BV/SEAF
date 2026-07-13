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

M2-02 - Project doctor. M2-01 generic project initialization and Milestone 1
are accepted.

Add a read-only `seaf doctor` command that diagnoses whether an initialized
project is ready for the supervised loop. It must check Git/repository state,
authoritative project configuration and policy, candidate-workspace
prerequisites, configured eval executables, and the explicitly selected model
provider. Human and JSON output must describe the same typed checks with
actionable remediation and a deterministic overall result.

Mandatory RED/GREEN covers a ready generic project, each independent failure
class, stable JSON/human reporting, and a complete before/after filesystem and
Git snapshot proving the command creates no project, candidate, run, cache, or
provider evidence. Live-provider checks must remain explicit; deterministic
fake-provider diagnosis must require no network or model process.

Keep this slice limited to doctor report contracts, CLI behavior, tests, guide,
and matching trackers. Do not add package/version metadata, release artifacts,
installed-binary smoke, or external golden-path execution; those are M2-03 and
later slices.
