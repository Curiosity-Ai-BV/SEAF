# Local Agent Boundaries

SEAF assumes model output is untrusted. The local loop can draft, summarize,
and propose changes, but deterministic controls decide what can proceed.

## Authoritative Gates

- Ticket and loop schemas fail closed on malformed or unknown fields.
- Context packing excludes configured secrets and generated folders.
- Role responses must be valid structured JSON for the expected role.
- Developer patches are parsed before policy review.
- The deterministic policy gate blocks forbidden paths and escalates
  review-required change types.
- Eval reports fail when required command checks fail.
- Human review remains required before applying, committing, merging, signing,
  or releasing.

Do not treat a model's claim that tests passed, a patch is safe, or an eval is
acceptable as evidence. The command output and persisted artifacts are the
evidence.

## Local-Only Boundary

The Phase 2 local loop writes local artifacts under `.seaf/loops/runs/` and
`.seaf/evals/`. Ollama smoke checks call a local Ollama server. Fake-provider
commands avoid live models and are the CI-safe path.

Sensitive code and policy surfaces remain guarded. Dependency, lockfile, CI,
eval, updater, auth, billing, and signing changes require human review. Signing
keys and production update roots must stay outside local agent workspaces and
agent-readable context.

## Artifact Review Checklist

Before trusting a run for follow-up work, review:

- `run.json` for status, current step, policy decisions, and eval report path.
- `context-manifest.json` for included files and digests.
- `log.md` for step order and failures.
- `prompts/` and `responses/` for prompt-injection or malformed model output.
- `artifacts/` for proposed specs, reviews, patches, and eval notes.
- `.seaf/evals/<report>.json` and `.seaf/evals/logs/` for deterministic check
  results.

If any artifact is missing, mismatched, malformed, or inconsistent with the
ticket, stop and regenerate or repair the run before continuing.
