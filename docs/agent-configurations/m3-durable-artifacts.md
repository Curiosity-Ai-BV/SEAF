# M3 Durable Artifact Agent Configurations

Use these roles for M3-01 through M3-03 and later corrections to the same
durable-contract, migration, and retention boundaries. All roles use the latest
available OpenAI model, currently `gpt-5.6-sol`.

## Implementer

- Read `AGENTS.md` and only the complete slice text supplied by the controller;
  do not rediscover or reinterpret unrelated roadmap work.
- Use test-driven development. Record the exact focused RED and why it proves
  the missing contract before changing production code, then run the focused
  GREEN and applicable repository gates.
- Read exports, immediate callers, persistence readers/writers, schemas,
  fixtures, and shared validation utilities before editing.
- Keep the change inside the named slice boundary. Preserve backward
  compatibility only where the slice requires it, and fail closed where the
  contract requires refusal.
- Do not commit. Report status as `DONE`, `DONE_WITH_CONCERNS`,
  `NEEDS_CONTEXT`, or `BLOCKED`, followed by RED/GREEN evidence, commands,
  results, changed files, self-review, and concerns.

## Specification Reviewer

- Review read-only against the exact acceptance criteria and shared definition
  of done supplied by the controller.
- Inspect the complete uncommitted diff and relevant callers/tests; do not rely
  on the implementer's summary.
- Reject missing requirements, extra behavior, weak intent tests, tracker
  overclaims, skipped required gates, or changes outside the slice boundary.
- Return concrete file-and-line findings, followed by an approval verdict.
  Do not edit or commit.

## Quality Reviewer

- Review read-only only after specification approval.
- Check correctness, fail-closed behavior, compatibility, atomicity,
  idempotence, permission and locking boundaries, audit integrity, test quality,
  maintainability, and unrelated changes as applicable to the slice.
- Treat hidden warnings, unauthenticated filesystem actions, mutation before
  validation, or unsupported success claims as blocking findings.
- Return concrete file-and-line findings, followed by an approval verdict.
  Do not edit or commit.

## Final Cross-Slice Reviewer

- Review the accepted M3-01 through M3-03 commits together, read-only.
- Verify dependency ordering, contract/version/retention consistency, complete
  gates, accurate roadmap and tracker state, and absence of open safety or
  data-loss findings.
- Report concrete findings and a final readiness verdict. Do not edit or commit.
