# Progress

## Prior Baseline

- [x] Phase 2 complete through P2-012; see
      `docs/phase-2-local-agent-loop.md`.

## Phase 0 Production-Readiness Baseline

- [x] P3-001: Create post-Phase-2 loop contract and current tracker.
- [x] P3-002: Reconcile stale docs so implemented primitives are separated from
      missing live integration.
- [x] P3-003: Fix default policy drift for CI, eval, policy, updater, and
      signing change categories.
- [x] P3-004: Fix generated artifact hygiene for `.seaf/loops/runs` and default
      context exclusions.
- [ ] P3-005: Harden CI determinism with locked commands, toolchain policy,
      workflow permissions, timeouts, concurrency, and split lint environments.

## Next Acceptance Criteria

- Live provider-backed loop execution replaces deterministic-runner behavior for
  non-smoke runs.
- Shell command execution is sandboxed by ticket/eval allowlists, working
  directory, environment, timeout, output, and redaction controls.
- Loop evals require real policy evidence and fail closed on missing,
  placeholder, mismatched, malformed, or rejected decisions.
- Public Rust, TypeScript, and JSON Schema contracts have drift tests before SDK
  or production-readiness claims.
