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

## CLI MVP

```bash
cargo run -p seaf-cli -- init --path /tmp/seaf-demo
cargo run -p seaf-cli -- goal validate examples/adaptive-notes/adaptive.yaml
cargo run -p seaf-cli -- policy validate examples/adaptive-notes/seaf.policy.json
cargo run -p seaf-cli -- task brief \
  --goal examples/adaptive-notes/adaptive.yaml \
  --policy examples/adaptive-notes/seaf.policy.json
cargo run -p seaf-cli -- eval run examples/adaptive-notes/seaf.evals.yaml \
  --goal-id reduce_time_to_first_note \
  --patch-id patch_local \
  --json
cargo run -p seaf-cli -- release prepare \
  --app-id dev.seaf.adaptive-notes \
  --version 0.1.0 \
  --source-commit abc123 \
  --artifact examples/adaptive-notes/events/note-created.json \
  --eval-report .seaf/evals/eval-report.json \
  --rollback-plan rollback/0.0.9
cargo run -p seaf-cli -- release verify examples/adaptive-notes/release-capsule.json
```

## SDK MVP

```ts
import { createSeafClient } from "@seaf/sdk";

const seaf = createSeafClient({ source: "adaptive-notes" });

await seaf.event("note.created", { source: "empty_state_button" });
await seaf.metric("startup.p95_ms", 842);
await seaf.feedback({
  surface: "empty_state",
  sentiment: "confused",
  message: "I did not realize I could start typing.",
});
```

The local runtime accepts the same event envelope through its Rust ingestion API and persists it in SQLite before producing aggregated signals.

## Agent Loop

This repository uses a disk-backed implementation loop:

- `docs/agent-loop.md` defines planner, implementer, evaluator, and commit/merge roles.
- `.seaf/loops/current/contract.md` records the active success criteria.
- `.seaf/loops/current/progress.md` records restartable progress.
- `.seaf/loops/current/log.md` is append-only trace context for debugging the loop.

### Supervised local evaluation

Provider-backed runs stop before executing model-modified code. Review the
candidate digest and target HEAD from `loop status`, approve those exact values,
then resume once to run the immutable ticket/eval checks locally in the
candidate:

```bash
cargo run -p seaf-cli -- loop run --ticket <ticket.yaml> --run-id <run-id> --json
cargo run -p seaf-cli -- loop status --run-id <run-id> --json
cargo run -p seaf-cli -- loop inspect --run-id <run-id> --json
cargo run -p seaf-cli -- loop approve --run-id <run-id> \
  --reviewer <reviewer> \
  --confirm-candidate-diff <digest-from-status> \
  --confirm-target-head <head-from-status> \
  --json
cargo run -p seaf-cli -- loop resume --run-id <run-id> --json
cargo run -p seaf-cli -- loop status --run-id <run-id> --json
cargo run -p seaf-cli -- loop promote --run-id <run-id> \
  --reviewer <reviewer> \
  --confirm-candidate-diff <digest-from-status> \
  --confirm-eval-report <eval-report-digest-from-status> \
  --confirm-target-head <head-from-status> \
  --json
```

Blocked or failed provider steps use an audited two-command recovery. `revise`
records the operator, reason, exact source state, and next attempt without
calling the provider; `rerun` consumes that exact authorization:

```bash
cargo run -p seaf-cli -- loop revise --run-id <run-id> \
  --from-step <provider-step> --actor <operator> --reason <reason> --json
cargo run -p seaf-cli -- loop rerun --run-id <run-id> \
  --recovery <recovery-id> --ticket <ticket.yaml> --json
```

Use the same explicit `--config` or `--policy` authority as the original run.
Changing authoritative inputs, provider/model, repository, or candidate requires
a new run. The former `loop resume --rerun-from` path is retired.

Approved resume uses the persisted canonical ticket and eval snapshots, not
live files, and makes no model-provider call. Before any check it publishes a
create-only execution intent; it then records indexed redacted logs, canonical
Testing evidence, and a bound EvalReport. A passing run becomes `eval_passed`;
a failed check becomes an approval-bound reported failure. An interrupted
evaluation attempt will not replay commands until M1-09c adds audited adoption
and invalidation.

Human approval authorizes local command execution under the developer account.
SEAF detects lasting source/candidate drift but is not an OS sandbox against
malicious same-user commands. Promotion requires a second fresh confirmation
and a completely clean target (apart from the bound runtime directory). It
durably records intent, applies the exact evaluated patch to the original
checkout, and leaves it unstaged and uncommitted for review. Crash retry adopts
only that exact patch; it does not delete the frozen candidate or contact a
model provider.
