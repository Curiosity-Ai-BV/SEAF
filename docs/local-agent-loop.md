# Local Agent Loop

This page documents the current Milestone 1 loop implementation as exercised
from the SEAF Cargo source workspace. CI uses fake-provider paths for repeatable
automation; Ollama commands are local live smoke checks. Packaged installation,
generic initialization, and external-project adoption remain Milestone 2 gates.

The local loop is disk-backed and review-first. Model output is untrusted
working material. Schema validation, the deterministic policy gate, configured
tests, eval reports, and human review remain authoritative.

## What Stays Local

- Ticket files, loop runs, prompts, raw responses, artifacts, eval logs, and
  benchmark summaries are written under the local checkout.
- Ollama requests go to the local Ollama API at `http://localhost:11434/api`
  unless a different base URL is passed.
- `seaf loop run --provider fake` uses deterministic provider responses, while
  `--provider ollama` sends the role requests to the configured Ollama API.
- CI-safe commands use `--provider fake` and do not require Ollama, network
  access, or installed local models.

No local-loop command commits, merges, signs a release, or deploys. Only an
explicit `loop promote` after exact human approval and passing bound evaluation
may modify the source checkout; it applies the authorized patch unstaged and
uncommitted for manual review.

## Demo Path

Use a clean checkout or dedicated clean worktree and run the commands from its
repository root. Confirm `git status --short` has no output before starting the
complete approve/evaluate/promote path:

```bash
cargo run -p seaf-cli -- ticket validate examples/local-loop/tickets/add-health-command.yaml
cargo run -p seaf-cli -- loop run --ticket examples/local-loop/tickets/add-health-command.yaml --policy examples/adaptive-notes/seaf.policy.json --run-id milestone-one-demo --json
cargo run -p seaf-cli -- loop status --run-id milestone-one-demo --json
cargo run -p seaf-cli -- loop inspect --run-id milestone-one-demo --json
cargo run -p seaf-cli -- loop approve --run-id milestone-one-demo \
  --reviewer reviewer@example.invalid \
  --confirm-candidate-diff <digest-from-status> \
  --confirm-target-head <head-from-status> --json
cargo run -p seaf-cli -- loop resume --run-id milestone-one-demo --json
cargo run -p seaf-cli -- loop status --run-id milestone-one-demo --json
cargo run -p seaf-cli -- loop promote --run-id milestone-one-demo \
  --reviewer reviewer@example.invalid \
  --confirm-candidate-diff <digest-from-status> \
  --confirm-eval-report <eval-report-digest-from-status> \
  --confirm-target-head <head-from-status> --json
cargo run -p seaf-cli -- loop bench --provider fake --fixture examples/agent-bench-lite --json
```

The complete path intentionally omits `--allow-dirty`: `loop run` refuses a
dirty tree by default, and promotion requires the exact clean target authority.
If you deliberately use `--allow-dirty` for a non-promotion demonstration,
limit that run to `loop status` and `loop inspect`; promotion will reject it. If
the fixed `milestone-one-demo` run ID already exists, choose a new run ID or
inspect/resume the existing run instead of overwriting it.

After the commands finish, review:

- `.seaf/loops/runs/milestone-one-demo/run.json`
- `.seaf/loops/runs/milestone-one-demo/inputs/`
- `.seaf/loops/runs/milestone-one-demo/context-manifest.json`
- `.seaf/loops/runs/milestone-one-demo/log.md`
- `.seaf/loops/runs/milestone-one-demo/prompts/`
- `.seaf/loops/runs/milestone-one-demo/responses/`
- `.seaf/loops/runs/milestone-one-demo/artifacts/`

Approved resume validates the loop, exact human approval, candidate, and
persisted ticket/eval authority before command checks run. Policy-gate evidence
is evaluated when the final Loop EvalReport is built; missing, malformed,
mismatched, or rejected evidence fails closed.

## Project Policy Authority

New provider runs fail closed unless they can resolve one policy authority.
The deterministic precedence is:

1. an explicit `--policy` file;
2. the policy named by explicit `--config` or Git-root `seaf.config.json`;
3. Git-root `seaf.policy.json`.

`seaf.config.json` is the only discovered project configuration filename. Its
`policy_path` is relative to the config directory. Config and policy files are
canonicalized and must remain inside the Git root, including through symlinks.
An explicit config is always loaded and validated even when `--policy`
overrides its policy choice.

Before the first provider request, the run writes canonical effective ticket,
policy, config, repository, eval-config, and provider-ticket snapshots. Their
bound digests and repository identity bind the canonical source worktree, Git
common directory, candidate, policy, and controlled checks.

Development consumes the exact approved spec, publishes policy-gated evidence,
and applies only to the isolated candidate. OutputReview receives the exact
verified Applied candidate subject. Testing cannot start until a human approves
the candidate digest and target HEAD. Evaluation uses the persisted ticket/eval
snapshots in the candidate, and promotion requires another fresh confirmation
before the exact evaluated patch can reach the source checkout.

## Failed-Run Recovery

First inspect persisted state:

```bash
cargo run -p seaf-cli -- loop status --run-id milestone-one-demo --json
```

Use the reported `run_directory` and `next_action` fields to decide what to
open next. For a failed run, inspect `log.md`, the failed step response under
`responses/`, the matching prompt under `prompts/`, and any step artifact under
`artifacts/`.

For a blocked or unapproved failed provider step, publish an audited revision
only after the blocker is understood. This records the operator and reason and
performs no provider call:

```bash
cargo run -p seaf-cli -- loop revise --run-id milestone-one-demo \
  --from-step <provider-step> --actor <operator> --reason <reason> --json
cargo run -p seaf-cli -- loop rerun --run-id milestone-one-demo \
  --recovery <recovery-id> \
  --ticket examples/local-loop/tickets/add-health-command.yaml \
  --policy examples/adaptive-notes/seaf.policy.json --json
```

Only `loop rerun --recovery <id>` may consume a pending revision before its
first durable provider request. After that request exists, ordinary `loop
resume` continues the exact attempt. The former `loop resume --rerun-from`
path is retired. Recovery currently covers provider steps through OutputReview;
complete evaluation prefixes use
`loop revise --from-step testing --eval-recovery adopt` and make zero provider
or command calls. Incomplete prefixes must use `--eval-recovery invalidate`
followed by `loop rerun --recovery <id>`; they are never replayed in place.

Recovery validates the persisted `run.json` before scaffolding or mutating a
workspace. Invalid JSON, missing run files, invalid run IDs, mismatched run IDs,
ambiguous attempt history, and changed candidate authority fail closed. The
rerun requires the original ticket and the same effective config/policy
authority used by `loop run`. Pass matching `--config` or `--policy` arguments
when the run used explicit authority; discovered Git-root authority needs no
extra flag.

Recovery canonically verifies ticket, policy, config, repository, eval-config,
and provider-ticket snapshots plus their bound `LoopRun` digests and repository
identity before appending the run log, contacting a provider, rebuilding
context, or applying a patch. Missing, noncanonical, tampered, changed, unsafe,
or cross-repository inputs are rejected with guidance to supply the matching
inputs or start a new run.

## Local Model Smoke

Mac Ollama setup is documented in
[`docs/local-models/ollama-gemma4.md`](local-models/ollama-gemma4.md).

The CI-safe benchmark is:

```bash
cargo run -p seaf-cli -- loop bench --provider fake --fixture examples/agent-bench-lite --json
```

The live local Ollama smoke is:

```bash
cargo run -p seaf-cli -- loop bench --provider ollama --model gemma4:e4b-mlx --fixture examples/agent-bench-lite
```

The Ollama smoke loads the AgentBench-lite fixture, sends one structured local
request, and requires response content with `ok == true`. If the model is not
installed, the provider error includes an `ollama pull` hint.

Verified locally on 2026-07-01 with `gemma4:e4b-mlx` installed:

```bash
cargo run -p seaf-cli -- model check --provider ollama --model gemma4:e4b-mlx --json
cargo run -p seaf-cli -- loop bench --provider ollama --model gemma4:e4b-mlx --fixture examples/agent-bench-lite --json
```

The model check passed through the local Ollama API, and the AgentBench-lite
Ollama smoke returned `schema_valid_rate = 1.0`, `eval_pass_rate = 1.0`,
`forbidden_violation_count = 0`, and `eval_weakening_accepted_count = 0`.

## Acceptance Boundary

Run `./scripts/test-milestone-one-acceptance.sh` for the focused source-workspace
gate. Its 14 exact tests cover the complete authoritative input snapshot set,
early and Development role dataflow, candidate Applying/Applied recovery,
OutputReview durable-response adoption, approval, separate zero-command
evaluation adoption and crash-cut convergence, invalidation with immutable
attempt history, rejecting reports, exact approved-patch promotion crash
adoption, and persisted clean v1 Testing compatibility. It does not establish
packaged or external-project readiness. This source-workspace gate is currently
supported on macOS and Linux only: the workflow executes it on Ubuntu, while
the current local verification evidence is from macOS. Windows and generic
platform support are not claimed.
