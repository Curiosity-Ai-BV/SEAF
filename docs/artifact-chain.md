# Artifact Chain

The current MVP chain is local and manual by design.

1. `seaf goal validate` and `seaf policy validate` fail closed on malformed contracts.
2. `seaf task brief` writes a JSON and Markdown task brief under `.seaf/tasks/`.
3. A human or coding agent creates a patch outside SEAF automation.
4. `seaf eval run` executes configured shell checks and writes an EvalReport under `.seaf/evals/`.
5. `seaf release prepare` refuses failing EvalReports and writes a ReleaseCapsule with artifact and eval report SHA-256 digests.
6. `seaf release verify` checks capsule structure and, when provided, verifies artifact and eval report digests.

No command in this chain commits, merges, signs a production release, or applies an update.
