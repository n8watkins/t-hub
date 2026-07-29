use super::*;

#[test]
fn list_captains_returns_the_versioned_snapshot() {
    let ctx = test_ctx("secret");
    ctx.captains
        .claim_test("cap-1", Some("alpha"), vec!["tab-1".into()])
        .unwrap();
    let v = dispatch(&ctx, "list_captains", &json!({})).unwrap();
    assert_eq!(v["count"], 1);
    assert_eq!(v["seq"], 1);
    assert_eq!(v["captains"][0]["shipSlug"], "alpha");
    assert_eq!(v["captains"][0]["terminalId"], "cap-1");
    assert_eq!(v["captains"][0]["workspaceTabIds"][0], "tab-1");
    assert_eq!(v["captains"][0]["crew"], json!([]));
}

#[test]
fn scribe_status_dispatches_and_returns_a_listening_bool() {
    // The read-tier scribe voice-gate: dispatches to crate::scribe and
    // always returns an object with a boolean `listening` field, whatever
    // the on-disk file says (fail-open guarantees the shape). Asserting the
    // shape (not the value) keeps this deterministic whether or not a real
    // Scribe status file exists on the test machine.
    let ctx = test_ctx("secret");
    let v = dispatch(&ctx, "scribe_status", &Value::Null).unwrap();
    assert!(v.is_object());
    assert!(v["listening"].is_boolean());
}

#[test]
fn claim_and_release_are_audited_and_forward_the_captains_snapshot() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    // A LIVE terminal to claim (the liveness gate): spawn it into tab-1.
    let cap_id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "tabId": "tab-1",
            "startupCommand": harness_command,
        }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&cap_id, "codex").unwrap();

    // Claim with no explicit workspaceTabIds does not infer Work Workspace
    // ownership from the Captain terminal's current placement.
    let v = dispatch(&ctx, "claim_captain", &json!({"captainSessionId": cap_id})).unwrap();
    assert_eq!(v["accepted"], "claim_captain");
    assert_eq!(v["audited"], true);
    assert_eq!(v["applied"], true);
    assert_eq!(v["captain"]["shipSlug"], format!("ship-{cap_id}"));
    assert_eq!(v["captain"]["workspaceTabIds"], json!([]));
    assert_eq!(v["captain"]["terminalId"], cap_id);

    let v = dispatch(
        &ctx,
        "release_captain",
        &json!({"captainSessionId": cap_id}),
    )
    .unwrap();
    assert_eq!(v["accepted"], "release_captain");
    assert_eq!(v["released"]["terminalId"], cap_id);
    assert_eq!(v["captains"], json!([]));

    // The claim + release each forwarded a sync_captains snapshot (filtering
    // out the spawn_terminal forward that seeded the live session).
    let sync_calls: Vec<_> = sink
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(c, _)| c == "sync_captains")
        .cloned()
        .collect();
    assert_eq!(sync_calls.len(), 2);
    assert_eq!(sync_calls[0].1["sync"]["captains"][0]["terminalId"], cap_id);
    assert_eq!(sync_calls[1].1["sync"]["captains"], json!([]));

    dispatch(&ctx, "close_terminal", &json!({"sessionId": cap_id})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
}

#[test]
fn claim_captain_relocates_the_tile_atomically_and_survives_retry_and_restart() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let path = captains_tmp("claim-relocation-restart");
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![TabRecord {
        id: "work-a".into(),
        name: "Work A".into(),
        tile_ids: Vec::new(),
    }]);
    let ctx = test_ctx("claim-relocation")
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }))
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let captain_id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "tabId": "work-a", "startupCommand": harness_command}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&captain_id, "codex").unwrap();

    let claimed = dispatch(
        &ctx,
        "claim_captain",
        &json!({
            "captainSessionId": captain_id,
            "shipSlug": "alpha",
            "workspaceTabIds": ["work-a"]
        }),
    )
    .unwrap();
    assert_eq!(claimed["accepted"], "claim_captain");
    let snapshot = tabs.snapshot_full();
    let work = snapshot.tabs.iter().find(|tab| tab.id == "work-a").unwrap();
    let captain_workspace = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
        .unwrap();
    assert!(!work.tile_ids.contains(&captain_id));
    assert_eq!(
        captain_workspace
            .tile_ids
            .iter()
            .filter(|tile| *tile == &captain_id)
            .count(),
        1
    );

    let unchanged = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({
            "baseSeq": snapshot.seq,
            "tabs": snapshot.tabs,
            "activeTabId": snapshot.active_tab_id
        }),
    )
    .unwrap();
    assert!(unchanged.get("reported").is_some());
    dispatch(
        &ctx,
        "claim_captain",
        &json!({
            "captainSessionId": captain_id,
            "shipSlug": "alpha",
            "workspaceTabIds": ["work-a"]
        }),
    )
    .unwrap();
    let after_retry = tabs.snapshot_full();
    assert_eq!(
        after_retry
            .tabs
            .iter()
            .flat_map(|tab| tab.tile_ids.iter())
            .filter(|tile| *tile == &captain_id)
            .count(),
        1
    );
    let restored = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(restored.captains.len(), 1);
    assert_eq!(
        restored.captains[0].terminal_id.as_deref(),
        Some(captain_id.as_str())
    );

    dispatch(&ctx, "close_terminal", &json!({"sessionId": captain_id})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn failed_claim_captain_persistence_keeps_the_original_work_placement() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let captains = Arc::new(CaptainsRegistry::load(captains_tmp(
        "claim-relocation-fail",
    )));
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![TabRecord {
        id: "work-a".into(),
        name: "Work A".into(),
        tile_ids: Vec::new(),
    }]);
    let ctx = test_ctx("claim-relocation-fail")
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }))
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let captain_id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "tabId": "work-a", "startupCommand": harness_command}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&captain_id, "codex").unwrap();
    let before = tabs.snapshot_full();
    captains.fail_next_persist("claim relocation persistence failure");
    let error = dispatch(
        &ctx,
        "claim_captain",
        &json!({"captainSessionId": captain_id, "shipSlug": "alpha"}),
    )
    .unwrap_err();
    assert!(error.contains("claim relocation persistence failure"));
    assert!(captains.snapshot().captains.is_empty());
    let after = tabs.snapshot_full();
    assert_eq!(after.seq, before.seq);
    assert!(after
        .tabs
        .iter()
        .find(|tab| tab.id == "work-a")
        .unwrap()
        .tile_ids
        .contains(&captain_id));
    assert!(!after
        .tabs
        .iter()
        .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
        .unwrap()
        .tile_ids
        .contains(&captain_id));

    dispatch(&ctx, "close_terminal", &json!({"sessionId": captain_id})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
}

#[test]
fn codex_claim_never_inherits_a_stale_claude_session_id() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let terminal_id = format!("codex{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let status = Arc::new(StatusBridge::new());
    status.ingest(
        "stale-claude-uuid",
        &json!({ "cwd": "/tmp", "tmux_session": tmux_target(&terminal_id) }),
        1,
    );
    let supervisor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync> =
        Arc::new(|visitor| visitor(&Supervisor::new()));
    let ctx = ControlContext::new(status, supervisor, "t".into()).with_apply_sink(Arc::new(
        RecordingSink {
            calls: StdMutex::new(Vec::new()),
        },
    ));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    tmux::new_session_with_env(
        &tmux_target(&terminal_id),
        "/tmp",
        Some(&harness_command),
        &[],
    )
    .unwrap();
    wait_for_harness_started(&terminal_id, "codex").unwrap();

    let mismatched_provider = dispatch(
        &ctx,
        "claim_captain",
        &json!({
            "captainSessionId": terminal_id,
            "provider": "claude",
            "providerSessionId": "spoofed-claude-id",
        }),
    )
    .unwrap_err();
    assert!(mismatched_provider.contains("does not match a live harness"));
    let spoofed_id = dispatch(
        &ctx,
        "claim_captain",
        &json!({
            "captainSessionId": terminal_id,
            "provider": "codex",
            "providerSessionId": "spoofed-codex-id",
        }),
    )
    .unwrap_err();
    assert!(spoofed_id.contains("cannot be trusted"));

    let value = dispatch(
        &ctx,
        "claim_captain",
        &json!({
            "captainSessionId": terminal_id,
            "provider": "codex",
        }),
    )
    .unwrap();
    assert_eq!(value["captain"]["provider"], "codex");
    assert!(value["captain"].get("providerSessionId").is_none());
    assert!(value["captain"].get("conversationId").is_none());
    assert!(value["captain"].get("claudeUuid").is_none());
    let tabs = ctx.tab_registry().snapshot();
    let captain_workspace = tabs
        .iter()
        .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
        .expect("claim creates the durable Captain Workspace when boot starts headless");
    assert_eq!(captain_workspace.name, CAPTAIN_WORKSPACE_NAME);
    assert_eq!(captain_workspace.tile_ids, vec![terminal_id.clone()]);

    dispatch(&ctx, "close_terminal", &json!({ "sessionId": terminal_id })).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
}

#[test]
fn claim_conflicts_liveness_and_bad_release_are_dispatch_errors() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("t").with_apply_sink(Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    }));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    let id1 = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "tabId": "tab-1",
            "startupCommand": harness_command,
        }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&id1, "codex").unwrap();
    let id2 = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "tabId": "tab-1",
            "startupCommand": harness_command,
        }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&id2, "codex").unwrap();

    dispatch(
        &ctx,
        "claim_captain",
        &json!({"captainSessionId": id1, "shipSlug": "alpha"}),
    )
    .unwrap();
    // A DIFFERENT live captain claiming the same ship is refused.
    let err = dispatch(
        &ctx,
        "claim_captain",
        &json!({"captainSessionId": id2, "shipSlug": "alpha"}),
    )
    .unwrap_err();
    assert!(err.contains("already captained"), "got: {err}");
    // A claim for a DEAD/unknown session is refused by the liveness gate
    // (else it would persist and linger forever).
    let err = dispatch(
        &ctx,
        "claim_captain",
        &json!({"captainSessionId": "nonexistent"}),
    )
    .unwrap_err();
    assert!(err.contains("no live terminal"), "got: {err}");
    let err = dispatch(&ctx, "release_captain", &json!({"shipSlug": "nope"})).unwrap_err();
    assert!(err.contains("no claim matches"), "got: {err}");
    assert!(dispatch(&ctx, "claim_captain", &json!({})).is_err());
    assert!(dispatch(&ctx, "release_captain", &json!({})).is_err());

    dispatch(&ctx, "close_terminal", &json!({"sessionId": id1})).unwrap();
    dispatch(&ctx, "close_terminal", &json!({"sessionId": id2})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
}

#[test]
fn idempotent_reclaim_does_not_bump_seq_or_forward() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    let id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "tabId": "tab-1",
            "startupCommand": harness_command,
        }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&id, "codex").unwrap();

    let v1 = dispatch(&ctx, "claim_captain", &json!({"captainSessionId": id})).unwrap();
    assert_eq!(v1["applied"], true);
    let seq1 = v1["seq"].as_u64().unwrap();
    // An identical re-claim changes nothing: seq stays put, no second forward.
    let v2 = dispatch(&ctx, "claim_captain", &json!({"captainSessionId": id})).unwrap();
    assert_eq!(
        v2["seq"].as_u64().unwrap(),
        seq1,
        "unchanged re-claim must not bump seq"
    );
    assert_eq!(v2["applied"], false, "unchanged re-claim must not forward");
    let sync_count = sink
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(c, _)| c == "sync_captains")
        .count();
    assert_eq!(sync_count, 1, "only the first (changing) claim forwards");

    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
}

#[test]
fn spawn_with_spawned_by_records_crew_and_close_terminal_removes_it() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    ctx.captains
        .claim_test("cap-1", Some("alpha"), vec![])
        .unwrap();

    // A claimed captain spawns crew: the link is recorded + synced.
    let v = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "spawnedBy": "cap-1"}),
    )
    .unwrap();
    assert_eq!(v["crewRecorded"], true);
    assert_eq!(v["spawnedBy"], "cap-1");
    let crew_id = v["id"].as_str().unwrap().to_string();
    let snap = ctx.captains.snapshot();
    assert_eq!(crew_tiles(&snap.captains[0]), vec![crew_id.clone()]);

    // Item-2 Phase B: a dead crew session is MARKED Removed (retained for
    // telemetry / reap-ship), not scrubbed (retiring the old silent-leak), and a
    // sync still forwards so every surface drops the crewmate live.
    dispatch(
        &ctx,
        "close_terminal",
        &json!({"sessionId": crew_id.clone()}),
    )
    .unwrap();
    let after = ctx.captains.snapshot();
    let cr = after.captains[0]
        .crew
        .iter()
        .find(|c| c.terminal_id == crew_id)
        .expect("crew ref retained, not scrubbed");
    assert!(matches!(cr.state, CrewState::Removed { .. }));

    // Forwards: sync_captains (crew add), spawn_terminal (with spawnedBy),
    // sync_tabs (tile drop), sync_captains (crew removal).
    let calls = sink.calls.lock().unwrap();
    let names: Vec<&str> = calls.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(
        names,
        [
            "sync_captains",
            "spawn_terminal",
            "sync_tabs",
            "sync_captains"
        ]
    );
    // The crew-add forward carries the crew as a CrewRef (terminalId + state).
    assert_eq!(
        calls[0].1["sync"]["captains"][0]["crew"][0]["terminalId"],
        crew_id
    );
    assert_eq!(calls[1].1["spawnedBy"], "cap-1");
    // The crew-removal forward retains the ref, now marked Removed (not scrubbed).
    assert_eq!(
        calls[3].1["sync"]["captains"][0]["crew"][0]["terminalId"],
        crew_id
    );
    assert_eq!(
        calls[3].1["sync"]["captains"][0]["crew"][0]["state"]["kind"],
        "removed"
    );
}

#[test]
fn spawn_with_an_unclaimed_spawned_by_still_spawns_without_a_crew_link() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let v = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "spawnedBy": "cap-ghost"}),
    )
    .unwrap();
    assert_eq!(v["accepted"], "spawn_terminal");
    assert_eq!(
        v["crewRecorded"], false,
        "no claim = no crew link, spawn unaffected"
    );
    assert!(ctx.captains.snapshot().captains.is_empty());
    let id = v["id"].as_str().unwrap().to_string();
    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    let calls = sink.calls.lock().unwrap();
    assert!(
        calls.iter().all(|(c, _)| c != "sync_captains"),
        "nothing captain-shaped changed, so no captains sync may be forwarded"
    );
}

#[test]
fn close_terminal_of_a_captain_orphans_its_claim() {
    let ctx = test_ctx("t");
    ctx.captains
        .claim_test("cap-1", Some("alpha"), vec![])
        .unwrap();
    // Item-2 Phase B: the captain's own session dies (already-gone tmux session:
    // the kill is idempotent, so dispatch succeeds and the registry cleanup runs).
    // The claim is MARKED Orphaned + un-pointed (retained for re-adoption by a
    // resumed captain of the same ship), NOT scrubbed - the old whole-record
    // `retain`-away was the C4 silent leak.
    dispatch(&ctx, "close_terminal", &json!({"sessionId": "cap-1"})).unwrap();
    let snap = ctx.captains.snapshot();
    assert_eq!(snap.captains.len(), 1, "record retained, not scrubbed");
    assert!(matches!(
        snap.captains[0].state,
        ClaimState::Orphaned { .. }
    ));
    assert!(snap.captains[0].terminal_id.is_none(), "un-pointed");
}

#[test]
fn close_terminal_releases_fleet_lock_during_external_effects() {
    let registry = Arc::new(CaptainsRegistry::new());
    registry
        .claim_test("cap-1", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    registry.record_crew("cap-1", "crew-1").unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(registry.workspace_projection());
    let context = Arc::new(
        test_ctx("close-effect-lock")
            .with_captains_registry(Arc::clone(&registry))
            .with_tab_registry(Arc::clone(&tabs)),
    );
    let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    registry.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "close_terminal_effect",
        reached: reached_tx,
        resume: resume_rx,
    }));
    let closing_context = Arc::clone(&context);
    let closing = std::thread::spawn(move || {
        close_terminal_with_policy(
            &closing_context,
            &json!({"sessionId": "crew-1"}),
            false,
            None,
        )
    });
    assert_eq!(
        reached_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        "close_terminal_effect"
    );
    assert_eq!(registry.snapshot().pending_fleet_operations.len(), 1);

    let (listed_tx, listed_rx) = std::sync::mpsc::sync_channel(1);
    let listing_context = Arc::clone(&context);
    std::thread::spawn(move || {
        listed_tx
            .send((
                dispatch(&listing_context, "list_captains", &Value::Null),
                dispatch(&listing_context, "list_tabs", &Value::Null),
            ))
            .unwrap();
    });
    let (captains, listed_tabs) = listed_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("Fleet readers must remain prompt during terminal effects");
    captains.unwrap();
    listed_tabs.unwrap();
    resume_tx.send(()).unwrap();
    closing.join().unwrap().unwrap();
    assert!(registry.snapshot().pending_fleet_operations.is_empty());
    assert!(matches!(
        registry.snapshot().captains[0].crew[0].state,
        CrewState::Removed { .. }
    ));
}
