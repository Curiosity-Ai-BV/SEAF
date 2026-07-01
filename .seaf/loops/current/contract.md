# Current Contract

## Goal

Implement P2-004, the safe local context packer for Phase 2 local agent-loop
prompts.

## Success Criteria

- Gather bounded context from ticket relevant files.
- Exclude secrets, signing material, generated folders, dependency folders, and
  forbidden paths.
- Enforce max bytes per file and total context bytes.
- Include file digests and warnings for traceability.
- Write `context-manifest.json` to a run directory.
- Keep the slice scoped to the P2-004 allowed files in
  `docs/phase-2-local-agent-loop.md`.
- Run spec-compliance and code-quality review before commit.
