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
M2-04 are accepted. Status: awaiting-fresh-exact-sha-authorization on
2026-07-14.

M2-04 now provides the reviewed deterministic archive/checksum scripts and the
read-only tag-push workflow for exactly `x86_64-unknown-linux-gnu` on Ubuntu
22.04 and `aarch64-apple-darwin` on macOS 15. The user's prior authorization
covered tag protection, immutable releases with automatic attestation, and the
tag/prerelease operation only at exact commit
`29c2cba739bdbc75bf871220b498bf66d6d82c4d`. During preflight, `origin/main`
had independently advanced to that commit; no branch push was performed.

Exact-old-SHA CI run
[`29312976772`](https://github.com/Curiosity-Ai-BV/SEAF/actions/runs/29312976772)
failed in Rust at `Release artifact readiness`: GNU tar rejected all-NUL
ordinary USTAR device fields. TypeScript passed; later Rust steps were skipped.
No `v0.1.0` tag, release, release asset, or release-workflow run was created,
and the failed ordinary-CI run was not rerun. TDD corrections `7b895b5` and
`f4d7c28` received independent specification and quality approval. The final
focused and full macOS gates and the full exact `linux/amd64` Rust 1.97/GNU tar
1.34 release-artifact suite pass at clean commit
`f4d7c28d27c345a8b0d7f6cc48c8c833b48f248a`.

The active tag ruleset is
[`Protect v0.1.0`](https://github.com/Curiosity-Ai-BV/SEAF/rules/18918424)
(ID `18918424`): target tag, include exactly `refs/tags/v0.1.0`, update and
deletion rules, no bypass actors, and `current_user_can_bypass` `never`. It has
no creation rule, so the initial tag creation remains allowed. Immutable
releases are enabled with state `{"enabled":true,"enforced_by_owner":false}`;
publication will automatically attest without adding workflow authority.

The old exact-SHA authorization does not transfer to the final commit. M2-05
may continue only after fresh explicit authorization names
`f4d7c28d27c345a8b0d7f6cc48c8c833b48f248a`. That operation may create and
push only lightweight tag `v0.1.0` directly at that commit, with no branch push;
wait for the initial tag workflow; require both native jobs and checksum
assembly to succeed; download the workflow outputs into a fresh external root;
verify the exact two-archive inventory and `SHA256SUMS`; install the native
macOS archive externally; and prove exact `seaf 0.1.0`, `info`, and fake-provider
doctor output. Native Linux execution remains the Ubuntu workflow row's
evidence. Only then may the prerelease be published from those already verified
two archives and `SHA256SUMS`, without rebuilding or substituting assets. Record
the tag, immutable commit SHA, workflow URL, release URL, asset checksums, and
smoke results before accepting M2-05.

Do not move or replace an existing tag, overwrite a release or asset, publish a
registry package, grant workflow write/OIDC/attestation authority, sign,
notarize, run the external golden path, or execute Ollama acceptance. Any tag,
workflow, checksum, asset, install, or doctor mismatch stops the slice without
claiming acceptance. M2-06 and M2-07 remain dependency-blocked until M2-05 is
accepted.
