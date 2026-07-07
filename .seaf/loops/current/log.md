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

## 2026-07-01 act | P2-004 context packer

Added the `seaf-loop` crate with local context packing, default safety excludes, ticket/policy forbidden-path filtering, UTF-8-safe byte limits, SHA-256 digests, warnings, and `context-manifest.json` writing. The manifest records metadata and excludes file content; the bundle carries the prompt content and untrusted-context marker.

## 2026-07-01 verify | P2-004 context packer

Spec review approved the crate scope and acceptance criteria. Code-quality review approved path normalization, symlink escape blocking, manifest/content separation, and byte-limit behavior, with a non-blocking follow-up for direct path-safety regression tests. Final checks passed: `cargo test --workspace`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `pnpm format:check`, and `git diff --check`. Committed as `5f36eba`.

## 2026-07-01 act | P2-005 state machine

Added durable loop workspace/state infrastructure in `seaf-loop`: run creation/resume, `run.json` persistence, prompt/response/artifact/log writing, attempt-indexed prompt/response artifacts, rerun-from reset, and a small step-runner test seam.

## 2026-07-01 verify | P2-005 state machine

Spec review required request persistence before step execution, attempt-indexed prompt/response artifacts, and a parseable empty context manifest. Code-quality review required duplicate-run protection, terminal `passed` semantics, persisted-running resume tests, blocked/failed output tests, and safe artifact extension handling. After fixes, spec and quality re-reviews approved. Final checks passed: `cargo test --workspace`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `pnpm format:check`, and `git diff --check`. Committed as `af7a2fa`.

## 2026-07-01 act | P2-006 role responses

Added local agent role prompts, response DTOs, handcrafted response schemas,
fail-closed parsing, one-shot invalid-JSON repair, developer patch-field
enforcement, reviewer issue arrays, and valid/invalid model-response fixtures.

## 2026-07-01 verify | P2-006 role responses

Spec review approved the P2-006 scope and acceptance criteria. Code-quality
review required status-aware developer patch validation and explicit role
mismatch regression coverage. After fixes, spec and quality re-reviews approved.
Final checks passed: `cargo test --workspace`,
`cargo fmt --all -- --check`,
`cargo clippy --all-targets --all-features -- -D warnings`,
`pnpm format:check`, and `git diff --check`. Committed as `bbc5665`.

## 2026-07-01 act | P2-007 patch policy gate

Added unified diff parsing, safe path normalization, binary-patch detection,
policy/category review gating, explicit apply gating, a testable `git apply`
runner seam, patch artifacts, and structured `PolicyDecision` artifacts.

## 2026-07-01 verify | P2-007 patch policy gate

Spec review approved the initial implementation. Code-quality review required
fail-closed malformed `diff --git` headers, clearer category-key versus
path-pattern review policy semantics, and a separate details field for git
command diagnostics. After fixes, spec and quality re-reviews approved. Final
checks passed: `cargo test --workspace`, `cargo fmt --all -- --check`,
`cargo clippy --all-targets --all-features -- -D warnings`,
`pnpm format:check`, and `git diff --check`. Committed as `0e5f9e5`.

## 2026-07-01 act | P2-003 Ollama provider

Added a dependency-free Ollama provider behind `ModelProvider`, request-builder
tests, an injectable HTTP client, a `seaf model check --provider ollama`
command, and a mechanical CLI dependency on `seaf-models`.

## 2026-07-01 verify | P2-003 Ollama provider

Spec review approved the provider and CLI scope. Code-quality review required
trying all resolved localhost addresses before failing and avoiding missing-model
pull hints for generic HTTP 404 API-root errors. After fixes, spec and quality
re-reviews approved. Final checks passed: `cargo test --workspace`,
`cargo fmt --all -- --check`,
`cargo clippy --all-targets --all-features -- -D warnings`,
`pnpm format:check`, and `git diff --check`. Manual smoke reached local Ollama
but reported missing `gemma4:e4b-mlx` with an `ollama pull` hint. Committed as
`3fe0744`.

## 2026-07-01 act | P2-008 local loop CLI

Added `ticket validate`, `loop run`, `loop status`, `loop resume`, and
`loop smoke` commands, a local `seaf-loop` CLI dependency, dirty-tree refusal
for loop runs, deterministic fake-provider loop execution, JSON outputs for
automation, and human-readable next-action summaries.

## 2026-07-01 verify | P2-008 local loop CLI

Spec review approved the CLI surface and public-API scope. Code-quality review
required safe user run ID validation, resume preflight before workspace
scaffolding, persisted run ID validation, and persisted/requested run ID
matching. After fixes, spec and quality re-reviews approved. Final checks
passed: `cargo test --workspace`, `cargo fmt --all -- --check`,
`cargo clippy --all-targets --all-features -- -D warnings`,
`pnpm format:check`, and `git diff --check`. Committed as `e7f04a2`.

## 2026-07-01 act | P2-010 EvalReport integration

Added loop-to-`EvalReport` integration in `seaf-loop`, optional
`seaf eval run --loop-run --ticket` CLI mode, a deterministic local-loop eval
config, and tests for loop identity binding, required loop checks, rejected
policy gates, command-mode backward compatibility, and product-path loop eval.

## 2026-07-01 verify | P2-010 EvalReport integration

Spec review approved the scope and acceptance coverage. Code-quality review
required loop artifact validation before command execution, product-path policy
evidence, policy decision `patch_id` binding to `run_id`, and no-op synthetic
policy evidence with `apply_requested = false`. After fixes, spec and quality
re-reviews approved. Final checks passed: `cargo test --workspace`,
`cargo fmt --all -- --check`,
`cargo clippy --all-targets --all-features -- -D warnings`,
`pnpm format:check`, `git diff --check`, and
`cargo run -p seaf-cli -- eval run examples/local-loop/seaf.evals.yaml --goal-id local_agent_loop_mvp --patch-id test --json`.
Committed as `1e86622`.

## 2026-07-01 spec | P2-009 AgentBench-lite scope

Amended the P2-009 ticket spec and current contract to make the benchmark
implementation scope reviewable: explicit AgentBench-lite fixture paths,
optional focused `seaf-loop` bench helper/tests, `loop bench` CLI wiring and
CLI tests, plus no-new-dependency preference. Clarified that fake-provider
execution is deterministic and CI-safe, Ollama is local-smoke only, JSON
summaries must include all required metrics, forbidden and eval-weakening
accepted counts are zero-tolerance failures, the fixture includes the five
initial tickets, and tests cover fake-provider summary plus zero-tolerance
failure handling.

## 2026-07-01 act | P2-009 AgentBench-lite

Added deterministic AgentBench-lite fixture loading and summary logic in
`seaf-loop`, `seaf loop bench` CLI wiring, a five-ticket fixture under
`examples/agent-bench-lite`, zero-tolerance failure handling, local Ollama smoke
execution, and focused benchmark/CLI tests.

## 2026-07-01 verify | P2-009 AgentBench-lite

Spec review required the Ollama path to perform real local smoke execution
rather than a placeholder. Code-quality review required fail-closed fixture file
loading, overflow-safe median calculation, Ollama smoke response validation, and
README alignment. After fixes, spec and quality re-reviews approved. Final
checks passed: `cargo test --workspace`, `cargo fmt --all -- --check`,
`cargo clippy --all-targets --all-features -- -D warnings`,
`pnpm format:check`, `git diff --check`, and
`cargo run -p seaf-cli -- loop bench --provider fake --fixture examples/agent-bench-lite --json`.
Manual Ollama smoke reached local Ollama and failed actionably because
`gemma4:e4b-mlx` is not installed, with an `ollama pull` hint. Committed as
`c711e04`.

## 2026-07-01 act | P2-011 local loop docs

Added the local agent-loop guide, Gemma/Ollama Mac setup guide, loop eval guide,
local security-boundary guide, and local-loop example README. The docs cover the
full demo path, local-only boundaries, untrusted model output, deterministic
gates, recovery with `loop status`/`loop resume`, artifact locations, CI-safe
fake commands, Ollama smoke behavior, and pending P2-012 CI hardening.

## 2026-07-01 verify | P2-011 local loop docs

Spec and docs-quality reviews approved. Quality review noted non-blocking
wording risks around fixed demo run IDs and eval policy-evidence timing; the
docs were clarified before commit. Final checks passed: `pnpm format:check`,
`git diff --check`, `cargo run -p seaf-cli -- ticket validate examples/local-loop/tickets/add-health-command.yaml`,
and `cargo run -p seaf-cli -- loop bench --provider fake --fixture examples/agent-bench-lite --json`.
Committed as `a070c4f`.

## 2026-07-01 act | P2-012 CI hardening

Updated `.github/workflows/ci.yml` to keep the Rust job on formatting, Clippy,
and `cargo test --workspace`, then added named guardrail steps for schema
fixtures, fake loop smoke without Ollama, AgentBench-lite fake provider output,
forbidden patch policy coverage, ticket validation, and AgentBench-lite CLI
execution. The TypeScript job remains on frozen pnpm install, format, lint,
typecheck, test, and build.

## 2026-07-01 verify | P2-012 CI hardening

Spec and CI-quality reviews approved. Review confirmed that focused test filters
run real tests, CI does not require Ollama, and TypeScript checks remain intact.
Final checks passed: `cargo test --workspace`, `cargo fmt --all -- --check`,
`cargo clippy --all-targets --all-features -- -D warnings`,
`pnpm install --frozen-lockfile`, `pnpm format:check`, `pnpm lint`,
`pnpm typecheck`, `pnpm test`, `pnpm build`, `git diff --check`, and all focused
P2-012 commands now listed in CI. Committed as `084688c`.

## 2026-07-07 act | P3-001 Phase 0 contract

Replaced the completed Phase 2 current contract with a post-Phase-2 Phase 0
production-readiness contract. Reset current progress to the Phase 0 P3-001
through P3-005 tracker, marking only P3-001 complete and leaving P3-002 through
P3-005 open. Preserved the production-readiness roadmap and made no code
changes.

## 2026-07-07 verify | P3-001 Phase 0 contract

Final documentation checks passed: `pnpm exec prettier --check
docs/production-readiness-roadmap.md .seaf/loops/current/contract.md
.seaf/loops/current/progress.md .seaf/loops/current/log.md` and
`git diff --check`.

## 2026-07-07 act | P3-002 stale docs

Updated `docs/mvp-backlog.md` to treat Phase 2 as the source of truth for
implemented local-loop primitives and to describe only the remaining
post-Phase-2 live integration work. Marked P3-002 complete in current progress;
P3-003 through P3-005 remain open. Final checks passed: `pnpm exec prettier
--check docs/mvp-backlog.md .seaf/loops/current/progress.md
.seaf/loops/current/log.md` and `git diff --check`.

## 2026-07-07 act | P3-003 default policy drift

Added a core regression that requires the default policy template and
adaptive-notes example policy to list every policy-gate human-review category.
Updated both policies to include CI, eval, policy, updater, and signing change
categories, then marked only P3-003 complete in current progress.

## 2026-07-07 act | P3-004 generated artifact hygiene

Added a context regression for ticket-requested `.seaf/loops/runs` artifacts,
then added `.seaf/**` to default context excludes and `.seaf/loops/runs/` to
`.gitignore`. Marked P3-004 complete; P3-005 remains open.
