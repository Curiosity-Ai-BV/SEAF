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

M2-05 - Human-authorized tagged prerelease. Milestone 1 and M2-01 through
M2-04 are accepted. Status: awaiting-explicit-user-authorization on 2026-07-14.

M2-04 now provides the reviewed deterministic archive/checksum scripts and the
read-only tag-push workflow for exactly `x86_64-unknown-linux-gnu` on Ubuntu
22.04 and `aarch64-apple-darwin` on macOS 15. It did not create or push a tag,
create a GitHub Release, or publish any durable artifact.

M2-05 may begin only after the user explicitly authorizes external repository
state changes. The authorized operation must identify the exact accepted clean
commit, create and push only tag `v0.1.0`, wait for the tag workflow, and require
both native jobs plus checksum assembly to succeed. Download the workflow
outputs into a fresh external root, verify the exact two-archive inventory and
`SHA256SUMS`, install the native macOS archive externally, and prove exact
`seaf 0.1.0`, `info`, and fake-provider doctor output. Native Linux execution
remains the Ubuntu workflow row's evidence.

If separately authorized, create a GitHub prerelease for the existing tag with
only the two verified archives and `SHA256SUMS`; do not rebuild or substitute
assets during publication. Record the tag, immutable commit SHA, workflow URL,
release URL, asset checksums, and smoke results in repository evidence and the
roadmap before accepting M2-05.

Do not move or replace an existing tag, overwrite a release or asset, publish a
registry package, grant workflow write/OIDC/attestation authority, sign,
notarize, run the external golden path, or execute Ollama acceptance. Any tag,
workflow, checksum, asset, install, or doctor mismatch stops the slice without
claiming acceptance. M2-06 and M2-07 remain dependency-blocked until M2-05 is
accepted.
