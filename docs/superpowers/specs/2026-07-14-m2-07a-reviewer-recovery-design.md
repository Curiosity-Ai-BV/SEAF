# M2-07a Authenticated Reviewer-Recovery Design

Date: 2026-07-14
Status: approved for implementation planning

## Context

The packaged live-Ollama acceptance harness currently assumes that all six
provider roles complete on their first attempt. A real `gemma4:latest` run
instead reached Spec Review and returned `request_changes`. The run stopped
safely with its candidate pristine, but the harness rejected that state before
human review.

That behavior exposes a contract gap. The production-use acceptance scenario
requires SEAF to revise and rerun a reviewer-blocked step without losing or
silently replacing audit history. The existing `loop revise` and `loop rerun`
commands preserve recovery authority, but a recovered Spec Creation request
does not receive the prior spec or the reviewer's findings. It therefore cannot
reliably address the requested changes.

M2-07a is a separately numbered remediation slice. It adds the minimum semantic
bridge needed for an authenticated reviewer-blocked recovery and updates the
packaged harness to execute that recovery once. M2-07 remains incomplete until
the updated packaged live-Ollama gate and the full repository gate pass and
accepted evidence is retained.

## Goals

- Recover in the same run from an actual Spec Review `request_changes` result.
- Give the revised Spec Creation attempt the exact authenticated prior spec and
  reviewer response.
- Preserve all attempt-1 artifacts and provider-exchange history.
- Keep the operator's recovery reason as audit evidence rather than model
  instruction.
- Fail before provider invocation when recovery evidence is absent, tampered,
  or inconsistent.
- Extend the packaged live-Ollama harness and sanitized evidence without adding
  an unbounded retry policy.

## Non-goals

- General workflow routing or automatic selection of arbitrary recovery steps.
- Feeding free-form operator reasons into model prompts.
- Retrying provider failures, malformed model responses, rejections, or a
  second reviewer block.
- Changing approval, evaluation, promotion, cleanup, or publication authority.
- Closing M2-07 without a fresh successful live execution and final review.

## Considered Approaches

### 1. Authenticated reviewer feedback

Reset from Spec Creation and construct attempt 2 with the prior Spec Creation
response and blocking Spec Review response loaded from the immutable recovery
source. This is the selected approach because it is same-run, source-grounded,
and independently verifiable.

### 2. Operator-transcribed guidance

Reuse `--reason` as revision guidance. This is smaller, but it can omit or alter
the reviewer's findings and would change an audit field into an instruction
channel. It is rejected.

### 3. Retry or restart until approval

Repeat the same review prompt or start a new run. This is nondeterministic and
does not prove preservation of same-run audit history. It is rejected.

## Recovery Architecture

The accepted flow is:

1. Spec Creation attempt 1 completes.
2. Spec Review attempt 1 returns `request_changes`, leaving the run blocked at
   Spec Review.
3. A human controller inspects the verified run and invokes
   `loop revise --from-step spec` with an actor and audit reason.
4. The recovery artifact binds the blocked source run and authorizes Spec
   Creation attempt 2.
5. `loop rerun --recovery <id>` constructs Spec Creation attempt 2 with its
   normal Research and Analysis prerequisites plus authenticated revision
   context.
6. Spec Review attempt 2 evaluates the revised spec. Approval resumes the
   existing Development, Output Review, human approval, evaluation, and
   promotion flow.

The original provider ledger remains an immutable prefix. Recovery adds new
attempt records and artifacts; it does not replace attempt 1.

## Revision-Context Contract

`revision_context` is present only when all of these conditions hold:

- the current step is Spec Creation;
- the step attempt is greater than one;
- the latest authenticated provider recovery resets from Spec Creation;
- the recovery source run is blocked at Spec Review; and
- its validated Spec Review response has decision `request_changes`.

The context contains:

- the prior validated Spec Creation response;
- the validated blocking Spec Review response; and
- the existing relative artifact paths and SHA-256 digests that identify those
  responses.

The operator actor and reason stay in recovery evidence and are not copied into
the model request. Initial runs and every other recovery path retain their
current prompt shape.

Loading the context must reuse the existing authenticated recovery source and
validated role-artifact boundaries. A missing recovery source, digest mismatch,
wrong run, wrong step, wrong role, wrong attempt, non-blocked source state, or
review decision other than `request_changes` is a terminal pre-provider error.
The failed check must not call the model or mutate run, candidate, or source
state.

## Packaged Harness Behavior

The passing live-Ollama scenario must demonstrate one real reviewer recovery.
If its initial provider sequence reaches human review without a reviewer block,
the M2-07a evidence requirement is not met and the harness stops. When the run
is blocked at Spec Review with terminal outcome `request_changes`, the harness:

1. verifies the run and snapshots attempt-1 immutable history;
2. revises from Spec Creation;
3. verifies recovery 1 binds Spec Creation attempt 1 to attempt 2;
4. reruns that exact recovery;
5. requires Spec Creation attempt 2 to pass and Spec Review attempt 2 to return
   `approve_spec`; and
6. proves the old provider ledger is an unchanged prefix and attempt-1 files
   remain byte-identical.

A second block, rejection, malformed response, provider failure, or unexpected
step stops the harness. There is no retry loop. The rejection scenario may use
the same one-shot helper if it is also reviewer-blocked, but the passing
scenario is the mandatory proof.

Because the provider recovery consumes recovery ID 1, the later interrupted
evaluation recovery must read and assert its next recovery ID rather than
assuming ID 1. Existing approval and promotion confirmations remain exact,
typed controller checkpoints.

## Sanitized Evidence

The live evidence document advances to schema version 2. For the passing run it
records:

- status transitions `blocked`, `pending`, `awaiting_human_review`, `approved`,
  `eval_passed`, and `promoted`;
- Spec Creation and Spec Review attempts 1 and 2;
- provider recovery identity and authenticated prior-spec and review-artifact
  digests;
- eight provider attempts and sixteen request/response ledger records;
- the evaluation interruption and its second recovery authority;
- exact candidate, EvalReport, target-HEAD, and promotion authorities; and
- proof that source state was unchanged until explicit promotion.

The existing rejection, cleanup, artifact-integrity, size, status-allowlist,
secret-redaction, and absolute-path omission checks remain mandatory. Raw
prompts, responses, provider bodies, provider metadata, command output, and
absolute paths remain omitted.

## Verification

Implementation begins with focused failing regressions that encode why the
behavior matters:

- a recovered Spec Creation prompt includes the exact authenticated prior spec
  and reviewer response but excludes the operator reason;
- tampered or mismatched recovery inputs fail before provider invocation and
  leave state unchanged;
- a scripted CLI run moves from Spec Review `request_changes` through Spec
  Creation attempt 2 and Spec Review approval while retaining attempt 1;
- provider records and immutable attempt-1 artifacts remain prefix- and
  byte-preserved;
- the shell harness accepts only the one-shot, exact recovery sequence and
  emits schema-2 sanitized evidence; and
- the existing packaged fake-provider gate and full repository gate continue
  to pass.

The final M2-07 execution then uses the tracked packaged command with an exact
installed Ollama model and an absolute evidence path outside the repository.
The human controller independently reviews and types the approval and promotion
authorities already displayed by the harness.

## Commit and Review Boundaries

1. Commit this approved design and reusable agent configuration.
2. Write and approve a concrete M2-07a implementation plan.
3. Implement M2-07a with focused regressions in its own commit boundary.
4. Obtain separate specification and quality reviews.
5. Execute the fresh live M2-07 gate and full repository gate.
6. Commit accepted sanitized evidence and matching roadmap documentation
   separately from the remediation code.

No tag, release, registry publication, or supported-preview publication is
authorized by M2-07a or M2-07.
