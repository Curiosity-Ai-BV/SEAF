# Preview Compatibility Handoff

This is the named handoff from the source-workspace Milestone 1 work into
M2-03 package identity/release notes and M3-05 supported preview readiness. It
does not claim that packaged installation, generic initialization, or external
project adoption has passed; those remain Milestone 2 and 3 gates.

## Rust Source Compatibility

- Public `TestingEvidence` v2 added `evaluation_attempt`, `recovery`, and
  `execution_intent`. Downstream Rust struct literals must provide those fields.
  Persisted clean v1 Testing JSON remains readable.
- `InitializedLoopRun::create_isolated` now requires an
  `&AuthoritativeRunInputSnapshots` argument. Downstream Rust callers must
  retain and pass the exact authoritative snapshots used to derive the run
  digests.

M2-03 must copy these notes into the packaged version/changelog surface. M3-05
must verify they remain in the supported preview notes before release-candidate
approval.
