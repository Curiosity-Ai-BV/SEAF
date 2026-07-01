# Current Contract

## Goal

Implement P2-012, CI hardening for the local agent-loop guardrails.

## Success Criteria

- Keep CI independent of Ollama and live local models.
- Add focused fake-loop/AgentBench-lite and forbidden-patch guardrail coverage
  to CI using existing commands or tests.
- Validate generated examples or schema fixtures in CI without broad rewrites.
- Preserve the existing Cargo, Rust format, Clippy, pnpm format, lint,
  typecheck, test, and build checks where they exist.
- Keep the slice scoped to the P2-012 allowed files in
  `docs/phase-2-local-agent-loop.md`.
- Run spec-compliance and code-quality review before commit.
