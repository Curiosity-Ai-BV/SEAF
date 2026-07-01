# Ollama Gemma 4 on macOS

This setup is for local developer smoke checks. It is not the CI path for the
Phase 2 local loop.

## Install

```bash
brew install ollama
ollama pull gemma4:e2b-mlx
ollama pull gemma4:e4b-mlx
ollama serve
```

Run `ollama serve` in a long-lived terminal. The SEAF default Ollama API base
URL is `http://localhost:11434/api`.

## SEAF Model Check

In another terminal, from the repository root:

```bash
cargo run -p seaf-cli -- model check --provider ollama --model gemma4:e4b-mlx
```

A passing check means the local provider answered a structured request. If
Ollama is stopped, the model is missing, the response is not JSON, or the
request times out, the command exits nonzero with a provider-specific message.
Missing models should include an `ollama pull gemma4:e4b-mlx` hint.

## AgentBench-lite Smoke

```bash
cargo run -p seaf-cli -- loop bench --provider ollama --model gemma4:e4b-mlx --fixture examples/agent-bench-lite
```

This is a live local structured smoke request. It requires the model response
content to parse as JSON and include `ok == true` before it emits the benchmark
summary. Use the fake-provider benchmark for deterministic CI-safe verification:

```bash
cargo run -p seaf-cli -- loop bench --provider fake --fixture examples/agent-bench-lite --json
```

`seaf loop run` is different: it currently uses deterministic fake local-loop
execution through the CLI wiring. Ollama checks and the Ollama AgentBench-lite
smoke verify local model availability, but full live Ollama agent-loop
execution is not the CI path.
