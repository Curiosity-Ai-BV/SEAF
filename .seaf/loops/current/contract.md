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

M2-07 - Executed Ollama acceptance. Milestone 1 and M2-01 through M2-06 are
accepted. Status: dependency-ready; implementation has not started.

M2-05's exact handoff is the immutable public
[`v0.1.0` prerelease](https://github.com/Curiosity-Ai-BV/SEAF/releases/tag/v0.1.0)
at `f4d7c28d27c345a8b0d7f6cc48c8c833b48f248a`. Only the lightweight tag was
pushed; no branch push occurred. The single initial
[tag workflow run](https://github.com/Curiosity-Ai-BV/SEAF/actions/runs/29318734239)
passed on attempt 1, its exact three artifacts passed inventory/checksum and
byte-identity checks, the packaged macOS arm64 CLI passed external
version/info/init/commit/fake-doctor smoke, and all three automatic immutable-
release attestations verified. Linux execution evidence is the successful
Ubuntu workflow job. The read-only workflow did not receive write, OIDC, or
attestation authority.

M2-06 and U8 are accepted. The ordinary-CI packaged gate fully verifies a
current native archive before installing it outside the source tree, then uses
only that binary in two fresh external repositories. It covers generic init,
candidate creation, wrong and exact human approval, real interruption,
audited attempt-2 evaluation recovery without provider replay, stable inspect,
verified promotion, exact exit-24 rejection evidence, candidate cleanup,
preservation of explicit nonempty untracked file and symlink sentinels, bounded
recursive artifact/digest validation, and source preservation. The installed CLI
command chain is reflected in README and both loop docs. The three pre-review
post-install adoption runs completed in 9 seconds, 8 seconds, and 8 seconds.

Do not publish another release or registry package, move or replace `v0.1.0`,
weaken immutable-release or tag protection, or claim Milestone 2 completion.
M2-07 requires separately executed local Ollama evidence and remains not
started; no Ollama command is authorized by the M2-06 acceptance.
