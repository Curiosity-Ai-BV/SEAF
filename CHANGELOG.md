# Changelog

This file records user-visible changes to SEAF. The project has not yet cut a
supported preview release.

## 0.1.0 (unreleased preview)

### Added

- Added stable CLI package identity through `seaf --version`.
- Added generic project initialization and read-only project readiness
  diagnostics for the supervised local coding loop.

### Rust source compatibility

- Public `TestingEvidence` v2 added `evaluation_attempt`, `recovery`, and
  `execution_intent`. Downstream Rust struct literals must provide those
  fields. Persisted clean v1 Testing JSON remains readable.
- `InitializedLoopRun::create_isolated` now requires an
  `&AuthoritativeRunInputSnapshots` argument. Downstream Rust callers must
  retain and pass the exact authoritative snapshots used to derive the run
  digests.

### Distribution

- All workspace crates are private Cargo packages. The `seaf-cli` name on
  crates.io belongs to an unrelated project; SEAF does not publish or install
  that package.
