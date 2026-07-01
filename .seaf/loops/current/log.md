# Loop Log

## 2026-06-30 gather | initial context

Read the blueprint, confirmed the repo was an initial README-only project, and extracted loop concepts from the supplied Karpathy image: separated roles, negotiated contracts, disk-backed progress, trace logs, restartable loops, scored subjective criteria, and periodic harness deletion.

## 2026-06-30 verify | slice 1 spec review

Spec review found two gaps: lint/format checks were not encoded in CI, and the loop role named commits but not merges. Added Rust fmt/clippy, Prettier format checks, package lint scripts, and explicit commit/merge agent wording.

## 2026-06-30 verify | slice 1 quality review

Quality review found that `@seaf/sdk` advertised `dist/index.js` without a build path. Added package build scripts, CI build execution, a build tsconfig that emits `dist/index.js`, and a package file allowlist.

## 2026-06-30 act | slice 2 contracts

Started Rust-owned models and CLI validation. Scope is limited to deterministic file parsing, actionable validation errors, safe template init, a fail-closed eval placeholder, and release capsule structure checks.

## 2026-06-30 verify | slice 2 quality review

Quality review found four fail-open/package issues: crate templates referenced workspace examples, unknown fields were accepted despite schema closure, eval placeholder exited 0, and NaN effect sizes passed validation. Moved templates into `seaf-core`, denied unknown serde fields, made placeholder eval return nonzero, and required finite positive effect sizes.

## 2026-06-30 act | slice 3 SDK and runtime

Started event/signal contracts, TypeScript SDK event emission, and SQLite-backed local runtime ingestion. Scope is local-only: no daemon lifecycle, no cloud upload, and signal summaries use aggregated counts only.

## 2026-06-30 verify | slice 3 quality review

Quality review found feedback privacy could be downgraded while carrying raw message text, and SDK runtime validation did not reject invalid privacy enum values. Enforced private-or-sensitive feedback privacy and added runtime privacy enum validation.

## 2026-06-30 act | slice 4 artifact chain

Replaced the fail-closed eval placeholder with a local eval runner, task brief generator, release capsule preparation command, and digest-aware release verification. This keeps the MVP agent loop manual but produces durable JSON/Markdown artifacts.

## 2026-06-30 verify | slice 4 spec review

Spec review found initialized eval templates could not be parsed because `thresholds` was not accepted, and release preparation accepted contradictory rejected/high-risk EvalReports. Accepted optional thresholds metadata and tightened EvalReport/release validation.

## 2026-07-01 gather | phase 2 spec authoring

Read the Phase 2 local-agent-loop plan, existing architecture/agent-loop docs, and current loop tracking files. Confirmed the active branch is `codex/seaf-foundation-agent-loop` and the next work should remain documentation-only.

## 2026-07-01 act | phase 2 ticket specs

Created `docs/phase-2-local-agent-loop.md` with overview, scope boundary, review protocol, current pending status, P2-001 selection, and ticket specs for P2-001 through P2-012.

## 2026-07-01 verify | phase 2 spec authoring

Ran `pnpm format:check` and `git diff --check`; both passed after formatting the new Phase 2 spec with Prettier.

## 2026-07-01 act | P2-001 contracts

Added `TicketSpec` and `LoopRun` contracts, JSON schemas, and local-loop fixtures. The implementation stayed in `seaf-core`, `specs/`, and `examples/local-loop/`.

## 2026-07-01 verify | P2-001 contracts

Spec review initially required durable invalid/valid fixtures and alignment with the plan's section 7.1 ticket example. Code-quality review then required tightening `policy_decisions` from arbitrary JSON to non-empty object entries. After fixes, spec and quality re-reviews approved. Final checks passed: `cargo test --workspace`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `pnpm format:check`, and `git diff --check`. Committed as `65fc489`.

## 2026-07-01 act | P2-002 model provider

Added the `seaf-models` crate with provider-neutral request/response/error DTOs, a synchronous `ModelProvider` trait, and a deterministic fake provider. The fake provider records requests and replays scripted responses without network access.

## 2026-07-01 verify | P2-002 model provider

Spec review accepted the mechanical `Cargo.lock` update for the new local workspace crate. Code-quality review required finite-temperature serde guards, atomic fake-provider state, and fail-closed DTO tests. After fixes, spec and quality re-reviews approved. Final checks passed: `cargo test --workspace`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `pnpm format:check`, and `git diff --check`. Committed as `946aa4d`.
