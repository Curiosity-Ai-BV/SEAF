use std::sync::{Arc, Barrier};
use std::{fs, path::Path};

#[cfg(unix)]
use std::os::unix::fs::symlink;

use seaf_core::{
    canonical_json_bytes, canonical_sha256_digest, validate_loop_run, ArtifactReference,
    LoopInputDigests, LoopStepName, ProviderExchangeKind, ProviderExchangeOutcome,
    ProviderExchangePhase, ProviderExchangeRecord, ProviderExchangeRecordReference, ProviderRole,
};
use seaf_loop::{
    classify_provider_exchange_record, load_provider_exchange_record,
    persist_provider_exchange_record_reference, stage_provider_exchange_record,
    validate_provider_exchange_record_append, write_provider_exchange_request,
    write_provider_exchange_response, ProviderExchangeCoordinates, ProviderExchangeRecordState,
    ProviderExchangeResponseAudit,
};
use seaf_models::{ModelError, ModelResponse};
use sha2::Digest;

fn digest(byte: char) -> String {
    byte.to_string().repeat(64)
}

fn coordinates(index: u32) -> ProviderExchangeCoordinates {
    ProviderExchangeCoordinates {
        run_id: "exchange-run".to_string(),
        step: LoopStepName::Research,
        role: ProviderRole::Researcher,
        step_attempt: 1,
        exchange_index: index,
        kind: if index == 1 {
            ProviderExchangeKind::Initial
        } else {
            ProviderExchangeKind::ContextRetry
        },
        context_round: (index > 1).then_some(index - 1),
    }
}

fn request_record(
    index: u32,
    previous_record_digest: Option<String>,
    request: ArtifactReference,
    expansion: Option<ArtifactReference>,
) -> ProviderExchangeRecord {
    let identity = coordinates(index);
    ProviderExchangeRecord {
        schema_version: 1,
        run_id: identity.run_id,
        step: identity.step,
        role: identity.role,
        step_attempt: identity.step_attempt,
        exchange_index: identity.exchange_index,
        kind: identity.kind,
        context_round: identity.context_round,
        phase: ProviderExchangePhase::Request,
        previous_record_digest,
        request,
        response: None,
        expansion,
        outcome: None,
    }
}

fn response_record(
    request_record_digest: String,
    request: ArtifactReference,
    response: ArtifactReference,
) -> ProviderExchangeRecord {
    let identity = coordinates(1);
    ProviderExchangeRecord {
        schema_version: 1,
        run_id: identity.run_id,
        step: identity.step,
        role: identity.role,
        step_attempt: identity.step_attempt,
        exchange_index: identity.exchange_index,
        kind: identity.kind,
        context_round: identity.context_round,
        phase: ProviderExchangePhase::Response,
        previous_record_digest: Some(request_record_digest),
        request,
        response: Some(response),
        expansion: None,
        outcome: Some(ProviderExchangeOutcome::NeedsContext),
    }
}

#[test]
fn legacy_loop_run_defaults_to_no_provider_exchange_records_and_schema_stays_closed() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/local-loop/runs/valid-loop-run.json"
    ));
    let mut legacy: serde_json::Value = serde_json::from_str(source).expect("fixture JSON");
    legacy
        .as_object_mut()
        .expect("run object")
        .remove("provider_exchange_records");
    let run: seaf_core::LoopRun = serde_json::from_value(legacy.clone()).expect("legacy run loads");
    assert!(run.provider_exchange_records.is_empty());
    assert!(validate_loop_run(&run).is_empty());

    let mut unknown = legacy;
    unknown["provider_exchange_count"] = serde_json::json!(0);
    assert!(serde_json::from_value::<seaf_core::LoopRun>(unknown).is_err());

    let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../specs/loop-run.schema.json"
    )))
    .expect("schema");
    assert_eq!(schema["additionalProperties"], false);
    assert!(schema["properties"]["provider_exchange_records"].is_object());
    assert_eq!(
        schema["properties"]["provider_exchange_records"]["items"]["additionalProperties"],
        false
    );
    let item = &schema["properties"]["provider_exchange_records"]["items"];
    let conditions = item["allOf"].as_array().expect("cross-field conditions");
    assert!(
        conditions.len() >= 4,
        "schema must condition context identity, phase/path, kind/path, and step/role/path"
    );
    let encoded = serde_json::to_string(conditions).expect("conditions JSON");
    for required_identity in [
        "context_round",
        "context_retry",
        "json_repair",
        "request",
        "response",
        "researcher",
        "01-research",
        "output_reviewer",
        "06-output-review",
    ] {
        assert!(encoded.contains(required_identity), "{required_identity}");
    }
}

#[test]
fn loop_run_validation_rejects_exchange_gaps_reordering_identity_and_malformed_pairs() {
    let mut run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "exchange-run".to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "G-1".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: LoopInputDigests {
            ticket: digest('a'),
            policy: digest('b'),
            config: digest('c'),
            repository: digest('d'),
            eval_config: None,
        },
    });
    let valid = ProviderExchangeRecordReference {
        run_id: run.run_id.clone(),
        step: LoopStepName::Research,
        role: ProviderRole::Researcher,
        step_attempt: 1,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
        phase: ProviderExchangePhase::Request,
        path: "artifacts/01-research.attempt-001.exchange-001.initial.request.record.json"
            .to_string(),
        digest: digest('e'),
    };
    run.provider_exchange_records.push(valid.clone());
    assert!(validate_loop_run(&run).is_empty());

    for mutation in ["gap", "identity", "path", "digest", "phase"] {
        let mut invalid = run.clone();
        let reference = &mut invalid.provider_exchange_records[0];
        match mutation {
            "gap" => reference.exchange_index = 2,
            "identity" => reference.run_id = "other-run".to_string(),
            "path" => reference.path = "prompts/01-research.prompt.md".to_string(),
            "digest" => reference.digest = "BAD".to_string(),
            "phase" => reference.phase = ProviderExchangePhase::Response,
            _ => unreachable!(),
        }
        assert!(!validate_loop_run(&invalid).is_empty(), "{mutation}");
    }

    let mut reordered = run;
    let mut response = valid;
    response.phase = ProviderExchangePhase::Response;
    response.path =
        "artifacts/01-research.attempt-001.exchange-001.initial.response.record.json".to_string();
    reordered.provider_exchange_records.insert(0, response);
    assert!(!validate_loop_run(&reordered).is_empty());
}

#[test]
fn exchange_artifacts_and_records_are_create_only_and_canonical() {
    let temp = tempfile::tempdir().expect("temp");
    let run_dir = temp.path().join("run");
    create_private_run_layout(&run_dir);
    let coordinates = coordinates(1);

    let request =
        write_provider_exchange_request(&run_dir, &coordinates, b"request").expect("request");
    assert!(request
        .path
        .contains("attempt-001.exchange-001.initial.request"));
    let record = request_record(1, None, request.clone(), None);
    let staged = stage_provider_exchange_record(&run_dir, &record).expect("stage record");
    let canonical = canonical_json_bytes(&record).expect("canonical");
    assert_eq!(
        staged.digest,
        canonical_sha256_digest(&record).expect("digest")
    );
    assert_eq!(
        fs::read(run_dir.join(&staged.path)).expect("record"),
        canonical
    );
    assert_eq!(
        stage_provider_exchange_record(&run_dir, &record).expect("record replay"),
        staged
    );
    assert_eq!(
        load_provider_exchange_record(&run_dir, &staged).expect("verified record"),
        record
    );

    let workspace = seaf_loop::LoopWorkspace::open(temp.path(), "run").expect("workspace");
    persist_provider_exchange_record_reference(&workspace, staged.clone())
        .expect("activate request record");
    assert_eq!(
        write_provider_exchange_request(&run_dir, &coordinates, b"request").expect("replay"),
        request
    );
    assert!(write_provider_exchange_request(&run_dir, &coordinates, b"different").is_err());
    let response = write_provider_exchange_response(
        &run_dir,
        &coordinates,
        &response_audit(ProviderExchangeOutcome::NeedsContext),
    )
    .expect("response");
    assert!(response
        .path
        .contains("attempt-001.exchange-001.initial.response"));

    let response_record = response_record(staged.digest.clone(), request, response);
    stage_provider_exchange_record(&run_dir, &response_record).expect("response record");
}

#[test]
fn context_retry_requires_a_distinct_round_and_matching_canonical_expansion() {
    let request = ArtifactReference {
        path: "prompts/01-research.attempt-001.exchange-002.context-retry.request.md".to_string(),
        digest: digest('a'),
    };
    let expansion = ArtifactReference {
        path: "artifacts/01-research.attempt-001.context-round-001.json".to_string(),
        digest: digest('b'),
    };
    let valid = request_record(2, Some(digest('c')), request.clone(), Some(expansion));
    assert!(seaf_core::validate_provider_exchange_record(&valid).is_empty());

    let mut missing_round = valid.clone();
    missing_round.context_round = None;
    assert!(!seaf_core::validate_provider_exchange_record(&missing_round).is_empty());
    let mut substituted_round = valid;
    substituted_round.context_round = Some(2);
    assert!(!seaf_core::validate_provider_exchange_record(&substituted_round).is_empty());

    let mut role_mismatch = request_record(1, None, request, None);
    role_mismatch.phase = ProviderExchangePhase::Response;
    role_mismatch.previous_record_digest = Some(digest('d'));
    role_mismatch.response = Some(ArtifactReference {
        path: "responses/01-research.attempt-001.exchange-001.initial.response.txt".to_string(),
        digest: digest('e'),
    });
    role_mismatch.outcome = Some(ProviderExchangeOutcome::PatchProposed);
    assert!(!seaf_core::validate_provider_exchange_record(&role_mismatch).is_empty());

    let mut zero_round_repair = request_record(
        2,
        Some(digest('f')),
        ArtifactReference {
            path: "prompts/01-research.attempt-001.exchange-002.json-repair.request.md".to_string(),
            digest: digest('a'),
        },
        Some(ArtifactReference {
            path: "artifacts/01-research.attempt-001.context-round-000.json".to_string(),
            digest: digest('b'),
        }),
    );
    zero_round_repair.kind = ProviderExchangeKind::JsonRepair;
    zero_round_repair.context_round = Some(0);
    assert!(
        !seaf_core::validate_provider_exchange_record(&zero_round_repair).is_empty(),
        "any present context round must be nonzero"
    );
}

#[test]
fn json_repair_coordinates_reject_a_zero_context_round() {
    let temp = tempfile::tempdir().expect("temp");
    let run_dir = temp.path().join("run");
    create_private_run_layout(&run_dir);
    let coordinates = ProviderExchangeCoordinates {
        run_id: "zero-round".to_string(),
        step: LoopStepName::Research,
        role: ProviderRole::Researcher,
        step_attempt: 1,
        exchange_index: 2,
        kind: ProviderExchangeKind::JsonRepair,
        context_round: Some(0),
    };

    assert!(write_provider_exchange_request(&run_dir, &coordinates, b"repair").is_err());
}

#[test]
fn state_append_is_ordered_durable_and_distinguishes_staged_records() {
    let temp = tempfile::tempdir().expect("temp");
    let runs = temp.path().join("runs");
    let workspace = seaf_loop::LoopWorkspace::create(&runs, "exchange-run").expect("workspace");
    let run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "exchange-run".to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "G-1".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: LoopInputDigests {
            ticket: digest('a'),
            policy: digest('b'),
            config: digest('c'),
            repository: digest('d'),
            eval_config: None,
        },
    });
    seaf_loop::state::save_run(&workspace, &run).expect("save run");

    let request =
        write_provider_exchange_request(workspace.run_directory(), &coordinates(1), b"request")
            .expect("request");
    let record = request_record(1, None, request, None);
    let staged = stage_provider_exchange_record(workspace.run_directory(), &record).expect("stage");
    assert_eq!(
        classify_provider_exchange_record(&workspace, &run, &staged).expect("classify"),
        ProviderExchangeRecordState::Staged
    );

    let updated = persist_provider_exchange_record_reference(&workspace, staged.clone())
        .expect("append and persist");
    assert_eq!(updated.provider_exchange_records, vec![staged.clone()]);
    assert_eq!(
        classify_provider_exchange_record(&workspace, &updated, &staged).expect("classify"),
        ProviderExchangeRecordState::Referenced { position: 0 }
    );
    let retried = persist_provider_exchange_record_reference(&workspace, staged)
        .expect("exact current-tail retry converges without duplication");
    assert_eq!(retried, updated);
}

#[test]
fn append_transitions_follow_the_prior_parsed_outcome() {
    let temp = tempfile::tempdir().expect("temp");
    let runs = temp.path().join("runs");
    let workspace = seaf_loop::LoopWorkspace::create(&runs, "exchange-run").expect("workspace");
    let run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "exchange-run".to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "G-1".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: LoopInputDigests {
            ticket: digest('a'),
            policy: digest('b'),
            config: digest('c'),
            repository: digest('d'),
            eval_config: None,
        },
    });
    seaf_loop::state::save_run(&workspace, &run).expect("save run");

    let initial = coordinates(1);
    let request = write_provider_exchange_request(workspace.run_directory(), &initial, b"request")
        .expect("request");
    let initial_request_record = request_record(1, None, request.clone(), None);
    let request_reference =
        stage_provider_exchange_record(workspace.run_directory(), &initial_request_record)
            .expect("stage");
    persist_provider_exchange_record_reference(&workspace, request_reference.clone())
        .expect("append request");
    let response = write_provider_exchange_response(
        workspace.run_directory(),
        &initial,
        &response_audit(ProviderExchangeOutcome::NeedsContext),
    )
    .expect("response");
    let response_record = response_record(request_reference.digest.clone(), request, response);
    let response_reference =
        stage_provider_exchange_record(workspace.run_directory(), &response_record).expect("stage");
    persist_provider_exchange_record_reference(&workspace, response_reference.clone())
        .expect("append response");

    let mut wrong = coordinates(2);
    wrong.kind = ProviderExchangeKind::JsonRepair;
    wrong.context_round = None;
    let wrong_request =
        write_provider_exchange_request(workspace.run_directory(), &wrong, b"wrong request")
            .expect("request bytes");
    let wrong_record = ProviderExchangeRecord {
        schema_version: 1,
        run_id: wrong.run_id.clone(),
        step: wrong.step,
        role: wrong.role,
        step_attempt: wrong.step_attempt,
        exchange_index: wrong.exchange_index,
        kind: wrong.kind,
        context_round: None,
        phase: ProviderExchangePhase::Request,
        previous_record_digest: Some(response_reference.digest.clone()),
        request: wrong_request,
        response: None,
        expansion: None,
        outcome: None,
    };
    let wrong_reference = write_staged_record_fixture(workspace.run_directory(), &wrong_record);
    assert!(
        persist_provider_exchange_record_reference(&workspace, wrong_reference.clone()).is_err()
    );
    fs::remove_file(workspace.run_directory().join(wrong_reference.path))
        .expect("remove rejected staged record fixture");
    fs::remove_file(workspace.run_directory().join(wrong_record.request.path))
        .expect("remove rejected request fixture");

    let retry = coordinates(2);
    let retry_request =
        write_provider_exchange_request(workspace.run_directory(), &retry, b"retry request")
            .expect("retry request");
    let expansion_bytes = b"canonical expansion";
    let expansion = ArtifactReference {
        path: "artifacts/01-research.attempt-001.context-round-001.json".to_string(),
        digest: hex::encode(sha2::Sha256::digest(expansion_bytes)),
    };
    write_private_fixture_file(
        workspace.run_directory().join(&expansion.path),
        expansion_bytes,
    );
    let retry_record = request_record(
        2,
        Some(response_reference.digest),
        retry_request,
        Some(expansion),
    );
    let retry_reference =
        stage_provider_exchange_record(workspace.run_directory(), &retry_record).expect("stage");
    persist_provider_exchange_record_reference(&workspace, retry_reference)
        .expect("needs_context permits exactly a context retry");
}

#[test]
fn record_loading_rejects_tampered_bound_request_bytes() {
    let temp = tempfile::tempdir().expect("temp");
    let run_dir = temp.path().join("run");
    create_private_run_layout(&run_dir);
    let request =
        write_provider_exchange_request(&run_dir, &coordinates(1), b"request").expect("request");
    let record = request_record(1, None, request.clone(), None);
    let reference = stage_provider_exchange_record(&run_dir, &record).expect("record");
    fs::write(run_dir.join(request.path), b"tampered").expect("tamper request");
    assert!(load_provider_exchange_record(&run_dir, &reference).is_err());
}

#[test]
fn staging_rejects_tampered_bound_bytes_before_record_publication() {
    let temp = tempfile::tempdir().expect("temp");
    let run_dir = temp.path().join("run");
    create_private_run_layout(&run_dir);
    let request =
        write_provider_exchange_request(&run_dir, &coordinates(1), b"request").expect("request");
    let record = request_record(1, None, request.clone(), None);
    fs::write(run_dir.join(request.path), b"tampered").expect("tamper request");

    assert!(stage_provider_exchange_record(&run_dir, &record).is_err());
    assert!(!run_dir
        .join("artifacts/01-research.attempt-001.exchange-001.initial.request.record.json")
        .exists());
}

#[test]
fn a_new_step_group_starts_at_initial_index_one_but_links_the_run_wide_head() {
    let temp = tempfile::tempdir().expect("temp");
    let runs = temp.path().join("runs");
    let workspace = seaf_loop::LoopWorkspace::create(&runs, "exchange-run").expect("workspace");
    let run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "exchange-run".to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "G-1".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: LoopInputDigests {
            ticket: digest('a'),
            policy: digest('b'),
            config: digest('c'),
            repository: digest('d'),
            eval_config: None,
        },
    });
    seaf_loop::state::save_run(&workspace, &run).expect("save run");
    let research = coordinates(1);
    let request = write_provider_exchange_request(workspace.run_directory(), &research, b"r1")
        .expect("request");
    let request_record = request_record(1, None, request.clone(), None);
    let request_ref =
        stage_provider_exchange_record(workspace.run_directory(), &request_record).expect("stage");
    persist_provider_exchange_record_reference(&workspace, request_ref.clone()).expect("append");
    let response = write_provider_exchange_response(
        workspace.run_directory(),
        &research,
        &response_audit(ProviderExchangeOutcome::Passed),
    )
    .expect("response");
    let mut response_record = response_record(request_ref.digest, request, response);
    response_record.outcome = Some(ProviderExchangeOutcome::Passed);
    let response_ref =
        stage_provider_exchange_record(workspace.run_directory(), &response_record).expect("stage");
    persist_provider_exchange_record_reference(&workspace, response_ref.clone()).expect("append");

    let analysis = ProviderExchangeCoordinates {
        run_id: "exchange-run".to_string(),
        step: LoopStepName::Analysis,
        role: ProviderRole::Analyzer,
        step_attempt: 1,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
    };
    let analysis_request =
        write_provider_exchange_request(workspace.run_directory(), &analysis, b"analysis")
            .expect("analysis request");
    let analysis_record = ProviderExchangeRecord {
        schema_version: 1,
        run_id: analysis.run_id,
        step: analysis.step,
        role: analysis.role,
        step_attempt: analysis.step_attempt,
        exchange_index: analysis.exchange_index,
        kind: analysis.kind,
        context_round: None,
        phase: ProviderExchangePhase::Request,
        previous_record_digest: Some(response_ref.digest),
        request: analysis_request,
        response: None,
        expansion: None,
        outcome: None,
    };
    let analysis_ref =
        stage_provider_exchange_record(workspace.run_directory(), &analysis_record).expect("stage");
    persist_provider_exchange_record_reference(&workspace, analysis_ref)
        .expect("append globally linked new group");
}

#[test]
fn loading_run_state_rejects_tampered_authoritative_exchange_records() {
    let temp = tempfile::tempdir().expect("temp");
    let runs = temp.path().join("runs");
    let workspace = seaf_loop::LoopWorkspace::create(&runs, "exchange-run").expect("workspace");
    let run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "exchange-run".to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "G-1".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: LoopInputDigests {
            ticket: digest('a'),
            policy: digest('b'),
            config: digest('c'),
            repository: digest('d'),
            eval_config: None,
        },
    });
    seaf_loop::state::save_run(&workspace, &run).expect("save run");
    let request =
        write_provider_exchange_request(workspace.run_directory(), &coordinates(1), b"request")
            .expect("request");
    let record = request_record(1, None, request, None);
    let reference =
        stage_provider_exchange_record(workspace.run_directory(), &record).expect("stage");
    persist_provider_exchange_record_reference(&workspace, reference.clone()).expect("append");
    fs::write(workspace.run_directory().join(reference.path), b"{}").expect("tamper record");

    assert!(seaf_loop::state::load_run(&workspace).is_err());
}

#[test]
fn invalid_response_allows_only_json_repair_and_terminal_outcomes_allow_no_next_request() {
    let (invalid_temp, invalid_workspace, invalid_head) =
        seeded_response_run("invalid-run", ProviderExchangeOutcome::InvalidResponse);
    let repair = ProviderExchangeCoordinates {
        run_id: "invalid-run".to_string(),
        step: LoopStepName::Research,
        role: ProviderRole::Researcher,
        step_attempt: 1,
        exchange_index: 2,
        kind: ProviderExchangeKind::JsonRepair,
        context_round: None,
    };
    let repair_request =
        write_provider_exchange_request(invalid_workspace.run_directory(), &repair, b"repair")
            .expect("repair request");
    let repair_record = ProviderExchangeRecord {
        schema_version: 1,
        run_id: repair.run_id.clone(),
        step: repair.step,
        role: repair.role,
        step_attempt: repair.step_attempt,
        exchange_index: repair.exchange_index,
        kind: repair.kind,
        context_round: None,
        phase: ProviderExchangePhase::Request,
        previous_record_digest: Some(invalid_head.digest),
        request: repair_request,
        response: None,
        expansion: None,
        outcome: None,
    };
    let repair_reference =
        stage_provider_exchange_record(invalid_workspace.run_directory(), &repair_record)
            .expect("stage repair");
    persist_provider_exchange_record_reference(&invalid_workspace, repair_reference.clone())
        .expect("invalid response permits JSON repair");
    let repair_response = write_provider_exchange_response(
        invalid_workspace.run_directory(),
        &repair,
        &response_audit(ProviderExchangeOutcome::InvalidResponse),
    )
    .expect("invalid repair response");
    let repair_response_record = ProviderExchangeRecord {
        schema_version: 1,
        run_id: "invalid-run".to_string(),
        step: repair.step,
        role: repair.role,
        step_attempt: repair.step_attempt,
        exchange_index: repair.exchange_index,
        kind: repair.kind,
        context_round: None,
        phase: ProviderExchangePhase::Response,
        previous_record_digest: Some(repair_reference.digest),
        request: repair_record.request,
        response: Some(repair_response),
        expansion: None,
        outcome: Some(ProviderExchangeOutcome::InvalidResponse),
    };
    let repair_response_reference =
        stage_provider_exchange_record(invalid_workspace.run_directory(), &repair_response_record)
            .expect("stage invalid repair response");
    persist_provider_exchange_record_reference(
        &invalid_workspace,
        repair_response_reference.clone(),
    )
    .expect("append invalid repair response");
    let second_repair = ProviderExchangeCoordinates {
        exchange_index: 3,
        ..repair.clone()
    };
    let second_request = write_provider_exchange_request(
        invalid_workspace.run_directory(),
        &second_repair,
        b"second repair",
    )
    .expect("second repair request");
    let second_record = repair_request_record(
        second_repair,
        repair_response_reference.digest,
        second_request,
        None,
    );
    let second_reference =
        write_staged_record_fixture(invalid_workspace.run_directory(), &second_record);
    assert!(
        persist_provider_exchange_record_reference(&invalid_workspace, second_reference).is_err(),
        "an invalid repair response must not permit another repair"
    );
    drop(invalid_temp);

    let (_terminal_temp, terminal_workspace, terminal_head) =
        seeded_response_run("terminal-run", ProviderExchangeOutcome::Passed);
    let next = ProviderExchangeCoordinates {
        run_id: "terminal-run".to_string(),
        ..repair
    };
    let next_request =
        write_provider_exchange_request(terminal_workspace.run_directory(), &next, b"next")
            .expect("next request bytes");
    let next_record = ProviderExchangeRecord {
        schema_version: 1,
        run_id: next.run_id,
        step: next.step,
        role: next.role,
        step_attempt: next.step_attempt,
        exchange_index: next.exchange_index,
        kind: next.kind,
        context_round: None,
        phase: ProviderExchangePhase::Request,
        previous_record_digest: Some(terminal_head.digest),
        request: next_request,
        response: None,
        expansion: None,
        outcome: None,
    };
    let next_reference =
        write_staged_record_fixture(terminal_workspace.run_directory(), &next_record);
    assert!(
        persist_provider_exchange_record_reference(&terminal_workspace, next_reference).is_err()
    );
}

#[test]
fn only_malformed_json_is_eligible_for_json_repair() {
    let invalid_responses = [
        (
            "schema",
            serde_json::json!({"role": "researcher"}).to_string(),
        ),
        (
            "role",
            serde_json::json!({
                "role": "analyzer",
                "status": "passed",
                "summary": "wrong role",
                "findings": [],
                "risks": [],
                "next_step_recommendation": "continue"
            })
            .to_string(),
        ),
        (
            "context",
            serde_json::json!({
                "role": "researcher",
                "status": "needs_context",
                "summary": "missing request",
                "findings": [],
                "risks": [],
                "next_step_recommendation": "load context"
            })
            .to_string(),
        ),
    ];
    for (suffix, content) in invalid_responses {
        let run_id = format!("non-repairable-{suffix}");
        let audit = ProviderExchangeResponseAudit::ModelResponse {
            response: ModelResponse {
                content,
                latency_ms: 1,
                raw_provider_metadata: serde_json::Value::Null,
            },
        };
        let (_temp, workspace, head) =
            seeded_response_audit_run(&run_id, audit, ProviderExchangeOutcome::InvalidResponse);
        let repair = ProviderExchangeCoordinates {
            run_id: run_id.clone(),
            step: LoopStepName::Research,
            role: ProviderRole::Researcher,
            step_attempt: 1,
            exchange_index: 2,
            kind: ProviderExchangeKind::JsonRepair,
            context_round: None,
        };
        let request = write_provider_exchange_request(
            workspace.run_directory(),
            &repair,
            b"repair invalid response",
        )
        .expect("repair request");
        let record = repair_request_record(repair, head.digest, request, None);
        let reference = write_staged_record_fixture(workspace.run_directory(), &record);

        assert!(
            persist_provider_exchange_record_reference(&workspace, reference).is_err(),
            "{suffix} invalidity is terminal and not JSON-repair eligible"
        );
    }
}

#[test]
fn wrong_spec_reviewer_decision_is_audited_as_a_terminal_invalid_response() {
    assert_wrong_reviewer_decision_is_terminal(
        "wrong-spec-decision",
        LoopStepName::SpecReview,
        ProviderRole::SpecReviewer,
        "spec_reviewer",
        "approve_for_tests",
    );
}

#[test]
fn repair_after_context_retry_must_inherit_the_exact_round_and_expansion_authority() {
    let (_missing_temp, missing_workspace, missing_head, _missing_expansion) =
        seeded_context_invalid_response_run("missing-context-repair");
    let missing = repair_coordinates("missing-context-repair", None);
    let missing_request =
        write_provider_exchange_request(missing_workspace.run_directory(), &missing, b"repair")
            .expect("repair request");
    let missing_record = repair_request_record(missing, missing_head.digest, missing_request, None);
    let missing_reference =
        write_staged_record_fixture(missing_workspace.run_directory(), &missing_record);
    assert!(
        persist_provider_exchange_record_reference(&missing_workspace, missing_reference).is_err(),
        "repair after a context retry must not drop its context authority"
    );

    let (_sub_temp, substituted_workspace, substituted_head, _original_expansion) =
        seeded_context_invalid_response_run("substituted-context-repair");
    let substituted = repair_coordinates("substituted-context-repair", Some(2));
    let substituted_request = write_provider_exchange_request(
        substituted_workspace.run_directory(),
        &substituted,
        b"repair",
    )
    .expect("repair request");
    let substitute_bytes = b"substituted expansion";
    let substitute_expansion = ArtifactReference {
        path: "artifacts/01-research.attempt-001.context-round-002.json".to_string(),
        digest: hex::encode(sha2::Sha256::digest(substitute_bytes)),
    };
    write_private_fixture_file(
        substituted_workspace
            .run_directory()
            .join(&substitute_expansion.path),
        substitute_bytes,
    );
    let substituted_record = repair_request_record(
        substituted,
        substituted_head.digest,
        substituted_request,
        Some(substitute_expansion),
    );
    let substituted_reference =
        write_staged_record_fixture(substituted_workspace.run_directory(), &substituted_record);
    assert!(
        persist_provider_exchange_record_reference(&substituted_workspace, substituted_reference)
            .is_err(),
        "repair must not substitute a different context round"
    );

    let (_valid_temp, valid_workspace, valid_head, expansion) =
        seeded_context_invalid_response_run("valid-context-repair");
    let valid = repair_coordinates("valid-context-repair", Some(1));
    let valid_request =
        write_provider_exchange_request(valid_workspace.run_directory(), &valid, b"repair")
            .expect("repair request");
    let valid_record =
        repair_request_record(valid, valid_head.digest, valid_request, Some(expansion));
    let valid_reference =
        stage_provider_exchange_record(valid_workspace.run_directory(), &valid_record)
            .expect("stage valid repair");
    let updated = persist_provider_exchange_record_reference(&valid_workspace, valid_reference)
        .expect("repair inherits exact context authority");
    assert_eq!(
        updated
            .provider_exchange_records
            .iter()
            .filter(|reference| reference.kind == ProviderExchangeKind::ContextRetry)
            .count(),
        2,
        "request and response references represent one context round; repair consumes none"
    );
}

#[test]
fn response_cannot_substitute_the_authoritative_request_reference() {
    let temp = tempfile::tempdir().expect("temp");
    let runs = temp.path().join("runs");
    let workspace = seaf_loop::LoopWorkspace::create(&runs, "exchange-run").expect("workspace");
    let run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "exchange-run".to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "G-1".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: LoopInputDigests {
            ticket: digest('a'),
            policy: digest('b'),
            config: digest('c'),
            repository: digest('d'),
            eval_config: None,
        },
    });
    seaf_loop::state::save_run(&workspace, &run).expect("save");
    let request =
        write_provider_exchange_request(workspace.run_directory(), &coordinates(1), b"request")
            .expect("request");
    let request_record = request_record(1, None, request.clone(), None);
    let request_ref = stage_provider_exchange_record(workspace.run_directory(), &request_record)
        .expect("stage request");
    persist_provider_exchange_record_reference(&workspace, request_ref.clone()).expect("append");
    let response = write_provider_exchange_response(
        workspace.run_directory(),
        &coordinates(1),
        &response_audit(ProviderExchangeOutcome::NeedsContext),
    )
    .expect("response");
    let substituted_request = ArtifactReference {
        digest: digest('f'),
        ..request
    };
    let response_record = response_record(request_ref.digest, substituted_request, response);
    assert!(stage_provider_exchange_record(workspace.run_directory(), &response_record).is_err());
}

#[test]
fn response_outcome_is_derived_from_the_canonical_audit_not_the_record_claim() {
    for (actual, claimed, kind) in [
        (
            ProviderExchangeOutcome::NeedsContext,
            ProviderExchangeOutcome::Passed,
            ProviderExchangeKind::Initial,
        ),
        (
            ProviderExchangeOutcome::Blocked,
            ProviderExchangeOutcome::InvalidResponse,
            ProviderExchangeKind::Initial,
        ),
        (
            ProviderExchangeOutcome::ProviderFailure,
            ProviderExchangeOutcome::Passed,
            ProviderExchangeKind::Initial,
        ),
        (
            ProviderExchangeOutcome::Passed,
            ProviderExchangeOutcome::InvalidResponse,
            ProviderExchangeKind::JsonRepair,
        ),
    ] {
        let temp = tempfile::tempdir().expect("temp");
        let run_dir = temp.path().join("run");
        create_private_run_layout(&run_dir);
        let coordinates = ProviderExchangeCoordinates {
            kind,
            exchange_index: if kind == ProviderExchangeKind::Initial {
                1
            } else {
                2
            },
            context_round: None,
            ..coordinates(1)
        };
        let request =
            write_provider_exchange_request(&run_dir, &coordinates, b"request").expect("request");
        let request_record = ProviderExchangeRecord {
            schema_version: 1,
            run_id: coordinates.run_id.clone(),
            step: coordinates.step,
            role: coordinates.role,
            step_attempt: coordinates.step_attempt,
            exchange_index: coordinates.exchange_index,
            kind,
            context_round: None,
            phase: ProviderExchangePhase::Request,
            previous_record_digest: (kind == ProviderExchangeKind::JsonRepair).then(|| digest('f')),
            request: request.clone(),
            response: None,
            expansion: None,
            outcome: None,
        };
        let request_reference = write_staged_record_fixture(&run_dir, &request_record);
        let response = write_response_audit_fixture(&run_dir, &request, &response_audit(actual));
        let response_record = ProviderExchangeRecord {
            schema_version: 1,
            run_id: coordinates.run_id,
            step: coordinates.step,
            role: coordinates.role,
            step_attempt: coordinates.step_attempt,
            exchange_index: coordinates.exchange_index,
            kind,
            context_round: None,
            phase: ProviderExchangePhase::Response,
            previous_record_digest: Some(request_reference.digest),
            request,
            response: Some(response),
            expansion: None,
            outcome: Some(claimed),
        };

        assert!(
            stage_provider_exchange_record(&run_dir, &response_record).is_err(),
            "actual {actual:?} must reject claimed {claimed:?} for {kind:?}"
        );
    }
}

#[test]
fn loading_response_record_rejects_a_tampered_canonical_response_audit() {
    let temp = tempfile::tempdir().expect("temp");
    let run_dir = temp.path().join("run");
    create_private_run_layout(&run_dir);
    let coordinates = coordinates(1);
    let request =
        write_provider_exchange_request(&run_dir, &coordinates, b"request").expect("request");
    let request_record = request_record(1, None, request.clone(), None);
    let request_reference =
        stage_provider_exchange_record(&run_dir, &request_record).expect("request record");
    let workspace = seaf_loop::LoopWorkspace::open(temp.path(), "run").expect("workspace");
    persist_provider_exchange_record_reference(&workspace, request_reference.clone())
        .expect("activate request record");
    let response = write_provider_exchange_response(
        &run_dir,
        &coordinates,
        &response_audit(ProviderExchangeOutcome::NeedsContext),
    )
    .expect("response");
    let response_record = response_record(request_reference.digest, request, response.clone());
    let response_reference =
        stage_provider_exchange_record(&run_dir, &response_record).expect("response record");
    fs::write(
        run_dir.join(response.path),
        canonical_json_bytes(&response_audit(ProviderExchangeOutcome::Passed))
            .expect("replacement audit"),
    )
    .expect("tamper audit");

    assert!(load_provider_exchange_record(&run_dir, &response_reference).is_err());
}

#[test]
fn non_advancing_outcomes_cannot_bypass_the_chain_with_a_new_step_or_attempt() {
    for (suffix, outcome) in [
        ("needs", ProviderExchangeOutcome::NeedsContext),
        ("invalid", ProviderExchangeOutcome::InvalidResponse),
        ("blocked", ProviderExchangeOutcome::Blocked),
        ("provider", ProviderExchangeOutcome::ProviderFailure),
    ] {
        let run_id = format!("cross-step-{suffix}");
        let (_temp, workspace, head) = seeded_response_run(&run_id, outcome);
        let next = staged_new_group_request(
            &workspace,
            &run_id,
            LoopStepName::Analysis,
            ProviderRole::Analyzer,
            head.digest,
        );
        assert!(
            persist_provider_exchange_record_reference(&workspace, next).is_err(),
            "{outcome:?} must not jump to a new step"
        );
    }

    let (_temp, workspace, head) =
        seeded_response_run("attempt-bypass", ProviderExchangeOutcome::Blocked);
    let next_attempt = staged_request_for_group(
        &workspace,
        "attempt-bypass",
        LoopStepName::Research,
        ProviderRole::Researcher,
        3,
        head.digest,
    );
    assert!(
        persist_provider_exchange_record_reference(&workspace, next_attempt).is_err(),
        "blocked outcome must not jump to a new step attempt"
    );
}

#[cfg(unix)]
#[test]
fn append_rejects_a_symlinked_stable_exchange_lock_without_changing_run_state() {
    let (temp, workspace, head) =
        seeded_response_run("unsafe-lock", ProviderExchangeOutcome::Passed);
    let record = staged_new_group_request(
        &workspace,
        "unsafe-lock",
        LoopStepName::Analysis,
        ProviderRole::Analyzer,
        head.digest,
    );
    let before = fs::read(workspace.run_directory().join("run.json")).expect("run bytes");
    let outside = temp.path().join("outside-lock");
    fs::write(&outside, b"outside").expect("outside");
    fs::remove_file(workspace.run_directory().join("provider-exchange.lock"))
        .expect("replace stable lock for attack regression");
    symlink(
        &outside,
        workspace.run_directory().join("provider-exchange.lock"),
    )
    .expect("lock symlink");

    assert!(persist_provider_exchange_record_reference(&workspace, record).is_err());
    assert_eq!(
        fs::read(workspace.run_directory().join("run.json")).expect("run bytes"),
        before
    );
}

#[test]
fn concurrent_identical_appenders_converge_on_the_exact_tail() {
    let (_temp, workspace, head) =
        seeded_response_run("concurrent-append", ProviderExchangeOutcome::Passed);
    let analysis_attempt_one = staged_request_for_group(
        &workspace,
        "concurrent-append",
        LoopStepName::Analysis,
        ProviderRole::Analyzer,
        1,
        head.digest.clone(),
    );
    let analysis_attempt_two = analysis_attempt_one.clone();
    let old_run = seaf_loop::state::load_run(&workspace).expect("old state");
    validate_provider_exchange_record_append(&workspace, &old_run, &analysis_attempt_one)
        .expect("attempt one is independently valid from old head");
    validate_provider_exchange_record_append(&workspace, &old_run, &analysis_attempt_two)
        .expect("the same exact candidate is independently valid from old head");
    let barrier = Arc::new(Barrier::new(3));
    let left_workspace = workspace.clone();
    let left_barrier = Arc::clone(&barrier);
    let left = std::thread::spawn(move || {
        left_barrier.wait();
        persist_provider_exchange_record_reference(&left_workspace, analysis_attempt_one)
    });
    let right_workspace = workspace.clone();
    let right_barrier = Arc::clone(&barrier);
    let right = std::thread::spawn(move || {
        right_barrier.wait();
        persist_provider_exchange_record_reference(&right_workspace, analysis_attempt_two)
    });
    barrier.wait();
    let results = [left.join().expect("left"), right.join().expect("right")];

    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 2);
    let persisted = seaf_loop::state::load_run(&workspace).expect("valid persisted run");
    assert_eq!(persisted.provider_exchange_records.len(), 3);
}

fn seeded_response_run(
    run_id: &str,
    outcome: ProviderExchangeOutcome,
) -> (
    tempfile::TempDir,
    seaf_loop::LoopWorkspace,
    ProviderExchangeRecordReference,
) {
    seeded_response_audit_run(run_id, response_audit(outcome), outcome)
}

fn seeded_response_audit_run(
    run_id: &str,
    audit: ProviderExchangeResponseAudit,
    outcome: ProviderExchangeOutcome,
) -> (
    tempfile::TempDir,
    seaf_loop::LoopWorkspace,
    ProviderExchangeRecordReference,
) {
    let temp = tempfile::tempdir().expect("temp");
    let workspace =
        seaf_loop::LoopWorkspace::create(&temp.path().join("runs"), run_id).expect("workspace");
    let run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: run_id.to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "G-1".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: LoopInputDigests {
            ticket: digest('a'),
            policy: digest('b'),
            config: digest('c'),
            repository: digest('d'),
            eval_config: None,
        },
    });
    seaf_loop::state::save_run(&workspace, &run).expect("save");
    let coordinates = ProviderExchangeCoordinates {
        run_id: run_id.to_string(),
        step: LoopStepName::Research,
        role: ProviderRole::Researcher,
        step_attempt: 1,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
    };
    let request =
        write_provider_exchange_request(workspace.run_directory(), &coordinates, b"request")
            .expect("request");
    let mut request_record = request_record(1, None, request.clone(), None);
    request_record.run_id = run_id.to_string();
    let request_ref = stage_provider_exchange_record(workspace.run_directory(), &request_record)
        .expect("stage request");
    persist_provider_exchange_record_reference(&workspace, request_ref.clone()).expect("append");
    let response =
        write_provider_exchange_response(workspace.run_directory(), &coordinates, &audit)
            .expect("response");
    let mut response_record = response_record(request_ref.digest, request, response);
    response_record.run_id = run_id.to_string();
    response_record.outcome = Some(outcome);
    let response_ref = stage_provider_exchange_record(workspace.run_directory(), &response_record)
        .expect("stage response");
    persist_provider_exchange_record_reference(&workspace, response_ref.clone()).expect("append");
    (temp, workspace, response_ref)
}

fn assert_wrong_reviewer_decision_is_terminal(
    run_id: &str,
    step: LoopStepName,
    role: ProviderRole,
    role_name: &str,
    decision: &str,
) {
    let temp = tempfile::tempdir().expect("temp");
    let workspace =
        seaf_loop::LoopWorkspace::create(&temp.path().join("runs"), run_id).expect("workspace");
    let run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: run_id.to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "G-1".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: LoopInputDigests {
            ticket: digest('a'),
            policy: digest('b'),
            config: digest('c'),
            repository: digest('d'),
            eval_config: None,
        },
    });
    seaf_loop::state::save_run(&workspace, &run).expect("save");
    let coordinates = ProviderExchangeCoordinates {
        run_id: run_id.to_string(),
        step,
        role,
        step_attempt: 1,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
    };
    let request =
        write_provider_exchange_request(workspace.run_directory(), &coordinates, b"review request")
            .expect("request");
    let request_record = ProviderExchangeRecord {
        schema_version: 1,
        run_id: run_id.to_string(),
        step,
        role,
        step_attempt: 1,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
        phase: ProviderExchangePhase::Request,
        previous_record_digest: None,
        request: request.clone(),
        response: None,
        expansion: None,
        outcome: None,
    };
    let request_reference = write_staged_record_fixture(workspace.run_directory(), &request_record);
    persist_provider_exchange_record_reference(&workspace, request_reference.clone())
        .expect("append request");
    let audit = ProviderExchangeResponseAudit::ModelResponse {
        response: ModelResponse {
            content: serde_json::json!({
                "role": role_name,
                "decision": decision,
                "summary": "wrong decision for reviewer role",
                "blocking_issues": [],
                "non_blocking_issues": []
            })
            .to_string(),
            latency_ms: 1,
            raw_provider_metadata: serde_json::Value::Null,
        },
    };
    let response =
        write_provider_exchange_response(workspace.run_directory(), &coordinates, &audit)
            .expect("response audit");
    let response_record = ProviderExchangeRecord {
        schema_version: 1,
        run_id: run_id.to_string(),
        step,
        role,
        step_attempt: 1,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
        phase: ProviderExchangePhase::Response,
        previous_record_digest: Some(request_reference.digest),
        request,
        response: Some(response),
        expansion: None,
        outcome: Some(ProviderExchangeOutcome::InvalidResponse),
    };
    let response_reference =
        stage_provider_exchange_record(workspace.run_directory(), &response_record)
            .expect("wrong reviewer decision remains auditable as invalid response");
    assert_eq!(
        load_provider_exchange_record(workspace.run_directory(), &response_reference)
            .expect("load invalid response"),
        response_record
    );
    persist_provider_exchange_record_reference(&workspace, response_reference.clone())
        .expect("append invalid reviewer response");
    let repair = ProviderExchangeCoordinates {
        exchange_index: 2,
        kind: ProviderExchangeKind::JsonRepair,
        ..coordinates
    };
    let repair_request =
        write_provider_exchange_request(workspace.run_directory(), &repair, b"repair")
            .expect("repair request");
    let repair_record =
        repair_request_record(repair, response_reference.digest, repair_request, None);
    let repair_reference = write_staged_record_fixture(workspace.run_directory(), &repair_record);
    assert!(
        persist_provider_exchange_record_reference(&workspace, repair_reference).is_err(),
        "wrong reviewer decision is terminal and not repairable"
    );
}

fn seeded_context_invalid_response_run(
    run_id: &str,
) -> (
    tempfile::TempDir,
    seaf_loop::LoopWorkspace,
    ProviderExchangeRecordReference,
    ArtifactReference,
) {
    let (temp, workspace, needs_context_head) =
        seeded_response_run(run_id, ProviderExchangeOutcome::NeedsContext);
    let retry = ProviderExchangeCoordinates {
        run_id: run_id.to_string(),
        step: LoopStepName::Research,
        role: ProviderRole::Researcher,
        step_attempt: 1,
        exchange_index: 2,
        kind: ProviderExchangeKind::ContextRetry,
        context_round: Some(1),
    };
    let request =
        write_provider_exchange_request(workspace.run_directory(), &retry, b"context retry")
            .expect("context retry request");
    let expansion_bytes = b"trusted context expansion";
    let expansion = ArtifactReference {
        path: "artifacts/01-research.attempt-001.context-round-001.json".to_string(),
        digest: hex::encode(sha2::Sha256::digest(expansion_bytes)),
    };
    write_private_fixture_file(
        workspace.run_directory().join(&expansion.path),
        expansion_bytes,
    );
    let mut request_record = request_record(
        2,
        Some(needs_context_head.digest),
        request.clone(),
        Some(expansion.clone()),
    );
    request_record.run_id = run_id.to_string();
    let request_reference =
        stage_provider_exchange_record(workspace.run_directory(), &request_record)
            .expect("stage context retry");
    persist_provider_exchange_record_reference(&workspace, request_reference.clone())
        .expect("append context retry");
    let response = write_provider_exchange_response(
        workspace.run_directory(),
        &retry,
        &response_audit(ProviderExchangeOutcome::InvalidResponse),
    )
    .expect("context response");
    let response_record = ProviderExchangeRecord {
        schema_version: 1,
        run_id: run_id.to_string(),
        step: retry.step,
        role: retry.role,
        step_attempt: retry.step_attempt,
        exchange_index: retry.exchange_index,
        kind: retry.kind,
        context_round: retry.context_round,
        phase: ProviderExchangePhase::Response,
        previous_record_digest: Some(request_reference.digest),
        request,
        response: Some(response),
        expansion: Some(expansion.clone()),
        outcome: Some(ProviderExchangeOutcome::InvalidResponse),
    };
    let response_reference =
        stage_provider_exchange_record(workspace.run_directory(), &response_record)
            .expect("stage invalid response");
    persist_provider_exchange_record_reference(&workspace, response_reference.clone())
        .expect("append invalid response");
    (temp, workspace, response_reference, expansion)
}

fn repair_coordinates(run_id: &str, context_round: Option<u32>) -> ProviderExchangeCoordinates {
    ProviderExchangeCoordinates {
        run_id: run_id.to_string(),
        step: LoopStepName::Research,
        role: ProviderRole::Researcher,
        step_attempt: 1,
        exchange_index: 3,
        kind: ProviderExchangeKind::JsonRepair,
        context_round,
    }
}

fn repair_request_record(
    coordinates: ProviderExchangeCoordinates,
    previous_record_digest: String,
    request: ArtifactReference,
    expansion: Option<ArtifactReference>,
) -> ProviderExchangeRecord {
    ProviderExchangeRecord {
        schema_version: 1,
        run_id: coordinates.run_id,
        step: coordinates.step,
        role: coordinates.role,
        step_attempt: coordinates.step_attempt,
        exchange_index: coordinates.exchange_index,
        kind: coordinates.kind,
        context_round: coordinates.context_round,
        phase: ProviderExchangePhase::Request,
        previous_record_digest: Some(previous_record_digest),
        request,
        response: None,
        expansion,
        outcome: None,
    }
}

fn response_audit(outcome: ProviderExchangeOutcome) -> ProviderExchangeResponseAudit {
    if outcome == ProviderExchangeOutcome::ProviderFailure {
        return ProviderExchangeResponseAudit::ProviderFailure {
            error: ModelError::provider("provider failed", false, serde_json::Value::Null),
        };
    }
    let content = match outcome {
        ProviderExchangeOutcome::Passed => serde_json::json!({
            "role": "researcher",
            "status": "passed",
            "summary": "done",
            "findings": [],
            "risks": [],
            "next_step_recommendation": "continue"
        })
        .to_string(),
        ProviderExchangeOutcome::Blocked => serde_json::json!({
            "role": "researcher",
            "status": "blocked",
            "summary": "blocked",
            "findings": [],
            "risks": [],
            "next_step_recommendation": "stop"
        })
        .to_string(),
        ProviderExchangeOutcome::NeedsContext => serde_json::json!({
            "role": "researcher",
            "status": "needs_context",
            "summary": "need context",
            "findings": [],
            "risks": [],
            "next_step_recommendation": "load context",
            "context_request": {"paths": ["src/lib.rs"], "reason": "needed"}
        })
        .to_string(),
        ProviderExchangeOutcome::InvalidResponse => "not valid JSON".to_string(),
        _ => panic!("unsupported research test outcome: {outcome:?}"),
    };
    ProviderExchangeResponseAudit::ModelResponse {
        response: ModelResponse {
            content,
            latency_ms: 1,
            raw_provider_metadata: serde_json::Value::Null,
        },
    }
}

fn staged_new_group_request(
    workspace: &seaf_loop::LoopWorkspace,
    run_id: &str,
    step: LoopStepName,
    role: ProviderRole,
    previous_record_digest: String,
) -> ProviderExchangeRecordReference {
    staged_request_for_group(workspace, run_id, step, role, 1, previous_record_digest)
}

fn staged_request_for_group(
    workspace: &seaf_loop::LoopWorkspace,
    run_id: &str,
    step: LoopStepName,
    role: ProviderRole,
    step_attempt: u32,
    previous_record_digest: String,
) -> ProviderExchangeRecordReference {
    let coordinates = ProviderExchangeCoordinates {
        run_id: run_id.to_string(),
        step,
        role,
        step_attempt,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
    };
    let request = write_provider_exchange_request(
        workspace.run_directory(),
        &coordinates,
        format!("{step:?}").as_bytes(),
    )
    .expect("new group request");
    let record = ProviderExchangeRecord {
        schema_version: 1,
        run_id: run_id.to_string(),
        step,
        role,
        step_attempt,
        exchange_index: 1,
        kind: ProviderExchangeKind::Initial,
        context_round: None,
        phase: ProviderExchangePhase::Request,
        previous_record_digest: Some(previous_record_digest),
        request,
        response: None,
        expansion: None,
        outcome: None,
    };
    write_staged_record_fixture(workspace.run_directory(), &record)
}

fn write_staged_record_fixture(
    run_directory: &Path,
    record: &ProviderExchangeRecord,
) -> ProviderExchangeRecordReference {
    assert_eq!(record.phase, ProviderExchangePhase::Request);
    let request_name = record
        .request
        .path
        .strip_prefix("prompts/")
        .and_then(|path| path.strip_suffix(".request.md"))
        .expect("canonical staged request path");
    let reference = ProviderExchangeRecordReference {
        run_id: record.run_id.clone(),
        step: record.step,
        role: record.role,
        step_attempt: record.step_attempt,
        exchange_index: record.exchange_index,
        kind: record.kind,
        context_round: record.context_round,
        phase: record.phase,
        path: format!("artifacts/{request_name}.request.record.json"),
        digest: canonical_sha256_digest(record).expect("record digest"),
    };
    write_private_fixture_file(
        run_directory.join(&reference.path),
        &canonical_json_bytes(record).expect("canonical record"),
    );
    reference
}

fn write_response_audit_fixture(
    run_directory: &Path,
    request: &ArtifactReference,
    audit: &ProviderExchangeResponseAudit,
) -> ArtifactReference {
    let request_name = request
        .path
        .strip_prefix("prompts/")
        .and_then(|path| path.strip_suffix(".request.md"))
        .expect("canonical request path");
    let bytes = canonical_json_bytes(audit).expect("canonical response audit");
    let reference = ArtifactReference {
        path: format!("responses/{request_name}.response.json"),
        digest: hex::encode(sha2::Sha256::digest(&bytes)),
    };
    write_private_fixture_file(run_directory.join(&reference.path), &bytes);
    reference
}

#[cfg(unix)]
fn create_private_run_layout(run_dir: &Path) {
    let parent = run_dir.parent().expect("run parent");
    let run_id = run_dir
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .expect("run id");
    let workspace = seaf_loop::LoopWorkspace::create(parent, run_id).expect("workspace");
    let run = seaf_loop::state::create_run(seaf_loop::state::NewLoopRun {
        run_id: "exchange-run".to_string(),
        ticket_id: "T-1".to_string(),
        goal_id: "G-1".to_string(),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        input_digests: LoopInputDigests {
            ticket: digest('a'),
            policy: digest('b'),
            config: digest('c'),
            repository: digest('d'),
            eval_config: None,
        },
    });
    seaf_loop::state::save_run(&workspace, &run).expect("save run");
}

#[cfg(not(unix))]
fn create_private_run_layout(_run_dir: &Path) {
    panic!("private loop workspace tests require Unix")
}

#[cfg(unix)]
fn write_private_fixture_file(path: impl AsRef<Path>, bytes: &[u8]) {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(path).unwrap();
    file.write_all(bytes).unwrap();
}

#[cfg(not(unix))]
fn write_private_fixture_file(_path: impl AsRef<Path>, _bytes: &[u8]) {
    panic!("private loop workspace tests require Unix")
}

#[cfg(unix)]
#[test]
fn exchange_writer_rejects_symlink_and_non_file_collisions() {
    let temp = tempfile::tempdir().expect("temp");
    let run_dir = temp.path().join("run");
    create_private_run_layout(&run_dir);
    let coordinates = coordinates(1);
    let expected = run_dir.join("prompts/01-research.attempt-001.exchange-001.initial.request.md");
    let outside = temp.path().join("outside");
    fs::write(&outside, b"outside").expect("outside");
    symlink(&outside, &expected).expect("symlink");
    assert!(write_provider_exchange_request(&run_dir, &coordinates, b"request").is_err());
    fs::remove_file(&expected).expect("remove link");
    fs::create_dir(&expected).expect("directory collision");
    assert!(write_provider_exchange_request(&run_dir, &coordinates, b"request").is_err());
}
