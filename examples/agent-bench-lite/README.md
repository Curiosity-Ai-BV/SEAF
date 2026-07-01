# AgentBench-lite

AgentBench-lite is a deterministic local fixture for evaluating SEAF's local
agent loop behavior without contacting Ollama or executing arbitrary fixture
code.

The `fake` provider path reads ticket files from `tickets/` and expected result
files from `expected/`, then emits an aggregate benchmark summary. It is
CI-safe by design.

Run the deterministic benchmark:

```bash
cargo run -p seaf-cli -- loop bench --provider fake --fixture examples/agent-bench-lite --json
```

The `ollama` provider runs a live local smoke check before emitting the same
benchmark summary shape. It loads the fixture, calls Ollama with the supplied
model, and requires the model to return structured JSON with `ok: true`.

```bash
cargo run -p seaf-cli -- loop bench --provider ollama --model gemma4:e4b-mlx --fixture examples/agent-bench-lite
```

If Ollama is not running, the model is not installed, or the smoke response is
not positive, the command fails with an actionable provider or smoke-validation
message instead of reporting a false pass.
