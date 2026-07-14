# Agent Loop

This repo follows a disk-backed agent loop for long-running work.

## Roles

- Planner: converts goals into small implementation slices and writes success criteria.
- Implementer: changes code for one slice only.
- Spec evaluator: checks the diff against the slice contract.
- Quality evaluator: checks maintainability, tests, and codebase fit.
- Commit/merge agent: stages, commits, and merges only after checks pass.

Implementers do not self-approve their own work. The commit/merge agent does not edit code.

## Files

- `.seaf/loops/current/contract.md`: the current slice contract.
- `.seaf/loops/current/progress.md`: restartable state and checklist.
- `.seaf/loops/current/log.md`: append-only trace entries.

The loop should be recoverable by reading those three files plus the git diff.

## Phase 2 Specs

The next local-agent-loop implementation phase is tracked in
[Phase 2 Local Agent Loop](phase-2-local-agent-loop.md). Development slices
should not start until the ticket spec has been independently reviewed.

## Loop

```text
gather -> reason -> act -> verify -> commit/merge -> repeat
```

If the harness becomes heavier than the work, delete or simplify the harness first.

## Current Command Chain

```text
seaf init -> seaf doctor --provider fake -> seaf loop run --provider fake
  -> seaf loop status/inspect -> seaf loop approve -> seaf loop resume
  -> seaf loop status -> seaf loop promote -> commit/merge role
```

The chain uses the installed CLI. `loop approve` binds the exact candidate diff
and target HEAD; `loop promote` separately binds that candidate, the final
EvalReport, and a fresh target HEAD. Interrupted incomplete evaluation uses
`loop revise --from-step testing --eval-recovery invalidate` followed by exact
`loop rerun --recovery <id>` and never replays the partial attempt in place.

The commit/merge role must verify the working tree, staged files, inspected run
authority, and EvalReport before staging or committing. The packaged external
gate additionally proves deterministic rejection and cleanup leave the source
repository unchanged.
