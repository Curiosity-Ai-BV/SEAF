# M2-07d Deterministic Review-Fixture Plan

## Evidence and scope

After M2-07c, a live run exercised real reviewer recovery but later received an
invalid Development response. A diagnostic run then completed every provider
role successfully, including Development and Output Review, but the first Spec
Review approved. The harness correctly rejected that run because first-pass
approval does not prove M2-07a recovery.

The current ticket asks the initial Spec Writer for an already-complete,
unambiguous spec, so either reviewer outcome is reasonable. This remediation
makes the acceptance fixture intentionally exercise the review loop while
keeping the reviewer decision model-backed: the trusted ticket instructs the
initial Spec Creation request, which has no `revision_context`, to contain one
exact mismatch with the final acceptance bytes. The real Spec Reviewer must
request changes. A recovered Spec Creation request, which contains authenticated
reviewer context, must correct the mismatch to the unchanged final acceptance
criteria before Development can run.

## Success criteria

1. The Ollama fixture ticket states an exact two-phase review protocol keyed to
   absence or presence of authenticated `revision_context`.
2. The initial draft mismatch is narrow and explicit; it changes only the
   proposed final text, not paths, permissions, or policy scope.
3. The final acceptance criteria and expected promoted bytes remain exactly
   `SEAF packaged Ollama acceptance passed.` plus one newline.
4. A harness preflight validates the fixture protocol and final-byte contract so
   the recovery stimulus cannot be removed silently.
5. The preflight test is observed RED before the fixture is updated, then the
   packaged fake gate, formatting, strict linting, workspace tests, and
   independent spec/quality review pass.
6. Only after those gates pass may one fresh live packaged Ollama acceptance be
   attempted. M2-07 remains pending unless the real reviewer requests changes,
   the recovered run completes, and validated sanitized evidence is published.

## Files and commit boundary

- Modify `scripts/test-packaged-external-golden-path.sh` first to assert the
  exact fixture protocol and final acceptance contract.
- Modify `fixtures/packaged-external-golden-path/ollama/seaf.ticket.yaml` only
  after the preflight is RED.
- Update `docs/local-agent-loop.md` only if needed to explain that the packaged
  acceptance fixture intentionally supplies a reviewable first draft.
- Do not modify M2-07 evidence or roadmap files during remediation.
- Commit separately as `Make packaged reviewer recovery deterministic`.
