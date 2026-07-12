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

## 2026-07-11 implement | M1-04a context request contract

RED added a valid Researcher `needs_context` fixture carrying a structured
request and a missing-request rejection. The focused parser test failed because
`context_request` was an unknown field. The expanded RED corpus then covered
agent and developer presence invariants, empty/too-many/duplicate paths,
absolute/current/parent/traversal/backslash/control paths, empty/control/large
reasons, unknown request fields, and unexpected requests on non-needs statuses.

GREEN adds the typed ContextRequest to AgentResponse and DeveloperResponse,
omitted when absent. Deserialization denies unknown request fields and accepts
only 1-8 unique, already-normalized repository-relative paths. Absolute paths,
Windows prefixes, empty/current/parent segments, backslashes, and control
characters fail closed. Reasons must be nonempty after trimming, control-free,
and at most 1,024 Unicode scalar values. Runtime status checks require the
request only for `needs_context`; the existing developer patch invariant is
unchanged.

Researcher, Analyzer, SpecWriter, and Developer schemas carry the same request
shape, bounds, patterns, and status-dependent presence rule. Reviewer schemas
remain unchanged. Schema-invalid JSON remains non-repairable. Provider behavior
also remains unchanged: a validated needs-context response persists and blocks,
without context repacking, retries, extra provider rounds, or manifests.

Verification passed: 13 focused role-response tests, 38 focused provider-runner
tests, `cargo fmt --all -- --check`, locked all-target and all-feature Clippy
with warnings denied, all 239 locked Rust workspace tests, `corepack pnpm
format:check`, and `git diff --check`. M1-04a is complete and M1-04b bounded
context expansion orchestration is active.

## 2026-07-11 implement | M1-R01 descendant pipe regression stability

Repeated full workspace gates had failed the existing
`eval_run_cleans_up_descendants_that_keep_pipes_open` regression near its own
1-second boundary, including observed elapsed times from approximately 1.011 to
1.050 seconds. Exact and serial reruns passed around 0.90 to 0.92 seconds. The
test configured a 1,000ms eval timeout and independently required the entire CLI
to finish in under 1 second, so harmless direct-child scheduling variance could
fail before the production pipe-drain behavior was evaluated.

Deterministic RED added a bounded 1,200ms direct-child delay after the detached
pipe owner was ready while preserving the old 1,000ms eval timeout. The exact
test failed after 1,264ms with `command timed out after 1000ms`, demonstrating
the test's timeout race rather than a product regression.

GREEN raises only the regression's eval timeout to 4,000ms and elapsed ceiling
to 3 seconds. The detached `setsid` descendant keeps inherited pipes open for
up to 8 seconds, so waiting for descendant EOF still exceeds the assertion by
a material margin. Each execution writes a stop sentinel after the CLI returns
and requires an exit marker, keeping the safety lifetime bounded and verifying
that escaped descendants terminate. The production 250ms drain grace and all
eval implementation logic remain unchanged.

The exact regression passed 20 consecutive executions with zero failures;
per-run wall times ranged from 2.13 to 2.25 seconds, including compilation of
the temporary Rust helper. M1-R01 is complete and M1-04b bounded context
expansion orchestration is active.

Final verification passed: `cargo fmt --all -- --check`, locked all-target and
all-feature Clippy with warnings denied, all 239 locked Rust workspace tests,
`corepack pnpm format:check`, and `git diff --check`.

## 2026-07-11 analyze | M1-04b implementation boundary

Source review found that the current LoopRunner persists the outer step request
before `ProviderStepRunner::run_step`, but persists the response only after that
method returns. A same-step context retry therefore needs explicit immutable
exchange ordering and authoritative run-state references; the mutable initial
`context-manifest.json` and filesystem scanning alone cannot prove round counts
or resume integrity.

M1-04b is split into four reviewable commits. M1-04b1 defines an additive,
all-or-nothing expansion primitive and canonical create-only artifact containing
the exact accepted bytes, without changing provider or LoopRun behavior.
M1-04b2a adds the durable exchange/state contract, M1-04b2b adds bounded live
same-role orchestration, and M1-04b2c adds resume/rerun/CLI reconciliation. The
immutable initial provider-request audit, not the mutable content-free initial
manifest, is the initial-byte authority. Context denials block; provider or
audit infrastructure failures fail when writable, while a failed durable write
must stop further calls and be reconciled later. Both the two-per-logical-step
and eight-per-run caps span all attempts; legacy calls consume zero expansion
rounds. Flat names are required because the existing workspace validator
intentionally accepts only regular child files in its audit directories.
M1-04b1 is active.

## 2026-07-11 implement | M1-04b1 additive context expansion artifact

RED introduced a focused integration suite before the production API existed.
`cargo test -p seaf-loop --test context_expansion` failed with E0432 for the
missing expansion create/load/reconstruct functions and types. The tests bound
safe single/multi-file ordering, already-loaded handling, strict atomic denial,
cumulative UTF-8 truncation, immutable initial-request authority, prior-byte
reconstruction, create-only collisions, layout symlinks, and tamper rejection.

GREEN adds a standalone version-1 canonical expansion primitive and codec. It
normalizes semantically unordered paths, enforces default/ticket/policy and
repository/symlink/UTF-8/byte controls before writing, and persists exact new
content with source/included metadata and digests. Each artifact binds the
run/step/role/attempt/round, validated request/reason, immutable initial prompt
path/digest, previous expansion path/digest, effective limits/exclusions,
already-loaded paths, and prior/result byte totals. Prior chains are verified
from immutable artifacts without rereading their live repository files.
Create-only writes accept only identical replay bytes and reject different
bytes plus symlinked parents or targets. LoopRun, provider calls, CLI behavior,
and the mutable initial manifest are unchanged.

Focused GREEN passed all 10 context-expansion tests. Final verification passed
`cargo fmt --all -- --check`, locked all-target/all-feature Clippy with warnings
denied, all 249 locked Rust workspace tests, `corepack pnpm format:check`, and
`git diff --check`. M1-04b1 is complete and M1-04b2a is active.

## 2026-07-11 review fix | M1-04b1 authority and chain validation

Spec review identified three gaps. RED added four focused failures proving that
an internal repository-directory symlink could alias a forbidden target, a
different safe run file could stand in for the initial prompt, substituted
initial loaded-path metadata was not artifact-bound, and a canonical forged
prior artifact could falsely classify an unloaded path as already loaded when
its supplied link digest was also changed.

GREEN rejects every symlink component in a requested repository path, derives
the exact initial prompt audit filename from step and attempt, and persists the
canonical complete initial loaded paths and initial byte total in every
expansion. Chain verification now checks those initial values at every round
and recomputes historical exclusions from the initial loaded set plus only the
files actually accepted by preceding artifacts. M1-04b1 still treats the
initial loaded-path/byte values as explicit expected input; M1-04b2a owns their
authoritative reconciliation to a structured provider-request audit.

Remaining spec review added a fifth RED: a canonically forged prior artifact
replaced its request and accepted file with a default-forbidden `.env`, made
all file digests and byte totals internally consistent, and recomputed the
supplied link digest. The next round incorrectly accepted that history. GREEN
now reapplies the artifact-bound effective default, ticket, and policy
forbidden controls to every represented prior request path before accepting a
chain link.

The corrected focused suite passed all 15 tests. Final verification passed
`cargo fmt --all -- --check`, locked all-target/all-feature Clippy with warnings
denied, all 254 locked Rust workspace tests, `corepack pnpm format:check`, and
`git diff --check`.

## 2026-07-11 quality fix | M1-04b1 recovery and publication safety

Quality review required an accepted immutable expansion to remain authoritative
when its live source later changes. RED changed the recovery assertion and the
old implementation failed with an existing-byte collision. A second
deterministic unit RED failed to compile because no opened-file/current-path
identity check existed. Concurrent identical creation, orphaned partial-temp,
and sparse invalid-UTF-8-tail regressions were also added for the publication
and streaming boundaries.

GREEN verifies and returns an existing fixed target before any live source
reread. New artifacts are written and synced to unique create-only regular temp
files in the same real parent, atomically hard-linked to the absent final name,
and reconciled against a complete byte-identical concurrent winner before temp
cleanup. Orphan temps are ignored. Repository sources now pass component
symlink checks, are opened once, rechecked inside the repository, and have the
opened/current file identity compared using device/inode on Unix or volume/file
index on Windows. A fixed 8 KiB stream hashes and validates all source bytes as
UTF-8 while retaining only the bounded prefix.

Focused GREEN passed the identity unit regression and all 18 context-expansion
integration tests. Final verification passed `cargo fmt --all -- --check`,
locked all-target/all-feature Clippy with warnings denied, all 258 locked Rust
workspace tests, `corepack pnpm format:check`, and `git diff --check`.

## 2026-07-11 final quality fix | M1-04b1 trusted recovery identity

Final quality review rejected self-authentication of an unreferenced existing
final artifact. RED canonically rewrote current accepted content together with
internally consistent file digests, sizes, totals, and outer bytes. Loading
with the original trusted digest rejected it, but creation incorrectly derived
a new digest and adopted the forgery. The changed-live-source recovery test was
also corrected to distinguish trusted load from creation.

GREEN removes existing-target adoption from creation. Creation always rebuilds
from current live inputs and delegates equality/collision handling to atomic
publication, so changed live bytes and canonical final-target forgeries collide.
`load_context_expansion` remains the sole trusted recovery path and requires a
caller-supplied path/digest; M1-04b2a will persist that authoritative identity.
Loading the original identity still returns accepted bytes without rereading
the changed live source.

Focused GREEN passed the identity unit regression and all 19 context-expansion
integration tests. Final verification passed `cargo fmt --all -- --check`,
locked all-target/all-feature Clippy with warnings denied, all 259 locked Rust
workspace tests, `corepack pnpm format:check`, and `git diff --check`.

## 2026-07-11 durability fix | M1-04b1 concurrent winner sync

Final durability review found that the no-replace publication winner synced the
parent directory, but an identical concurrent/retry loser returned success
after byte verification without flushing the same directory entry. The
AlreadyExists/byte-identical branch now syncs the real parent before reporting
success, matching the winning publication branch. The existing concurrent
identical-creator regression passed, as did all 259 locked workspace tests and
the full Rust and documentation gates.

## 2026-07-11 implement | M1-04b2a durable context exchange contract

RED first introduced the provider-exchange integration suite. Its initial
compile failed with E0432/E0609 because the typed records, LoopRun reference
field, immutable writers, classifier, and append protocol did not exist. Later
focused REDs proved that bound request tampering was accepted before staging,
new step groups could not link the run-wide ledger head, and run loading ignored
tampered authoritative record bytes.

GREEN adds version-1 two-phase canonical records with exact role outcomes and
typed ordered LoopRun references. Request and response phases bind the run,
step, role, attempt, exchange index/kind, optional distinct context round,
global prior-record digest, audited request/response identities, trusted
M1-04b1 expansion identity, and parsed outcome. Sequence validation rejects
gaps, reorderings, identity/path/digest mismatches, wrong phase and role/outcome
pairings, broken global links, response substitutions, and invalid transitions.
Only `needs_context` can start the next context round, only malformed JSON can
start repair, and terminal outcomes end that step-attempt exchange chain.

The M1-04b1 atomic no-replace publisher is now shared by expansion and exchange
artifacts. Request, response, and record creation is create-only and
byte-identical-idempotent; expansion records reference the existing canonical
M1-04b1 artifact rather than copying it. Safe real-file and digest checks happen
before record publication and again on load. Staged records are inspectable but
never auto-adopted, and conflicting authoritative identities fail closed. The
append API verifies one record against the run-wide head before persisting its
reference, while run loading re-verifies the full authoritative chain.

Focused GREEN passed all 13 provider-exchange tests and the live provider
single-call regression. Final verification passed `cargo fmt --all -- --check`,
locked all-target/all-feature Clippy with warnings denied, all 272 locked Rust
workspace tests, `corepack pnpm format:check`, and `git diff --check`. M1-04b2a
is complete and M1-04b2b is active.

## 2026-07-11 spec fix | M1-04b2a atomic append and repair authority

Spec review found that exchange references were appended with a truncating
write and no lock, so concurrent callers could both report success while one
update was lost. RED made two same-head appenders succeed and proved a symlinked
prospective lock path was ignored. A separate injected pre-publication RED could
not compile because no atomic replacement seam existed. GREEN adds a stable
real-file provider-exchange lock, reloads and verifies state while holding it,
writes and syncs a unique same-parent temporary file, atomically replaces
`run.json` on macOS/Linux, syncs the parent, and then unlocks. Exactly one
concurrent caller commits; the stale-head caller rejects. Pre-publication
failure leaves the prior state byte-identical and parseable, while orphaned
temporary-name collisions advance to a fresh reservation. This narrow safety
boundary is pulled forward because another provider call depends on it; M1-10
still generalizes atomic locking to every other mutation.

Repair REDs showed that JSON repair could not retain context authority. Repair
records may now carry either both a nonzero context round and expansion or
neither. An invalid initial response repairs without context, while an invalid
context response requires the repair to inherit the exact round and expansion.
Missing, zero, or substituted authority rejects, and repair records do not
consume expansion rounds.

The LoopRun JSON Schema now has closed cross-field conditionals for
step/role/path stem, kind/path, phase/path, and context-round presence. The docs
attribute gaps, ordering, run-wide links, outcomes, and bound-byte checks only
to Rust runtime/state validation rather than claiming JSON Schema can validate
array history. Focused GREEN passed 17 provider-exchange integration tests and
both atomic-state unit tests. Final verification passed
`cargo fmt --all -- --check`, locked all-target/all-feature Clippy with warnings
denied, all 278 locked Rust workspace tests, `corepack pnpm format:check`, and
`git diff --check`.

## 2026-07-11 quality fix | M1-04b2a derived outcomes and transition closure

Quality review found that a record caller could claim any role-compatible
outcome and that non-advancing responses could jump to a new step or attempt.
RED bound valid `needs_context` content to a false `passed` claim, blocked
content to `invalid_response`, a provider failure to `passed`, and a valid
repair response to `invalid_response`. Further REDs attempted cross-step and
incremented-attempt bypasses after needs-context, invalid, blocked, and provider
failure outcomes, and exercised a second repair after an invalid repair.

GREEN replaces raw response bytes with a canonical typed audit containing the
complete `ModelResponse` or `ModelError`. Stage and load verify its path/digest
and canonical bytes, parse model content through the existing exact role
parser, derive the precise outcome, and reject any record mismatch. Invalid
JSON/schema derives `invalid_response`; a failure envelope derives
`provider_failure`. Only malformed JSON is repair-eligible; schema, role,
reviewer-decision, and context-contract invalidity is terminal. Only the
role-specific advancing success may start the exact next provider step.
Same-group needs-context enters context retry, malformed JSON permits one
repair, and all other outcomes terminate that chain without a step/attempt
escape.

The ledger lock is documented as cooperative SEAF-process concurrency only.
Preexisting unsafe paths still fail closed and the opened lock identity is
rechecked immediately before publication, but no claim is made against a
hostile same-user process replacing directory entries inside the critical
section. M1-10 will generalize locking, and M1-11 private artifact protection
will strengthen that boundary. Focused GREEN passed 20 provider-exchange
integration tests.
Final verification passed `cargo fmt --all -- --check`, locked
all-target/all-feature Clippy with warnings denied, all 281 locked Rust
workspace tests, `corepack pnpm format:check`, and `git diff --check`.

## 2026-07-11 final spec fix | M1-04b2a repair eligibility and valid concurrency proof

Final review found that every `invalid_response` outcome could enter repair,
even when exact role parsing had classified valid JSON as schema-, role-, or
context-contract-invalid. RED showed schema-invalid canonical content starting
a repair. GREEN retains the exact parser classification alongside the outcome:
only `RoleResponseError::InvalidJson` is repair-eligible. Other invalid response
classes remain durably representable but terminal.

Reviewer parsing also accepts both approval decision strings structurally. RED
showed that SpecReviewer `approve_for_tests` content was unstageable as an
invalid response; the symmetric OutputReviewer `approve_spec` case was covered
in the same regression. These cross-role decisions now derive terminal
`invalid_response`, stage and load canonically, and cannot begin repair.

The concurrency regression previously raced one valid Analysis candidate
against a SpecCreation candidate that was already invalid from the Research
head. The corrected test stages two distinct immutable Analysis attempts,
preflights both independently against the same old successful Research state,
then races them. Exactly one locked append succeeds; the loser rejects only
after reload reveals its stale head. Focused GREEN passed 22 provider-exchange
integration tests. Final verification passed `cargo fmt --all -- --check`,
locked all-target/all-feature Clippy with warnings denied, all 283 locked Rust
workspace tests, `corepack pnpm format:check`, and `git diff --check`.

## 2026-07-11 implement | M1-04b2b bounded live context orchestration

RED fake-provider callbacks inspected `run.json` immediately before each call
and found no durable request record. The initial three regressions covered a
needs-context retry, malformed-JSON repair followed by context, and provider
failure; all compiled and failed at the first call with zero exchange records
instead of one. Further REDs covered every denial class, exact and one-over
caps, cross-attempt and cross-role counting, initial/response/expansion/next
request collisions, terminal schema failure, and one completion transition.

GREEN adds fresh-run-only orchestration through an explicit fresh preparation
hook, exact step-attempt handoff, and a verified append-only exchange-reference
handoff that prevents `LoopRunner` from overwriting atomic ledger appends with
stale in-memory state without granting the step runner unrelated state authority.
Every call now has a durable request record first and a canonical full
response/failure audit and response record afterward. Malformed JSON receives
one audited repair in any round. A valid context request creates an immutable
expansion and durably binds the next request before retrying the same role.

Retry prompts are rebuilt from the verified exact initial exchange request and
the ordered verified expansion chain. The live path never rereads accepted
expansion content from the repository. Legacy M1-04b1 initial prompt identities
remain valid, while fresh rounds bind the exchange request. Accepted context
request records enforce two expansions per logical step across attempts and
eight per run across roles; initial and repair exchanges consume zero.

Unsafe, unavailable, duplicate-only, byte-exhausted, and cap-exhausted requests
finish blocked with canonical denial evidence. Provider and response/audit
failures finish failed with canonical failure evidence. Source unavailability
is distinct from publication failure: unsafe artifact targets, immutable
collisions, or publication I/O return a clear error, make no later provider call
or terminal claim, and leave staged state for M1-04b2c. Focused GREEN passed 20
context-expansion tests, 11 live
context integration tests, 38 provider-runner regressions, and both cap unit
tests. Final verification passed `cargo fmt --all -- --check`, locked
all-target/all-feature Clippy with warnings denied, all 297 locked Rust
workspace tests, `corepack pnpm format:check`, and `git diff --check`.
M1-04b2b is complete and M1-04b2c is active.

## 2026-07-11 spec and quality fix | M1-04b2b ordering and terminal closure

Spec RED showed that a missing Development patch gate returned an error after a
durable `patch_proposed` response but left the writable step `Running`. A
trusted-request substitution regression initially mutated too early and was
replaced by a private unit observer at the exact boundary: the response record
is durable, then the trusted initial request becomes a symlink before context
reconstruction. Quality RED added a concurrent valid exchange suffix after an
ordinary state writer captured its intended ledger vector; the compare-and-save
API did not yet exist, so the regression failed to compile. Final quality
review then found the empty-vector branch still used an unlocked write. A
runner-level RED let a second cooperative writer append the first request after
the controller captured empty state; the delayed controller incorrectly
returned success and erased that request.

GREEN moves live classification behind typed response-audit publication. A
specialized internal response-record seam reads the verified canonical audit,
derives the exact outcome with the shared classifier, publishes the derived
record without accepting a caller outcome, and returns the classification only
after its reference is durable. Trusted immutable-read safety is now distinct
from repository request safety, so audit substitution can never produce
`context_denied`. Post-response interpretation and gating errors produce a
canonical failed step when evidence can still be written; the missing-gate
regression now records one failed Development outcome and remains terminal on a
second step call.

The stale ordinary-state race is closed narrowly for provider-vector
preservation. Every post-creation `LoopRunner` step-state write uses the
existing exchange lock and atomic writer, reloads and verifies the current
chain under lock, and requires the intended vector, including empty, to match
exactly before publication. A concurrent first request or later suffix makes
the older writer fail without changing `run.json`. General non-ledger state
coordination remains M1-10 scope. The compare helper remains crate-private; its
nonempty race proof is a private unit test and the empty-vector proof runs
through `LoopRunner`. Focused GREEN passed 20 context-expansion, 22
provider-exchange, 11 live-context, 38 provider-runner, and 24 state integration
tests plus the private audit-TOCTOU and nonempty compare unit regressions.
Final re-verification passed `cargo fmt --all -- --check`, locked
all-target/all-feature Clippy with warnings denied, all 300 locked Rust
workspace tests, `corepack pnpm format:check`, and `git diff --check`.

## 2026-07-11 implement | M1-04b2c context recovery and CLI integration

RED showed that resume returned to the legacy single-call path: a durable
context-retry request was replaced by a live attempt-two initial request, an
orphan response passed preparation, and rerun made an unaudited call without
preserving caps. Further crash-cut regressions covered staged initial and repair
responses, malformed-JSON repair requests, standalone and referenced expansion
tampering, response/request/record substitution, missing and reordered staged
suffixes, empty-ledger interruption, conventional prompts before an exchange
request, and state-head publication. Real CLI REDs covered default resume,
explicit rerun, repository changes, cap exhaustion, and all four durable
artifact classes.

GREEN adds a pre-provider reconciliation transaction under the provider
exchange lock. It validates the authoritative chain, computes one unique
linked staged-record suffix, verifies every bound file and the complete
prospective chain, rejects all remaining exchange-family files as orphans, and
atomically publishes the new vector once. Request phases retry the exact
audited ModelRequest; response phases are interpreted without another provider
call. Standalone expansions are never self-authenticated. The same state
machine recovers context retries and the one eligible JSON repair, closes
durable terminal responses, and retains the b2b outcome taxonomy.

Fresh initial requests now bind a closed metadata-only repository-context
authority beside the one content-bearing readable context. Recovery verifies
the readable bytes against path, source/included digest, byte, truncation,
limit, exclusion, and warning metadata. First and later resumed rounds
therefore use original accepted bytes and totals after live sources change.
Context-free roles such as OutputReview carry no authority and still prepare
for recovery; attempt-two replay still requires its rerun authorization.

Conventional prompt recovery accepts only the byte-identical exact next
attempt. Skips, stale prompts, unsafe files, or missing rerun authority fail
before an exchange write. Recovery validates every initial exchange against its
exact conventional prompt before publishing a staged suffix. Explicit rerun
publishes its immutable old-head authorization and reset state in one exchange-
locked transaction; a pre-publication failure is byte-identically retryable.
Live append and authoritative replay both verify the authorization. Every new
group uses the exact next durable attempt, including normal role advancement.
Terminal legacy needs-context runs remain inert until explicit rerun, while an
incomplete empty ledger starts audited attempt one. Two-per-step and eight-per-
run caps are counted from the entire immutable ledger across resume and rerun.

The CLI now accepts `loop resume --rerun-from <provider-step>`. Captured Ollama
regressions prove default resume sends the exact durable request, including old
accepted context after repository mutation. Request, response, expansion, and
record tampering fails before any provider call or run-tree mutation. Broader
inspect/revise recovery remains M1-09 scope. Focused GREEN passed 32 live
context, 22 exchange-contract, 38 provider-runner, 24 state, and 85 CLI
integration tests. M1-04b2c is complete and M1-05 is active.

## 2026-07-11 implement | M1-05a candidate workspace lifecycle contract

RED real-Git tests could not compile because no candidate lifecycle API
existed. The contract was split from provider/CLI integration so its identity
and cleanup boundary can be reviewed independently.

GREEN adds a closed typed CandidateWorkspaceState to LoopRun and its schema.
The state binds the canonical external path, source root, Git common directory,
repository identity digest, starting HEAD/tree, candidate HEAD/index tree,
full-index staged diff digest, applied patch digest, and active/cleaned
lifecycle. Rust validation closes cross-field invariants that JSON Schema
cannot express and explicit-null Option behavior remains aligned with schema.

Candidate creation uses a deterministic path outside the source checkout,
disables checkout hooks, rechecks the source identity after creation, rolls
back post-add failures, and crash-adopts only the exact registered clean
worktree. Physical validation rejects missing, symlinked, substituted,
wrong-repository, moved-HEAD/tree, unstaged, untracked, committed, or
digest-tampered state. Cleanup accepts LoopStatus, refuses active or unsafe
candidates, removes only the bound worktree, and is idempotent only from valid
retained cleaned evidence with no path or registration.

Focused GREEN passed 8 real temporary-Git lifecycle tests and all 33 seaf-core
tests. M1-05a remains incomplete until independent spec and quality approval;
M1-05b will wire provider context, indexed patch application, resume, candidate
patch evidence, and explicit CLI cleanup.

## 2026-07-11 spec and quality fix | M1-05a pre-apply and cleanup trust boundary

Review rejected the first lifecycle draft because registration, detached HEAD,
ignored untracked bytes, repository helpers, and cleanup authority were not all
closed. It also exposed a slice-boundary error: a caller-supplied applied patch
digest could not be trustworthy before M1-05b owns exact patch bytes, policy
evidence, and indexed application.

M1-05a is now explicitly pre-apply only. Candidate state has no applied-patch
field and must retain the starting HEAD/tree and empty staged diff. Creation
uses a detached no-checkout worktree, reads the exact index, discovers cached
filter attributes, disables every safely named filter driver, and materializes
raw bytes without hooks. All Git subprocesses remove repository/config/object
redirection variables and disable hooks, fsmonitor, system/global config, and
attribute injection. Validation requires exact registration and detached HEAD,
rejects staged/unstaged and ordinary or ignored untracked bytes, and compares
indexed objects in one sanitized cat-file batch, including executable-mode and
symlink semantics. Modes 100644, 100755, and 120000 are supported; gitlinks
fail closed.

Cleanup now loads the authoritative LoopRun under a candidate lock, rejects
pending/running runs, atomically publishes Cleaning before removal through the
existing provider-state publication lock order, verifies the exact physical
candidate, and publishes Cleaned afterward. A retry safely reconciles durable
Cleaning when the path and registration are both absent, including after the
source HEAD advances. Lock opens are no-follow with opened/path identity
checks, creation syncs the lock and parent, and the lock order is documented.

## 2026-07-11 spec and quality fix | M1-05a raw object compatibility

Follow-up review proved that checkout-index still applied built-in Git
transforms such as ident even after external filter drivers were neutralized.
RED expanded `$Id$` bytes and failed raw index verification. GREEN removes
checkout-index entirely: the detached worktree reads the exact index, requests
one object at a time from a single sanitized cat-file batch process, and streams
regular bytes directly to private candidate paths with exact executable-mode
parity. On Unix, raw symlink target bytes are created and compared without UTF-8
conversion; other platforms fail closed for symlinks. Parent creation rejects
symlinks and non-directories, regular I/O is bounded, and symlink targets are
capped at 4096 bytes. Ident, hostile filters/helpers, executable files, and a
non-UTF-8 symlink target have focused proof.

Every Git command now disables replace refs; regressions bind the original
commit, tree, and blob despite all three replacement types. Candidate creation
uses the candidate lock, concurrent creators converge by exact adoption, and a
failed worktree-add never deletes an unproven remnant. Unix authority root,
repository, and worktree directories are verified private 0700.

Cleanup publication now uses full canonical LoopRun compare-and-swap under the
provider lock. A concurrent state change before intent prevents removal, an
ordinary stale Active publisher cannot replace Cleaning, and the final
Cleaning-to-Cleaned transition succeeds only from the exact expected state.

Final quality review found that provider exchange recovery still compared only
the ledger vector before constructing a prospective run from caller authority.
A stale terminal Active run could therefore adopt a staged exchange suffix and
replace persisted Cleaning state. Reconciliation now requires the entire
persisted LoopRun to equal its verified authority before preflight or
publication. A regression stages a valid Research request, persists Cleaning,
and proves stale Active reconciliation leaves run.json byte-identical and the
suffix unadopted. The context-free recovery fixture now persists its exact
prepared authority before invoking the stricter seam.

## 2026-07-11 review and controller | M1-05a accepted

Fresh spec and quality reviews approved the final frozen lifecycle boundary
after verifying registration, detachedness, raw materialization, helper and
replace-ref isolation, private authority directories, full-state publication,
and interruption-safe cleanup. No provider, patch-application, or CLI behavior
entered this slice; those remain M1-05b.

The controller passed 23 candidate integration tests, 16 seaf-loop library
tests including cleanup and reconciliation CAS faults, 22 provider-exchange
tests, 38 provider-runner tests, 33 seaf-core tests, and the complete locked
Rust workspace. Workspace Clippy with warnings denied, Rust and Prettier
formatting, package lint, typecheck, 8 SDK tests, SDK build, and diff check all
passed. M1-05a is complete and M1-05b is active.

## 2026-07-12 implementation | M1-05b1 indexed candidate patch transaction

M1-05b is split into four reviewable boundaries. M1-05b1 closes the durable
candidate mutation contract; M1-05b2 is now active for provider start/resume,
followed by Development/OutputReview integration and explicit CLI cleanup.

LoopRun now has a backward-compatible execution mode: missing state defaults to
legacy proposal-only and cannot carry candidate authority, while explicit
isolated-candidate runs require it. Candidate patch state is a closed Applying
or Applied transaction. Its create-only canonical intent binds the exact
Development artifact, policy digest, changed paths, starting identity, planned
tree, and expected staged-diff artifact. Full-state compare-and-swap persists
Applying before the real candidate index changes and Applied only after exact
tree/diff verification and create-only observed evidence.

Application plans through a private index, applies to the real candidate index,
and raw-rematerializes only changed paths from exact index blobs. Configured
filters are neutralized, ident bytes remain raw, and add/delete/executable/
symlink transitions are preserved. Resume accepts pristine Applying, exact
staged Applying, or fully verified Applied replay. It rejects partial indexes,
unrelated drift, artifact tampering, and coherent rewritten intent/diff/index
state by recomputing the plan from authoritative Development evidence. Allowed
and RequiresHumanReview decisions materialize candidate-only regardless of
apply-request audit intent; Rejected and already-applied evidence never mutate.

Candidate artifacts use the shared atomic create-only publisher, including file
and parent-directory durability on fresh publication and exact retry. Private
planning indexes use unique names so crash orphans never block another attempt.
Genuine fault hooks prove stale pre-Applying CAS, durable Applying before index
mutation, exact materialized Applying before Applied publication, and post-
Applied replay without pre-created future evidence. Materialization requires
completed Development on a running run, handles exact directory/file
transitions, and refuses unrelated directory contents. A narrow decode
migration keeps pre-B1 M1-05a candidate runs usable while explicit legacy mode
still forbids candidate authority.

Focused candidate tests pass 31/31 with 6 candidate-specific unit/fault tests;
core passes 33/33, provider exchange 22/22, provider step runner 38/38, and the
complete locked Rust workspace passes.

## 2026-07-12 implementation | M1-05b2 provider candidate authority

Provider CLI startup now uses a typed two-stage isolated initialization path.
A minimal run directory first publishes a closed Provisioning LoopRun, then
provisions the exact persisted candidate and full-state-CAS advances it to
Active. Only after Active validation does the retry-safe synced scaffold appear;
the complete canonical ticket, policy, config, repository, and provider-ticket
snapshot set is preflighted and atomically create-only published before context,
provider preparation, or the semantic start log. Exact scaffold and snapshot
prefixes converge after interruption, while any collision causes zero later
publication. Planning failure before run.json removes only the exact empty
minimal directory so the same run ID remains retryable.

Resume compares live input digests before mutation, validates or provisions the
candidate before snapshot repair and provider reconciliation, and derives both
context and patch roots from the candidate. ProviderStepRunner independently
requires both roots to canonicalize to that candidate and authenticates every
durable or staged Initial request against the exact candidate before
reconciliation may publish; the first Initial retains its step-specific context
reconstruction role afterward. Initial context and every additive expansion
bind the repository digest, candidate-path digest, and starting HEAD/tree;
every predecessor must match. Missing exact snapshots repair, noncanonical
collisions fail before any new snapshot, and pre-B2 provider history without
candidate authority fails with start-new-run guidance.

Patch gating remains proposal-only. Apply intent is audited, but the only Git
operation is `git apply --check` in the candidate; a command spy proves its cwd
and that neither candidate nor source bytes/tree/status change. Dirty source-
only context is excluded, while NeedsContext reads and persists candidate
bytes. Fresh, incomplete, and terminal/rerun legacy ProviderStepRunner flows
fail closed; deterministic non-provider legacy LoopRunner behavior is unchanged.
Exact Applied candidate evidence may resume, but Applying resume and any
Applying/Applied rerun reset remain blocked until the next integration slice.

Real provisioning cuts cover before create, after create before CAS, after
Active publication, and stale CAS with exact remnant adoption. Candidate and
provider locks preserve candidate-before-provider order; a no-follow,
opened-identity-checked repository operation lock is keyed from canonical Git
common-directory bytes in a private shared candidate-authority namespace. It
therefore serializes add/adopt/remove operations even when distinct source
worktrees and repository digests share one Git worktree registry. Atomic 0700
directory creation closes the parallel first-creator permission race.

The historical provider integration suites required intentional legacy setup.
They were moved into cfg(test) unit modules and use a pub(crate), cfg(test)-only
harness, so no bypass exists in normal dependency builds. A separate public-
constructor integration target proves fresh legacy provider rejection, zero
provider calls, and failed-start workspace cleanup.

The complete locked Rust workspace passes: CLI 85, core 33, seaf-loop library
95, candidate 34, context expansion 22, provider exchange 22, state 28, plus
the focused provider candidate/isolation/staged-authority suites and all
remaining tests and doc-tests. Clippy with all targets/features and warnings
denied passes. Independent spec and quality re-reviews approved the final
frozen boundary after verifying every-Initial candidate authentication and
canonical Git-common-directory lock sharing across linked worktrees. The final
controller gate repeated the complete Rust workspace, Clippy, package lint,
typecheck, 8 SDK tests, SDK build, Rust/Prettier formatting, and diff check with
no failures. M1-05b2 is accepted, and M1-05b3 is active in the roadmap and
current-loop trackers.

## 2026-07-12 implementation | M1-05b3 Development and OutputReview integration

Completed isolated Development now publishes its response exchange, canonical
Development evidence, unique policy decision, and completed step state before
the B1 transaction may mutate the candidate. Candidate application must reload
as exact Applied before the semantic Development finish log or OutputReview.
Rejected, blocked, provider-failed, and candidate-application-failed paths leave
no review call; the application-fault case retains the already durable evidence
without falsely claiming the step finished.

A candidate-locked read-only verifier rechecks the full Development, policy,
intent, applied-evidence, candidate-tree, and staged-diff chain and returns a
closed review projection. OutputReview receives only that projection, the run
and input digests, and approved SpecCreation/SpecReview identities. It never
receives repository context, ticket text, proposal body, or an unverified live
diff. Resume safely migrates only the no-review-history pre-B3 state, recovers
real pristine/materialized Applying cuts, and verifies Applied read-only.

Every staged, durable, and fresh OutputReview Initial request is authenticated
as an exact full subject and envelope. Recovery and fresh publication validate
the prospective ledger inside the provider lock and bind the persisted run
model. The exported raw append refuses this record identity, leaving only the
crate-private authenticated path. Applied runs permit OutputReview-only rerun;
earlier reruns fail before ticket handling, scaffold, snapshot repair, provider
reconciliation, or logs. CLI coverage proves OutputReview attempt two preserves
attempt one and that a naturally blocked pre-Development Research rerun retains
the context cap.

The first independent review found forbidden-rerun mutation ordering and an
authoritative-model/locked-append gap. Re-review then found the public raw
append bypass. All findings returned to the implementer, received focused
regressions, and were independently approved after correction. A final
test-only compatibility adjustment preserved the historical in-crate legacy
harness without compiling a production bypass.

The controller's final gate passes the complete locked workspace: CLI 86, core
33, seaf-loop library 98, candidate integration 34, provider candidate 11,
provider exchange 22, state 28, all remaining integration/doc tests, and SDK 8.
Clippy with all targets/features and warnings denied, package lint, typecheck,
SDK build, Rust/Prettier formatting, and diff checks pass. M1-05b3 is accepted;
M1-05b4 explicit candidate cleanup is active.

## 2026-07-12 implementation | M1-05b4a authoritative run-directory binding

Reconnaissance split explicit cleanup into a safety prerequisite and the CLI
surface. A same-run-ID copy of an authoritative run directory could otherwise
target the original deterministic candidate and record only the copy as
Cleaned. Candidate schema version 2 now binds authority to the SHA-256 digest
of the canonical real absolute run-directory OS bytes. Runtime validation and
the public JSON Schema admit only closed versions 1 and 2; version 1 is
forensic-only and every operation directs users to a new run or manually
verified worktree recovery.

Provisioning/adoption, public creation, candidate patch application,
verification, and cleanup now reject copied, moved, symlinked, tampered, or
legacy authority before mutation. Public creation requires an already
persisted matching plan and delegates to provisioning. Original crash-remnant
adoption and Active/Cleaning/Cleaned recovery remain available only from the
bound original directory.

Both independent reviews found the same cleanup ordering flaw: after preflight,
the locked body reloaded candidate state and selected the repository-lock
namespace before revalidating the digest. A deterministic RED swapped both the
digest and Git common directory after candidate-lock acquisition and created
the malicious lock. The correction validates the reloaded authority before
repository-lock selection; the GREEN regression proves no malicious lock, run
publication, source change, or candidate change. Spec and quality re-review
approved the frozen result.

The controller's final gate passes the complete locked workspace: CLI 86, core
33, seaf-loop library 99, candidate integration 39, provider candidate 11,
provider exchange 22, state 28, all remaining integration/doc tests, and SDK 8.
Clippy with all targets/features and warnings denied, package lint, typecheck,
SDK build, Rust/Prettier formatting, and diff checks pass. M1-05b4a is accepted;
M1-05b4b explicit cleanup CLI is active.

## 2026-07-12 implementation | M1-05b4b explicit candidate cleanup CLI

Added `seaf loop cleanup --run-id ID [--runs-root PATH] [--json]` as the only
candidate cleanup trigger. The command validates the requested ID, minimally
opens the existing run, resolves the current repository with inherited Git
repository/config/object/index redirection removed, and delegates to the
authoritative Active-to-Cleaning-to-Cleaned transaction. Success emits a
dedicated seven-field report; exact Cleaned retries preserve identical run
bytes and output.

The first CLI RED was the absent cleanup subcommand. Real terminal-run coverage
then proved exact worktree and registration removal while preserving source
HEAD, status, staged/unstaged diffs, and tracked bytes. Boundary tests cover
active refusal, wrong repository, copied run, persisted run-ID mismatch,
inherited Git redirection, traversal, missing run, idempotence, help, and no
false JSON success. The normal-build isolated Development matrix now includes
provider timeout and proves no source/candidate change, patch transaction, or
OutputReview.

Independent review found four real safety gaps. Wrong-source and tampered
common-directory REDs showed cleanup could create the persistent repository
lock before rejecting static authority. A mismatched persisted run ID could
remove the candidate. Inherited `GIT_DIR`/`GIT_WORK_TREE` could impersonate the
source repository. The CLI also combined a locked candidate result with an
unlocked later run reread. Corrections prevalidate source/common/path authority
before repository-lock selection and revalidate under it; bind run ID before
and after candidate locking; sanitize destructive Git discovery; and return a
typed locked run/status/candidate outcome with no post-success reread. Focused
REDs failed the old paths, and both reviewers approved the final regressions.

Controller-focused verification passes seven cleanup CLI tests, eight cleanup
unit tests, six cleanup integration tests, and the isolated timeout boundary.
The final gate passes the complete locked workspace: CLI 94, core 33, loop
library 105, candidate integration 39, provider candidate 11, provider exchange
22, state 28, all remaining integration/doc tests, and SDK 8. Clippy with all
targets/features and warnings denied, package lint, typecheck, SDK build,
Rust/Prettier formatting, and diff checks pass. M1-05b4b and M1-05 are accepted;
M1-06 human approval is active.

## 2026-07-12 implementation | M1-06a stop before human review

M1-06 was split so the execution stop can be reviewed independently from the
human approval transaction. Isolated OutputReview now advances atomically to a
closed `awaiting_human_review` state with Testing current but still pending;
Testing and EvalReport do not execute or publish artifacts. The locked
workspace-aware publication path reloads the immutable terminal OutputReview
record, requires its canonical outcome to be `ApproveForTests`, and binds it to
the latest review attempt. Provider append/reconciliation, rerun, cleanup, and
ordinary state publication stay inert while waiting. Historical isolated
Testing/EvalReport prefixes without approval fail before ticket or provider
work, while exact pre-M1-06 Completed runs retain load/cleanup compatibility.

The first review round found schema duplicate-name parity, a misleading cleanup
lock assertion, unauthenticated barrier publication, late CLI preflight, and a
public-writer replacement path. Corrections added per-name schema rules, exact
private lock proof, authenticated ledger authority, pre-ticket rejection, and
barrier freezing. The second round demonstrated that an authenticated
`RequestChanges` response could be relabelled passed by a custom runner and
that public writers could mint a forged barrier. Exact REDs now reach the
`ApproveForTests` guard and prove both public writers reject the forged state
without changing bytes. Only the locked provider seam can create the barrier;
concurrent writer TOCTOU and hostile artifact replacement remain assigned to
M1-10 and M1-11.

Spec and quality re-reviews approved the frozen boundary. Controller-focused
verification passes core 34, provider-candidate 15, state 29, and CLI 95,
including the previously timing-sensitive descendant-pipe regression. M1-06a
is accepted and M1-06b exact human approval is active. The final full locked
workspace, all-target/all-feature Clippy with warnings denied, Rust/Prettier
formatting, package lint/typecheck, 8 SDK tests, SDK build, and diff check pass.

## 2026-07-12 implementation | M1-06b exact human approval

Added explicit `Approved` state and closed versioned `HumanApprovalEvidence`
inside LoopRun. The evidence binds the run, bounded reviewer identity and
timestamp, exact applied candidate-diff reference and target HEAD, unique typed
Development policy digest, current approving OutputReview artifact, and exact
initial/latest-terminal authenticated provider record references. Approved
retains Testing and EvalReport as pending and artifact-free. Runner, rerun,
cleanup, provider append/reconciliation/reset, ordinary publication, and public
state-writer paths remain inert; exact approval retry revalidates authority and
does not rewrite bytes.

`seaf loop approve` requires exact staged-diff and target-HEAD confirmations.
Awaiting and Approved run/status reports expose both values in JSON and human
output, so the public workflow no longer depends on internal run-file parsing.
Dirty tracked and untracked source bytes remain supported and unchanged while
the source HEAD must still match candidate authority.

Quality review found a deterministic stale-physical-authority window between
initial candidate/source verification and provider-lock acquisition. The final
transaction keeps candidate-to-provider lock order, compares the complete run,
then re-derives physical candidate/source state, confirmations, policy, review
artifact, provider bindings, and intended approval evidence inside the provider
lock before atomic publication. Its first concurrent tests depended on lock
polling and sleeps; re-review required deterministic proof. A private in-crate
hook now synchronously injects run-state, candidate-worktree, and source-HEAD
changes at the exact post-verification/pre-provider boundary. The run change
hits full CAS; both physical changes hit the inner validator; no public fault
hook or timing assumption remains.

Spec and quality approved the corrected frozen result. Controller verification
passes the deterministic three-case publication-race unit, all 17 provider
candidate tests, core 35, and CLI 96, including public confirmation discovery,
dirty-source preservation, substitutions, inert Approved state, and exact
retry. The final locked workspace, all-target/all-feature Clippy with warnings
denied, Rust/Prettier formatting, package lint/typecheck, 8 SDK tests, SDK
build, and diff check pass. M1-06 is accepted and M1-07 integrated
Testing/EvalReport is active.

## 2026-07-12 implementation | M1-07a reusable controlled eval engine

Extracted deny-unknown typed eval configuration and validation into `seaf-core`
and the shell-free controlled command planner/executor into `seaf-loop`.
Standalone `seaf eval run` remains the report and log owner and preserves valid
configuration flags, report semantics, exit behavior, and paths. The reusable
engine plans every check before execution, intersects eval and ticket
allowlists, confines cwd and candidate-relative executables to the canonical
execution root, clears the child environment, preserves trusted executable,
timeout, process-group, output-drain, cap, and redaction behavior, and returns
sanitized output for caller persistence.

Quality review found two audit-integrity gaps. Raw capture stopped at the
persisted byte cap before obvious-secret classification, exposing a truncated
token prefix. A bounded 26-byte classification lookahead now covers the longest
recognized prefix plus minimum suffix before sanitized output is truncated to
the exact configured cap. Distinct check names could also sanitize to one
replacing log path. CLI preflight now rejects exact duplicates, sanitized-name
collisions, and ASCII case-folded collisions before directory creation or any
command; table-driven regressions prove zero marker, report, or log-directory
side effects. A final review correction aligned the tracker and acceptance text
with these intentional fail-closed invalid cases.

Spec and quality re-reviews approve the corrected slice. Controller verification
passes CLI 98, core 37, loop 108, the 5-test shared eval engine, candidate 39,
provider candidate 17, provider exchange 22, state 29, and every remaining Rust
and doc-test suite. Strict all-target/all-feature Clippy, Rust and Prettier
formatting, package lint/typecheck, 8 SDK tests, SDK build, and diff checks pass.
The first pnpm launcher on `PATH` lacked its managed binary; the exact pinned
11.7.0 binary from the active Node installation ran the package gate. M1-07a is
accepted and M1-07b immutable eval configuration authority is active.

## 2026-07-12 implementation | M1-07b immutable eval configuration authority

New provider runs now require `ticket.eval.config` before run-directory,
candidate, or provider side effects. The portable repository-relative spelling
rejects empty, absolute, traversal, dot-segment, repeated/trailing separator,
backslash, colon/drive, control, symlink, missing, directory, non-UTF-8, and
malformed authority. Supported platforms open the exact file with no-follow,
bind pre/open/post file identity, and read only from the verified handle; other
platforms fail closed. The shared typed config is parsed once, serialized as
canonical JSON at `inputs/eval-config.json`, and bound through an optional
historical-compatible input digest that is mandatory at every current isolated
provider entrypoint.

Incomplete resume compares live typed authority and preflights the complete
intended snapshot set before candidate recovery, scaffold, reconciliation,
logs, or provider work. Direct isolated resume independently verifies persisted
canonical digests and typed eval bytes before recovery. Create-only repair now
accepts only a contiguous missing suffix; interior holes, collisions,
substitution, noncanonical bytes, forged generic JSON, and digest mismatch stay
byte-identical failures. Historical Approved runs without eval authority remain
readable and inert with start-new-run guidance.

Initial spec and quality review found four related gaps: interior snapshot holes
were silently filled, collision checks occurred after resume mutation, the
public snapshot seam accepted generic JSON instead of typed EvalConfig, and raw
path normalization plus an lstat/canonicalize/read race allowed ambiguous or
replaced authority. Correction REDs reproduced dot-path acceptance, interior
repair, later-file prefix gaps, and forged canonical JSON. Exact-prefix
preflight, the redundant direct-resume gate, shared typed validation, raw
portable checks, and deterministic no-follow replacement proof closed them.
Both re-reviews approve the result.

Controller verification passes CLI 102 plus the deterministic binary unit,
core 37, loop 109, state 31, candidate 39, provider candidate 17, provider
exchange 22, and every remaining Rust/doc-test suite. Strict all-target/all-
feature Clippy, Rust/Prettier formatting, package lint/typecheck, 8 SDK tests,
SDK build, and diff checks pass through the pinned pnpm 11.7.0 binary. M1-07b is
accepted and M1-07c Approved Testing/EvalReport is active.

## 2026-07-12 implementation | M1-07c1 evaluation evidence and terminal contracts

Added versioned, deny-unknown canonical Testing evidence binding the run,
ticket, goal, immutable eval config, exact approved candidate diff and starting
HEAD, approval and policy digests, ordered checks, unique log path/digest pairs,
aggregate result, and canonically encoded ordered timestamps. EvalReport checks
now optionally carry stdout/stderr digests and integrated reports optionally
carry typed loop evidence while historical standalone reports remain readable.
LoopRun now supports `eval_passed` and a closed approval-bound reported-failure
shape without making Approved runs executable.

A workspace-aware final-authority loader securely reads and digests Testing and
EvalReport artifacts, reconstructs the exact Approved predecessor, and validates
all cross-artifact identities, decisions, ordered checks, logs, aggregate result,
status, and terminal outcome. Direct Testing/EvalReport provider steps now fail
closed with zero model calls. Public writers cannot mint a final outcome from
Approved; passing outcomes cannot be replaced, rerun, reconciled, or cleaned;
reported failures permit only exact retry or ordered candidate cleanup.

Review exposed three authority classes that the initial implementation did not
close. Final state first referenced merely self-consistent artifact claims, so
publication now validates real workspace artifacts. The final transaction also
had to prove that its reconstructed Approved authority was the exact locked
current predecessor, not a substituted self-consistent bundle. Finally, current
final results could be replaced, crossed between pass and failure, or downgraded;
the in-lock relation now makes EvalPassed immutable and limits final Failed to
identity-preserving cleanup with canonical time ordering. Testing cannot start
before approval, and canonical decimal timestamps reject ambiguous encodings.

Both fresh spec and quality reviews approve the corrected slice with no
actionable findings. Controller verification passes all 480 workspace tests,
including new evidence, final-authority, substitution, exact-predecessor,
terminal-freeze, and cleanup-only regressions. Strict all-target/all-feature
Clippy, Rust and Prettier formatting, package lint/typecheck, 8 SDK tests, SDK
build, and diff checks pass through pinned pnpm 11.7.0. M1-07c1 is accepted and
M1-07c2 locked Approved evaluation execution is active.

## 2026-07-12 implementation | M1-07c2 locked Approved evaluation transaction

Exact Approved `loop resume` now enters a dedicated local controller without
requiring live ticket/config files or contacting the model provider. Under the
candidate lock it authenticates the approval, latest OutputReview exchange,
policy decision, source and candidate state, and canonical ticket/eval snapshots;
plans every check against both allowlists; and publishes a versioned create-only
intent bound to the exact Approved run and complete plan before the first
command. Any partial prior evaluation prefix refuses byte-identical replay until
M1-09 provides audited recovery.

The bounded engine executes only in the candidate and revalidates Approved,
candidate, source, input, cwd, and executable identity before every spawn.
Indexed create-only stdout/stderr logs are redacted and digest-bound. Canonical
Testing evidence and the integrated EvalReport bind the immutable config,
approved diff, human approval, policy decision, ordered checks, logs, and one
another. After physical revalidation, the controller retains the candidate lock,
takes the provider lock, and uses the M1-07c1 exact-predecessor relation to
publish `eval_passed` or an approval-bound reported failure. Failed exits,
spawn failures, and timeouts produce rejecting evidence; publication failures
cannot claim a terminal result. No promotion or model call occurs.

Initial review found three production blockers. Strict candidate verification
rejected Git-ignored Cargo `target/` output after a successful check, so a narrow
evaluation-only relation now permits ignored generated output while keeping
HEAD, index, staged diff, and tracked worktree exact; every approval, cleanup,
and future promotion caller stays strict. Lasting source-worktree mutation could
also publish success because only source HEAD/tree were checked. A canonical
source authority now preserves pre-existing dirty state and binds HEAD, staged
and tracked diffs, plus path/type/mode/content identity for ignored and
nonignored untracked entries before intent and at every later authority gate,
excluding only the bound run directory. Finally, planned cwd/executable paths
and authority reads had replacement windows; per-spawn identity/digest checks
and no-follow opened-handle reads with before/after path and parent identity
close them.

REDs reproduced a real candidate Cargo build stranded after success, lasting
ordinary and ignored source mutations, later-executable substitution, symlink
and regular-file replacement during verified reads, independent allowlist
denial, partial replay, config/intent/log/candidate/concurrent tamper, failed
commands, timeout, and publication collision. Both independent code re-reviews
approve the corrected behavior; the final quality note was formatting-only and
was resolved by Rust formatting. Controller documentation now describes the
`run -> status -> approve -> resume` flow, local-execution boundary, artifacts,
interruption behavior, and frozen pre-promotion result. M1-07 is accepted and
M1-08 promotion integrity is active.

Controller verification passes CLI 109, core 49, loop unit 114, candidate 39,
eval-engine 7, provider-candidate 25, provider-exchange 22, and every remaining
Rust and doc-test suite. Strict all-target/all-feature Clippy, Rust and Prettier
formatting, package lint/typecheck, 8 SDK tests, SDK build, and diff checks pass
through pinned pnpm 11.7.0.

## 2026-07-12 implementation | M1-08 promotion integrity

Added a closed versioned `promoted` state and `seaf loop promote`. Status exposes
the exact candidate diff, Testing/EvalReport references, policy digest,
EvalPassed run digest, and target HEAD required for a fresh bounded reviewer
confirmation. Promotion reloads and physically verifies the complete frozen
M1-07 authority and active candidate, requires the caller to be the exact source
repository, and rejects tracked, staged, untracked, ignored, stale-HEAD,
conflicting, failed, historical, cleaned, or substituted targets before source
mutation. Only the authoritative bound runtime directory may be ignored.

A canonical create-only promotion intent binds the exact EvalPassed predecessor,
reviewer, candidate, Testing/EvalReport, policy decision, target HEAD, and
monotonic timestamp before application. Under candidate then repository-
operation locking, source `git apply --check` and apply run with sanitized Git
authority and filter drivers neutralized from both current-target and evaluated-
candidate attribute views. The patch is applied unstaged and uncommitted. Raw
index/blob/worktree comparison verifies every regular file, executable mode,
and symlink without filters or replace refs. The provider-lock full-state CAS
rereads the intent and complete final authority before publishing immutable
Promoted evidence. A crash after apply is adopted only when the source contains
exactly the intended patch; the candidate remains frozen and present.

Initial review found four authority gaps. All-path Git status/diff could execute
or be normalized by unrelated clean filters; a staged rename pair could be
misparsed as the excluded runtime and allow mutation before rejection; intent
validation omitted run identity, canonical time, and nested-lock physical
rereads; and the temporary index was world-readable. Corrections replaced
filter-sensitive comparisons with direct indexed-blob checks, removed porcelain
rename parsing in favor of exact index-tree equality, validated and reread the
intent under repository and provider locks, and moved the index into a unique
0700 directory with a 0600 file and recursive cleanup. Final re-review found and
closed one adjacent seam: fresh apply/check now unions and neutralizes filter
drivers from both attribute views immediately before each command.

REDs cover wrong confirmations, dirty/staged/ignored/sibling-runtime state,
stale HEAD, wrong repository, artifact/candidate/log/config tamper, conflict,
concurrent CAS, intent-before-apply, real process-crash adoption, wrong/noncanonical
and lock-wait-substituted intent, staged rename, filter execution/normalization,
private-index permissions, public writer/provider/rerun/cleanup freeze, exact
retry, candidate retention, and absence of provider calls. Both independent
reviewers approve the corrected implementation. M1-08 is accepted and M1-09
audited recovery operations are active.

Controller verification passes CLI 119, core 50, loop unit 117, candidate 39,
provider-candidate 25, provider-exchange 22, and every remaining Rust and
doc-test suite. Strict all-target/all-feature Clippy, Rust and Prettier
formatting, package lint/typecheck, 8 SDK tests, SDK build, and diff checks pass
through pinned pnpm 11.7.0.

## 2026-07-12 implementation | M1-09b audited provider recovery

Added closed `latest_recovery` authority plus versioned create-only source-run
snapshots and `RecoveryAttemptV1`. Each recovery binds its sequential ID,
actor/reason/time, selected provider step and attempts, exact source run and
input/candidate/source-worktree state, prior provider and recovery heads, and
the expected pure-reset projection. `loop revise` takes the candidate then
provider lock, publishes this evidence and reset through full CAS, and performs
no provider call. Only exact `loop rerun --recovery <id>` may publish the first
request; once that request is durable, ordinary resume owns the established
provider and candidate crash-recovery seams.

Recovery preserves the complete provider ledger and every historical byte.
Role and Development policy artifacts are create-only and attempt-indexed after
attempt 1. The reset clears only selected/downstream current pointers and their
dependent policy, approval, and evaluation references. Historical legacy rerun
authorization remains readable, but the CLI and public runner writer now return
migration guidance before mutation and production cannot publish a new legacy
authorization. Blocked/failed pristine provider steps and exact Applied
OutputReview review states are eligible; active recovery, input/candidate/source
substitution, evaluation prefixes, lifecycle/final states, gaps, collisions,
ambiguous history, and exhausted attempts fail closed.

Review found and closed staged-request reconciliation, prompt-only retry,
historical-chain authorization, pending-retry physical validation, macOS
filename portability, source-checkout proof, Applying-resume composition, public
legacy-writer bypass, and fixed policy-artifact overwrite seams. A full earlier-
step replay proves downstream attempt progression while old ledger and artifact
bytes remain a prefix. Competing revisions publish exactly one actor/reason
winner. Independent specification and quality re-review approve the final slice
with no remaining blockers.

Controller verification passes the full workspace: CLI 133, core 51, loop unit
130, candidate 39, context expansion 22, policy 13, provider-candidate 22,
provider-exchange 22, state 32, and every remaining Rust integration and doc-test
suite. Strict all-target/all-feature Clippy, Rust and Prettier formatting,
package lint/typecheck, 8 SDK tests, SDK build, and diff checks pass through
pinned pnpm 11.7.0. M1-09b is accepted and M1-09c Approved-evaluation recovery
is active.
