# Current Contract

## Goal

Implement P2-011, documentation and Mac setup guide for the local agent loop.

## Success Criteria

- Create or update the local agent loop docs listed in the P2-011 allowed files.
- Include one complete demo path from ticket validation through loop run,
  benchmark/eval report generation, and artifact review.
- Explain what remains local-only and why model output is untrusted.
- Explain failed-run recovery, including `loop status`, `loop resume`, and
  artifact inspection.
- Include Mac setup commands for `brew install ollama`, pulling
  `gemma4:e2b-mlx` and `gemma4:e4b-mlx`, `ollama serve`, and model checks.
- Keep the slice scoped to the P2-011 allowed files in
  `docs/phase-2-local-agent-loop.md`.
- Run spec-compliance and code-quality review before commit.
