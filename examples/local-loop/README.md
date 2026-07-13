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

Use a clean checkout or dedicated clean worktree. Confirm `git status --short`
has no output, then run the complete sequence from the repository root:

```bash
cargo run -p seaf-cli -- ticket validate examples/local-loop/tickets/add-health-command.yaml
cargo run -p seaf-cli -- loop run --ticket examples/local-loop/tickets/add-health-command.yaml --policy examples/adaptive-notes/seaf.policy.json --run-id local-loop-demo --json
cargo run -p seaf-cli -- loop status --run-id local-loop-demo --json
cargo run -p seaf-cli -- loop approve --run-id local-loop-demo --reviewer reviewer@example.invalid --confirm-candidate-diff <digest-from-status> --confirm-target-head <head-from-status> --json
cargo run -p seaf-cli -- loop resume --run-id local-loop-demo --json
cargo run -p seaf-cli -- loop status --run-id local-loop-demo --json
cargo run -p seaf-cli -- loop promote --run-id local-loop-demo --reviewer reviewer@example.invalid --confirm-candidate-diff <digest-from-status> --confirm-eval-report <eval-report-digest-from-status> --confirm-target-head <head-from-status> --json
cargo run -p seaf-cli -- loop bench --provider fake --fixture examples/agent-bench-lite --json
```

The complete sequence intentionally omits `--allow-dirty`. If you deliberately
use `--allow-dirty` for a non-promotion demonstration, limit that run to
`loop run`, `loop status`, and `loop inspect`; promotion rejects an initially
dirty target.

`loop run` stops at `awaiting_human_review`; it does not execute the candidate.
Copy the exact digest and target HEAD from `loop status` into `loop approve`.
The Approved `loop resume` uses only the persisted ticket/eval snapshots and
makes no model call. It writes
`artifacts/07-testing.attempt-001.execution-intent.json`, indexed redacted check
logs, `artifacts/07-testing.attempt-001.json`, and
`artifacts/08-eval-report.attempt-001.json`, then records `eval_passed` or an
approval-bound reported failure. Historical fixed-path v1 evaluation evidence
remains readable. A complete interrupted evaluation prefix can be adopted with
zero provider or command calls. An incomplete prefix is never replayed in place;
it must be invalidated and continued as a new recovery-bound indexed attempt.

Review all generated evidence under `.seaf/loops/runs/local-loop-demo/`. If the
run ID already exists, choose a new one or inspect its current state. Human
approval authorizes local execution under your account; SEAF is not an OS
sandbox. After `eval_passed`, the second `loop status` exposes the EvalReport
digest required by `loop promote`. Promotion accepts only the exact frozen
candidate and a clean confirmed target HEAD, records intent before mutation,
and applies the patch unstaged without committing. An exact retry after a crash
adopts only those already-applied bytes. The candidate remains available for
review; no model call, merge, push, or deploy occurs.

## Recovery

Check status:

```bash
cargo run -p seaf-cli -- loop status --run-id local-loop-demo --json
```

Inspect `log.md`, `run.json`, the failed step response under `responses/`, and
the matching artifact under `artifacts/`. For a blocked or failed provider
step, record an audited revision and consume it with an exact rerun:

```bash
cargo run -p seaf-cli -- loop revise --run-id local-loop-demo \
  --from-step <provider-step> --actor <operator> --reason <reason> --json
cargo run -p seaf-cli -- loop rerun --run-id local-loop-demo \
  --recovery <recovery-id> \
  --ticket examples/local-loop/tickets/add-health-command.yaml \
  --policy examples/adaptive-notes/seaf.policy.json --json
```

For interrupted Testing, use
`loop revise --from-step testing --eval-recovery adopt` only for a complete
exact prefix. Use `--eval-recovery invalidate` for an incomplete prefix, then
`loop rerun --recovery <id>` to own the new indexed attempt. Ordinary resume
never replays incomplete evaluation commands in place.

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

This fixture is Milestone 1 source-workspace evidence. It does not prove a
packaged install, generic initialization, or adoption in an external project.
