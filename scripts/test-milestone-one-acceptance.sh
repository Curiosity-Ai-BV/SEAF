#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output_file="$(mktemp)"
trap 'rm -f "$output_file"' EXIT

run_exact() {
  local label="$1"
  local expected_name="$2"
  shift 2

  : >"$output_file"
  echo "==> $label"
  set +e
  cargo test --locked "$@" "$expected_name" -- --exact --test-threads=1 2>&1 | tee "$output_file"
  local status=${PIPESTATUS[0]}
  set -e

  if [[ $status -ne 0 ]]; then
    echo "Milestone 1 acceptance failed: $expected_name exited with status $status" >&2
    exit "$status"
  fi

  if ! grep -Fq "test $expected_name ... ok" "$output_file"; then
    echo "Milestone 1 acceptance failed: exact test '$expected_name' was missing or did not pass" >&2
    exit 1
  fi

  if ! grep -Eq 'test result: ok\. 1 passed; 0 failed; [0-9]+ ignored; [0-9]+ measured; [0-9]+ filtered out' "$output_file"; then
    echo "Milestone 1 acceptance failed: '$expected_name' did not produce exactly one passing test" >&2
    exit 1
  fi
}

cd "$repo_root"

run_exact \
  "U1 complete canonical effective-input snapshot set" \
  "loop_run_persists_canonical_effective_inputs_and_matching_digests" \
  -p seaf-cli --test cli

run_exact \
  "U1 full provider run uses authoritative inputs and canonical digests" \
  "loop_run_fake_uses_provider_artifacts_and_real_policy_decision" \
  -p seaf-cli --test cli

run_exact \
  "U2 exact validated early-role chain" \
  "legacy_provider_step_runner_tests::early_role_requests_chain_only_exact_validated_prerequisites_and_persist_canonical_artifacts" \
  -p seaf-loop --lib

run_exact \
  "U2 Development uses only exact approved spec and repository context" \
  "legacy_provider_step_runner_tests::development_request_uses_exact_approved_spec_and_only_developer_repository_context" \
  -p seaf-loop --lib

run_exact \
  "Candidate Applying and Applied crash recovery" \
  "candidate_workspace::tests::candidate_application_recovers_from_each_real_publication_cut" \
  -p seaf-loop --lib

run_exact \
  "OutputReview response-cut adoption" \
  "model_runner::output_review_response_recovery_tests::output_review_response_cut_adopts_without_provider_replay_or_source_mutation" \
  -p seaf-loop --lib

run_exact \
  "Exact human approval" \
  "human_approval_binds_the_exact_reviewed_candidate_without_running_tests" \
  -p seaf-loop --test provider_candidate_boundary

run_exact \
  "Incomplete Testing invalidation and indexed rerun" \
  "loop_revise_testing_invalidate_and_rerun_use_no_provider_configuration" \
  -p seaf-cli --test cli

run_exact \
  "Complete report-prefix zero-command adoption" \
  "evaluation_adoption_finalizes_complete_prefix_without_command_execution_and_retries_inertly" \
  -p seaf-loop --test provider_candidate_boundary

run_exact \
  "Complete report-prefix crash-cut convergence" \
  "evaluation_adoption_resumes_source_recovery_and_report_crash_cuts_exactly" \
  -p seaf-loop --test provider_candidate_boundary

run_exact \
  "Failed evaluation rejection and source preservation" \
  "approved_eval_failed_command_publishes_rejecting_bound_terminal_report" \
  -p seaf-cli --test cli

run_exact \
  "Promotion process-crash adoption" \
  "loop_promote_persists_intent_before_apply_and_adopts_exact_patch_after_process_crash" \
  -p seaf-cli --test cli

run_exact \
  "Full approval/evaluation/report/promotion inert retry" \
  "loop_promote_applies_only_the_frozen_eval_passed_patch_and_exact_retry_is_inert" \
  -p seaf-cli --test cli

run_exact \
  "Persisted clean Testing v1 JSON compatibility" \
  "testing_evidence_loads_only_canonical_digest_bound_run_evidence" \
  -p seaf-loop --test testing_evidence

echo "Milestone 1 source-workspace acceptance passed (14 exact tests)."
