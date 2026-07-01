# Current Contract

## Goal

Implement P2-005, the restartable loop workspace and state machine for Phase 2
local agent-loop runs.

## Success Criteria

- Create durable run workspaces with `run.json`, context manifest, prompt,
  response, artifact, and log directories/files.
- Persist run status after each state-machine step.
- Completed steps are resumable and not repeated unless a rerun-from seam is
  used later.
- Store every model request and response artifact.
- Keep the slice scoped to the P2-005 allowed files in
  `docs/phase-2-local-agent-loop.md`.
- Run spec-compliance and code-quality review before commit.
