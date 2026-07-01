# Current Contract

## Goal

Implement P2-003, local Ollama provider support behind the model-provider
abstraction.

## Success Criteria

- Add Ollama support without making CI depend on a live Ollama server.
- Use default base URL `http://localhost:11434/api`.
- Use `/api/chat` with `stream: false`.
- Send structured response schemas through `format` when supplied.
- Use low temperature by default for structured local-loop steps.
- Surface actionable errors for Ollama not running, model missing, timeout, and
  non-JSON responses.
- Keep any live Ollama smoke check manual or explicitly skipped when Ollama is
  unavailable.
- Keep the slice scoped to the P2-003 allowed files in
  `docs/phase-2-local-agent-loop.md`.
- Run spec-compliance and code-quality review before commit.
