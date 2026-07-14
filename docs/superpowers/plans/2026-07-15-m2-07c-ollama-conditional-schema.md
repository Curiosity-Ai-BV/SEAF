# M2-07c Ollama Conditional-Schema Plan

## Evidence and scope

The post-M2-07b live acceptance still failed safely at Research. A separate
diagnostic fixture run retained ephemeral state long enough to identify the
exact mismatch without publishing raw content: Research was valid, but Analysis
returned `status: "passed"` together with an invalid `context_request`. Ollama's
server continued to report incomplete conversion of SEAF's JSON schema.

The current agent and developer response schemas express status-dependent
fields with `if`/`then`/`else`. A direct local Ollama diagnostic proved that
closed `oneOf` object branches enforce both supported cases: a `passed` branch
omitted `context_request`, and a `needs_context` branch required and populated
it. This remediation changes only the schema representation; Rust parsing and
runtime validation remain authoritative and unchanged.

## Success criteria

1. Researcher, Analyzer, and Spec Writer schemas use closed `oneOf` branches:
   `passed`/`blocked` cannot contain `context_request`, while `needs_context`
   requires it.
2. The Developer schema uses closed branches for `patch_proposed`, `blocked`,
   and `needs_context`. `patch_proposed` requires `patch`; only
   `needs_context` requires `context_request`.
3. Existing field shapes and context-request constraints are preserved. No
   parser relaxation, schema-invalid response repair, or provider retry is
   added.
4. Tests are written first and observed RED against the old conditional schema.
   They validate the schema/runtime parity for every status branch.
5. Focused role/model tests, formatting, locked Clippy, workspace tests, the
   packaged fake gate, and independent spec/quality review pass.
6. Only after those gates pass may one fresh live packaged Ollama acceptance be
   attempted. M2-07 remains pending unless that run publishes validated,
   sanitized evidence.

## Files and commit boundary

- Modify `crates/seaf-loop/tests/role_response.rs` first.
- Modify `crates/seaf-loop/src/role_response.rs` only after the focused test is
  RED.
- Do not modify M2-07 evidence or roadmap files during remediation.
- Commit the defect separately as `Make role schemas grammar compatible`.
