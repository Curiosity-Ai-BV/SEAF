# M2-07a Authenticated Reviewer Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Recover one live Spec Review `request_changes` result in the same run
by giving Spec Creation attempt 2 the exact authenticated prior spec and review
feedback, then complete and evidence the packaged M2-07 Ollama gate.

**Architecture:** Expose a crate-private accessor for the latest verified
provider-recovery source, then have `ProviderStepRunner` construct a typed
`revision_context` only for an authorized Spec Creation recovery. Extend the
packaged live-Ollama harness with one bounded Spec Review recovery branch,
prefix-preservation assertions, dynamic recovery IDs, and schema-2 sanitized
evidence.

**Tech Stack:** Rust 2021, `serde`/`serde_json`, SEAF durable loop artifacts,
Bash, Node.js assertions embedded in the existing packaged harness, Cargo,
pnpm/Prettier.

## Global Constraints

- Work only on M2-07a remediation and the later M2-07 evidence/documentation
  closure; preserve unrelated work.
- The operator actor and `--reason` remain audit-only and must not enter the
  model prompt.
- Only an authenticated recovery from Spec Creation whose source run is
  blocked at Spec Review with `request_changes` may receive revision context.
- Missing, tampered, or inconsistent recovery inputs fail before provider
  invocation and without source, candidate, or run mutation.
- Attempt-1 artifacts and provider-exchange records remain immutable; new
  records append to the authenticated prefix.
- Perform at most one reviewer recovery. A second block, rejection, malformed
  response, timeout, or provider failure stops loudly.
- Approval and promotion remain exact typed human-controller checkpoints.
- Do not publish a tag, release, registry package, or preview release.
- M2-07a code and M2-07 execution evidence use separate commit boundaries.

---

## File Map

- `crates/seaf-loop/src/recovery.rs`: expose the already-verified latest
  provider recovery and immutable source run to sibling modules.
- `crates/seaf-loop/src/model_runner.rs`: define and construct the typed Spec
  Creation revision subject and add it to the recovered prompt.
- `crates/seaf-loop/src/test_suites/spec_creation_revision_recovery.rs`: build
  isolated candidate fixtures for successful recovery, exact prompt content,
  immutable history, and fail-closed cases.
- `scripts/test-packaged-external-golden-path.sh`: execute the bounded live
  reviewer recovery, preserve history, use dynamic recovery IDs, and emit
  schema-2 evidence.
- `docs/local-agent-loop.md`: document that Spec Creation reviewer recovery
  derives guidance from authenticated artifacts while the operator reason is
  audit-only.
- `docs/production-use-implementation-plan.md` and
  `docs/production-readiness-roadmap.md`: update only after fresh M2-07 evidence
  and all gates are accepted.
- `docs/evidence/m2-07-packaged-ollama.json`: retain the final sanitized
  schema-2 evidence after it is produced at an external temporary path and
  reviewed.

### Task 1: Authenticated Spec Creation Revision Context

**Files:**

- Modify: `crates/seaf-loop/src/recovery.rs`
- Modify: `crates/seaf-loop/src/model_runner.rs`
- Create: `crates/seaf-loop/src/test_suites/spec_creation_revision_recovery.rs`

**Interfaces:**

- Produces:
  `pub(crate) fn load_verified_latest_provider_recovery_source(workspace: &LoopWorkspace, run: &LoopRun) -> Result<(RecoveryAttemptV1, LoopRun), RecoveryError>`.
- Produces private serialized prompt types
  `RevisionRoleArtifact` and `SpecCreationRevisionContext`.
- Consumes the existing `ValidatedRoleArtifact::load`,
  `required_artifact_pair`, `RecoveryAttemptV1`, and authenticated recovery
  lineage.

- [ ] **Step 1: Add a failing same-run recovery regression**

Create `spec_creation_revision_recovery.rs` using the isolated-run setup pattern
from `output_review_response_recovery.rs`. The first provider must return valid
Research, Analysis, and Spec Creation responses followed by:

```rust
serde_json::json!({
    "role": "spec_reviewer",
    "decision": "request_changes",
    "summary": "State the single-file boundary explicitly.",
    "blocking_issues": [{
        "summary": "The file boundary is implicit.",
        "evidence": "The proposed spec does not state that no other path may change."
    }],
    "non_blocking_issues": []
})
```

After asserting `LoopStatus::Blocked` at `LoopStepName::SpecReview`, snapshot all
run files except mutable `run.json` and `log.md`, call:

```rust
let revision = crate::recovery::revise_provider_step(
    &workspace,
    LoopStepName::SpecCreation,
    "operator@example.invalid",
    "address authenticated Spec Review feedback",
)
.expect("create recovery");
assert_eq!(revision.recovery.source_step_attempt, 1);
assert_eq!(revision.recovery.next_step_attempt, 2);
```

Resume with `InitializedLoopRun::resume_isolated_for_rerun`, a provider whose
first response is a revised valid Spec Creation response, and
`.with_recovery_attempt(LoopStepName::SpecCreation, 2)`. Inspect its first
request user message and assert:

```rust
assert_eq!(role_input["revision_context"]["prior_spec"]["artifact"]["step"], "spec_creation");
assert_eq!(role_input["revision_context"]["reviewer_feedback"]["artifact"]["step"], "spec_review");
assert_eq!(role_input["revision_context"]["reviewer_feedback"]["artifact"]["response"]["decision"], "request_changes");
assert!(!requests[0].messages[0].content.contains("address authenticated Spec Review feedback"));
```

Complete Spec Review attempt 2 with `approve_spec`; assert attempts 1 and 2 are
inspectable, the original provider ledger is an unchanged prefix, and every
snapshotted attempt-1 file is byte-identical.

- [ ] **Step 2: Run the focused test and verify the RED state**

Run:

```bash
cargo test -p seaf-loop spec_creation_recovery_uses_authenticated_reviewer_feedback -- --exact --nocapture
```

Expected: FAIL because the test module or `revision_context` does not yet
exist. A provider request that lacks `revision_context` is also an acceptable
RED result.

- [ ] **Step 3: Expose the verified provider-recovery source**

In `recovery.rs`, add the crate-private accessor and reuse the existing lineage
validator rather than reparsing artifacts:

```rust
fn load_verified_provider_recovery_source(
    workspace: &LoopWorkspace,
    run: &LoopRun,
    reference: &RecoveryReference,
) -> Result<(RecoveryAttemptV1, LoopRun), RecoveryError> {
    validate_operator_run_envelope(workspace, run)?;
    let (recovery, source, projection) =
        load_verified_recovery_lineage(workspace, reference)?;
    validate_current_descendant(
        workspace,
        run,
        reference,
        &source,
        &projection,
        &recovery,
    )?;
    Ok((recovery, source))
}

pub(crate) fn load_verified_latest_provider_recovery_source(
    workspace: &LoopWorkspace,
    run: &LoopRun,
) -> Result<(RecoveryAttemptV1, LoopRun), RecoveryError> {
    let reference = run
        .latest_recovery
        .as_ref()
        .ok_or_else(|| RecoveryError::invalid("run has no latest provider recovery"))?;
    load_verified_provider_recovery_source(workspace, run, reference)
}
```

Refactor `load_verified_recovery` to call the private reference-aware helper
and return only the recovery. This preserves callers that authenticate an exact
historical reference while the new prompt path deliberately uses the latest
authority.

- [ ] **Step 4: Add typed revision-context construction**

In `model_runner.rs`, add:

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct RevisionRoleArtifact {
    artifact_path: String,
    artifact_digest: String,
    artifact: ValidatedRoleArtifact,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct SpecCreationRevisionContext {
    prior_spec: RevisionRoleArtifact,
    reviewer_feedback: RevisionRoleArtifact,
}
```

Add a loader that returns `Ok(None)` for attempt 1. For attempt 2+, require the
latest recovery step and next attempt to equal `SpecCreation` and the current
attempt; require the source run to be `Blocked` at `SpecReview`; load source-run
Spec Creation and Spec Review artifacts with their exact recorded paths,
digests, run ID, roles, and steps; and require:

```rust
matches!(
    &review.artifact.response,
    RoleResponse::Reviewer(response)
        if response.decision == ReviewDecision::RequestChanges
)
```

Return `RunnerError::Step` with a precise recovery-context message on every
mismatch. Do not read `recovery.actor` or `recovery.reason` into the returned
subject.

In `structured_role_prompt`, construct the existing JSON object, then add:

```rust
if step == LoopStepName::SpecCreation {
    if let Some(context) = self.spec_creation_revision_context()? {
        prompt["revision_context"] = serde_json::to_value(context).map_err(|error| {
            RunnerError::Step(format!(
                "failed to serialize SpecCreation revision context: {error}"
            ))
        })?;
    }
}
```

Include the new test suite beside the existing model-runner suites:

```rust
#[cfg(test)]
mod spec_creation_revision_recovery_tests {
    include!("test_suites/spec_creation_revision_recovery.rs");
}
```

- [ ] **Step 5: Add fail-closed regressions**

In the new test suite, add table-driven cases for a substituted recovery-source
digest, wrong recovery step, source status other than `Blocked`, Spec Review
status other than `Blocked`, and reviewer decision other than
`RequestChanges`. For each case snapshot source, candidate, and run files;
prepare the recovered runner; assert an error containing
`SpecCreation revision context`; assert `provider.requests().is_empty()`; and
assert all three snapshots are unchanged.

- [ ] **Step 6: Run focused tests and Rust quality checks**

Run:

```bash
cargo test -p seaf-loop spec_creation_revision_recovery -- --nocapture
cargo fmt --all -- --check
cargo clippy -p seaf-loop --all-targets --all-features -- -D warnings
```

Expected: all focused tests pass; formatting and clippy exit 0 with no skipped
checks.

- [ ] **Step 7: Commit the core remediation**

```bash
git add crates/seaf-loop/src/recovery.rs crates/seaf-loop/src/model_runner.rs crates/seaf-loop/src/test_suites/spec_creation_revision_recovery.rs
git commit -m "Recover specs from authenticated review feedback"
```

### Task 2: One-Shot Packaged Reviewer Recovery and Schema-2 Evidence

**Files:**

- Modify: `scripts/test-packaged-external-golden-path.sh`
- Modify: `docs/local-agent-loop.md`

**Interfaces:**

- Consumes the Task 1 `revision_context` behavior through packaged
  `loop revise --from-step spec` and `loop rerun --recovery <id>`.
- Produces a shell helper that returns the provider-attempt count and latest
  recovery ID for later evaluation recovery.
- Produces sanitized evidence schema version 2.

- [ ] **Step 1: Add a bounded live-state classifier**

Replace the first-pass-only review assertion with a Node-backed classifier that
accepts only `awaiting_human_review` or the exact recoverable tuple
`blocked/spec_review/initial/request_changes`. Any other state must print only
the safe step, exchange kind, and outcome summary before failing.

For the passing scenario, require the recoverable tuple. Snapshot the existing
provider ledger references and all attempt-1 prompt, response, artifact, and
provider-record bytes before revision. The rejection scenario may accept
first-pass human review or use the same one-shot recovery helper.

- [ ] **Step 2: Verify shell syntax and the existing packaged gate**

Run:

```bash
bash -n scripts/test-packaged-external-golden-path.sh
./scripts/test-packaged-external-golden-path.sh
```

Expected: syntax passes and the deterministic fake-provider packaged gate
remains green. The live path still lacks recovery until the next step, which is
the intended RED boundary for the real failure observed in M2-07.

- [ ] **Step 3: Implement exactly one provider recovery**

For a recoverable run, execute the packaged binary with:

```bash
loop revise --run-id "$run_id" --runs-root "$runs_root" \
  --from-step spec --actor "$operator" \
  --reason "address authenticated packaged Spec Review feedback" --json
```

Assert `command == "revise"`, `status == "pending"`, `current_step ==
"spec_creation"`, `source_step_attempt == 1`, and `next_step_attempt == 2`.
Read the returned `recovery_id`, then execute:

```bash
loop rerun --run-id "$run_id" --runs-root "$runs_root" \
  --recovery "$recovery_id" --ticket seaf.ticket.yaml \
  --base-url "$ollama_base_url" --timeout-ms "$role_timeout_ms" --json
```

Require `awaiting_human_review`. Assert the exact ordered provider attempts:

```text
research/1 passed
analysis/1 passed
spec_creation/1 passed
spec_review/1 request_changes
spec_creation/2 passed
spec_review/2 approve_spec
development/1 patch_proposed
output_review/1 approve_for_tests
```

Each attempt must contain exactly one verified initial request and one verified
initial response. Reject repair, context expansion, retry, or extra exchanges.
Assert the pre-recovery provider ledger is an unchanged prefix and every
snapshotted attempt-1 file remains byte-identical.

- [ ] **Step 4: Make later evaluation recovery IDs lineage-aware**

Replace hard-coded evaluation recovery ID 1 with the ID returned by the Testing
invalidation. Assert it is exactly the provider recovery ID plus one when the
passing scenario recovered Spec Creation. Pass that ID to `loop rerun` and
verify both recovery artifacts remain authenticated and inspectable.

- [ ] **Step 5: Emit and validate schema-2 evidence**

Set `schema_version` to 2. Replace the ambiguous
`provider_exchange_count: 6` with:

```javascript
provider_attempt_count: 8,
provider_ledger_record_count: 16,
reviewer_recovery: {
  recovery_id: providerRecoveryId,
  source_step: 'spec_creation',
  blocked_step: 'spec_review',
  source_attempt: 1,
  revised_attempt: 2,
  prior_spec_artifact_digest: priorSpecDigest,
  reviewer_artifact_digest: reviewerDigest,
  attempt_one_immutable: true,
  ledger_prefix_preserved: true,
},
```

Use the exact passing status transitions:

```javascript
[
  "blocked",
  "pending",
  "awaiting_human_review",
  "approved",
  "eval_passed",
  "promoted",
];
```

Retain all existing omission, size, digest, status-allowlist, rejection,
cleanup, source-preservation, and promotion assertions. Evidence must contain
no raw prompt, response, provider body, provider metadata, command output, or
absolute path.

- [ ] **Step 6: Document the operator/model boundary**

In `docs/local-agent-loop.md`, immediately after the existing revise/rerun
example, state that `--reason` is sanitized audit evidence and is not model
guidance. Document that revising from `spec` after authenticated Spec Review
`request_changes` supplies the prior spec and review artifact automatically;
all other recovery paths retain their current prompt contract.

- [ ] **Step 7: Run the packaged fake gate and focused checks**

Run:

```bash
bash -n scripts/test-packaged-external-golden-path.sh
./scripts/test-packaged-external-golden-path.sh
cargo test -p seaf-loop spec_creation_revision_recovery -- --nocapture
corepack pnpm exec prettier --check docs/local-agent-loop.md docs/superpowers/plans/2026-07-14-m2-07a-reviewer-recovery.md
git diff --check
```

Expected: every command exits 0. The packaged fake-provider gate remains fully
executed rather than skipped.

- [ ] **Step 8: Commit the harness remediation**

```bash
git add scripts/test-packaged-external-golden-path.sh docs/local-agent-loop.md
git commit -m "Exercise packaged reviewer recovery"
```

### Task 3: Independent Review and Full Remediation Verification

**Files:**

- Review only: all M2-07a changes since commit `b083601`

**Interfaces:**

- Consumes the Task 1 and Task 2 commits.
- Produces accepted specification and quality-review verdicts before live
  evidence execution.

- [ ] **Step 1: Run the complete repository gate**

Run without skipping or filtering:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --workspace
corepack pnpm format:check
corepack pnpm lint:packages
corepack pnpm typecheck
corepack pnpm test
corepack pnpm build
```

Expected: every command exits 0. Surface diagnostic output even when a command
exits successfully.

- [ ] **Step 2: Obtain specification review**

Use the Specification Reviewer role in
`docs/agent-configurations/m2-07.md`. Review the exact diff from `b083601` and
verify every approved design requirement, especially audit-only operator
reason, authenticated reviewer feedback, one-shot recovery, immutable history,
and dynamic recovery IDs. Fix accepted findings with focused regressions and
rerun affected gates.

- [ ] **Step 3: Obtain quality review**

After specification approval, use the Quality Reviewer role in
`docs/agent-configurations/m2-07.md`. Review fail-closed behavior, secret/path
disclosure, shell safety, evidence schema, maintainability, and unrelated
changes. Fix accepted findings and rerun affected gates.

- [ ] **Step 4: Record the remediation boundary**

Run:

```bash
git status --short
git log --oneline b083601..HEAD
git diff --check b083601..HEAD
```

Expected: only planned M2-07a files changed, all changes are committed, and the
range diff is clean.

### Task 4: Execute and Close M2-07 Evidence

**Files:**

- Create: `docs/evidence/m2-07-packaged-ollama.json`
- Modify: `docs/production-use-implementation-plan.md`
- Modify: `docs/production-readiness-roadmap.md`

**Interfaces:**

- Consumes the reviewed M2-07a packaged harness.
- Produces accepted, sanitized M2-07 evidence and closes Milestone 2 only when
  every live and repository gate passes.

- [ ] **Step 1: Run the tracked packaged live-Ollama gate**

Use the exact installed model that passes the packaged model check and the full
role sequence. Start with the currently demonstrated structured-output-capable
model:

```bash
./scripts/test-packaged-external-golden-path.sh \
  --local-live-ollama \
  --model gemma4:latest \
  --evidence-out /tmp/seaf-m2-07-<source-commit>-evidence.json
```

Expected: a real initial Spec Review `request_changes`, authenticated Spec
Creation recovery, human-review checkpoint, exact approval, real evaluation
interruption and recovery, exact promotion, deterministic rejection/cleanup,
and final sanitized evidence publication. Model or environment failure leaves
M2-07 pending.

- [ ] **Step 2: Act as the delegated human controller**

At approval, independently inspect the verified candidate diff and target HEAD;
type only the exact values displayed by the harness. At promotion, inspect the
candidate digest, EvalReport digest, target HEAD, frozen candidate diff, and
source-preservation facts; type only their exact displayed values. Stop on any
drift or unexplained output.

- [ ] **Step 3: Validate and retain sanitized evidence**

Check the external file is regular, mode-restricted, schema 2, below 32 KiB,
free of absolute paths and prohibited raw fields, and bound to the current
source, archive, harness, fixtures, candidate, EvalReport, and recovery
digests. Copy only the reviewed sanitized JSON to
`docs/evidence/m2-07-packaged-ollama.json` using `apply_patch`, then run:

```bash
corepack pnpm exec prettier --check docs/evidence/m2-07-packaged-ollama.json
git diff --check
```

Expected: both checks pass.

- [ ] **Step 4: Update the roadmap only from accepted facts**

In `docs/production-use-implementation-plan.md`, mark M2-07 accepted with the
execution date, model, source commit, evidence path/digest, live status flow,
and full-gate result. Record M2-07a as the separately numbered remediation.

In `docs/production-readiness-roadmap.md`, mark Milestone 2 complete and make
M3-01 the next dependency only if all M2-07 acceptance criteria and independent
reviews passed. Do not alter M3-01 through M3-06 scope.

- [ ] **Step 5: Re-run documentation and final boundary checks**

Run:

```bash
corepack pnpm format:check
git diff --check
git status --short
```

Expected: formatting passes and only the evidence and two roadmap documents are
uncommitted.

- [ ] **Step 6: Commit M2-07 evidence and documentation**

```bash
git add docs/evidence/m2-07-packaged-ollama.json docs/production-use-implementation-plan.md docs/production-readiness-roadmap.md
git commit -m "Accept packaged Ollama production gate"
```

- [ ] **Step 7: Verify final state**

Run:

```bash
git status --short
git log -4 --oneline
```

Expected: the worktree is clean; M2-07a remediation and M2-07 evidence have
separate commits; M2-07 and Milestone 2 are closed; M3-01 is next.
