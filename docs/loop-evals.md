# Loop Evals

Loop evals bind a persisted `LoopRun` and its `TicketSpec` into SEAF's existing
`EvalReport` chain.

## CI-Safe Commands

Validate the example ticket:

```bash
cargo run -p seaf-cli -- ticket validate examples/local-loop/tickets/add-health-command.yaml
```

Run the deterministic benchmark:

```bash
cargo run -p seaf-cli -- loop bench --provider fake --fixture examples/agent-bench-lite --json
```

Run a deterministic local loop:

```bash
cargo run -p seaf-cli -- loop run --ticket examples/local-loop/tickets/add-health-command.yaml --policy examples/adaptive-notes/seaf.policy.json --run-id local-eval-demo --allow-dirty --json
```

Generate an eval report from the loop artifacts:

```bash
cargo run -p seaf-cli -- eval run examples/local-loop/seaf.evals.yaml --loop-run .seaf/loops/runs/local-eval-demo/run.json --ticket examples/local-loop/tickets/add-health-command.yaml --output .seaf/evals/local-eval-demo-report.json --json
```

Generated eval artifacts are written to `.seaf/evals/`, with command stdout and
stderr logs under `.seaf/evals/logs/`.

## Closed Validation

`seaf eval run --loop-run <run.json> --ticket <ticket.yaml>` validates the loop
run and ticket before writing logs, writing an output report, or running command
checks. Policy-gate evidence is evaluated when the final loop EvalReport is
built; the report fails closed when policy evidence is missing, malformed, bound
to the wrong run, or rejected by the deterministic gate.

Loop eval reports include loop-level checks such as:

- `schema_validation`
- `patch_policy_gate`
- `spec_review`
- `output_review`
- configured command checks from the eval YAML

A successful loop report uses the loop `run_id` as `patch_id`, the ticket
`goal_id` as `goal_id`, and `approve_for_human_review` as the decision. A
failing gate or command check produces a rejected report.

## Benchmark Meaning

AgentBench-lite reports aggregate local-loop metrics:

- schema-valid rate
- repair-success rate
- patch-apply rate
- eval-pass rate
- forbidden violation count
- eval-weakening accepted count
- median latency

`forbidden_violation_count` and `eval_weakening_accepted_count` are
zero-tolerance. Nonzero values make the benchmark fail even when JSON output is
requested.

The fake provider path is deterministic and CI-safe. The Ollama path performs a
live local structured smoke request and requires `ok == true`; use it for local
model readiness, not as the required CI signal.
