# Local Agent Loop

Current slice context: P2-011 on branch `codex/seaf-foundation-agent-loop`.
This page documents the Phase 2 local loop as it exists now. P2-012 CI
hardening is still pending, so use the fake-provider paths for repeatable
automation and treat Ollama commands as local smoke checks.

The local loop is disk-backed and review-first. Model output is untrusted
working material. Schema validation, the deterministic policy gate, configured
tests, eval reports, and human review remain authoritative.

## What Stays Local

- Ticket files, loop runs, prompts, raw responses, artifacts, eval logs, and
  benchmark summaries are written under the local checkout.
- Ollama requests go to the local Ollama API at `http://localhost:11434/api`
  unless a different base URL is passed.
- `seaf loop run` currently uses deterministic fake local-loop execution
  through the CLI wiring. Passing `--provider ollama` is metadata for the run;
  it is not the full live Ollama agent-loop execution path.
- CI-safe commands use `--provider fake` and do not require Ollama, network
  access, or installed local models.

No local-loop command commits, merges, signs a release, applies an update, or
turns model output into trusted evidence by itself.

## Demo Path

Run the commands from the repository root:

```bash
git switch codex/seaf-foundation-agent-loop
cargo run -p seaf-cli -- ticket validate examples/local-loop/tickets/add-health-command.yaml
cargo run -p seaf-cli -- loop run --ticket examples/local-loop/tickets/add-health-command.yaml --run-id p2-011-demo --allow-dirty --json
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

## Pending Work

P2-012 still needs CI hardening. Until then, use fake-provider commands for
automation and keep live Ollama checks as local developer smoke only.
