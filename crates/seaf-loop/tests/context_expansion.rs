use std::{
    io::{Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Barrier},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

use seaf_core::LoopStepName;
use seaf_loop::{
    create_context_expansion, load_context_expansion, reconstruct_context_expansion_files,
    ArtifactReference, CandidateContextAuthority, CandidateContextAuthorityKind,
    ContextExpansionRequest, ContextLimits, ContextRequest, Role,
};
use sha2::{Digest, Sha256};

fn fixture() -> (tempfile::TempDir, ContextExpansionRequest) {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo = temp.path().join("repo");
    let run = temp.path().join("run");
    std::fs::create_dir_all(repo.join("src")).expect("repo");
    create_private_directory(&run);
    create_private_directory(&run.join("artifacts"));
    create_private_directory(&run.join("prompts"));
    std::fs::write(repo.join("src/a.rs"), "alpha\n").expect("a");
    std::fs::write(repo.join("src/b.rs"), "beta\n").expect("b");
    let initial_request_bytes = b"immutable provider request";
    write_private_file(
        run.join("prompts/01-research.attempt-002.prompt.md"),
        initial_request_bytes,
    );
    (
        temp,
        ContextExpansionRequest {
            repository_root: repo,
            run_directory: run,
            run_id: "run-1".to_string(),
            step: LoopStepName::Research,
            role: Role::Researcher,
            step_attempt: 2,
            context_round: 1,
            context_request: ContextRequest {
                paths: vec!["src/b.rs".to_string(), "src/a.rs".to_string()],
                reason: "Need both modules".to_string(),
            },
            initial_provider_request: ArtifactReference {
                path: "prompts/01-research.attempt-002.prompt.md".to_string(),
                digest: hex::encode(Sha256::digest(initial_request_bytes)),
            },
            previous_expansion: None,
            candidate_authority: None,
            initial_loaded_paths: vec!["README.md".to_string()],
            initial_context_bytes: 10,
            ticket_forbidden_files: Vec::new(),
            policy_forbidden_paths: Vec::new(),
            default_exclude_globs: Vec::new(),
            limits: ContextLimits {
                max_bytes_per_file: 64,
                max_total_bytes: 64,
            },
        },
    )
}

#[cfg(unix)]
fn create_private_directory(path: &Path) {
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700).create(path).unwrap();
}

#[cfg(not(unix))]
fn create_private_directory(_path: &Path) {
    panic!("private loop workspace tests require Unix")
}

#[cfg(unix)]
fn write_private_file(path: impl AsRef<Path>, bytes: &[u8]) {
    let path = path.as_ref();
    if path.exists() {
        std::fs::write(path, bytes).unwrap();
        return;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(path).unwrap();
    file.write_all(bytes).unwrap();
}

#[cfg(not(unix))]
fn write_private_file(_path: impl AsRef<Path>, _bytes: &[u8]) {
    panic!("private loop workspace tests require Unix")
}

#[test]
fn expansion_rejects_initial_request_from_a_different_candidate_authority() {
    let (_temp, mut request) = fixture();
    let expected = CandidateContextAuthority {
        kind: CandidateContextAuthorityKind::IsolatedCandidate,
        repository_identity_digest: "a".repeat(64),
        candidate_path_digest: "b".repeat(64),
        starting_head: "c".repeat(40),
        starting_tree: "d".repeat(40),
    };
    let mut observed = expected.clone();
    observed.candidate_path_digest = "e".repeat(64);
    request.candidate_authority = Some(expected);
    let role_input = serde_json::json!({
        "repository_context_authority": { "candidate_authority": observed }
    })
    .to_string();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "messages": [{ "content": role_input }]
    }))
    .unwrap();
    write_private_file(
        request
            .run_directory
            .join(&request.initial_provider_request.path),
        &bytes,
    );
    request.initial_provider_request.digest = hex::encode(Sha256::digest(&bytes));

    let error = create_context_expansion(&request)
        .expect_err("another candidate's initial request must not authorize expansion");
    assert!(
        error.to_string().contains("candidate authority mismatch"),
        "{error}"
    );
}

#[test]
fn second_round_rejects_a_valid_link_to_predecessor_from_another_candidate() {
    let (_temp, mut first) = fixture();
    first.context_request.paths = vec!["src/a.rs".to_string()];
    let created = create_context_expansion(&first).expect("round one");
    let mut predecessor = created.artifact;
    predecessor.candidate_authority = Some(CandidateContextAuthority {
        kind: CandidateContextAuthorityKind::IsolatedCandidate,
        repository_identity_digest: "a".repeat(64),
        candidate_path_digest: "b".repeat(64),
        starting_head: "c".repeat(40),
        starting_tree: "d".repeat(40),
    });
    let predecessor_bytes = seaf_core::canonical_json_bytes(&predecessor).unwrap();
    std::fs::write(
        first.run_directory.join(&created.identity.path),
        &predecessor_bytes,
    )
    .unwrap();
    let predecessor_identity = ArtifactReference {
        path: created.identity.path,
        digest: hex::encode(Sha256::digest(&predecessor_bytes)),
    };

    let mut second = first;
    second.context_round = 2;
    second.previous_expansion = Some(predecessor_identity);
    second.context_request.paths = vec!["src/b.rs".to_string()];
    let error = create_context_expansion(&second)
        .expect_err("every predecessor must share current candidate authority");
    assert!(error.to_string().contains("authority mismatch"), "{error}");
}

#[test]
fn expansion_is_canonical_additive_and_deterministic() {
    let (_temp, request) = fixture();
    let created = create_context_expansion(&request).expect("create expansion");

    assert_eq!(
        created.identity.path,
        "artifacts/01-research.attempt-002.context-round-001.json"
    );
    assert_eq!(
        created
            .artifact
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        vec!["src/a.rs", "src/b.rs"]
    );
    assert_eq!(created.artifact.prior_total_context_bytes, 10);
    assert_eq!(created.artifact.resulting_total_context_bytes, 21);
    assert_eq!(
        load_context_expansion(&request, &created.identity).expect("verified load"),
        created.artifact
    );

    let replay = create_context_expansion(&request).expect("idempotent replay");
    assert_eq!(replay, created);

    std::fs::write(request.repository_root.join("src/a.rs"), "changed\n").expect("change");
    let recovered = load_context_expansion(&request, &created.identity)
        .expect("trusted identity recovers accepted bytes without live reread");
    assert_eq!(recovered, created.artifact);
    assert!(create_context_expansion(&request)
        .expect_err("changed live bytes must not replace an accepted expansion")
        .to_string()
        .contains("different bytes"));

    let (_other_temp, mut semantically_equivalent) = fixture();
    semantically_equivalent.context_request.paths.reverse();
    let equivalent =
        create_context_expansion(&semantically_equivalent).expect("equivalent expansion");
    assert_eq!(equivalent.identity.digest, created.identity.digest);
    assert_eq!(equivalent.artifact, created.artifact);
}

#[test]
fn expansion_accepts_fresh_live_exchange_request_authority_without_breaking_legacy_identity() {
    let (_temp, mut request) = fixture();
    let legacy = request
        .run_directory
        .join(&request.initial_provider_request.path);
    let bytes = std::fs::read(&legacy).expect("legacy request bytes");
    request.initial_provider_request.path =
        "prompts/01-research.attempt-002.exchange-001.initial.request.md".to_string();
    write_private_file(
        request
            .run_directory
            .join(&request.initial_provider_request.path),
        &bytes,
    );

    create_context_expansion(&request).expect("fresh exchange authority is accepted");
}

#[test]
fn expansion_excludes_loaded_paths_but_rejects_duplicate_only() {
    let (_temp, mut request) = fixture();
    request.initial_loaded_paths.push("src/a.rs".to_string());
    let created = create_context_expansion(&request).expect("mixed expansion");
    assert_eq!(created.artifact.excluded_loaded_paths, vec!["src/a.rs"]);
    assert_eq!(created.artifact.files.len(), 1);
    assert_eq!(created.artifact.files[0].path, "src/b.rs");

    let (_temp, mut request) = fixture();
    request.context_request.paths = vec!["README.md".to_string()];
    let error = create_context_expansion(&request).expect_err("duplicate-only must fail");
    assert!(error.to_string().contains("no new context files"));
    assert!(!request
        .run_directory
        .join("artifacts/01-research.attempt-002.context-round-001.json")
        .exists());
}

#[test]
fn expansion_rejects_every_unsafe_or_unavailable_new_path_atomically() {
    for bad_path in [
        "../escape",
        "/absolute",
        "src\\backslash.rs",
        "src/\u{0}control.rs",
        ".env",
        "missing.rs",
        "src",
        "src/binary.bin",
    ] {
        let (_temp, mut request) = fixture();
        if bad_path == "src/binary.bin" {
            std::fs::write(request.repository_root.join(bad_path), [0xff, 0xfe]).expect("binary");
        }
        request.context_request.paths = vec!["src/a.rs".to_string(), bad_path.to_string()];
        assert!(create_context_expansion(&request).is_err(), "{bad_path}");
        assert!(!request
            .run_directory
            .join("artifacts/01-research.attempt-002.context-round-001.json")
            .exists());
    }

    for (bad_path, ticket_forbidden, policy_forbidden) in [
        ("src/a.rs", vec!["src/a.rs".to_string()], Vec::new()),
        ("src/b.rs", Vec::new(), vec!["src/b.rs".to_string()]),
    ] {
        let (_temp, mut request) = fixture();
        request.context_request.paths = vec![bad_path.to_string()];
        request.ticket_forbidden_files = ticket_forbidden;
        request.policy_forbidden_paths = policy_forbidden;
        assert!(create_context_expansion(&request).is_err());
        assert!(!request
            .run_directory
            .join("artifacts/01-research.attempt-002.context-round-001.json")
            .exists());
    }

    let (_temp, mut request) = fixture();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/tmp", request.repository_root.join("outside"))
            .expect("symlink");
        request.context_request.paths = vec!["outside/file".to_string()];
        assert!(create_context_expansion(&request).is_err());
    }
}

#[test]
#[cfg(unix)]
fn expansion_rejects_repository_directory_symlink_aliases_before_forbidden_matching() {
    let (_temp, mut request) = fixture();
    std::fs::create_dir(request.repository_root.join("secrets")).expect("secrets");
    std::fs::write(request.repository_root.join("secrets/key"), "secret\n").expect("secret");
    std::os::unix::fs::symlink("secrets", request.repository_root.join("public"))
        .expect("directory alias");
    request.context_request.paths = vec!["public/key".to_string()];
    request.policy_forbidden_paths = vec!["secrets/**".to_string()];

    assert!(create_context_expansion(&request).is_err());
    assert!(!request
        .run_directory
        .join("artifacts/01-research.attempt-002.context-round-001.json")
        .exists());
}

#[test]
fn expansion_applies_utf8_safe_per_file_and_cumulative_limits_without_omitting_files() {
    let (_temp, mut request) = fixture();
    std::fs::write(request.repository_root.join("src/a.rs"), "ééé").expect("unicode");
    request.context_request.paths = vec!["src/a.rs".to_string()];
    request.limits.max_bytes_per_file = 5;
    request.limits.max_total_bytes = 14;
    let created = create_context_expansion(&request).expect("truncated expansion");
    assert_eq!(created.artifact.files[0].content, "éé");
    assert_eq!(created.artifact.files[0].included_bytes, 4);
    assert!(created.artifact.files[0].truncated);

    let (_temp, mut request) = fixture();
    request.initial_context_bytes = 63;
    let error =
        create_context_expansion(&request).expect_err("cannot include every requested file");
    assert!(error.to_string().contains("omit"));
    assert!(!request
        .run_directory
        .join("artifacts/01-research.attempt-002.context-round-001.json")
        .exists());

    let (_temp, mut request) = fixture();
    std::fs::write(request.repository_root.join("src/a.rs"), b"").expect("empty");
    request.context_request.paths = vec!["src/a.rs".to_string()];
    assert!(create_context_expansion(&request)
        .expect_err("empty files are not useful context")
        .to_string()
        .contains("zero useful bytes"));
}

#[test]
fn expansion_create_only_writer_and_loader_reject_collisions_and_tampering() {
    let (_temp, request) = fixture();
    let created = create_context_expansion(&request).expect("create");
    let absolute = request.run_directory.join(&created.identity.path);
    std::fs::write(&absolute, b"different").expect("tamper");
    let error = create_context_expansion(&request).expect_err("collision");
    assert!(error.to_string().contains("different bytes"));
    assert!(load_context_expansion(&request, &created.identity).is_err());

    let (_temp, request) = fixture();
    let created = create_context_expansion(&request).expect("create");
    let absolute = request.run_directory.join(&created.identity.path);
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&absolute).unwrap()).unwrap();
    value["files"][0]["content"] = serde_json::json!("substituted");
    std::fs::write(&absolute, seaf_core::canonical_json_bytes(&value).unwrap()).unwrap();
    assert!(load_context_expansion(&request, &created.identity).is_err());
}

#[test]
fn create_never_adopts_a_canonical_self_consistent_unreferenced_target_forgery() {
    let (_temp, request) = fixture();
    let created = create_context_expansion(&request).expect("create");
    let absolute = request.run_directory.join(&created.identity.path);
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&absolute).expect("artifact")).expect("json");
    let substituted = b"omega\n";
    let substituted_digest = hex::encode(Sha256::digest(substituted));
    value["files"][0]["content"] = serde_json::json!("omega\n");
    value["files"][0]["included_sha256"] = serde_json::json!(substituted_digest);
    value["files"][0]["source_sha256"] = serde_json::json!(substituted_digest);
    value["files"][0]["included_bytes"] = serde_json::json!(substituted.len());
    value["files"][0]["source_bytes"] = serde_json::json!(substituted.len());
    let forged = seaf_core::canonical_json_bytes(&value).expect("canonical forgery");
    std::fs::write(&absolute, forged).expect("write forgery");

    assert!(load_context_expansion(&request, &created.identity).is_err());
    assert!(create_context_expansion(&request).is_err());
}

#[test]
fn expansion_requires_the_exact_safe_immutable_initial_request_audit() {
    for mutation in ["missing", "digest", "content"] {
        let (_temp, mut request) = fixture();
        let path = request
            .run_directory
            .join(&request.initial_provider_request.path);
        match mutation {
            "missing" => std::fs::remove_file(path).expect("remove audit"),
            "digest" => request.initial_provider_request.digest = "b".repeat(64),
            "content" => std::fs::write(path, b"changed").expect("change audit"),
            _ => unreachable!(),
        }
        assert!(create_context_expansion(&request).is_err(), "{mutation}");
    }

    #[cfg(unix)]
    {
        let (_temp, request) = fixture();
        let path = request
            .run_directory
            .join(&request.initial_provider_request.path);
        std::fs::remove_file(&path).expect("remove audit");
        std::os::unix::fs::symlink("/tmp/outside", &path).expect("audit symlink");
        assert!(create_context_expansion(&request).is_err());
    }
}

#[test]
fn expansion_rejects_a_different_safe_file_as_the_initial_request_authority() {
    let (_temp, mut request) = fixture();
    let substitute = "prompts/substitute.prompt.md";
    std::fs::copy(
        request
            .run_directory
            .join(&request.initial_provider_request.path),
        request.run_directory.join(substitute),
    )
    .expect("copy prompt");
    request.initial_provider_request.path = substitute.to_string();

    assert!(create_context_expansion(&request).is_err());
}

#[test]
fn loader_rejects_substituted_initial_loaded_metadata() {
    let (_temp, request) = fixture();
    let created = create_context_expansion(&request).expect("create");

    let mut substituted_paths = request.clone();
    substituted_paths.initial_loaded_paths = vec!["LICENSE".to_string()];
    assert!(load_context_expansion(&substituted_paths, &created.identity).is_err());

    let mut substituted_bytes = request.clone();
    substituted_bytes.initial_context_bytes += 1;
    assert!(load_context_expansion(&substituted_bytes, &created.identity).is_err());
}

#[test]
#[cfg(unix)]
fn expansion_rejects_a_symlinked_artifact_parent_even_when_it_stays_inside_the_run() {
    let (_temp, request) = fixture();
    let artifacts = request.run_directory.join("artifacts");
    let real_artifacts = request.run_directory.join("real-artifacts");
    std::fs::remove_dir(&artifacts).expect("remove artifacts");
    create_private_directory(&real_artifacts);
    std::os::unix::fs::symlink(&real_artifacts, &artifacts).expect("artifact parent symlink");

    assert!(create_context_expansion(&request).is_err());
    assert!(std::fs::read_dir(real_artifacts)
        .expect("real artifacts")
        .next()
        .is_none());
}

#[test]
#[cfg(unix)]
fn expansion_rejects_a_symlinked_existing_artifact_target() {
    let (_temp, request) = fixture();
    let outside = request.run_directory.join("outside.json");
    write_private_file(&outside, b"outside");
    let artifact = request
        .run_directory
        .join("artifacts/01-research.attempt-002.context-round-001.json");
    std::os::unix::fs::symlink(&outside, &artifact).expect("artifact symlink");

    assert!(create_context_expansion(&request).is_err());
    assert_eq!(std::fs::read(&outside).expect("outside bytes"), b"outside");
}

#[test]
fn concurrent_identical_creators_publish_one_complete_artifact() {
    let (_temp, request) = fixture();
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let request = request.clone();
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            create_context_expansion(&request)
        }));
    }
    barrier.wait();
    let left = threads
        .remove(0)
        .join()
        .expect("left thread")
        .expect("left");
    let right = threads
        .remove(0)
        .join()
        .expect("right thread")
        .expect("right");

    assert_eq!(left, right);
    assert_eq!(
        load_context_expansion(&request, &left.identity).expect("complete published artifact"),
        left.artifact
    );
}

#[test]
fn orphaned_partial_temp_file_is_never_consumed_as_the_final_artifact() {
    let (_temp, request) = fixture();
    let orphan = request
        .run_directory
        .join("artifacts/.01-research.attempt-002.context-round-001.json.tmp-orphan");
    write_private_file(&orphan, b"{\"partial\":");

    let created = create_context_expansion(&request).expect("publish final artifact");
    assert!(orphan.is_file());
    assert_eq!(
        load_context_expansion(&request, &created.identity).expect("load complete final"),
        created.artifact
    );
}

#[test]
fn streaming_context_validation_checks_invalid_utf8_beyond_a_tiny_retained_prefix() {
    let (_temp, mut request) = fixture();
    let path = request.repository_root.join("src/large.rs");
    let mut file = std::fs::File::create(&path).expect("large file");
    file.write_all(b"valid prefix\n").expect("prefix");
    file.seek(SeekFrom::Start(16 * 1024 * 1024))
        .expect("sparse seek");
    file.write_all(&[0xff]).expect("invalid tail");
    request.context_request.paths = vec!["src/large.rs".to_string()];
    request.limits.max_bytes_per_file = 4;

    assert!(create_context_expansion(&request).is_err());
    assert!(!request
        .run_directory
        .join("artifacts/01-research.attempt-002.context-round-001.json")
        .exists());
}

#[test]
fn loader_rejects_expected_identity_and_link_substitution() {
    let (_temp, request) = fixture();
    let created = create_context_expansion(&request).expect("create");

    let mut wrong = request.clone();
    wrong.run_id = "other-run".to_string();
    assert!(load_context_expansion(&wrong, &created.identity).is_err());
    wrong = request.clone();
    wrong.step = LoopStepName::Analysis;
    assert!(load_context_expansion(&wrong, &created.identity).is_err());
    wrong = request.clone();
    wrong.role = Role::Analyzer;
    assert!(load_context_expansion(&wrong, &created.identity).is_err());
    wrong = request.clone();
    wrong.step_attempt = 3;
    assert!(load_context_expansion(&wrong, &created.identity).is_err());
    wrong = request.clone();
    wrong.context_round = 2;
    assert!(load_context_expansion(&wrong, &created.identity).is_err());
    wrong = request.clone();
    wrong.initial_provider_request.digest = "c".repeat(64);
    assert!(load_context_expansion(&wrong, &created.identity).is_err());
}

#[test]
fn prior_expansion_is_verified_and_reconstructed_without_live_repository_reread() {
    let (_temp, mut first) = fixture();
    first.context_request.paths = vec!["src/a.rs".to_string()];
    let first_created = create_context_expansion(&first).expect("first");
    std::fs::write(
        first.repository_root.join("src/a.rs"),
        "changed live bytes\n",
    )
    .expect("change");
    std::fs::write(first.repository_root.join("src/c.rs"), "gamma\n").expect("c");

    let mut second = first.clone();
    second.context_round = 2;
    second.context_request = ContextRequest {
        paths: vec!["src/c.rs".to_string()],
        reason: "Need follow-up".to_string(),
    };
    second.previous_expansion = Some(first_created.identity.clone());
    let second_created = create_context_expansion(&second).expect("second");
    assert_eq!(second_created.artifact.prior_total_context_bytes, 16);

    let files = reconstruct_context_expansion_files(&second, &second_created.identity)
        .expect("reconstruct chain");
    assert_eq!(
        files
            .iter()
            .find(|file| file.path == "src/a.rs")
            .unwrap()
            .content,
        "alpha\n"
    );

    let first_path = first.run_directory.join(&first_created.identity.path);
    std::fs::write(first_path, b"tampered").expect("tamper prior");
    let mut third = second.clone();
    third.context_round = 3;
    third.previous_expansion = Some(second_created.identity);
    third.context_request.paths = vec!["src/a.rs".to_string()];
    assert!(create_context_expansion(&third).is_err());
}

#[test]
fn prior_chain_recomputes_historical_exclusions_instead_of_trusting_canonical_forgery() {
    let (_temp, mut first) = fixture();
    first.context_request.paths = vec!["src/a.rs".to_string()];
    let created = create_context_expansion(&first).expect("first");
    let path = first.run_directory.join(&created.identity.path);
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("artifact")).expect("json");
    value["context_request"]["paths"] = serde_json::json!(["src/a.rs", "src/b.rs"]);
    value["excluded_loaded_paths"] = serde_json::json!(["src/b.rs"]);
    let forged_bytes = seaf_core::canonical_json_bytes(&value).expect("canonical forgery");
    std::fs::write(&path, &forged_bytes).expect("forge prior");

    std::fs::write(first.repository_root.join("src/c.rs"), "gamma\n").expect("c");
    let mut second = first.clone();
    second.context_round = 2;
    second.context_request = ContextRequest {
        paths: vec!["src/c.rs".to_string()],
        reason: "Need follow-up".to_string(),
    };
    second.previous_expansion = Some(ArtifactReference {
        path: created.identity.path,
        digest: hex::encode(Sha256::digest(&forged_bytes)),
    });

    assert!(create_context_expansion(&second).is_err());
}

#[test]
fn prior_chain_reapplies_bound_forbidden_controls_to_canonical_forgery() {
    let (_temp, mut first) = fixture();
    first.context_request.paths = vec!["src/a.rs".to_string()];
    let created = create_context_expansion(&first).expect("first");
    let path = first.run_directory.join(&created.identity.path);
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("artifact")).expect("json");
    let forbidden_content = b"secret\n";
    let forbidden_digest = hex::encode(Sha256::digest(forbidden_content));
    value["context_request"]["paths"] = serde_json::json!([".env"]);
    value["files"] = serde_json::json!([{
        "path": ".env",
        "content": "secret\n",
        "source_sha256": forbidden_digest,
        "included_sha256": forbidden_digest,
        "source_bytes": forbidden_content.len(),
        "included_bytes": forbidden_content.len(),
        "truncated": false
    }]);
    value["resulting_total_context_bytes"] =
        serde_json::json!(first.initial_context_bytes + forbidden_content.len());
    let forged_bytes = seaf_core::canonical_json_bytes(&value).expect("canonical forgery");
    std::fs::write(&path, &forged_bytes).expect("forge prior");

    std::fs::write(first.repository_root.join("src/c.rs"), "gamma\n").expect("c");
    let mut second = first.clone();
    second.context_round = 2;
    second.context_request = ContextRequest {
        paths: vec!["src/c.rs".to_string()],
        reason: "Need follow-up".to_string(),
    };
    second.previous_expansion = Some(ArtifactReference {
        path: created.identity.path,
        digest: hex::encode(Sha256::digest(&forged_bytes)),
    });

    assert!(create_context_expansion(&second).is_err());
}
