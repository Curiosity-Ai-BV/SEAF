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

M2-04 - Release artifact workflow. Milestone 1 and M2-01 through M2-03 are
accepted. Status: active on 2026-07-13.

Build checksummed native CLI artifacts without creating a tag or publishing a
GitHub Release. The binding prebuilt matrix is intentionally limited to
`ubuntu-22.04` / `x86_64-unknown-linux-gnu` and `macos-15` /
`aarch64-apple-darwin`; broader Linux, Intel macOS, Windows, musl, and cross-
compiled targets remain unsupported until they have their own native evidence.
The Cargo workspace version is authoritative, so version `0.1.0` requires the
exact future tag `v0.1.0` and exact binary output `seaf 0.1.0`.

Each native build produces exactly
`seaf-v0.1.0-<target>.tar.gz`. The archive has one matching root directory and
exactly four regular entries in stable order: `CHANGELOG.md`, `LICENSE`,
`README.md`, and executable `seaf`. Normalize modes, owners, timestamps, USTAR
metadata, and the gzip header so identical input bytes produce identical
archives. Bound every input and output, reject unsafe archive entries before
extraction, keep build/output/install roots outside the repository, and prove
the repository status is unchanged.

The aggregate release-assets directory contains exactly the two native archives
and a newline-terminated `SHA256SUMS`. Its two lowercase SHA-256 lines use two
spaces, basenames only, and lexical filename order. Assembly and verification
must reject missing, extra, duplicate, renamed, path-bearing, malformed, or
tampered inputs. A local native smoke extracts only after validation, installs
the archive binary into a fresh external `bin`, and proves exact `--version` and
`info` without resolving the source or Cargo target binary.

Add a tag-push-only GitHub Actions workflow that hard-codes the two native
runners, checks exact tag/version/ref/host authority, builds with locked Cargo,
packages and smokes each artifact, assembles the exact checksum bundle, and
uploads only short-lived immutable workflow artifacts. Use top-level
`contents: read`, no secrets or deployment environment, checkout without
persisted credentials, full-SHA action pins, and context values passed through
environment variables. Do not grant write/OIDC/attestation permissions or add
`workflow_dispatch`, `pull_request_target`, release API calls, tag creation, or
publication. Ordinary CI runs the same native artifact contract locally but
never takes tag or publication authority.

Mandatory RED first proves the artifact scripts, workflow, documentation, and
CI seam are absent. Focused GREEN proves deterministic packaging, exact
inventory/modes/metadata, target/version refusal, aggregate checksum shape and
tamper rejection, installed artifact identity, workflow formatting, and status
preservation. Full repository gates remain required.

Keep this slice limited to deterministic artifact construction, checksum
assembly, tag-gated read-only workflow artifacts, native smoke coverage,
release-artifact documentation, and matching trackers. Do not create or push a
tag, create a GitHub Release, publish a registry package, sign/notarize, modify
release-capsule domain commands, run the external golden path, or execute Ollama
acceptance. Those authorities remain M2-05 through M2-07.
