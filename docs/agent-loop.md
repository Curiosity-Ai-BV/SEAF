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

## Loop

```text
gather -> reason -> act -> verify -> commit/merge -> repeat
```

If the harness becomes heavier than the work, delete or simplify the harness first.
