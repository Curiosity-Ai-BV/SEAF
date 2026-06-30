# Threat Model

SEAF assumes agents can make mistakes and that telemetry, logs, dependencies, and plugins can be hostile inputs.

## MVP Risks

- Agent modifies sensitive code such as updater, signing, auth, billing, CI, or eval definitions.
- Agent bypasses tests by weakening checks instead of fixing behavior.
- Telemetry or feedback prompt-injects an agent task.
- Raw private payloads leave the local runtime.
- Release metadata omits provenance, rollback, or digest verification.
- Update verification accepts stale, downgraded, or tampered artifacts.
- Plugins gain filesystem or network access beyond declared capabilities.
- Dependency changes introduce supply-chain risk without human review.

## Required Controls

- Policy files must represent forbidden paths and review-required change types.
- Eval reports must fail closed when required checks fail.
- Release capsules must verify artifact and eval report digests.
- Raw private events must not be uploaded by default.
- Signing keys and update root policy must remain outside agent-readable contexts.

See `docs/security/forbidden-shortcuts.md` for the current shortcut ban list.
