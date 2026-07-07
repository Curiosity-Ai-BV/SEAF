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
5. Phase 2 local-loop primitives: ticket/run contracts, provider abstraction,
   context packing, role response schemas, patch parser, deterministic policy
   gate, loop CLI, AgentBench-lite, EvalReport integration, docs, and CI
   guardrails.

## Remaining Integration

Phase 2 implemented the parser, risk classification, `git apply --check`,
policy-decision artifact, and gated apply primitives. The remaining work is to
wire those primitives into live, provider-backed loop execution.

1. Live role execution integration.
   - Run loop steps through `ModelProvider` and structured role outputs instead
     of the deterministic runner for non-smoke paths.
   - Feed bounded context manifests into prompts and persist provider
     requests/responses.
2. Real patch evidence integration.
   - Route developer patch output through the implemented parser and policy gate.
   - Persist real patch digests, changed paths, decisions, review requirements,
     and apply status.
   - Limit synthetic empty-patch policy evidence to explicit smoke paths.
3. Controlled application and command checks.
   - Enforce ticket autonomy, clean-worktree requirements, and opt-in patch application.
   - Sandbox eval commands by allowlist, working directory, environment,
     timeout, output limits, and redaction.
4. Demo and product integration.
   - Promote Adaptive Notes from example data to a runnable local demo after the
     live loop path exists.
   - Keep commit, merge, signing, and release actions under explicit human review.

## Commit/Merge Role

Implementation agents may generate patches only. The commit/merge role is the only role allowed to stage, commit, or merge, and only after independent checks pass.
