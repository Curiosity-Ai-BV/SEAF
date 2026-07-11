# Local Agent Loop

Current slice context: Phase 2 complete on branch
`codex/seaf-foundation-agent-loop`. This page documents the Phase 2 local loop
as implemented. CI uses fake-provider paths for repeatable automation; Ollama
commands are local live smoke checks.

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

No local-loop command commits, merges, signs a release, applies an update, or
turns model output into trusted evidence by itself.

## Demo Path

Run the commands from the repository root:

```bash
git switch codex/seaf-foundation-agent-loop
cargo run -p seaf-cli -- ticket validate examples/local-loop/tickets/add-health-command.yaml
cargo run -p seaf-cli -- loop run --ticket examples/local-loop/tickets/add-health-command.yaml --policy examples/adaptive-notes/seaf.policy.json --run-id p2-011-demo --allow-dirty --json
cargo run -p seaf-cli -- loop status --run-id p2-011-demo --json
cargo run -p seaf-cli -- loop bench --provider fake --fixture examples/agent-bench-lite --json
cargo run -p seaf-cli -- eval run examples/local-loop/seaf.evals.yaml --loop-run .seaf/loops/runs/p2-011-demo/run.json --ticket examples/local-loop/tickets/add-health-command.yaml --output .seaf/evals/p2-011-demo-eval-report.json --json
```

The demo uses `--allow-dirty` because documentation slices and agent worktrees
often already contain reviewable changes. For normal implementation work,
prefer a clean working tree and omit `--allow-dirty`; `loop run` refuses dirty
trees by default. If the fixed `p2-011-demo` run ID already exists, choose a
new run ID or inspect/resume the existing run instead of overwriting it.

After the commands finish, review:

- `.seaf/loops/runs/p2-011-demo/run.json`
- `.seaf/loops/runs/p2-011-demo/inputs/`
- `.seaf/loops/runs/p2-011-demo/context-manifest.json`
- `.seaf/loops/runs/p2-011-demo/log.md`
- `.seaf/loops/runs/p2-011-demo/prompts/`
- `.seaf/loops/runs/p2-011-demo/responses/`
- `.seaf/loops/runs/p2-011-demo/artifacts/`
- `.seaf/evals/p2-011-demo-eval-report.json`
- `.seaf/evals/logs/`

The eval command validates the loop run and ticket before command checks run.
Policy-gate evidence is evaluated when the final loop EvalReport is built;
missing, malformed, mismatched, or rejected policy evidence fails closed.

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

Before the first provider request, the run writes canonical effective
`ticket.json`, `policy.json`, and `config.json` snapshots under `inputs/`. The
three SHA-256 values in `run.json` digest those exact typed inputs. The effective
config snapshot records the winning policy path for explicit-policy and
root-policy fallback runs as well as config-backed runs.

## Failed-Run Recovery

First inspect persisted state:

```bash
cargo run -p seaf-cli -- loop status --run-id p2-011-demo --json
```

Use the reported `run_directory` and `next_action` fields to decide what to
open next. For a failed run, inspect `log.md`, the failed step response under
`responses/`, the matching prompt under `prompts/`, and any step artifact under
`artifacts/`.

Resume only after the blocker is understood:

```bash
cargo run -p seaf-cli -- loop resume --run-id p2-011-demo --json
```

`loop resume` validates the persisted `run.json` before scaffolding or mutating
a workspace. Invalid JSON, missing run files, invalid run IDs, and mismatched
run IDs fail closed.

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

## Pending Work

New runs now bind their effective inputs. The next production-use slice is
resume integrity: resuming a provider run must verify those persisted inputs
before rebuilding provider context or patch-gate state.
