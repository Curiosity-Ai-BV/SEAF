# Current Contract

## Goal

Implement P2-002, the model-provider abstraction and deterministic fake
provider for the Phase 2 local agent loop.

## Success Criteria

- Add a provider-neutral model request/response API.
- Add a deterministic fake provider that can script a sequence of responses.
- Provider errors can be serialized into loop artifacts.
- Tests can exercise the provider abstraction without network access.
- Keep the slice scoped to the P2-002 allowed files in
  `docs/phase-2-local-agent-loop.md`.
- Run spec-compliance and code-quality review before commit.
