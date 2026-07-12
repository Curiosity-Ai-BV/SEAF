# Local Loop Example

This fixture exercises the supervised local loop. It is intentionally small so
future agents can validate the command chain without relying on a live model
for evaluation.

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
cargo run -p seaf-cli -- loop run --ticket examples/local-loop/tickets/add-health-command.yaml --policy examples/adaptive-notes/seaf.policy.json --run-id local-loop-demo --allow-dirty --json
cargo run -p seaf-cli -- loop status --run-id local-loop-demo --json
cargo run -p seaf-cli -- loop approve --run-id local-loop-demo --reviewer reviewer@example.invalid --confirm-candidate-diff <digest-from-status> --confirm-target-head <head-from-status> --json
cargo run -p seaf-cli -- loop resume --run-id local-loop-demo --json
cargo run -p seaf-cli -- loop bench --provider fake --fixture examples/agent-bench-lite --json
```

`loop run` stops at `awaiting_human_review`; it does not execute the candidate.
Copy the exact digest and target HEAD from `loop status` into `loop approve`.
The Approved `loop resume` uses only the persisted ticket/eval snapshots and
makes no model call. It writes a create-only execution intent, indexed redacted
check logs, `artifacts/07-testing.json`, and
`artifacts/08-eval-report.json`, then records `eval_passed` or an approval-bound
reported failure. If an attempt is interrupted after intent, resume refuses to
replay it until audited recovery lands in M1-09.

Review all generated evidence under `.seaf/loops/runs/local-loop-demo/`. If the
run ID already exists, choose a new one or inspect its current state. Human
approval authorizes local execution under your account; SEAF is not an OS
sandbox. This flow does not promote the candidate into the original checkout.

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

Use fake-provider commands for CI-safe checks. The Ollama benchmark command is a
small live smoke check; `loop run --provider ollama --model <model>` executes the
full role sequence and uses the same resolved project policy gate as fake runs.
