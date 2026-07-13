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
./scripts/test-milestone-one-acceptance.sh
./scripts/test-package-readiness.sh
pnpm install
pnpm build
pnpm typecheck
```

## CLI MVP

### Install the private CLI from a checkout

SEAF requires the latest stable Rust toolchain. From a SEAF checkout, install
the private CLI package and verify its exact identity:

```bash
rustup update stable
cargo install --locked --path crates/seaf-cli
seaf --version
# seaf 0.1.0
```

Do **not** run `cargo install seaf-cli`: the package with that name on crates.io
is an unrelated project. All SEAF workspace packages have registry publication
disabled. Tagged SEAF binaries are planned but are not available yet.

Current support is limited to macOS and Linux; see the
[supported-platform policy](https://github.com/Curiosity-Ai-BV/SEAF/blob/main/docs/supported-platforms.md).

### Bootstrap an existing project

Run the installed CLI from the project you want SEAF to initialize:

```bash
mkdir -p /tmp/seaf-demo
git -C /tmp/seaf-demo init
cd /tmp/seaf-demo
seaf init
seaf ticket validate seaf.ticket.yaml
git add seaf.config.json seaf.policy.json seaf.evals.yaml seaf.ticket.yaml .seaf/.gitignore
git -c user.name="SEAF Demo" -c user.email="demo@seaf.invalid" \
  commit -m "Initialize SEAF"
seaf doctor --provider fake
seaf loop run \
  --ticket seaf.ticket.yaml --provider fake --run-id first-seaf-run --json
```

The generic initializer detects `Cargo.toml` and `package.json` and writes only
editable project configuration, policy, eval, starter-ticket, and state-ignore
files. It never writes provider configuration: `--provider fake` or
`--provider ollama` remains the explicit CLI authority for each new run. Use
`--template adaptive-notes` only when you intentionally want the specialized
example files. Before starting a loop, run the
[project doctor](#diagnose-project-readiness).

### Diagnose project readiness

From the initialized project's Git worktree, use the installed CLI to plan the
same inputs, candidate workspace, and eval commands that a loop would use:

```bash
seaf doctor --provider fake
seaf doctor --provider fake --json
```

Doctor reports eight ordered checks for Git, project inputs, the ticket,
candidate-workspace planning, eval configuration/executables, and the provider.
It never creates loop or candidate state and plans eval commands without
executing them. The fake provider is fully local and makes no provider call.

Ollama validation is offline by default and therefore reports provider
readiness as blocked. Authorize the single bounded live health request
explicitly:

```bash
seaf doctor \
  --provider ollama --model qwen2.5-coder:7b
seaf doctor \
  --provider ollama --model qwen2.5-coder:7b --live-provider --timeout-ms 5000
```

By default doctor discovers `seaf.ticket.yaml` at the Git root. `--ticket`,
when supplied, follows `loop run` and loads its caller-relative path directly,
including an external or symlinked ticket. `--config` and `--policy` remain
repository-contained and use the same precedence as `loop run`. Live Ollama
doctor requests accept only `localhost` or numeric IP addresses,
share one absolute deadline across connect/write/read work, and cap the raw
response at 1 MiB. Exit status is 0 only when ready, 1 for a complete non-ready
report, and 2 for invalid command usage.

### Adaptive Notes example

```bash
seaf init --path /tmp/seaf-demo --template adaptive-notes
seaf goal validate examples/adaptive-notes/adaptive.yaml
seaf policy validate examples/adaptive-notes/seaf.policy.json
seaf task brief \
  --goal examples/adaptive-notes/adaptive.yaml \
  --policy examples/adaptive-notes/seaf.policy.json
seaf eval run examples/adaptive-notes/seaf.evals.yaml \
  --goal-id reduce_time_to_first_note \
  --patch-id patch_local \
  --json
seaf release prepare \
  --app-id dev.seaf.adaptive-notes \
  --version 0.1.0 \
  --source-commit abc123 \
  --artifact examples/adaptive-notes/events/note-created.json \
  --eval-report .seaf/evals/eval-report.json \
  --rollback-plan rollback/0.0.9
seaf release verify examples/adaptive-notes/release-capsule.json
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
a failed check becomes an approval-bound reported failure. A complete
interrupted evaluation prefix is adopted without provider or command calls:

```bash
cargo run -p seaf-cli -- loop revise --run-id <run-id> \
  --from-step testing --eval-recovery adopt \
  --actor <operator> --reason <reason> --json
```

An incomplete prefix is never replayed in place. Invalidate it, then run the
new recovery-bound indexed attempt:

```bash
cargo run -p seaf-cli -- loop revise --run-id <run-id> \
  --from-step testing --eval-recovery invalidate \
  --actor <operator> --reason <reason> --json
cargo run -p seaf-cli -- loop rerun --run-id <run-id> \
  --recovery <recovery-id> --json
```

Human approval authorizes local command execution under the developer account.
SEAF detects lasting source/candidate drift but is not an OS sandbox against
malicious same-user commands. Promotion requires a second fresh confirmation
and a completely clean target (apart from the bound runtime directory). It
durably records intent, applies the exact evaluated patch to the original
checkout, and leaves it unstaged and uncommitted for review. Crash retry adopts
only that exact patch; it does not delete the frozen candidate or contact a
model provider.

Milestone 1 verifies the complete loop when SEAF runs from this Cargo source
workspace. The package-readiness gate separately verifies an extracted CLI
package outside the source tree through version, information, initialization,
and fake-provider doctor checks. The packaged external golden path remains
Milestone 2 work. The supported loop platforms are macOS and Linux: CI executes
on Ubuntu, and current local verification is on macOS. Windows and untested
architectures are not claimed.
