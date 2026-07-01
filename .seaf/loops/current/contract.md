# Current Contract

## Goal

Implement P2-008, CLI commands for local model, ticket, and loop operations.

## Success Criteria

- Add `ticket validate`, `loop run`, `loop status`, `loop resume`, and
  `loop smoke` command coverage without duplicating core loop behavior in CLI.
- Keep existing `model check` behavior working with JSON output.
- Return nonzero on validation failures.
- Provide JSON output for automation and human-readable next-action summaries.
- Make `loop run` refuse dirty working trees unless `--allow-dirty` is
  provided.
- Keep the slice scoped to the P2-008 allowed files in
  `docs/phase-2-local-agent-loop.md`.
- Run spec-compliance and code-quality review before commit.
