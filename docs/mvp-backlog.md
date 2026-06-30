# MVP Backlog

The first MVP should prove this local deterministic chain:

```text
GoalSpec -> Local Signal -> Agent Task Brief -> Patch -> EvalReport -> ReleaseCapsule -> Verified Update Metadata
```

## Implemented Slices

1. Monorepo foundation with Rust and TypeScript workspaces.
2. Goal, policy, eval report, release capsule, event, and signal contracts.
3. CLI validation, project initialization, task brief generation, eval execution, and release capsule preparation/verification.
4. TypeScript SDK event emission and Rust local runtime event ingestion.

## Next Slices

1. Patch artifact interface.
   - Accept a unified diff or patch directory.
   - Validate with `git apply --check`.
   - Record patch digest before application.
2. Patch risk classifier.
   - Detect forbidden paths, dependency files, migrations, auth/payment/update paths, and eval/CI edits.
   - Require human review when policy demands it.
3. Controlled patch application.
   - Apply patches only in an explicit branch or worktree mode.
   - Refuse dirty unrelated baselines.
4. Integration/commit command.
   - Commit only after evals pass, capsule digests verify, patch digest matches, and no unrelated files are dirty.
   - Merge only with an explicit target branch.
5. Adaptive Notes demo shell.
   - Emit SDK events.
   - Create a first-note workflow.
   - Demonstrate local signal generation and eval/report/release flow.

## Commit/Merge Role

Implementation agents may generate patches only. The commit/merge role is the only role allowed to stage, commit, or merge, and only after independent checks pass.
