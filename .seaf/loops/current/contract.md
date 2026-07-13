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

M2-01 - Generic project initialization. Milestone 1 and M1-12 interruption
recovery acceptance are complete.

Make the default `seaf init` output stack-neutral and immediately editable in a
clean external repository. It must plan the complete output set before writing,
then refuse any existing-file conflict atomically without changing any target.
The default output set includes policy, eval, starter ticket, provider/project
configuration, and ignore entries. No default path, command, label, or content
may assume the SEAF workspace or the Adaptive Notes example.

Named examples remain explicit opt-in modes and preserve their documented
specialized behavior. Mandatory RED/GREEN uses representative empty Rust and
Node fixture repositories plus an existing-file conflict fixture. Tests must
parse and validate every generated public file, prove the generic commands are
appropriate to each fixture, prove the default contains no SEAF/Adaptive Notes
assumptions, and prove conflict refusal leaves a byte-exact repository snapshot.

Keep this slice limited to initialization templates, the init CLI, fixtures,
tests, the bootstrap quickstart, and matching trackers. Do not add `doctor`,
package/version metadata, release automation, or packaged-binary acceptance;
those are M2-02 and later slices.
