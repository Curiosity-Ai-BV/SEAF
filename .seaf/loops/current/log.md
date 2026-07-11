# Loop Log

## 2026-07-11 gather | production-use program

Committed the independently reviewed production-use roadmap as `4a20922` on
`codex/production-use-milestones`. Started S0 to establish one shared execution
contract for U1-U11. Production code is blocked until the contract has separate
spec and quality approval.

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

## 2026-07-07 act | P3-005 CI determinism

Hardened CI with read-only contents permissions, per-ref concurrency cancellation,
job timeouts, locked Cargo checks, and a documented stable Rust toolchain policy.
Split root lint into `lint:rust` and `lint:packages`, then pointed the TypeScript
CI job at package-only lint so it does not invoke Rust setup.

## 2026-07-11 verify | S0 execution contract

The first spec review found promotion/eval ordering, persistent-execution,
slice-size, external release, and per-repository pilot-gate gaps. The contract
was split into smaller dependency-ordered slices and re-review approved it. The
quality review then found a contradictory sandbox exception, mutable revision
ambiguity, non-executable verification shorthand, and an untracked final
publication action. Those were corrected with a strict pre-execution human
gate, immutable revision attempts, an exact command matrix, and M3-06. Quality
re-review approved. Prettier and `git diff --check` passed.

## 2026-07-11 act | M1-01a project configuration and input digests

Added the deny-unknown-fields `ProjectConfig` contract with safe relative policy
path validation, public schema, and valid/invalid fixtures. Added deterministic
typed canonical JSON and SHA-256 helpers. Made lowercase 64-hex ticket, policy,
and config digests required in `LoopRun`, its public schema, state creation, and
runner construction. No configuration discovery, input snapshot persistence, or
resume enforcement was added.

## 2026-07-11 verify | M1-01a witnessed TDD and checks

RED: `cargo test -p seaf-core project_config --locked` failed to compile with
the expected missing `ProjectConfig`, load/validate, and canonical helper errors
before production edits. GREEN: focused `seaf-core`, state, eval-report, and
provider-runner tests passed, including exact digest propagation. The first
workspace run correctly exposed stale handcrafted CLI LoopRun fixtures; after
adding the required digests, the focused regressions and the previously
timing-sensitive descendant-cleanup test passed. Final locked workspace tests,
Rust formatting, and Clippy passed. `corepack pnpm format:check` then identified
only the pre-existing roadmap table alignment and was rerun after formatting.

## 2026-07-11 verify | M1-01a quality review fixes

RED: reverse-ordered root and nested serialized maps produced different
canonical bytes, and an embedded-newline project policy path passed runtime
validation. GREEN: canonical serialization now recursively sorts JSON object
keys, and Rust plus the public schema reject C0/C1 control characters with
newline, NUL, and control fixtures. The provider-backed fake-loop regression
was already green and now explicitly verifies persisted digests against the
canonical effective ticket, compiled default policy, and typed absent config.
Focused regressions and the full locked workspace tests passed, followed by
Rust formatting, Clippy with warnings denied, Prettier, and `git diff --check`.

## 2026-07-11 act | M1-01b authoritative configuration and snapshots

Added optional `loop run --config` and `--policy` inputs. New provider runs now
resolve policy authority in this order: explicit policy, explicit or Git-root
`seaf.config.json`, then Git-root `seaf.policy.json`; absence fails closed. All
explicit/discovered config and policy files canonicalize inside the Git root,
and config policy paths resolve from the config directory. The winning typed
ticket, policy, and effective config are canonically snapshotted under the run
`inputs/` directory before provider execution, and `LoopRun.input_digests`
matches those values. The compiled default policy no longer authorizes new fake
or Ollama provider runs. Resume comparison remains intentionally deferred to
M1-02.

## 2026-07-11 verify | M1-01b witnessed TDD and focused checks

RED: `cargo test -p seaf-cli --locked
loop_run_project_config_policy_changes_fake_gating_and_explicit_policy_wins --
--exact` failed because `loop run` did not recognize `--config`. GREEN:
`cargo test -p seaf-cli --test cli --locked loop_run_` passed all 12 focused
tests, covering precedence and real custom gating, config-relative resolution,
canonical snapshots/digests, no-authority and invalid-config zero side effects,
invalid-policy zero side effects, symlink escape, root fallback, fake-provider
execution, and mocked Ollama.

Final verification passed: `cargo fmt --all -- --check`, locked Clippy for all
targets/features with warnings denied, `cargo test --locked --workspace`,
`corepack pnpm format:check`, and `git diff --check`.

## 2026-07-11 verify | M1-01b explicit-policy precedence review fix

Spec review found that a malformed discovered Git-root `seaf.config.json`
blocked a valid higher-precedence `--policy`. RED: the focused
`loop_run_explicit_policy_bypasses_malformed_discovered_config` regression
failed with project-config validation. GREEN: discovery is now skipped when an
explicit policy is supplied without `--config`; an explicitly supplied config
is still loaded and validated before the policy override. The new regression,
the explicit-invalid-config guard, and all 13 focused `loop_run_` tests passed.

## 2026-07-11 act | M1-02 resume configuration integrity

Added `loop resume --config` and `--policy` with the same authority precedence
as new runs. New provider runs now persist canonical repository identity beside
the ticket, policy, and config inputs. Incomplete resume verifies current typed
inputs, every canonical input snapshot, all three `LoopRun` digests, canonical
worktree root, and Git common directory before opening mutable loop state or
constructing a provider runner. Verified policy/config now drives resumed fake
and Ollama context and patch gating; the compiled default-policy fallback was
removed. Existing ticket apply-authority downgrade behavior remains unchanged.
M1-02 is complete and M1-03 is active.

## 2026-07-11 verify | M1-02 witnessed TDD and full gates

RED: `cargo test -p seaf-cli --test cli --locked loop_resume_ -- --nocapture`
ran 15 tests with 7 passing and 8 expected failures. Resume rejected the new
flags, accepted missing/noncanonical snapshots and a mismatched run digest,
used the compiled policy for mocked Ollama, and did not persist repository
identity. GREEN: the same focused command passed all 15 tests, covering
same-path policy mutation, config mutation with unchanged effective policy,
missing/noncanonical snapshots, run-digest mismatch, matching explicit
authority, unsafe authority paths, copied-repository rejection, byte-for-byte
zero run-directory mutation, zero provider calls, recovery behavior, and
verified-policy Ollama gating. All 13 focused `loop_run_` tests and all 4
existing provider-resume recovery tests also passed.

Final verification passed: `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets --all-features -- -D warnings`,
`cargo test --locked --workspace`, `corepack pnpm format:check`, and
`git diff --check`.

## 2026-07-11 verify | M1-02 repository-digest spec review fix

Spec review found that repository identity was compared only with the mutable
`inputs/repository.json` snapshot. A copied run could therefore rewrite that
snapshot canonically for the destination repository and sever the original
binding. RED: the exact copied-run regression failed because resume succeeded
after that rewrite. The core fixture test also failed to compile because
`LoopInputDigests` had no `repository` field.

GREEN: `LoopInputDigests`, runtime validation, public schema, valid fixture,
state tests, and all call sites now require a lowercase 64-hex repository
digest. New provider runs hash the canonical repository identity into immutable
`run.json`. Resume compares the run-bound digest with both the canonical
snapshot and current repository before mutation or provider calls. The focused
core LoopRun tests passed 6/6, state digest propagation passed, canonical CLI
snapshot/digest persistence passed, and all 16 resume tests passed, including
the rewritten-snapshot attack.

Full verification passed after the fix: `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets --all-features -- -D warnings`,
`cargo test --locked --workspace`, `corepack pnpm format:check`, and
`git diff --check`. M1-02 remains complete and M1-03 remains the next active
slice.

## 2026-07-11 verify | M1-02 workspace and verified-state quality fixes

Quality review found two remaining resume seams. First, `LoopWorkspace::open`
called `ensure_layout`, so a symlinked run directory, audit file, or layout
directory could be followed and resume could write before rejecting it. Second,
CLI preflight validated one `LoopRun`, but `LoopRunner::resume` reread
`run.json`, discarding that verified instance before execution.

RED was witnessed in two stages. The exact verified-state seam test first
failed to compile because `LoopRunner::resume_verified` did not exist. After
adding only that handoff, all three symlink groups failed because resume
accepted a symlinked run directory, symlinked `run.json`, `log.md`, and
`context-manifest.json`, and symlinked `prompts`, `responses`, and `artifacts`
directories.

GREEN: existing workspace open is now read-only validation. It rejects a
symlinked or non-directory run root, canonical containment outside the
canonical runs root, non-regular or symlinked audit files, and non-directory or
symlinked layout directories before `prepare_run` or log append. External
targets remain byte-for-byte unchanged. `LoopRunner::resume_verified` consumes
the exact preflight `LoopRun`, and CLI provider resume now uses that API instead
of rereading `run.json`. Seven focused state-resume tests and all 16 CLI resume
tests passed.

Full verification passed after both fixes: `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets --all-features -- -D warnings`,
`cargo test --locked --workspace`, `corepack pnpm format:check`, and
`git diff --check`. No locking or atomic-write behavior from M1-10 was added.

## 2026-07-11 verify | M1-02 child artifact symlink quality fix

The remaining quality review finding was that real top-level layout directories
could still contain symlinked child targets. `fs::write` followed those links
when persisting the next prompt, deterministic response, or step artifact.
RED: three separate regressions all failed because `run_next_step` succeeded
with symlinked `prompts/01-research.prompt.md`,
`responses/01-research.raw.txt`, and `artifacts/01-research.md` targets.

GREEN: prompt attempt discovery rejects symlinked and non-regular matching
entries. The shared artifact writer now accepts only safe relative paths,
requires a real run directory, canonically contains the existing parent inside
that run directory, rejects symlinked/non-regular targets, creates absent files
with create-new semantics, and truncates only a validated regular file. This
preserves legitimate context-manifest and rerun artifact replacement behavior.
All three focused child-target regressions passed with external target bytes
unchanged, followed by the complete `seaf-loop` suite and all 16 CLI resume
tests.

Full verification passed after the fix: `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets --all-features -- -D warnings`,
`cargo test --locked --workspace`, `corepack pnpm format:check`, and
`git diff --check`. No locking or atomic-write behavior from M1-10 was added.

## 2026-07-11 verify | M1-02 exhausted prompt-attempt quality fix

Final quality review found unchecked `highest_attempt + 1` arithmetic. A
persisted `u32::MAX` prompt-attempt filename caused a debug overflow after the
runner had already marked the step running, saved `run.json`, and appended the
log. RED: the exact maximum-attempt regression panicked in `artifacts.rs` and
could not return the required no-mutation error.

GREEN: prompt attempt allocation now uses `checked_add` and returns an
actionable exhausted-sequence error that recommends starting a new run.
Read-only attempt allocation now happens before step-state transition, run save,
log append, request creation, or artifact writes. The regression verifies the
entire run tree is byte-for-byte unchanged and the step runner receives no
request or execution call. The exact regression and all 21 state tests passed.

Full verification passed after the fix: `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets --all-features -- -D warnings`,
`cargo test --locked --workspace`, `corepack pnpm format:check`, and
`git diff --check`. No locking or atomic-write behavior from M1-10 was added.

## 2026-07-11 verify | M1-02 resume child preflight and cached-attempt fix

Follow-up quality review found resume still prepared provider state and appended
the resume log before child-file or next-attempt validation. RED: four
`resume_verified` regressions all returned a runner instead of failing before
prepare. The cases covered a regular maximum-`u32` prompt attempt plus
symlinked prompt, response, and artifact children; each test snapshots the
entire run tree and asserts zero prepare, request, and execution calls.

GREEN: existing workspace open now preflights every child entry under
`prompts`, `responses`, and `artifacts` as a canonically contained regular
non-symlink file. Resume computes and caches the persisted next step's checked
attempt before `prepare_run` or log append, and `run_next_step` consumes that
attempt once. New and rerun runners retain on-demand attempt calculation before
state mutation. All four focused preflight tests, all 22 state tests, the full
`seaf-loop` suite, and all 16 CLI resume tests passed.

The first full workspace gate hit the existing timing-sensitive
`eval_run_cleans_up_descendants_that_keep_pipes_open` threshold at 1.0499
seconds. Its exact rerun passed, and the complete locked workspace rerun passed
all 213 tests. Final formatting, locked Clippy, `corepack pnpm format:check`, and
`git diff --check` passed. No locking or atomic-write behavior from M1-10 was
added.

## 2026-07-11 implement | M1-03a validated early role artifact chain

RED began with a focused research-request regression that failed on the missing
effective-ticket seam. The next RED proved the early chain lacked structured
canonical artifacts and run-state digests. Focused state REDs then rejected the
existing acceptance of unpaired and malformed artifact integrity metadata.

GREEN adds canonical validated role envelopes for Research, Analysis,
SpecCreation, and SpecReview. Each envelope binds run ID, step, role, the parsed
role response, and its canonical response digest; `run.json` separately binds
the safe artifact path and canonical artifact digest. Requests carry the exact
effective TicketSpec, run ID, all four input digests, and only the required
prior role responses. Raw provider transcripts remain separate response audit
files. Blocked and failed early responses retain their validated artifacts but
do not enable downstream steps.

Resume now loads the exact preflight LoopRun, verifies every required early
artifact before context-manifest writes or log/provider/state mutation, and
rejects missing, tampered, noncanonical, wrong-run, wrong-role, wrong-step, and
wrong-digest evidence. The matrix snapshots the whole run tree and asserts zero
provider requests for every failure. New-run and resumed happy paths both pass.
M1-03b remains intentionally out of scope and is now active.

Verification passed: focused role/provider/state/core tests, all CLI resume
tests, `cargo fmt --all -- --check`, locked all-target/all-feature Clippy with
warnings denied, the full locked 221-test Rust workspace, `corepack pnpm
format:check`, and `git diff --check`.

## 2026-07-11 verify | M1-03a prepared-ticket and schema-pair fixes

Follow-up review found two contract gaps. First, a directly constructed
`ProviderStepRunner` could prepare without a TicketSpec and retain the legacy
generic early-role request/artifact behavior. RED showed the missing-ticket
run prepared successfully; a second matrix showed substituted ticket ID, goal
ID, and canonical ticket digest also prepared. GREEN makes every prepared
provider run require the exact effective TicketSpec and binds all three fields
to the exact LoopRun before artifact loading or context packing. The tests
preconfigure live context, snapshot the complete runs tree, and assert no tree
change or provider request for every failure.

Second, the loop-run JSON Schema allowed a string artifact path paired with a
null digest, or the reverse, because it checked property presence rather than
non-null value pairing. RED found no exclusive artifact-pair schema branches.
GREEN permits exactly three representations: both valid strings, both explicit
nulls, or both absent. Runtime and schema parity regressions cover both mismatch
directions.

The first full workspace run exposed one legitimate compatibility conflict:
resume had cloned and modified the verified ticket to disable renewed apply
authority, breaking the new canonical ticket digest binding. The fix keeps the
provider ticket exact and applies the persisted-authority restriction only to
the separate patch-gate configuration. The exact mocked-Ollama resume
regression passed afterward.

Final verification passed: focused authority and schema tests, `cargo fmt
--all -- --check`, locked all-target/all-feature Clippy with warnings denied,
the complete locked 224-test Rust workspace, `corepack pnpm format:check`, and
`git diff --check`. M1-03b behavior remains untouched.

## 2026-07-11 implement | M1-03b exact development and output-review evidence

RED began with Development still emitting the legacy generic prose request,
which could not provide the exact approved spec identity. The next RED found
that a validated `patch_proposed` response could complete without any patch
gate or Development artifact. GREEN adds a structured Development request
bound to run/input digests and the exact canonical SpecCreation and approving
SpecReview envelopes, paths, and digests. The initially bounded repository
context is retained only for Development because it needs the named source
files to construct a patch; unrelated Research and Analysis bodies are absent.

Every validated DeveloperResponse now persists a canonical run-bound artifact.
Patch proposals require exactly one authoritative gate and produce typed
DevelopmentEvidence binding the validated response, exact patch, patch digest,
normalized changed paths, and exact PolicyDecision. Rejected evidence persists
and stops. Blocked or needs-context responses persist without invented policy
fields and do not advance. Allowed and human-review evidence may reach
OutputReview under the existing state semantics; human approval remains M1-06.

OutputReview receives only verified DevelopmentEvidence, approved-spec
identities, the run ID, and input digests. Its validated response is persisted
as the canonical OutputReview role envelope. Resume at Development reuses the
verified approved spec. Resume at OutputReview reuses the exact persisted
evidence, verifies the run's exact policy decision, and does not rerun the
patch gate. Missing, tampered, noncanonical, wrong-run, wrong-role, wrong-step,
wrong-digest, substituted-patch, and policy-mismatch matrices fail before any
provider, gate, or durable mutation.

Focused provider, role, policy, state, and CLI resume suites passed. M1-03b is
complete and M1-04 bounded additional context is active. Context-request flow,
candidate worktrees, human approval, eval/promotion, and commits remain out of
scope.

Final verification passed: `cargo fmt --all -- --check`, locked all-target and
all-feature Clippy with warnings denied, all 233 locked Rust workspace tests,
`corepack pnpm format:check`, and `git diff --check`.

## 2026-07-11 verify | M1-03b OutputReview approval binding

Spec review found that OutputReview resume verified the canonical SpecReview
envelope but did not reassert its `approve_spec` decision. RED canonically
rewrote the persisted decision to `approve_for_tests`, `request_changes`, and
`reject`, recomputed the response and outer artifact digests, and updated the
supplied LoopRun digest binding. All three substitutions resumed successfully
instead of failing before the resume log.

GREEN makes OutputReview preparation and request construction reuse the same
`require_approved_spec_review` guard as Development. Downstream resume also
applies that guard after canonical artifact verification and before any log,
provider, gate, or state mutation. The matrix snapshots the whole run tree and
asserts zero provider and patch-runner calls for every changed decision.

Verification passed: all 37 focused provider-runner tests, all four focused CLI
provider-resume tests, `cargo fmt --all -- --check`, locked all-target and
all-feature Clippy with warnings denied, all 234 locked Rust workspace tests,
`corepack pnpm format:check`, and `git diff --check`.

## 2026-07-11 verify | M1-03b evidence reparse and proposal-only gating

Quality review found two remaining trust-boundary gaps. First, coordinated
tampering could replace the patch, developer response, evidence paths, policy
decision, run policy decision, and every digest together. RED showed resume
accepted a forbidden-path patch whose substituted evidence still claimed only
`docs/example.md`. GREEN independently reparses the exact canonical patch and
requires its normalized paths to equal both DevelopmentEvidence and
PolicyDecision paths. Malformed and binary patches remain valid rejected
evidence only when their exact gate rejection kind is preserved.

Second, an apply-enabled provider run could execute `GitApply` against the
source checkout before Development evidence and run state were durably saved.
RED forced artifact persistence to fail after gating and observed `applied:
true` plus source mutation. GREEN adds a proposal-only provider gate path: it
preserves `apply_requested`, may run `GitApplyCheck`, never runs `GitApply`, and
always leaves `applied` false. The regression uses a mutation-capable runner,
forces the later artifact write to fail, and proves the source remains
byte-for-byte unchanged. Generic `gate_patch` retains explicit apply behavior
for non-provider callers; OutputReview resume still reuses evidence without
rerunning the gate.

Verification passed: all 38 focused provider-runner tests, all 13 generic
policy-gate tests, `cargo fmt --all -- --check`, locked all-target and
all-feature Clippy with warnings denied, all 235 locked Rust workspace tests,
`corepack pnpm format:check`, and `git diff --check`.
