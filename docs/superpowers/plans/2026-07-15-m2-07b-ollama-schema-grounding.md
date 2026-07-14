# M2-07b Ollama Schema Grounding Plan

## Evidence and scope

Two fresh packaged `gemma4:latest` attempts failed safely with no published
evidence: the first reached Development after the required reviewer recovery,
then returned `invalid_response`; the bounded retry returned
`invalid_response` at Research. Ollama remained reachable and returned HTTP
200. Its local server log reported incomplete JSON-schema conversion because
SEAF role schemas contain unsupported regex syntax.

Ollama's structured-output guidance recommends both passing the JSON schema in
`format` and including the schema text in the prompt. SEAF currently does only
the former. This remediation is limited to grounding Ollama structured requests
with the exact schema text. It does not weaken the schema, add response retries,
repair schema-invalid role output, or change non-Ollama providers.

## Success criteria

1. An Ollama request with `response_schema` keeps the exact schema in `format`
   and appends its compact JSON representation to the trusted system message.
2. An unstructured Ollama request keeps its existing system message unchanged.
3. Focused tests are written first and observed failing for the missing schema
   grounding, then pass with the minimum implementation.
4. Rust formatting, locked Clippy, workspace tests, the packaged fake gate, and
   independent spec/quality review pass in a separate defect commit.
5. Only after those gates pass may one fresh live packaged Ollama acceptance be
   attempted. M2-07 remains pending unless that run publishes validated,
   sanitized evidence.

## Files and commit boundary

- Modify `crates/seaf-models/tests/ollama.rs` first.
- Modify `crates/seaf-models/src/ollama.rs` only after the focused test is RED.
- Do not modify the M2-07 evidence or roadmap during remediation.
- Commit the defect separately as `Ground Ollama structured responses`.

