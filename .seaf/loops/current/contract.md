# Current Contract

## Goal

Implement P2-010, EvalReport integration for local loop outcomes.

## Success Criteria

- Keep existing `seaf eval run` behavior backward compatible.
- Represent loop-level checks as existing `EvalCheck` objects.
- Emit a rejected EvalReport when the patch policy gate fails.
- Generate EvalReports with `patch_id = run_id`, `goal_id = ticket.goal_id`,
  `approve_for_human_review` on success, and `reject` on failure.
- Include checks named `schema_validation`, `patch_policy_gate`,
  `spec_review`, `output_review`, and configured command checks.
- Keep the slice scoped to the P2-010 allowed files in
  `docs/phase-2-local-agent-loop.md`.
- Run spec-compliance and code-quality review before commit.
