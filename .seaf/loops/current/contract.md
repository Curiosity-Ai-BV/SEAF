# Current Contract

## Goal

Phase 0 starts the production-readiness baseline after the completed Phase 2
local agent-loop plan. The goal is to remove stale tracker signals and close the
cheap guardrail gaps that must be fixed before SEAF allows live model-driven
patch proposals.

## Assumptions

- Phase 2 is complete through P2-012; `docs/phase-2-local-agent-loop.md` remains
  the historical source for that work.
- `docs/production-readiness-roadmap.md` is the roadmap for Phase 0 and later
  production-readiness work.
- Production-ready does not mean automatic merge, deployment, signing, cloud
  agent execution, or bypassing human review.
- P3 tickets use the roadmap numbering. P3-001 is docs/tracker work only.

## Success Criteria

- P3-001 through P3-005 are tracked in `.seaf/loops/current/progress.md` without
  claiming incomplete work is done.
- Phase 0 closes the baseline gaps called out in the roadmap: stale docs,
  default policy category drift, generated artifact hygiene, and CI
  determinism.
- The next implementation contract must treat live provider-backed execution,
  command sandboxing, real policy evidence, and schema drift tests as acceptance
  criteria before broader production-readiness claims.
- Verification commands and skipped checks are reported explicitly.

## Scope Boundaries

In scope for Phase 0:

- Current loop tracker reset after Phase 2.
- Backlog wording that distinguishes implemented primitives from missing live
  integration.
- Default/example policy category alignment.
- Generated loop artifact ignore and context-exclusion hygiene.
- Deterministic CI hardening.

Out of scope for Phase 0:

- Implementing live provider-backed role execution.
- Applying model-generated patches without the existing human and policy gates.
- Production signing, verified updates, cloud execution, or deployment
  automation.
- Weakening schemas, evals, policy gates, CI checks, or forbidden-path coverage.

## Verification Expectations

- P3-001 uses documentation hygiene only:
  `pnpm exec prettier --check docs/production-readiness-roadmap.md .seaf/loops/current/contract.md .seaf/loops/current/progress.md .seaf/loops/current/log.md`
  and `git diff --check`.
- Later P3 tickets require focused tests for the touched guardrail plus the
  relevant workspace checks from the roadmap exit gate.
- Any skipped or failing verification must be logged and reported before the
  controller considers a commit.
