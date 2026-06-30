# Self-Evolving Application Framework

SEAF is a goal-directed framework for building applications that can improve through controlled AI-assisted development loops.

SEAF connects:

- goal specs
- telemetry and feedback
- local and cloud coding agents
- evals and policy gates
- release provenance
- signed updates
- staged rollout and rollback

SEAF does not let production apps rewrite themselves directly. All changes flow through controlled patches, evals, provenance, signing, and verified updates.

## MVP Flow

```text
GoalSpec -> Local Signal -> Agent Task -> Patch -> EvalReport -> ReleaseCapsule -> Verified Update
```

## Workspace

```text
crates/
  seaf-core/           Shared Rust domain models and validation.
  seaf-cli/            Developer CLI.
  seaf-local-runtime/  Local observation/runtime MVP.

packages/
  seaf-sdk-js/         TypeScript instrumentation SDK.

specs/                 JSON schemas for public data contracts.
examples/              Valid and invalid example configs.
docs/                  Architecture, threat model, agent loop, and roadmap.
```

## Development

```bash
cargo test
pnpm install
pnpm build
pnpm typecheck
```

## Agent Loop

This repository uses a disk-backed implementation loop:

- `docs/agent-loop.md` defines planner, implementer, evaluator, and commit/merge roles.
- `.seaf/loops/current/contract.md` records the active success criteria.
- `.seaf/loops/current/progress.md` records restartable progress.
- `.seaf/loops/current/log.md` is append-only trace context for debugging the loop.
