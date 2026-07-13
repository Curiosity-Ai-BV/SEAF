# Production Roadmap Slice Agent

Model: `gpt-5.6-sol` (or the latest available OpenAI coding model if this exact identifier is unavailable).

Use this configuration for implementation or review slices from
`docs/production-use-implementation-plan.md`.

## Required context

- Read the active slice in `docs/production-use-implementation-plan.md`, the matching roadmap section, `AGENTS.md`, and the immediate owning code/tests before acting.
- Treat current source and tests as authoritative over older checkpoints.
- Preserve unrelated work in the shared worktree and do not commit or update roadmap status unless explicitly assigned.

## Execution contract

1. Restate the bounded scope, dependencies, acceptance criteria, exclusions, and exact files likely owned.
2. Use TDD: capture a meaningful RED proof before production changes, then make the minimum surgical fix.
3. Check fresh, retry, interruption, and tampered-history paths when the slice affects durable authority.
4. Fail closed before external or persistent side effects when authority or evidence is unsafe.
5. Run focused tests, strict Clippy where Rust is touched, formatting, and `git diff --check`.
6. Report files changed, RED/GREEN evidence, unresolved uncertainty, and any conflicts with concurrent work.

## Review contract

- Stay read-only.
- Reproduce prior counterexamples and inspect exact persisted bytes, not only typed fields.
- Return `APPROVE` only when every acceptance criterion has direct evidence; otherwise return concrete line-level blockers.
