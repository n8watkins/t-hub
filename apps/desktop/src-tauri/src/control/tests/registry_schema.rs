use super::*;

#[test]
fn legacy_v0_captains_json_migrates_in_place() {
    // D2/MED-6: the versioned reader accepts the legacy shape (captainSessionId +
    // crew: [string], no role/state) AND special-cases the cortana slug -> the
    // first-class Cortana singleton, seeded from the live incumbent.
    let path = captains_tmp("legacy-v0");
    let legacy = serde_json::json!({
        "seq": 5,
        "captains": [
            { "shipSlug": "cortana", "captainSessionId": "cor-x", "crew": ["c1", "c2"] },
            { "shipSlug": "t-hub-app", "captainSessionId": "cap-y", "workspaceTabIds": ["t1"], "crew": [] }
        ]
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

    let reg = CaptainsRegistry::load(path.clone());
    let snap = reg.snapshot();
    assert_eq!(snap.seq, 5, "seq preserved across the migration");
    let cor = snap
        .captains
        .iter()
        .find(|c| c.ship_slug == "cortana")
        .unwrap();
    assert_eq!(
        cor.role,
        FleetRole::Cortana,
        "legacy cortana slug seeds the singleton role"
    );
    assert_eq!(
        cor.terminal_id.as_deref(),
        Some("cor-x"),
        "captainSessionId -> terminal_id"
    );
    assert_eq!(cor.state, ClaimState::Active);
    assert_eq!(
        crew_tiles(cor),
        vec!["c1".to_string(), "c2".to_string()],
        "crew strings -> CrewRef"
    );
    assert!(cor.crew.iter().all(|c| c.state == CrewState::Active));
    let cap = snap
        .captains
        .iter()
        .find(|c| c.ship_slug == "t-hub-app")
        .unwrap();
    assert_eq!(
        cap.role,
        FleetRole::Captain,
        "a normal ship stays a Captain"
    );
    assert_eq!(cap.assignment_id, "assignment:unbound:t-hub-app");
    assert_eq!(cap.display_name, "t-hub-app");
    assert_eq!(cap.workspace_tab_ids, vec!["t1"]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn future_registry_schema_is_preserved_and_blocks_writes() {
    let path = captains_tmp("future-schema");
    let body = json!({
        "schemaVersion": CAPTAINS_SCHEMA_VERSION + 1,
        "seq": 99,
        "captains": [],
        "projects": [],
        "futureField": {"must": "survive"},
    })
    .to_string();
    std::fs::write(&path, &body).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    assert!(registry.write_blocked.is_some());
    let error = registry
        .claim_test("cap-future", Some("future"), vec![])
        .unwrap_err();
    assert!(error.contains("read-only"), "got: {error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), body);

    let prefix = format!("{}.corrupt.", path.file_name().unwrap().to_string_lossy());
    let quarantined = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix));
    assert!(!quarantined, "future schemas must never be quarantined");
    let _ = std::fs::remove_file(path);
}

#[test]
fn orphan_replacement_requires_its_exact_registry_schema() {
    let path = captains_tmp("orphan-replacement-old-schema");
    std::fs::write(
        &path,
        json!({
            "schemaVersion": 22,
            "cortana": {
                "recovery": {
                    "kind": "replacingOrphan"
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    assert!(matches!(
        CaptainsRegistry::read_snapshot(&path),
        Err(SnapshotReadError::IncompatibleRecovery { .. })
    ));

    std::fs::write(
        &path,
        json!({
            "schemaVersion": CAPTAINS_SCHEMA_VERSION,
            "cortana": {
                "identityId": "legacy-orphan-identity",
                "generation": 1,
                "terminalId": "a1b2c3d4",
                "harness": "codex",
                "recovery": {
                    "kind": "replacingOrphan",
                    "operation_id": "missing-effect-identity",
                    "started_at": 1,
                    "orphan_terminal_id": "a1b2c3d4",
                    "orphan_identity_id": "legacy-orphan-identity",
                    "orphan_generation": 1,
                    "harness": "codex"
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    assert!(matches!(
        CaptainsRegistry::read_snapshot(&path),
        Err(SnapshotReadError::IncompatibleRecovery { .. })
    ));
    std::fs::remove_file(path).ok();
}

#[test]
fn schema_v12_without_release_recovery_upgrades_to_v13_on_the_next_write() {
    let path = captains_tmp("schema-v12-upgrade");
    let _ = std::fs::remove_file(&path);
    let legacy = CaptainsSnapshot {
        schema_version: 12,
        seq: 1,
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
    };
    std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

    assert_eq!(
        CaptainsRegistry::read_snapshot(&path)
            .unwrap()
            .schema_version,
        12
    );
    let registry = CaptainsRegistry::load(path.clone());
    registry
        .claim_test("captain-v13", Some("schema-v13"), vec![])
        .unwrap();
    let persisted: CaptainsSnapshot =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(persisted.schema_version, CAPTAINS_SCHEMA_VERSION);
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn schema_13_fixture_loads_without_network_access() {
    let path = captains_tmp("schema-13-fixture-load");
    std::fs::write(&path, SCHEMA_13_REGISTRY_FIXTURE).unwrap();

    let snapshot = CaptainsRegistry::read_snapshot(&path).unwrap();
    assert_eq!(snapshot.schema_version, 13);
    assert_eq!(snapshot.captains[0].ship_slug, "aurora");
    assert_eq!(
        snapshot.captains[0].crew[0].terminal_id,
        "tile-aurora-worker"
    );
    assert_eq!(
        snapshot.projects[0].remote_url.as_deref(),
        Some("https://example.invalid/aurora.git")
    );
    assert_eq!(
        snapshot.projects[0].root_path.as_deref(),
        Some("/sanitized/workspaces/aurora")
    );
    assert_eq!(snapshot.projects[0].vcs_capability.as_deref(), Some("git"));
    assert_eq!(
        snapshot.projects[0].git_main_root.as_deref(),
        Some("/sanitized/workspaces/aurora")
    );

    let loaded = CaptainsRegistry::load(path.clone());
    assert_eq!(loaded.snapshot().seq, 41);
    assert!(loaded.snapshot().agent_sessions.is_empty());

    let _ = std::fs::remove_file(path);
}

#[test]
fn schema_13_fixture_first_write_creates_migration_backup() {
    let path = captains_tmp("schema-13-fixture-migration-backup");
    std::fs::write(&path, SCHEMA_13_REGISTRY_FIXTURE).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    registry
        .claim_provider(
            "tile-lyra-captain",
            Some("lyra"),
            FleetRole::Captain,
            Some("codex"),
            None,
            vec![],
            &|_| false,
            &|_| tmux::SessionLiveness::Alive,
        )
        .unwrap();

    let file_name = path.file_name().unwrap().to_string_lossy();
    let prefix = format!("{file_name}.migration-v{CAPTAINS_SCHEMA_VERSION}.");
    let backups: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        std::fs::read_to_string(backups[0].path()).unwrap(),
        SCHEMA_13_REGISTRY_FIXTURE
    );

    for backup in backups {
        let _ = std::fs::remove_file(backup.path());
    }
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn schema_17_fixture_loads_without_network_access() {
    let path = captains_tmp("schema-17-fixture-load");
    std::fs::write(&path, SCHEMA_17_REGISTRY_FIXTURE).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.schema_version, CAPTAINS_SCHEMA_VERSION);
    assert_eq!(snapshot.seq, 108);
    assert_eq!(
        snapshot.agent_sessions[0].agent_session_id,
        "agent-aurora-17"
    );
    assert_eq!(snapshot.agent_checkpoints[0].cursor, 7);
    assert_eq!(snapshot.agent_events[0].kind, "checkpoint");

    let _ = std::fs::remove_file(path);
}

#[test]
fn schema_18_fixture_migrates_project_identity_and_pending_release_losslessly() {
    let path = captains_tmp("schema-18-fixture-load");
    std::fs::write(&path, SCHEMA_18_REGISTRY_FIXTURE).unwrap();
    let original: Value = serde_json::from_str(SCHEMA_18_REGISTRY_FIXTURE).unwrap();
    let mut diagnostic: CaptainsSnapshot = serde_json::from_value(original.clone()).unwrap();
    migrate_project_identities(&mut diagnostic).unwrap();
    if let Err(error) = CaptainsRegistry::validate_snapshot(&diagnostic) {
        panic!("schema-v18 fixture validation failed: {error}");
    }

    let snapshot = CaptainsRegistry::read_snapshot(&path).unwrap();
    assert_eq!(snapshot.schema_version, 18);
    assert_eq!(snapshot.seq, original["seq"].as_u64().unwrap());
    assert_eq!(
        snapshot.projects[0].root_path.as_deref(),
        Some("/sanitized/workspaces/aurora")
    );
    assert_eq!(
        snapshot.projects[0].repo_root,
        "/sanitized/workspaces/aurora"
    );
    assert_eq!(snapshot.projects[0].vcs_capability.as_deref(), Some("git"));
    assert_eq!(
        snapshot.projects[0].git_main_root.as_deref(),
        Some("/sanitized/workspaces/aurora")
    );
    assert_eq!(
        snapshot.captains[0].project_id.as_deref(),
        Some("project-aurora")
    );
    assert_eq!(
        snapshot.captains[0].crew[0].conversation_id.as_deref(),
        Some("conversation-aurora-worker-18")
    );
    assert_eq!(
        snapshot.agent_sessions[0].agent_session_id,
        "agent-aurora-18"
    );
    assert_eq!(snapshot.agent_checkpoints[0].cursor, 7);
    assert_eq!(snapshot.workspaces[1].id, "workspace-aurora");
    assert_eq!(snapshot.pending_dispatch_releases.len(), 1);

    let registry = CaptainsRegistry::load(path.clone());
    let _ = registry
        .claim_test("schema-18-reload", Some("schema-18-reload"), vec![])
        .unwrap();
    let persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(persisted["schemaVersion"], CAPTAINS_SCHEMA_VERSION);
    let file_name = path.file_name().unwrap().to_string_lossy();
    let prefix = format!("{file_name}.migration-v{CAPTAINS_SCHEMA_VERSION}.");
    let backups: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        std::fs::read_to_string(backups[0].path()).unwrap(),
        SCHEMA_18_REGISTRY_FIXTURE
    );

    let reloaded = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(
        reloaded.projects[0].root_path.as_deref(),
        Some("/sanitized/workspaces/aurora")
    );
    assert_eq!(reloaded.pending_dispatch_releases.len(), 1);
    for backup in backups {
        let _ = std::fs::remove_file(backup.path());
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn first_schema_v18_write_creates_a_timestamped_migration_backup() {
    let path = captains_tmp("schema-v18-migration-backup");
    let legacy = CaptainsSnapshot {
        schema_version: 17,
        seq: 1,
        captains: vec![],
        cortana: crate::cortana_reconcile::CortanaDurableIdentity::default(),
        agent_sessions: vec![],
        agent_checkpoints: vec![],
        agent_events: vec![],
        projects: vec![],
        workspaces: vec![FleetWorkspaceRecord::captain_workspace()],
        pending_fleet_operations: vec![],
        retired_fleet_tile_ids: vec![],
        pending_dispatch_claims: vec![],
        pending_dispatch_releases: vec![],
        pending_git_initializations: vec![],
    };
    std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    registry
        .claim_test("migration-backup-captain", None, vec![])
        .unwrap();

    let file_name = path.file_name().unwrap().to_string_lossy();
    let prefix = format!("{file_name}.migration-v{CAPTAINS_SCHEMA_VERSION}.");
    let backups: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .collect();
    assert_eq!(backups.len(), 1);
    let backup_body = std::fs::read_to_string(backups[0].path()).unwrap();
    assert_eq!(backup_body, serde_json::to_string(&legacy).unwrap());

    for backup in backups {
        let _ = std::fs::remove_file(backup.path());
    }
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

fn pending_release_snapshot_document(profile: &str, crew_session_id: &str) -> Value {
    let registry = powder_lifecycle_registry_with_profile_and_crew(None, profile, crew_session_id);
    let mut snapshot = registry.snapshot();
    let crew = &mut snapshot.captains[0].crew[0];
    crew.state = CrewState::CleanupPending { since: 1 };
    let work = crew.powder_work.as_mut().unwrap();
    work.dispatch_release_recovery = true;
    snapshot
        .pending_dispatch_releases
        .push(PendingDispatchRelease {
            crew_session_id: crew.terminal_id.clone(),
            project_id: "project-powder-lifecycle".into(),
            connection_profile: profile.into(),
            connection_endpoint_identity: format!("hmac-sha256:{}", "0".repeat(64)),
            repository: "t-hub".into(),
            card_id: work.card_id.clone(),
            run_id: work.run_id.clone(),
            agent: work.agent.clone().unwrap(),
            operation_id: "initial-claim:actor-t-hub:incompatible-load".into(),
            created_at: 1,
            state: PendingDispatchReleaseState::InFlight,
        });
    serde_json::to_value(snapshot).unwrap()
}

fn assert_incompatible_release_load_blocks_actions(
    path: &Path,
    primary_body: &str,
    backup_body: Option<&str>,
) {
    let backup = path.with_extension("json.bak");
    let registry = Arc::new(CaptainsRegistry::load(path.to_path_buf()));
    assert!(registry.write_blocked.is_some());
    assert!(registry.snapshot().captains.is_empty());
    assert!(registry.snapshot().pending_dispatch_releases.is_empty());
    assert!(registry
        .claim_test("blocked-captain", Some("blocked-ship"), vec![])
        .unwrap_err()
        .contains("read-only"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), primary_body);
    match backup_body {
        Some(body) => assert_eq!(std::fs::read_to_string(&backup).unwrap(), body),
        None => assert!(!backup.exists()),
    }
    let prefix = format!("{}.corrupt.", path.file_name().unwrap().to_string_lossy());
    assert!(
        !std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix)),
        "incompatible release state must never be quarantined"
    );
}

#[test]
fn production_load_blocks_v11_release_recovery_without_backup() {
    let path = captains_tmp("incompatible-v11-release-primary");
    let _ = std::fs::remove_file(&path);
    let profile = "incompatible-v11-primary-profile";
    let mut document = pending_release_snapshot_document(profile, "incompatible-v11-primary-crew");
    document["schemaVersion"] = json!(11);
    document["pendingDispatchReleases"][0]
        .as_object_mut()
        .unwrap()
        .remove("connectionEndpointIdentity");
    let body = serde_json::to_string(&document).unwrap();
    std::fs::write(&path, &body).unwrap();

    assert_incompatible_release_load_blocks_actions(&path, &body, None);
    let _ = std::fs::remove_file(path);
}

#[test]
fn production_load_blocks_v12_unsalted_endpoint_digest_recovery() {
    let path = captains_tmp("incompatible-v12-unsalted-release-primary");
    let _ = std::fs::remove_file(&path);
    let profile = "incompatible-v12-unsalted-profile";
    let mut document = pending_release_snapshot_document(profile, "incompatible-v12-unsalted-crew");
    document["schemaVersion"] = json!(12);
    let release = document["pendingDispatchReleases"][0]
        .as_object_mut()
        .unwrap();
    release.remove("connectionEndpointIdentity");
    release.insert(
        "connectionEndpointDigest".into(),
        json!(format!("sha256:{}", "0".repeat(64))),
    );
    let body = serde_json::to_string(&document).unwrap();
    std::fs::write(&path, &body).unwrap();

    assert_incompatible_release_load_blocks_actions(&path, &body, None);
    let _ = std::fs::remove_file(path);
}

#[test]
fn production_load_preserves_v11_release_primary_over_clean_stale_backup() {
    let path = captains_tmp("incompatible-v11-release-primary-backup");
    let backup = path.with_extension("json.bak");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&backup);
    let profile = "incompatible-v11-backup-profile";
    let mut document = pending_release_snapshot_document(profile, "incompatible-v11-backup-crew");
    document["schemaVersion"] = json!(11);
    document["pendingDispatchReleases"][0]
        .as_object_mut()
        .unwrap()
        .remove("connectionEndpointIdentity");
    let primary_body = serde_json::to_string(&document).unwrap();
    let backup_body = json!({
        "schemaVersion": 11,
        "seq": 1,
        "captains": [],
        "projects": [],
        "pendingDispatchClaims": [],
        "pendingDispatchReleases": [],
    })
    .to_string();
    std::fs::write(&path, &primary_body).unwrap();
    std::fs::write(&backup, &backup_body).unwrap();

    assert_incompatible_release_load_blocks_actions(&path, &primary_body, Some(&backup_body));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(backup);
}

#[test]
fn production_load_blocks_actions_when_backup_has_incompatible_release_recovery() {
    let path = captains_tmp("incompatible-release-backup");
    let backup = path.with_extension("json.bak");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&backup);
    let profile = "incompatible-backup-profile";
    let primary_body = json!({
        "schemaVersion": CAPTAINS_SCHEMA_VERSION,
        "seq": 9,
        "captains": [],
        "projects": [],
        "pendingDispatchClaims": [],
        "pendingDispatchReleases": [],
    })
    .to_string();
    let mut document = pending_release_snapshot_document(profile, "incompatible-backup-crew");
    document["schemaVersion"] = json!(11);
    document["pendingDispatchReleases"][0]
        .as_object_mut()
        .unwrap()
        .remove("connectionEndpointIdentity");
    let backup_body = serde_json::to_string(&document).unwrap();
    std::fs::write(&path, &primary_body).unwrap();
    std::fs::write(&backup, &backup_body).unwrap();

    assert_incompatible_release_load_blocks_actions(&path, &primary_body, Some(&backup_body));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(backup);
}

#[test]
fn production_load_rejects_raw_endpoint_field_in_schema_v13_release_recovery() {
    let path = captains_tmp("incompatible-raw-release-endpoint");
    let _ = std::fs::remove_file(&path);
    let profile = "incompatible-raw-profile";
    let mut document = pending_release_snapshot_document(profile, "incompatible-raw-crew");
    document["pendingDispatchReleases"][0]
        .as_object_mut()
        .unwrap()
        .insert(
            "connectionEndpoint".into(),
            json!("http://gateway.invalid/path-token?access_token=query-token#fragment-token"),
        );
    let body = serde_json::to_string(&document).unwrap();
    std::fs::write(&path, &body).unwrap();

    assert_incompatible_release_load_blocks_actions(&path, &body, None);
    let _ = std::fs::remove_file(path);
}

#[test]
fn pre_v13_release_recovery_is_rejected_before_any_recovery_can_run() {
    let path = captains_tmp("schema-v12-release-recovery");
    let _ = std::fs::remove_file(&path);
    let registry = powder_lifecycle_registry_with_profile_and_crew(
        None,
        "legacy-release-profile",
        "legacy-release-crew",
    );
    let mut snapshot = registry.snapshot();
    let crew = &mut snapshot.captains[0].crew[0];
    crew.state = CrewState::CleanupPending { since: 1 };
    let work = crew.powder_work.as_mut().unwrap();
    work.dispatch_release_recovery = true;
    snapshot
        .pending_dispatch_releases
        .push(PendingDispatchRelease {
            crew_session_id: crew.terminal_id.clone(),
            project_id: "project-powder-lifecycle".into(),
            connection_profile: "legacy-release-profile".into(),
            connection_endpoint_identity: format!("hmac-sha256:{}", "0".repeat(64)),
            repository: "t-hub".into(),
            card_id: work.card_id.clone(),
            run_id: work.run_id.clone(),
            agent: work.agent.clone().unwrap(),
            operation_id: "initial-claim:actor-t-hub:legacy".into(),
            created_at: 1,
            state: PendingDispatchReleaseState::InFlight,
        });
    snapshot.schema_version = 12;
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();

    let error = CaptainsRegistry::read_snapshot(&path).unwrap_err();
    assert!(error
        .to_string()
        .contains("dispatch release recovery state incompatible"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn schema_v11_raw_endpoint_release_recovery_fails_closed_before_network() {
    let path = captains_tmp("schema-v11-raw-release-endpoint");
    let _ = std::fs::remove_file(&path);
    let registry = powder_lifecycle_registry_with_profile_and_crew(
        None,
        "legacy-raw-release-profile",
        "legacy-raw-release-crew",
    );
    let mut snapshot = registry.snapshot();
    let crew = &mut snapshot.captains[0].crew[0];
    crew.state = CrewState::CleanupPending { since: 1 };
    let work = crew.powder_work.as_mut().unwrap();
    work.dispatch_release_recovery = true;
    snapshot
        .pending_dispatch_releases
        .push(PendingDispatchRelease {
            crew_session_id: crew.terminal_id.clone(),
            project_id: "project-powder-lifecycle".into(),
            connection_profile: "legacy-raw-release-profile".into(),
            connection_endpoint_identity: format!("hmac-sha256:{}", "0".repeat(64)),
            repository: "t-hub".into(),
            card_id: work.card_id.clone(),
            run_id: work.run_id.clone(),
            agent: work.agent.clone().unwrap(),
            operation_id: "initial-claim:actor-t-hub:legacy-raw".into(),
            created_at: 1,
            state: PendingDispatchReleaseState::InFlight,
        });
    snapshot.schema_version = 11;
    let mut raw = serde_json::to_value(&snapshot).unwrap();
    let release = raw["pendingDispatchReleases"][0].as_object_mut().unwrap();
    release.remove("connectionEndpointIdentity");
    release.insert(
        "connectionEndpoint".into(),
        json!("https://gateway.example/api?access_token=legacy-secret"),
    );
    std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();

    assert!(CaptainsRegistry::read_snapshot(&path).is_err());
    let _ = std::fs::remove_file(path);
}

#[test]
fn release_recovery_snapshot_pairs_are_required_and_fail_closed_before_network() {
    let path = captains_tmp("release-recovery-pairs");
    let _ = std::fs::remove_file(&path);
    let registry =
        powder_lifecycle_registry_with_profile_and_crew(None, "pair-profile", "pair-crew");
    let mut paired = registry.snapshot();
    let crew = &mut paired.captains[0].crew[0];
    crew.state = CrewState::CleanupPending { since: 1 };
    let work = crew.powder_work.as_mut().unwrap();
    work.dispatch_release_recovery = true;
    let recovery = PendingDispatchRelease {
        crew_session_id: crew.terminal_id.clone(),
        project_id: "project-powder-lifecycle".into(),
        connection_profile: "pair-profile".into(),
        connection_endpoint_identity: format!("hmac-sha256:{}", "0".repeat(64)),
        repository: "t-hub".into(),
        card_id: work.card_id.clone(),
        run_id: work.run_id.clone(),
        agent: work.agent.clone().unwrap(),
        operation_id: "initial-claim:actor-t-hub:pair".into(),
        created_at: 1,
        state: PendingDispatchReleaseState::InFlight,
    };
    paired.pending_dispatch_releases = vec![recovery.clone()];
    assert!(CaptainsRegistry::validate_snapshot(&paired).is_ok());

    let mut orphan = paired.clone();
    orphan.pending_dispatch_releases[0].crew_session_id = "missing-crew".into();
    assert!(CaptainsRegistry::validate_snapshot(&orphan).is_err());

    let mut foreign = paired.clone();
    foreign.pending_dispatch_releases[0].project_id = "foreign-project".into();
    assert!(CaptainsRegistry::validate_snapshot(&foreign).is_err());

    let mut mismatched = paired.clone();
    mismatched.pending_dispatch_releases[0].agent = "foreign-agent".into();
    assert!(CaptainsRegistry::validate_snapshot(&mismatched).is_err());

    let mut missing_record = paired.clone();
    missing_record.pending_dispatch_releases.clear();
    assert!(CaptainsRegistry::validate_snapshot(&missing_record).is_err());

    let mut active_crew = paired.clone();
    active_crew.captains[0].crew[0].state = CrewState::Active;
    assert!(CaptainsRegistry::validate_snapshot(&active_crew).is_err());

    let mut malformed_identity = paired.clone();
    malformed_identity.pending_dispatch_releases[0].card_id = "card\ncontrol".into();
    assert!(CaptainsRegistry::validate_snapshot(&malformed_identity).is_err());

    let mut oversized_identity = paired.clone();
    oversized_identity.pending_dispatch_releases[0].operation_id = "x".repeat(513);
    assert!(CaptainsRegistry::validate_snapshot(&oversized_identity).is_err());

    std::fs::write(&path, serde_json::to_vec(&orphan).unwrap()).unwrap();
    assert!(CaptainsRegistry::read_snapshot(&path).is_err());
    let _ = std::fs::remove_file(path);
}

#[test]
fn future_backup_schema_is_preserved_and_blocks_writes() {
    let path = captains_tmp("future-backup-schema");
    let backup = path.with_extension("json.bak");
    let primary = CaptainsSnapshot {
        schema_version: CAPTAINS_SCHEMA_VERSION,
        seq: 4,
        captains: vec![],
        cortana: crate::cortana_reconcile::CortanaDurableIdentity::default(),
        agent_sessions: vec![],
        agent_checkpoints: vec![],
        agent_events: vec![],
        projects: vec![],
        workspaces: vec![FleetWorkspaceRecord::captain_workspace()],
        pending_fleet_operations: vec![],
        retired_fleet_tile_ids: vec![],
        pending_dispatch_claims: vec![],
        pending_dispatch_releases: vec![],
        pending_git_initializations: vec![],
    };
    let backup_body = json!({
        "schemaVersion": CAPTAINS_SCHEMA_VERSION + 1,
        "seq": 5,
        "captains": [],
        "projects": [],
        "futureField": "preserve",
    })
    .to_string();
    std::fs::write(&path, serde_json::to_vec(&primary).unwrap()).unwrap();
    std::fs::write(&backup, &backup_body).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    assert_eq!(
        registry.snapshot().seq,
        4,
        "supported primary remains readable"
    );
    assert!(registry.write_blocked.is_some());
    assert!(registry
        .claim_test("cap-future", Some("future"), vec![])
        .unwrap_err()
        .contains("read-only"));
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), backup_body);

    let prefix = format!("{}.corrupt.", backup.file_name().unwrap().to_string_lossy());
    assert!(!std::fs::read_dir(backup.parent().unwrap())
        .unwrap()
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix)));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(backup);
}

#[test]
fn future_backup_is_not_quarantined_when_primary_is_corrupt() {
    let path = captains_tmp("future-backup-corrupt-primary");
    let backup = path.with_extension("json.bak");
    let primary_body = "{ invalid";
    let backup_body = json!({
        "schemaVersion": CAPTAINS_SCHEMA_VERSION + 1,
        "seq": 9,
        "captains": [],
        "projects": [],
    })
    .to_string();
    std::fs::write(&path, primary_body).unwrap();
    std::fs::write(&backup, &backup_body).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    assert!(registry.write_blocked.is_some());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), primary_body);
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), backup_body);
    assert!(registry
        .claim_test("cap-future", Some("future"), vec![])
        .unwrap_err()
        .contains("read-only"));

    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(backup);
}

#[test]
fn semantic_registry_corruption_recovers_from_validated_backup() {
    let path = captains_tmp("semantic-corruption");
    let backup = path.with_extension("json.bak");
    let invalid = json!({
        "schemaVersion": CAPTAINS_SCHEMA_VERSION,
        "seq": 2,
        "captains": [],
        "projects": [
            {
                "projectId": "duplicate",
                "name": "One",
                "repoRoot": "/tmp/one",
                "createdAt": 0,
                "updatedAt": 0
            },
            {
                "projectId": "duplicate",
                "name": "Two",
                "repoRoot": "/tmp/two",
                "createdAt": 0,
                "updatedAt": 0
            }
        ]
    });
    let valid = CaptainsSnapshot {
        schema_version: CAPTAINS_SCHEMA_VERSION,
        seq: 1,
        captains: vec![],
        cortana: crate::cortana_reconcile::CortanaDurableIdentity::default(),
        agent_sessions: vec![],
        agent_checkpoints: vec![],
        agent_events: vec![],
        projects: vec![],
        workspaces: vec![FleetWorkspaceRecord::captain_workspace()],
        pending_fleet_operations: vec![],
        retired_fleet_tile_ids: vec![],
        pending_dispatch_claims: vec![],
        pending_dispatch_releases: vec![],
        pending_git_initializations: vec![],
    };
    std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
    std::fs::write(&backup, serde_json::to_vec(&valid).unwrap()).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    let restored = registry.snapshot();
    assert_eq!(restored.seq, valid.seq);
    assert!(restored.captains.is_empty());
    assert!(restored.projects.is_empty());
    assert!(!path.exists(), "invalid primary should be quarantined");
    assert!(backup.exists());

    let prefix = format!("{}.corrupt.", path.file_name().unwrap().to_string_lossy());
    let quarantined = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .expect("semantic corruption should be quarantined");
    let _ = std::fs::remove_file(quarantined.path());
    let _ = std::fs::remove_file(backup);
}

#[test]
fn current_schema_rejects_semantically_impossible_snapshots() {
    let base = json!({
        "schemaVersion": CAPTAINS_SCHEMA_VERSION,
        "seq": 1,
        "captains": [],
        "projects": [],
        "workspaces": [{
            "id": CAPTAIN_WORKSPACE_ID,
            "name": CAPTAIN_WORKSPACE_NAME,
            "kind": "captain",
            "tileIds": []
        }],
    });
    let cases = [
        (
            "active-without-terminal",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [{"shipSlug": "alpha", "role": "captain", "state": {"kind": "active"}}],
                "projects": [],
            }),
        ),
        (
            "relative-project-root",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [],
                "projects": [{
                    "projectId": "p",
                    "name": "P",
                    "repoRoot": "relative/path",
                    "createdAt": 1,
                    "updatedAt": 1
                }],
            }),
        ),
        (
            "incomplete-powder-binding",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [],
                "projects": [{
                    "projectId": "p",
                    "name": "P",
                    "repoRoot": "/tmp/p",
                    "powder": {"connectionProfile": "", "repository": "repo"},
                    "createdAt": 1,
                    "updatedAt": 1
                }],
            }),
        ),
        (
            "unknown-captain-provider",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [{"shipSlug": "alpha", "role": "captain", "terminalId": "cap-a", "provider": "other"}],
                "projects": [],
            }),
        ),
        (
            "captain-provider-harness-mismatch",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [{"shipSlug": "alpha", "role": "captain", "terminalId": "cap-a", "provider": "codex", "harness": "claude"}],
                "projects": [],
            }),
        ),
        (
            "claude-continuity-mismatch",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [{
                    "shipSlug": "alpha", "role": "captain", "terminalId": "cap-a",
                    "provider": "claude", "harness": "claude",
                    "providerSessionId": "provider-a", "claudeUuid": "claude-b"
                }],
                "projects": [],
            }),
        ),
        (
            "codex-crew-with-claude-uuid",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [{
                    "shipSlug": "alpha", "role": "captain", "terminalId": "cap-a",
                    "crew": [{
                        "terminalId": "crew-a", "provider": "codex", "harness": "codex",
                        "providerSessionId": "codex-a", "claudeUuid": "claude-a"
                    }]
                }],
                "projects": [],
            }),
        ),
    ];
    assert!(CaptainsRegistry::validate_snapshot(&serde_json::from_value(base).unwrap()).is_ok());
    for (name, value) in cases {
        let snapshot: CaptainsSnapshot = serde_json::from_value(value).unwrap();
        assert!(
            CaptainsRegistry::validate_snapshot(&snapshot).is_err(),
            "{name} was accepted"
        );
    }
}

#[test]
fn registry_mutations_reject_noncanonical_harnesses_and_providers() {
    let registry = CaptainsRegistry::new();
    let invalid_claim = registry.claim_provider(
        "cap-a",
        Some("alpha"),
        FleetRole::Captain,
        Some("other"),
        Some("session-a"),
        vec![],
        &|_| false,
        &|_| tmux::SessionLiveness::Alive,
    );
    assert!(invalid_claim.unwrap_err().contains("codex"));
    assert!(registry
        .bind_ship_context("alpha", "project-a", "task", "other")
        .unwrap_err()
        .contains("codex"));
    assert!(registry
        .bind_crew_context(
            "cap-a",
            "crew-a",
            "task",
            "other",
            None,
            None,
            PowderWorkBinding {
                card_id: "card-a".into(),
                run_id: "run-a".into(),
                agent: None,
                claim_expires_at: None,
                mutation_intent: None,
                dispatch_release_recovery: false,
                state: PowderWorkState::Active,
            },
        )
        .unwrap_err()
        .contains("codex"));
    assert!(registry.snapshot().captains.is_empty());
}
