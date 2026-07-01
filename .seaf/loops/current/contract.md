# Current Contract

## Goal

Implement P2-006, local agent role prompts and structured response schemas for
Phase 2 loop runs.

## Success Criteria

- Add roles for Researcher, Analyzer, Spec Writer, Spec Reviewer, Developer, and
  Output Reviewer.
- Each role has a structured schema and focused valid/invalid tests.
- Markdown-only responses are rejected.
- Invalid JSON has one repair-attempt seam and then fails closed.
- Developer responses keep unified diff content only in the `patch` field.
- Reviewer responses expose blocking and non-blocking issue arrays.
- Keep the slice scoped to the P2-006 allowed files in
  `docs/phase-2-local-agent-loop.md`.
- Run spec-compliance and code-quality review before commit.
