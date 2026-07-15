# M2-07e Developer Diff-Grounding Plan

## Evidence and scope

M2-07d produced the required real reviewer `request_changes` and authenticated
spec recovery. Development then stopped before human review. A retained
ephemeral diagnostic showed the exact cause: the Developer selected
`patch_proposed`, the correct file, and the correct final text, but its `patch`
contained only `---` and `+++` headers plus the replacement line. It omitted
both the `diff --git` header and the `@@` hunk header, so SEAF correctly rejected
it as an incomplete unified diff.

This remediation strengthens only the trusted Developer system prompt with a
small generic git-style unified-diff skeleton and explicit required components.
It does not relax patch parsing, synthesize a patch, retry the model, or add
fixture-specific paths or content to the product prompt.

## Success criteria

1. The Developer prompt states that `patch_proposed` requires a complete
   git-style unified diff in the `patch` field.
2. The prompt includes a generic skeleton containing `diff --git`, `---`, `+++`,
   and `@@` lines, and explicitly forbids prose or omitted hunk headers inside
   `patch`.
3. Other role prompts and all runtime patch validation remain unchanged.
4. A focused prompt-contract test is written first and observed RED against the
   old Developer prompt, then passes with the minimum prompt change.
5. Focused role/provider tests, formatting, locked Clippy, workspace tests, the
   packaged fake gate, and independent spec/quality review pass.
6. Only after those gates pass may one fresh live packaged Ollama acceptance be
   attempted. M2-07 remains pending unless the complete recovered flow publishes
   validated, sanitized evidence.

## Files and commit boundary

- Modify `crates/seaf-loop/tests/role_response.rs` first.
- Modify `crates/seaf-loop/src/role_response.rs` only after the focused test is
  RED.
- Do not modify fixture, harness, evidence, or roadmap files during remediation.
- Commit separately as `Ground Developer unified diff output`.
