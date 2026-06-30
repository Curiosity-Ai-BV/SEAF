# SEAF Architecture

SEAF is a controlled improvement framework, not unsafe self-modifying software. Production applications do not rewrite themselves directly. They emit goal-relevant signals, agents propose patches, eval gates validate those patches, and release capsules connect approved changes to provenance and rollback metadata.

## MVP Boundaries

- `seaf-core` owns shared domain models, validation, digests, and release capsule verification.
- `seaf-cli` owns developer commands such as project initialization, goal validation, eval execution, and release verification.
- `seaf-local-runtime` owns local event ingestion, persistence, privacy filtering, and signal summarization.
- `@seaf/sdk` owns application instrumentation for events, metrics, and feedback.
- `specs/` owns the public JSON schemas for cross-language contracts.
- `docs/agent-loop.md` owns the implementation harness used by coding agents.

## Safety Invariants

- Every change traces to a goal.
- Agents operate inside explicit policies.
- Evaluation runs before release metadata is prepared.
- Release capsules bind source, goal, eval report, artifact digest, rollout policy, and rollback metadata.
- Human review remains the default for sensitive changes.
