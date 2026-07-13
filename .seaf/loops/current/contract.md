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

M2-03 - Package metadata and version identity. Milestone 1, M2-01 generic
project initialization, and M2-02 project doctor are accepted.

Make the CLI identifiable and Cargo-packagable. Add `seaf --version`, complete
the publishable Cargo metadata, make every packaged internal dependency
versioned, add the repository license and changelog, and document the supported
platform policy. The package and release notes must consume the Rust source-
compatibility entries already recorded in
`docs/preview-compatibility-handoff.md`.

Mandatory RED/GREEN evidence starts from the current missing version/package
contract. Cargo package dry-runs must succeed for every publishable crate, and
an installed-package smoke must invoke the packaged `seaf` binary and prove its
version identity outside the source workspace. Verification includes the
focused packaging checks plus the full repository gate matrix.

Keep this slice limited to package metadata, version identity, license,
changelog, supported-platform documentation, packaging tests/scripts, and
matching trackers. Do not add release artifact automation, checksums, tag or
publication authority, external golden-path execution, or Ollama acceptance;
those are M2-04 through M2-07.
