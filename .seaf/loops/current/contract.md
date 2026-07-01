# Current Contract

## Goal

Implement P2-007, patch parser and deterministic policy gate for Phase 2 loop
runs.

## Success Criteria

- Parse unified diff paths from model-generated patches.
- Forbidden patches never apply.
- Bad patches leave the working tree unchanged.
- Allowed patches apply only when the caller explicitly asks to apply them.
- Without explicit apply, patches are only written as artifacts.
- Run `git apply --check` before applying any patch.
- Block or escalate forbidden path, eval/CI/policy/dependency/updater/signing,
  auth/billing, binary patch, and path traversal changes according to policy.
- Emit a durable `PolicyDecision` artifact.
- Keep the slice scoped to the P2-007 allowed files in
  `docs/phase-2-local-agent-loop.md`.
- Run spec-compliance and code-quality review before commit.
