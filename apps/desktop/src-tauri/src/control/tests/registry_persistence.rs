use super::*;

fn dispatch_test_repo_root() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn captain_identity_and_multiple_project_captains_survive_restart() {
    let path = captains_tmp("multiple-captains-one-project");
    let reg = CaptainsRegistry::load(path.clone());
    reg.upsert_project(ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-shared".into(),
        name: "Shared".into(),
        repo_root: dispatch_test_repo_root(),
        remote_url: None,
        default_branch: Some("main".into()),
        powder: None,
        created_at: 1,
        updated_at: 1,
    })
    .unwrap();
    reg.claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    reg.claim_test("captain-b", Some("beta"), vec!["work-b".into()])
        .unwrap();
    reg.bind_ship_context("alpha", "project-shared", "Assignment A", "codex")
        .unwrap();
    reg.bind_ship_context("beta", "project-shared", "Assignment B", "claude")
        .unwrap();
    reg.rename_captain(Some("captain-a"), None, "  Alpha Lead  ")
        .unwrap();

    let restored = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(restored.captains.len(), 2);
    let alpha = restored
        .captains
        .iter()
        .find(|captain| captain.terminal_id.as_deref() == Some("captain-a"))
        .unwrap();
    let beta = restored
        .captains
        .iter()
        .find(|captain| captain.terminal_id.as_deref() == Some("captain-b"))
        .unwrap();
    assert_eq!(alpha.assignment_id, "assignment:project-shared:alpha");
    assert_eq!(alpha.display_name, "Alpha Lead");
    assert_eq!(beta.assignment_id, "assignment:project-shared:beta");
    assert_ne!(alpha.assignment_id, beta.assignment_id);
    assert!(reg
        .rename_captain(Some("captain-a"), None, &"x".repeat(121))
        .unwrap_err()
        .contains("at most 120 bytes"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn registry_persists_across_reloads_including_seq() {
    let path = captains_tmp("roundtrip");
    {
        let reg = CaptainsRegistry::load(path.clone());
        reg.claim_test("cap-1", Some("alpha"), vec!["tab-1".into()])
            .unwrap();
        reg.record_crew("cap-1", "crew-1").unwrap();
    }
    // A fresh load (an app restart) resumes the same claims AND revision.
    let reg = CaptainsRegistry::load(path.clone());
    let snap = reg.snapshot();
    assert_eq!(snap.seq, 2);
    assert_eq!(snap.captains.len(), 1);
    assert_eq!(snap.captains[0].ship_slug, "alpha");
    assert_eq!(crew_tiles(&snap.captains[0]), vec!["crew-1".to_string()]);
    // And keeps counting monotonically from there.
    reg.claim_test("cap-2", Some("beta"), vec![]).unwrap();
    assert_eq!(CaptainsRegistry::load(path.clone()).snapshot().seq, 3);

    // Atomic write discipline: the temp file is renamed over the target, so
    // no `.tmp` sibling is ever left behind after the writes above.
    let stem = path.file_name().unwrap().to_string_lossy().into_owned();
    let leftover_tmp = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(&stem) && n.ends_with(".tmp")
        });
    assert!(!leftover_tmp, "atomic write must leave no .tmp file behind");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn corrupt_registry_recovers_from_validated_backup_and_quarantines_primary() {
    let path = captains_tmp("backup-recovery");
    let backup = path.with_extension("json.bak");
    {
        let reg = CaptainsRegistry::load(path.clone());
        reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap();
        reg.record_crew("cap-1", "crew-1").unwrap();
        reg.checkpoint(Some("cap-1"), None, None, None, Some("durable checkpoint"))
            .unwrap();
    }
    assert!(
        backup.exists(),
        "a prior validated revision must be retained"
    );
    std::fs::write(&path, b"{ definitely not json").unwrap();

    let recovered = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(recovered.captains.len(), 1);
    assert_eq!(recovered.captains[0].ship_slug, "alpha");
    assert_eq!(crew_tiles(&recovered.captains[0]), vec!["crew-1"]);
    let quarantined = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| {
            entry.file_name().to_string_lossy().starts_with(&format!(
                "{}.corrupt.",
                path.file_name().unwrap().to_string_lossy()
            ))
        });
    assert!(quarantined, "the corrupt primary must be quarantined");
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(backup);
}

#[test]
fn concurrent_distinct_ship_claims_create_distinct_project_captains() {
    let reg = Arc::new(CaptainsRegistry::new());
    reg.upsert_project(ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-one".into(),
        name: "One".into(),
        repo_root: "/tmp".into(),
        remote_url: None,
        default_branch: None,
        powder: None,
        created_at: 0,
        updated_at: 0,
    })
    .unwrap();
    reg.claim_test("cap-a", Some("alpha"), vec![]).unwrap();
    reg.claim_test("cap-b", Some("beta"), vec![]).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut joins = Vec::new();
    for (ship, assignment) in [("alpha", "A"), ("beta", "B")] {
        let reg = Arc::clone(&reg);
        let barrier = Arc::clone(&barrier);
        joins.push(std::thread::spawn(move || {
            barrier.wait();
            reg.bind_ship_context(ship, "project-one", assignment, "codex")
        }));
    }
    barrier.wait();
    let results = joins
        .into_iter()
        .map(|join| join.join().unwrap())
        .collect::<Vec<_>>();
    assert!(results.iter().all(Result::is_ok));

    let snapshot = reg.snapshot();
    let project_captains = snapshot
        .captains
        .iter()
        .filter(|captain| captain.project_id.as_deref() == Some("project-one"))
        .collect::<Vec<_>>();
    assert_eq!(project_captains.len(), 2);
    assert_ne!(
        project_captains[0].assignment_id,
        project_captains[1].assignment_id
    );
    let mut ship_slugs = project_captains
        .iter()
        .map(|captain| captain.ship_slug.as_str())
        .collect::<Vec<_>>();
    ship_slugs.sort_unstable();
    assert_eq!(ship_slugs, vec!["alpha", "beta"]);
}

#[test]
fn concurrent_equivalent_project_registrations_dedupe_canonical_identity() {
    let reg = Arc::new(CaptainsRegistry::new());
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let roots = [
        "/tmp/t-hub-equivalent/./root",
        "/tmp/t-hub-equivalent/root/",
    ];
    let joins = roots
        .into_iter()
        .enumerate()
        .map(|(index, root)| {
            let reg = Arc::clone(&reg);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                reg.upsert_project(ProjectRecord {
                    root_path: Some(root.into()),
                    vcs_capability: Some("none".into()),
                    git_main_root: None,
                    project_id: format!("project-{index}"),
                    name: format!("Project {index}"),
                    repo_root: root.into(),
                    remote_url: None,
                    default_branch: None,
                    powder: None,
                    created_at: 0,
                    updated_at: 0,
                })
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for join in joins {
        join.join().unwrap().unwrap();
    }
    let projects = reg.projects();
    assert_eq!(projects.len(), 1);
    assert_eq!(
        projects[0].root_path.as_deref(),
        Some("/tmp/t-hub-equivalent/root")
    );
    assert_eq!(projects[0].vcs_capability.as_deref(), Some("none"));
}

#[test]
fn linked_worktree_project_identity_keeps_selected_root_separate_from_git_main_root() {
    let registry = CaptainsRegistry::new();
    let project = registry
        .upsert_project(ProjectRecord {
            root_path: Some("/home/natkins/project/.claude/worktrees/feature".into()),
            vcs_capability: Some("git".into()),
            git_main_root: Some("/home/natkins/project".into()),
            project_id: "linked-project".into(),
            name: "Linked Project".into(),
            repo_root: "/home/natkins/project/.claude/worktrees/feature".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    assert_eq!(
        project.root_path.as_deref(),
        Some("/home/natkins/project/.claude/worktrees/feature")
    );
    assert_eq!(
        project.git_main_root.as_deref(),
        Some("/home/natkins/project")
    );
    assert_eq!(registry.projects()[0], project);
}

#[test]
fn distinct_linked_roots_do_not_dedupe_on_shared_git_main_root() {
    let registry = CaptainsRegistry::new();
    for (id, root) in [
        ("linked-a", "/home/natkins/project/.claude/worktrees/a"),
        ("linked-b", "/home/natkins/project/.claude/worktrees/b"),
    ] {
        registry
            .upsert_project(ProjectRecord {
                root_path: Some(root.into()),
                vcs_capability: Some("git".into()),
                git_main_root: Some("/home/natkins/project".into()),
                project_id: id.into(),
                name: id.into(),
                repo_root: root.into(),
                remote_url: None,
                default_branch: Some("main".into()),
                powder: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
    }
    assert_eq!(registry.projects().len(), 2);
    assert_eq!(
        registry
            .projects()
            .iter()
            .map(|project| project.root_path.clone().unwrap())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        2
    );
}

#[test]
fn current_schema_migration_preserves_linked_worktree_identity_metadata() {
    let path = captains_tmp("linked-migration");
    std::fs::write(
        &path,
        json!({
            "schemaVersion": CAPTAINS_SCHEMA_VERSION,
            "seq": 1,
            "captains": [],
            "projects": [{
                "projectId": "linked-project",
                "name": "Linked Project",
                "repoRoot": "/home/natkins/project/.claude/worktrees/feature",
                "rootPath": "/home/natkins/project/.claude/worktrees/feature",
                "vcsCapability": "git",
                "gitMainRoot": "/home/natkins/project",
                "createdAt": 1,
                "updatedAt": 1
            }],
            "workspaces": [{
                "id": "captains-reserved",
                "name": "Captain Workspace",
                "kind": "captain",
                "tileIds": []
            }]
        })
        .to_string(),
    )
    .unwrap();
    let project = CaptainsRegistry::load(path.clone()).projects()[0].clone();
    assert_eq!(
        project.root_path.as_deref(),
        Some("/home/natkins/project/.claude/worktrees/feature")
    );
    assert_eq!(
        project.git_main_root.as_deref(),
        Some("/home/natkins/project")
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn targeted_provision_rollback_preserves_unrelated_concurrent_mutation() {
    let reg = CaptainsRegistry::new();
    let claimed = reg
        .claim_test("cap-a", Some("alpha"), vec![])
        .unwrap()
        .record;
    reg.claim_test("cap-b", Some("beta"), vec![]).unwrap();
    reg.rollback_provisioned_claim("cap-a", &claimed, None)
        .unwrap();
    let snapshot = reg.snapshot();
    assert!(snapshot
        .captains
        .iter()
        .any(|captain| captain.ship_slug == "beta"));
    assert!(!snapshot
        .captains
        .iter()
        .any(|captain| captain.ship_slug == "alpha"));
}

#[test]
fn registry_mutation_fails_and_restores_memory_when_persistence_fails() {
    let blocker = captains_tmp("persist-blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let reg = CaptainsRegistry::load(blocker.join("captains.json"));

    let error = reg
        .claim_test("cap-1", Some("alpha"), vec!["tab-1".into()])
        .unwrap_err();

    assert!(error.contains("could not be created"), "got: {error}");
    assert_eq!(reg.snapshot().seq, 0);
    assert!(reg.snapshot().captains.is_empty());
    std::fs::remove_file(blocker).unwrap();
}

#[test]
fn captain_and_crew_checkpoints_survive_registry_reload() {
    let path = captains_tmp("checkpoint-roundtrip");
    let _ = std::fs::remove_file(&path);
    let reg = CaptainsRegistry::load(path.clone());
    reg.claim_test("captain-1", Some("checkpoint-ship"), vec![])
        .unwrap();
    reg.record_crew("captain-1", "crew-1").unwrap();
    reg.checkpoint(
        None,
        Some("checkpoint-ship"),
        None,
        Some("thread-captain"),
        Some("Review Crew result, then update Powder."),
    )
    .unwrap();
    reg.checkpoint(
        Some("captain-1"),
        None,
        Some("crew-1"),
        Some("thread-crew"),
        Some("Implementing persistence tests."),
    )
    .unwrap();

    let restored = CaptainsRegistry::load(path.clone()).snapshot();
    let captain = &restored.captains[0];
    assert_eq!(captain.conversation_id.as_deref(), Some("thread-captain"));
    assert_eq!(
        captain.resume_point.as_deref(),
        Some("Review Crew result, then update Powder.")
    );
    assert_eq!(
        captain.crew[0].conversation_id.as_deref(),
        Some("thread-crew")
    );
    assert_eq!(
        captain.crew[0].resume_point.as_deref(),
        Some("Implementing persistence tests.")
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn captain_checkpoint_command_updates_the_manifest() {
    let ctx = test_ctx("secret");
    ctx.captains
        .claim_test("captain-checkpoint", Some("checkpoint-command"), vec![])
        .unwrap();

    let response = dispatch(
        &ctx,
        "captain_checkpoint",
        &json!({
            "shipSlug": "checkpoint-command",
            "conversationId": "thread-123",
            "resumePoint": "Resume by reconciling Powder events."
        }),
    )
    .unwrap();

    assert_eq!(response["accepted"], "captain_checkpoint");
    assert_eq!(response["target"], "captain");
    assert_eq!(response["captain"]["conversationId"], "thread-123");
    assert_eq!(
        response["captain"]["resumePoint"],
        "Resume by reconciling Powder events."
    );
}

#[test]
fn corrupt_or_missing_persistence_starts_empty() {
    let missing = CaptainsRegistry::load(captains_tmp("missing"));
    assert_eq!(missing.snapshot().seq, 0);
    assert!(missing.snapshot().captains.is_empty());

    let path = captains_tmp("corrupt");
    std::fs::write(&path, b"{not json").unwrap();
    let reg = CaptainsRegistry::load(path.clone());
    assert!(reg.snapshot().captains.is_empty());
    // The first mutation heals the file.
    reg.claim_test("cap-1", None, vec![]).unwrap();
    let healed = CaptainsRegistry::load(path.clone());
    assert_eq!(healed.snapshot().captains.len(), 1);
    let _ = std::fs::remove_file(&path);
}

// -----------------------------------------------------------------------
// Incident D: captains persistence no longer holds the registry lock
// -----------------------------------------------------------------------

#[test]
fn captains_persist_writes_through_off_the_lock() {
    // The write-through still happens (durability preserved), now via the
    // off-lock `persist` path.
    let dir = std::env::temp_dir().join(format!("t-hub-captains-persist-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("captains.json");
    let _ = std::fs::remove_file(&path);
    let reg = CaptainsRegistry::load(path.clone());
    reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap();
    let body = std::fs::read_to_string(&path).expect("captains.json written through");
    assert!(
        body.contains("alpha"),
        "persisted body must carry the claim: {body}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn captains_persist_is_monotonic_and_drops_a_stale_snapshot() {
    // Two writers that dropped `inner` in one order but reach disk in the other
    // must not regress the file: an older-seq snapshot is dropped.
    let dir = std::env::temp_dir().join(format!("t-hub-captains-mono-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("captains.json");
    let _ = std::fs::remove_file(&path);
    let reg = CaptainsRegistry::load(path.clone());
    reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap(); // seq -> 1 on disk
    let newer = reg.snapshot(); // seq 1
                                // Hand-persist a STALE snapshot (seq 0): it must be dropped, not clobber.
    reg.persist(CaptainsSnapshot {
        schema_version: CAPTAINS_SCHEMA_VERSION,
        seq: 0,
        captains: vec![],
        cortana: crate::cortana_reconcile::CortanaDurableIdentity::default(),
        agent_sessions: vec![],
        agent_checkpoints: vec![],
        agent_events: vec![],
        projects: vec![],
        workspaces: vec![],
        pending_fleet_operations: vec![],
        retired_fleet_tile_ids: vec![],
        pending_dispatch_claims: vec![],
        pending_dispatch_releases: vec![],
        pending_git_initializations: vec![],
    })
    .unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        body.contains("alpha"),
        "stale seq-0 snapshot must not clobber the claim: {body}"
    );
    // A NEWER snapshot (seq 1, already on disk) is allowed to (re)write.
    reg.persist(newer).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn project_powder_and_ship_context_survive_registry_reload() {
    let path = captains_tmp("project-context");
    let _ = std::fs::remove_file(&path);
    let reg = CaptainsRegistry::load(path.clone());
    let project = reg
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-thub".into(),
            name: "T-Hub".into(),
            repo_root: "/home/test/t-hub".into(),
            remote_url: Some("https://example.test/t-hub.git".into()),
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "t-hub".into(),
                event_cursor: 0,
            }),
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
    let project = reg
        .advance_project_powder_cursor(&project.project_id, "production", "t-hub", 17)
        .unwrap();
    reg.claim_test("cap-1", Some("t-hub"), vec![]).unwrap();
    reg.bind_ship_context("t-hub", &project.project_id, "Own T-Hub stability", "codex")
        .unwrap();

    let restored = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(restored.schema_version, CAPTAINS_SCHEMA_VERSION);
    assert_eq!(restored.projects, vec![project]);
    let captain = restored
        .captains
        .iter()
        .find(|c| c.ship_slug == "t-hub")
        .unwrap();
    assert_eq!(captain.project_id.as_deref(), Some("project-thub"));
    assert_eq!(captain.assignment.as_deref(), Some("Own T-Hub stability"));
    assert_eq!(captain.harness.as_deref(), Some("codex"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn project_registry_rejects_split_identity_and_invalid_powder_binding() {
    let reg = CaptainsRegistry::new();
    let base = ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-one".into(),
        name: "One".into(),
        repo_root: "/repo/one".into(),
        remote_url: None,
        default_branch: None,
        powder: None,
        created_at: 0,
        updated_at: 0,
    };
    reg.upsert_project(base.clone()).unwrap();

    let mut repointed = base.clone();
    repointed.repo_root = "/repo/two".into();
    assert!(reg
        .upsert_project(repointed)
        .unwrap_err()
        .contains("already bound"));

    let mut duplicate_root = base.clone();
    duplicate_root.project_id = "project-two".into();
    let updated = reg.upsert_project(duplicate_root).unwrap();
    assert_eq!(updated.project_id, "project-one");
    assert_eq!(reg.projects().len(), 1);

    let mut invalid_powder = base;
    invalid_powder.powder = Some(PowderProjectBinding {
        connection_profile: "default".into(),
        repository: " ".into(),
        event_cursor: 0,
    });
    assert!(reg
        .upsert_project(invalid_powder)
        .unwrap_err()
        .contains("Powder"));
}

#[test]
fn a_stalled_persist_keeps_the_previous_snapshot_readable() {
    // The core Incident-D proof: with persistence moved OFF the `inner` lock, a
    // STALLED disk write (here a hook that blocks while holding only the
    // `persist` mutex) must NOT block a concurrent reader that only touches
    // `inner`. Under the OLD code (persist under the registry lock) the
    // reader below would hang for the duration of the stall - so this test would
    // TIME OUT and fail, which is exactly the regression guard we want.
    use std::sync::mpsc;
    let dir = std::env::temp_dir().join(format!("t-hub-captains-stall-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("captains.json");
    let _ = std::fs::remove_file(&path);
    let reg = Arc::new(CaptainsRegistry::load(path));

    // The hook stands in for a stalled OneDrive-backed write: it signals that a
    // persist is in progress, then blocks (holding `persist`, NOT `inner`) until
    // the test releases it.
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = StdMutex::new(release_rx);
    reg.set_persist_hook(Box::new(move || {
        let _ = started_tx.send(());
        let _ = release_rx.lock().unwrap().recv(); // block: the write is stalled
    }));

    // A background mutator builds a candidate and stalls while persisting it.
    // The prior snapshot remains published and `inner` is free while this stalls.
    let writer_reg = reg.clone();
    let writer = std::thread::spawn(move || {
        writer_reg
            .claim_test("cap-1", Some("alpha"), vec![])
            .unwrap();
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the persist hook should have started (mutation reached persist)");

    // NOW, while the persist is stalled: a concurrent reader must return promptly
    // (it only takes `inner`). Run it on a thread so a REGRESSION (reader blocked
    // on `inner`) surfaces as a timeout instead of hanging the suite forever.
    let reader_reg = reg.clone();
    let (read_tx, read_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let snap = reader_reg.snapshot();
        let _ = read_tx.send(snap.captains.len());
    });
    let n = read_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("a reader was BLOCKED by a stalled persist (regression: persist holds `inner`)");
    assert_eq!(n, 0, "the reader sees only the last durable snapshot");

    // Release the stalled write; the mutator finishes cleanly.
    let _ = release_tx.send(());
    writer.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
