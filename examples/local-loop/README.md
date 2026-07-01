# Local Loop Example

This fixture supports P2-011 on branch `codex/seaf-foundation-agent-loop`.
It is intentionally small so future agents can validate the local-loop command
chain without relying on a live model.

## Files

- `tickets/add-health-command.yaml`: ready ticket for the demo loop.
- `tickets/invalid-empty-ticket.yaml`: invalid ticket fixture for validation
  failures.
- `seaf.evals.yaml`: required eval config used by the loop eval demo.
- `runs/valid-loop-run.json`: standalone valid `LoopRun` fixture.

## Complete Demo

From the repository root:

```bash
cargo run -p seaf-cli -- ticket validate examples/local-loop/tickets/add-health-command.yaml
cargo run -p seaf-cli -- loop run --ticket examples/local-loop/tickets/add-health-command.yaml --run-id local-loop-demo --allow-dirty --json
cargo run -p seaf-cli -- loop status --run-id local-loop-demo --json
cargo run -p seaf-cli -- loop bench --provider fake --fixture examples/agent-bench-lite --json
cargo run -p seaf-cli -- eval run examples/local-loop/seaf.evals.yaml --loop-run .seaf/loops/runs/local-loop-demo/run.json --ticket examples/local-loop/tickets/add-health-command.yaml --output .seaf/evals/local-loop-demo-report.json --json
```

Review generated artifacts under `.seaf/loops/runs/local-loop-demo/` and the
eval report at `.seaf/evals/local-loop-demo-report.json`. If `local-loop-demo`
already exists, choose a new run ID or inspect/resume the existing run.

## Recovery

Check status:

```bash
cargo run -p seaf-cli -- loop status --run-id local-loop-demo --json
```

Inspect `log.md`, `run.json`, the failed step response under `responses/`, and
the matching artifact under `artifacts/`. Resume after the blocker is clear:

```bash
cargo run -p seaf-cli -- loop resume --run-id local-loop-demo --json
```

Model output in these artifacts is not authoritative. Schema validation, policy
evidence, command checks, eval reports, and human review decide whether the run
can proceed.

## Local Model Smoke

Ollama setup is documented in `docs/local-models/ollama-gemma4.md`. The live
smoke path is:

```bash
cargo run -p seaf-cli -- loop bench --provider ollama --model gemma4:e4b-mlx --fixture examples/agent-bench-lite
```

Use fake-provider commands for CI-safe checks. P2-012 CI hardening is still
pending.
