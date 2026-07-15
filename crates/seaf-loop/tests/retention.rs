use std::{fs, path::Path};

use seaf_core::{canonical_json_bytes, canonical_sha256_digest, LoopInputDigests, LoopStatus};
use seaf_loop::{
    load_verified_purge_result, purge_loop_runs,
    state::{create_run, finish_step, save_run, NewLoopRun, LOOP_STEPS},
    LoopWorkspace, PurgeMode, RetentionPolicy,
};

fn create_run_with_status(runs_root: &Path, run_id: &str, status: LoopStatus, updated_at: &str) {
    let workspace = LoopWorkspace::create(runs_root, run_id).expect("workspace");
    let mut run = create_run(NewLoopRun {
        run_id: run_id.to_string(),
        ticket_id: format!("ticket-{run_id}"),
        goal_id: "retention-tests".to_string(),
        provider: "fake".to_string(),
        model: "fake-local".to_string(),
        input_digests: LoopInputDigests {
            ticket: "a".repeat(64),
            policy: "b".repeat(64),
            config: "c".repeat(64),
            repository: "d".repeat(64),
            eval_config: None,
        },
    });
    if status == LoopStatus::Completed {
        for step in LOOP_STEPS {
            finish_step(
                &mut run,
                step,
                seaf_core::LoopStepStatus::Completed,
                None,
                None,
            )
            .expect("complete step");
        }
    }
    assert_eq!(run.status, status);
    run.updated_at = updated_at.to_string();
    save_run(&workspace, &run).expect("save run");
}

#[test]
fn dry_run_is_byte_inert_and_budget_selects_oldest_terminal_run() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    create_run_with_status(&runs_root, "old-completed", LoopStatus::Completed, "10");
    create_run_with_status(&runs_root, "new-completed", LoopStatus::Completed, "20");
    let before = read_tree(&runs_root);

    let all = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::DryRun,
    )
    .expect("dry-run plan");
    let old_bytes = all
        .decision
        .selected
        .iter()
        .find(|run| run.run_id == "old-completed")
        .expect("old selection")
        .bytes;
    let report = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: all.decision.snapshot.managed_bytes - old_bytes,
        },
        PurgeMode::DryRun,
    )
    .expect("budget plan");

    assert_eq!(
        report
            .decision
            .selected
            .iter()
            .map(|run| run.run_id.as_str())
            .collect::<Vec<_>>(),
        ["old-completed"]
    );
    assert!(report.deleted.is_empty());
    assert!(report.converged.is_none());
    assert!(report.audit_path.is_none());
    assert_eq!(read_tree(&runs_root), before, "dry-run must be byte-inert");
}

#[test]
fn active_and_busy_locked_runs_are_never_selected_for_deletion() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    create_run_with_status(&runs_root, "active", LoopStatus::Pending, "1");
    create_run_with_status(&runs_root, "locked", LoopStatus::Completed, "2");
    create_run_with_status(&runs_root, "eligible", LoopStatus::Completed, "3");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(runs_root.join("locked/provider-exchange.lock"))
        .expect("lock file");
    lock.lock().expect("hold run lock");

    let report = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect("purge eligible run");

    assert!(runs_root.join("active").is_dir());
    assert!(runs_root.join("locked").is_dir());
    assert!(!runs_root.join("eligible").exists());
    let converged = report.converged.as_ref().expect("converged evidence");
    assert_eq!(converged.protected_active, vec!["active"]);
    assert_eq!(converged.protected_locked, vec!["locked"]);
    assert_eq!(
        report
            .deleted
            .iter()
            .map(|run| run.run_id.as_str())
            .collect::<Vec<_>>(),
        ["eligible"]
    );
}

#[test]
fn successful_purge_has_a_verified_audit_and_retry_is_idempotent() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    create_run_with_status(&runs_root, "purged", LoopStatus::Completed, "1");

    let report = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect("purge");
    let audit_path = report.audit_path.clone().expect("audit path");
    let audit_bytes = fs::read(&audit_path).expect("audit bytes");
    let verified = load_verified_purge_result(&runs_root).expect("verified audit");
    assert_eq!(verified, report);

    let retry = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect("idempotent retry");
    assert_eq!(retry, report);
    assert_eq!(fs::read(&audit_path).expect("retry audit"), audit_bytes);

    let mut tampered: serde_json::Value = serde_json::from_slice(&audit_bytes).expect("JSON");
    tampered["converged"]["managed_bytes"] = serde_json::json!(1234);
    fs::write(&audit_path, serde_json::to_vec_pretty(&tampered).unwrap()).expect("tamper audit");
    let error = load_verified_purge_result(&runs_root).expect_err("tamper must fail");
    assert!(error.to_string().contains("digest"), "{error}");
}

#[test]
fn equal_update_times_use_run_id_as_the_stable_retention_tie_break() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    create_run_with_status(&runs_root, "tie-a", LoopStatus::Completed, "10");
    create_run_with_status(&runs_root, "tie-b", LoopStatus::Completed, "10");
    let all = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::DryRun,
    )
    .expect("all eligible");
    let first_bytes = all.decision.selected[0].bytes;

    let one = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: all.decision.snapshot.managed_bytes - first_bytes,
        },
        PurgeMode::DryRun,
    )
    .expect("one selected");

    assert_eq!(one.decision.selected.len(), 1);
    assert_eq!(one.decision.selected[0].run_id, "tie-a");
}

#[test]
fn partial_budget_releases_unselected_eligible_guards_before_convergence() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    create_run_with_status(&runs_root, "older", LoopStatus::Completed, "1");
    create_run_with_status(&runs_root, "newer", LoopStatus::Completed, "2");
    let all = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::DryRun,
    )
    .expect("inventory");
    let older_bytes = all
        .decision
        .selected
        .iter()
        .find(|run| run.run_id == "older")
        .unwrap()
        .bytes;
    let policy = RetentionPolicy {
        max_managed_bytes: all.decision.snapshot.managed_bytes - older_bytes,
    };

    let report = purge_loop_runs(&runs_root, policy, PurgeMode::Apply).expect("partial purge");
    assert_eq!(
        report
            .deleted
            .iter()
            .map(|run| run.run_id.as_str())
            .collect::<Vec<_>>(),
        ["older"]
    );
    assert!(runs_root.join("newer").is_dir());
    assert!(
        report
            .converged
            .as_ref()
            .unwrap()
            .protected_locked
            .is_empty(),
        "an invocation-owned observation guard is not external lock authority"
    );
    let audit_bytes = fs::read(runs_root.join(".retention-purge.result.json")).unwrap();
    let retry = purge_loop_runs(&runs_root, policy, PurgeMode::Apply).expect("exact retry");
    assert_eq!(retry, report);
    assert_eq!(
        fs::read(runs_root.join(".retention-purge.result.json")).unwrap(),
        audit_bytes
    );
}

#[test]
fn retention_orders_canonical_unix_seconds_numerically_and_rejects_other_domains() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    create_run_with_status(&runs_root, "time-9", LoopStatus::Completed, "9");
    create_run_with_status(&runs_root, "time-10", LoopStatus::Completed, "10");
    let all = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::DryRun,
    )
    .unwrap();
    let oldest_bytes = all
        .decision
        .selected
        .iter()
        .find(|run| run.run_id == "time-9")
        .unwrap()
        .bytes;
    let one = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: all.decision.snapshot.managed_bytes - oldest_bytes,
        },
        PurgeMode::DryRun,
    )
    .unwrap();
    assert_eq!(one.decision.selected[0].run_id, "time-9");

    create_run_with_status(
        &runs_root,
        "unsupported-time",
        LoopStatus::Completed,
        "2026-07-15T10:00:00Z",
    );
    let error = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect_err("unsupported timestamp domain must fail closed");
    assert!(error.to_string().contains("Unix seconds"), "{error}");
    assert!(!runs_root.join(".retention-purge.intent.json").exists());
}

#[test]
fn verified_audit_path_resolves_against_a_relocated_runs_root() {
    let temp = tempfile::tempdir().expect("temp");
    let original = temp.path().join("runs-original");
    create_run_with_status(&original, "relocated", LoopStatus::Completed, "1");
    purge_loop_runs(
        &original,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect("initial purge");
    let relocated = temp.path().join("runs-relocated");
    fs::rename(&original, &relocated).expect("move runs root");

    let verified = load_verified_purge_result(&relocated).expect("relocated verified result");
    assert_eq!(
        verified.audit_path.as_deref(),
        Some(relocated.join(".retention-purge.result.json").as_path())
    );
    assert!(verified.audit_path.unwrap().is_file());
    let retry = purge_loop_runs(
        &relocated,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect("relocated exact retry");
    assert_eq!(
        retry.audit_path.as_deref(),
        Some(relocated.join(".retention-purge.result.json").as_path())
    );
}

#[test]
fn legacy_absolute_audit_result_remains_verified_and_resolves_to_the_current_root() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    create_run_with_status(&runs_root, "legacy-audit", LoopStatus::Completed, "1");
    purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .unwrap();
    let result_path = runs_root.join(".retention-purge.result.json");
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&fs::read(&result_path).unwrap()).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("continuation_required");
    legacy["audit_path"] = serde_json::json!(result_path.display().to_string());
    legacy["audit_digest"] = serde_json::Value::String(String::new());
    legacy["audit_digest"] = serde_json::Value::String(canonical_sha256_digest(&legacy).unwrap());
    fs::write(&result_path, canonical_json_bytes(&legacy).unwrap()).unwrap();

    let relocated = temp.path().join("relocated");
    fs::rename(&runs_root, &relocated).unwrap();
    let verified = load_verified_purge_result(&relocated).unwrap();
    assert_eq!(
        verified.audit_path.as_deref(),
        Some(relocated.join(".retention-purge.result.json").as_path())
    );
}

#[test]
fn manifest_heavy_supported_inventory_converges_with_bounded_control_files() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    for run_index in 0..3 {
        let run_id = format!("manifest-heavy-{run_index}");
        create_run_with_status(&runs_root, &run_id, LoopStatus::Completed, "1");
        let artifacts = runs_root.join(&run_id).join("artifacts");
        for entry_index in 0..1800 {
            let name = format!("{entry_index:04}-{}", "x".repeat(210));
            let path = artifacts.join(name);
            fs::write(&path, b"x").expect("manifest-heavy artifact");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
    }

    let report = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect("bounded batches must converge a supported manifest-heavy inventory");
    assert_eq!(report.deleted.len(), 3);
    assert!(report.converged.as_ref().unwrap().managed_bytes == 0);
    assert!(
        fs::metadata(runs_root.join(".retention-purge.result.json"))
            .unwrap()
            .len()
            <= 2 * 1024 * 1024
    );
    assert!(!runs_root.join(".retention-purge.intent.json").exists());
}

#[test]
fn exact_operator_entry_cap_allows_bounded_seaf_control_entries() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    fs::create_dir(&runs_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&runs_root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for index in 0..4096 {
        fs::create_dir(runs_root.join(format!(".excluded-{index:04}"))).unwrap();
    }

    let report = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect("SEAF control entries must have a separate bounded allowance");
    assert!(report.deleted.is_empty());
    assert_eq!(
        report
            .converged
            .as_ref()
            .unwrap()
            .excluded_root_entries
            .len(),
        4096
    );
    assert!(load_verified_purge_result(&runs_root).is_ok());
}

#[test]
fn long_authenticated_run_id_uses_a_fixed_length_intent_bound_tombstone() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    let run_id = format!("r{}", "x".repeat(229));
    create_run_with_status(&runs_root, &run_id, LoopStatus::Completed, "1");

    let report = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect("long authenticated run ID must converge through a fixed tombstone name");
    assert_eq!(report.deleted[0].run_id, run_id);
    assert!(!runs_root.join(&run_id).exists());
    assert!(load_verified_purge_result(&runs_root).is_ok());
}

#[cfg(unix)]
#[test]
fn externally_hard_linked_artifact_fails_before_intent_without_unlinking_either_name() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    create_run_with_status(&runs_root, "hard-linked", LoopStatus::Completed, "1");
    let managed = runs_root.join("hard-linked/log.md");
    let external = temp.path().join("external-log.md");
    fs::hard_link(&managed, &external).expect("external hard link");
    let before = fs::read(&managed).expect("managed bytes");

    for mode in [PurgeMode::DryRun, PurgeMode::Apply] {
        let error = purge_loop_runs(
            &runs_root,
            RetentionPolicy {
                max_managed_bytes: 0,
            },
            mode,
        )
        .expect_err("multi-link managed evidence must fail closed");
        assert!(error.to_string().contains("hard-link"), "{mode:?}: {error}");
        assert_eq!(fs::read(&managed).unwrap(), before);
        assert_eq!(fs::read(&external).unwrap(), before);
        assert!(!runs_root.join(".retention-purge.intent.json").exists());
    }
}

#[test]
fn migrated_run_and_dot_prefixed_migration_state_are_preserved_by_policy() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    create_run_with_status(&runs_root, "migrated", LoopStatus::Completed, "1");
    fs::write(
        runs_root.join("migrated/migration-v0-v1.result.json"),
        b"preserved migration result",
    )
    .expect("migration result");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            runs_root.join("migrated/migration-v0-v1.result.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let backup = runs_root.join(".migrated.migration-v0-v1.backup");
    fs::create_dir(&backup).expect("backup namespace");

    let report = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect("protected migration evidence");

    assert!(runs_root.join("migrated").is_dir());
    assert!(backup.is_dir());
    assert_eq!(
        report.decision.snapshot.protected_migration_evidence,
        ["migrated"]
    );
    assert!(report
        .decision
        .snapshot
        .excluded_root_entries
        .contains(&".migrated.migration-v0-v1.backup".to_string()));
    assert!(report
        .converged
        .unwrap()
        .excluded_root_entries
        .contains(&".migrated.migration-v0-v1.backup".to_string()));
}

#[test]
fn malformed_matching_migration_intent_refuses_purge_without_deleting_source() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    create_run_with_status(&runs_root, "pending-migration", LoopStatus::Completed, "1");
    let intent = runs_root.join(".pending-migration.migration-v0-v1.intent.json");
    fs::write(&intent, b"not authenticated migration intent").expect("intent fixture");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&intent, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let error = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect_err("matching unauthenticated migration control must fail closed");

    assert!(error
        .to_string()
        .contains("could not authenticate pending migration state"));
    assert!(runs_root.join("pending-migration").is_dir());
    assert!(!runs_root.join(".retention-purge.intent.json").exists());
}

#[test]
fn unrelated_dot_prefixed_migration_state_does_not_protect_an_eligible_run() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    create_run_with_status(&runs_root, "ordinary", LoopStatus::Completed, "1");
    fs::write(
        runs_root.join(".different-run.migration-v0-v1.intent.json"),
        b"operator-owned excluded state",
    )
    .expect("excluded fixture");

    let report = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect("unrelated dot-prefixed state must not change retention eligibility");

    assert!(!runs_root.join("ordinary").exists());
    assert_eq!(report.deleted[0].run_id, "ordinary");
    assert!(report
        .decision
        .snapshot
        .excluded_root_entries
        .contains(&".different-run.migration-v0-v1.intent.json".to_string()));
}

#[test]
fn changed_excluded_state_creates_fresh_audit_instead_of_returning_stale_result() {
    let temp = tempfile::tempdir().expect("temp");
    let runs_root = temp.path().join("runs");
    create_run_with_status(&runs_root, "purged-once", LoopStatus::Completed, "1");
    let first = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect("first purge");
    fs::create_dir(runs_root.join(".later-migration.backup")).expect("later exclusion");

    let refreshed = purge_loop_runs(
        &runs_root,
        RetentionPolicy {
            max_managed_bytes: 0,
        },
        PurgeMode::Apply,
    )
    .expect("fresh audit for changed exclusions");

    assert_ne!(refreshed.audit_digest, first.audit_digest);
    assert!(refreshed
        .decision
        .snapshot
        .excluded_root_entries
        .contains(&".later-migration.backup".to_string()));
    assert!(refreshed
        .converged
        .unwrap()
        .excluded_root_entries
        .contains(&".later-migration.backup".to_string()));
}

fn read_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn visit(root: &Path, path: &Path, entries: &mut Vec<(String, Vec<u8>)>) {
        let mut children = fs::read_dir(path)
            .expect("read tree")
            .collect::<Result<Vec<_>, _>>()
            .expect("entries");
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let path = child.path();
            if path.is_dir() {
                visit(root, &path, entries);
            } else {
                entries.push((
                    path.strip_prefix(root)
                        .expect("relative")
                        .to_string_lossy()
                        .into_owned(),
                    fs::read(path).expect("file"),
                ));
            }
        }
    }
    let mut entries = Vec::new();
    visit(root, root, &mut entries);
    entries
}
