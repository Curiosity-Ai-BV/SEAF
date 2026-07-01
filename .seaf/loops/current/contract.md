# Current Contract

## Goal

Implement P2-009, AgentBench-lite for repeatable local model loop evaluation.

## Success Criteria

- Add AgentBench-lite fixtures for deterministic fake-provider execution and
  local Ollama smoke execution.
- Produce a JSON summary with schema-valid rate, repair-success rate,
  patch-apply rate, eval-pass rate, forbidden violation count,
  eval-weakening accepted count, and median latency.
- Treat forbidden and eval-weakening accepted violations as zero-tolerance
  failures.
- Cover initial tickets for CLI health, validation tests, docs-only changes,
  forbidden CI change rejection, and eval-weakening rejection.
- Keep the slice scoped to the P2-009 allowed files in
  `docs/phase-2-local-agent-loop.md`.
- Run spec-compliance and code-quality review before commit.
