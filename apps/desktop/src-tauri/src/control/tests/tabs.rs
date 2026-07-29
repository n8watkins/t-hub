use super::*;

#[test]
fn startup_tab_authority_is_retryable_until_reconciled_projection_is_published() {
    let tabs = Arc::new(TabRegistry::new_pending_startup());
    let ctx = test_ctx("secret").with_tab_registry(tabs.clone());
    let request = || ControlRequest {
        token: "secret".into(),
        command: "list_tabs".into(),
        args: Value::Null,
        session: String::new(),
        host: "secret".into(),
        v: None,
    };

    let pending = dispatch_authenticated(&ctx, request());
    assert!(!pending.ok);
    assert!(pending.retryable);
    assert!(pending
        .error
        .as_deref()
        .is_some_and(|error| error.contains("startup reconciliation is still pending")));

    tabs.replace(vec![TabRecord {
        id: "work-intermediate".into(),
        name: "Intermediate".into(),
        tile_ids: vec![],
    }]);
    let still_pending = dispatch_authenticated(&ctx, request());
    assert!(!still_pending.ok);
    assert!(still_pending.retryable);

    tabs.publish_startup(vec![TabRecord {
        id: "work-ready".into(),
        name: "Ready".into(),
        tile_ids: vec!["live-terminal".into()],
    }]);
    let ready = dispatch_authenticated(&ctx, request());
    assert!(
        ready.ok,
        "expected reconciled registry, got {:?}",
        ready.error
    );
}

#[test]
fn focus_tab_is_organization_apply() {
    // Headless-org: focus_tab is STRICT (the tab must exist in the registry)
    // and mirrors the new active tab there. No sink (headless): accepted +
    // audited, but not applied.
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "focus_tab", &json!({"tabId": "tab-1"})).unwrap_err();
    assert!(err.contains("unknown tabId"), "got: {err}");

    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    let v = dispatch(&ctx, "focus_tab", &json!({"tabId": "tab-1"})).unwrap();
    assert_eq!(v["accepted"], "focus_tab");
    assert_eq!(v["audited"], true);
    assert_eq!(v["applied"], false);
    assert_eq!(
        ctx.tab_registry().snapshot_full().active_tab_id.as_deref(),
        Some("tab-1")
    );
}

#[test]
fn new_tab_returns_a_tab_id_and_registers_it() {
    // TASK C: new_tab mints an id CORE-side, returns it, and records it so
    // list_tabs sees it immediately (addressable before any frontend report).
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "new_tab", &json!({"name": "Logs"})).unwrap();
    assert_eq!(v["accepted"], "new_tab");
    assert_eq!(v["name"], "Logs");
    let tab_id = v["tabId"].as_str().expect("new_tab returns a tabId");
    assert!(!tab_id.is_empty());

    let tabs = dispatch(&ctx, "list_tabs", &Value::Null).unwrap();
    let arr = tabs["tabs"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], tab_id);
    assert_eq!(arr[0]["name"], "Logs");
    assert_eq!(arr[0]["tileIds"].as_array().unwrap().len(), 0);
}

#[test]
fn new_tab_auto_names_when_no_name_given() {
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "new_tab", &Value::Null).unwrap();
    assert_eq!(v["name"], "Workspace 1");
    let v2 = dispatch(&ctx, "new_tab", &Value::Null).unwrap();
    assert_eq!(v2["name"], "Workspace 2");
}

#[test]
fn new_tab_then_move_tile_reflected_in_list_tabs() {
    // The headless acceptance for #22: new_tab -> get its id -> move_tile a
    // terminal into it -> list_tabs shows the tile in that tab.
    let ctx = test_ctx("t");
    let created = dispatch(&ctx, "new_tab", &json!({"name": "Target"})).unwrap();
    let tab_id = created["tabId"].as_str().unwrap().to_string();

    dispatch(
        &ctx,
        "move_tile",
        &json!({"terminalId": "term-xyz", "tabId": tab_id}),
    )
    .unwrap();

    let tabs = dispatch(&ctx, "list_tabs", &Value::Null).unwrap();
    let target = tabs["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == tab_id.as_str())
        .expect("target tab present");
    let tiles: Vec<&str> = target["tileIds"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(tiles, vec!["term-xyz"]);
}

#[test]
fn move_tile_uses_durable_fleet_authority_without_waiting_for_projection_lock() {
    let context = Arc::new(test_ctx("move-identity-transaction"));
    context
        .captains
        .create_workspace("work-a", "Work A", None)
        .unwrap();
    context
        .captains
        .create_workspace("work-b", "Work B", None)
        .unwrap();
    context.tab_registry().replace(vec![
        TabRecord {
            id: "work-a".into(),
            name: "Work A".into(),
            tile_ids: vec!["ordinary".into()],
        },
        TabRecord {
            id: "work-b".into(),
            name: "Work B".into(),
            tile_ids: Vec::new(),
        },
    ]);
    let tabs = context.tab_registry();
    let transaction = tabs.identity_transaction();
    let (sent, received) = std::sync::mpsc::channel();
    let moving_context = Arc::clone(&context);
    let moving = std::thread::spawn(move || {
        let result = dispatch(
            &moving_context,
            "move_tile",
            &json!({"terminalId": "ordinary", "tabId": "work-b"}),
        );
        sent.send(result).unwrap();
    });
    received
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    assert!(context
        .captains
        .snapshot()
        .workspaces
        .iter()
        .find(|workspace| workspace.id == "work-b")
        .unwrap()
        .tile_ids
        .contains(&"ordinary".to_string()));
    drop(transaction);
    moving.join().unwrap();
    assert!(tabs
        .snapshot()
        .iter()
        .find(|tab| tab.id == "work-b")
        .unwrap()
        .tile_ids
        .contains(&"ordinary".to_string()));
}

#[test]
fn rollback_restore_cannot_clobber_a_concurrent_valid_move() {
    let context = Arc::new(test_ctx("rollback-move-transaction"));
    context.tab_registry().replace(vec![
        TabRecord {
            id: "work-a".into(),
            name: "Work A".into(),
            tile_ids: vec!["ordinary".into()],
        },
        TabRecord {
            id: "work-b".into(),
            name: "Work B".into(),
            tile_ids: Vec::new(),
        },
    ]);
    let tabs = context.tab_registry();
    tabs.move_tile("ordinary", CAPTAIN_WORKSPACE_ID).unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    let moving_context = Arc::clone(&context);
    let moving = std::thread::spawn(move || {
        sent.send(dispatch(
            &moving_context,
            "move_tile",
            &json!({"terminalId": "ordinary", "tabId": "work-b"}),
        ))
        .unwrap();
    });
    received
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    moving.join().unwrap();
    tabs.replace(context.captains.workspace_projection());
    let snapshot = tabs.snapshot();
    assert!(snapshot
        .iter()
        .find(|tab| tab.id == "work-b")
        .unwrap()
        .tile_ids
        .contains(&"ordinary".to_string()));
    assert_eq!(
        snapshot
            .iter()
            .flat_map(|tab| tab.tile_ids.iter())
            .filter(|tile| tile.as_str() == "ordinary")
            .count(),
        1
    );
}

#[test]
fn close_terminal_does_not_hold_or_wait_for_projection_identity_during_effects() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let terminal_id = format!(
        "close-race-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    create_test_tmux_session(&tmux_target(&terminal_id)).unwrap();
    let context = Arc::new(test_ctx("close-identity-transaction"));
    context.tab_registry().replace(vec![TabRecord {
        id: "work-a".into(),
        name: "Work A".into(),
        tile_ids: vec![terminal_id.clone()],
    }]);
    let tabs = context.tab_registry();
    let transaction = tabs.identity_transaction();
    let (sent, received) = std::sync::mpsc::channel();
    let closing_context = Arc::clone(&context);
    let closing_id = terminal_id.clone();
    let closing = std::thread::spawn(move || {
        sent.send(close_terminal(
            &closing_context,
            &json!({"sessionId": closing_id}),
        ))
        .unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline
        && (tmux::has_session(&tmux_target(&terminal_id))
            || !context
                .captains
                .snapshot()
                .pending_fleet_operations
                .is_empty())
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!tmux::has_session(&tmux_target(&terminal_id)));
    assert!(context
        .captains
        .snapshot()
        .pending_fleet_operations
        .is_empty());
    received
        .recv_timeout(Duration::from_secs(2))
        .expect("close_terminal must not wait for the projection identity mutex")
        .unwrap();
    drop(transaction);
    closing.join().unwrap();
    assert!(!tmux::has_session(&tmux_target(&terminal_id)));
    assert!(!tabs
        .snapshot()
        .iter()
        .any(|tab| tab.tile_ids.contains(&terminal_id)));
}

#[test]
fn move_racing_claim_cannot_leave_an_active_captain_in_work() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let path = captains_tmp("move-vs-claim-transaction");
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    let tabs = Arc::new(TabRegistry::new());
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let captain_id = format!(
        "claim-race-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    tmux::new_session_with_env(
        &tmux_target(&captain_id),
        "/tmp",
        Some(&harness_command),
        &[],
    )
    .unwrap();
    wait_for_harness_started(&captain_id, "codex").unwrap();
    tabs.replace(vec![
        TabRecord {
            id: "work-a".into(),
            name: "Work A".into(),
            tile_ids: vec![captain_id.clone()],
        },
        TabRecord {
            id: "work-b".into(),
            name: "Work B".into(),
            tile_ids: Vec::new(),
        },
    ]);
    let context = Arc::new(
        test_ctx("move-vs-claim-transaction")
            .with_captains_registry(Arc::clone(&captains))
            .with_tab_registry(Arc::clone(&tabs))
            .with_apply_sink(Arc::new(RecordingSink {
                calls: StdMutex::new(Vec::new()),
            })),
    );
    let entered = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let hook_entered = Arc::clone(&entered);
    let hook_release = Arc::clone(&release);
    let first_persist = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let hook_first_persist = Arc::clone(&first_persist);
    captains.set_persist_hook(Box::new(move || {
        if hook_first_persist.swap(false, Ordering::SeqCst) {
            hook_entered.wait();
            hook_release.wait();
        }
    }));
    let claiming_context = Arc::clone(&context);
    let claiming_id = captain_id.clone();
    let claiming = std::thread::spawn(move || {
        dispatch(
            &claiming_context,
            "claim_captain",
            &json!({
                "captainSessionId": claiming_id,
                "shipSlug": "claim-race",
                "provider": "codex"
            }),
        )
    });
    entered.wait();
    let (move_sent, move_received) = std::sync::mpsc::channel();
    let moving_context = Arc::clone(&context);
    let moving_id = captain_id.clone();
    let moving = std::thread::spawn(move || {
        move_sent
            .send(dispatch(
                &moving_context,
                "move_tile",
                &json!({"terminalId": moving_id, "tabId": "work-b"}),
            ))
            .unwrap();
    });
    assert!(move_received
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    release.wait();
    claiming.join().unwrap().unwrap();
    captains.set_persist_hook(Box::new(|| {}));
    let move_error = move_received
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap_err();
    assert!(move_error.contains("belongs to Captain Workspace"));
    moving.join().unwrap();
    let snapshot = tabs.snapshot();
    assert!(snapshot
        .iter()
        .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
        .unwrap()
        .tile_ids
        .contains(&captain_id));
    assert!(!snapshot
        .iter()
        .filter(|tab| tab.kind() == WorkspaceKind::Work)
        .any(|tab| tab.tile_ids.contains(&captain_id)));

    close_terminal(&context, &json!({"sessionId": captain_id})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn stale_report_is_rejected_and_answers_with_the_snapshot() {
    // Headless-org acceptance for requirement 2: a server-side move_tile must
    // survive a UI report that predates it (the exact lost-update repro: three
    // accepted move_tile calls, registry silently reverted by the reporter).
    let ctx = test_ctx("t");
    // UI boots and reports its layout (legacy/no baseSeq → accepted).
    let v = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [
                {"id": "t1", "name": "Workspace 1", "tileIds": ["aa"]},
                {"id": "t2", "name": "hidden", "tileIds": []},
            ], "activeTabId": "t1", "baseSeq": 0}),
    )
    .unwrap();
    let seq = v["seq"].as_u64().unwrap();

    // A headless move into the hidden tab bumps the revision.
    dispatch(
        &ctx,
        "move_tile",
        &json!({"terminalId": "aa", "tabId": "t2"}),
    )
    .unwrap();

    // The UI (which never applied the move - hidden tab, suspended webview…)
    // reports its stale layout: REJECTED, answered with the snapshot.
    let v = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [
                {"id": "t1", "name": "Workspace 1", "tileIds": ["aa"]},
                {"id": "t2", "name": "hidden", "tileIds": []},
            ], "activeTabId": "t1", "baseSeq": seq}),
    )
    .unwrap();
    assert_eq!(v["stale"], true);
    let tabs = v["tabs"].as_array().unwrap();
    let t2 = tabs.iter().find(|t| t["id"] == "t2").unwrap();
    assert_eq!(
        t2["tileIds"],
        json!(["aa"]),
        "the move survives the stale report"
    );

    // list_tabs stays truthful: the tile is in the hidden tab.
    let tabs = dispatch(&ctx, "list_tabs", &Value::Null).unwrap();
    let t2 = tabs["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "t2")
        .unwrap();
    assert_eq!(t2["tileIds"], json!(["aa"]));

    // A report based on the CURRENT revision is accepted (normal UI flow).
    let cur = tabs["seq"].as_u64().unwrap();
    let v = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [
                {"id": "t1", "name": "Workspace 1", "tileIds": []},
                {"id": "t2", "name": "hidden", "tileIds": ["aa"]},
            ], "activeTabId": "t1", "baseSeq": cur}),
    )
    .unwrap();
    assert_eq!(v["reported"], 2);
}

#[test]
fn close_tab_headless_lifecycle_policy() {
    // Requirement 3: tiles leave their tab on close_terminal, and an emptied
    // auto-created tab is closeable headlessly - with the documented guards.
    let ctx = test_ctx("t");
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "t1".into(),
            name: "Workspace 1".into(),
            tile_ids: vec!["keep".into()],
        },
        TabRecord {
            id: "t2".into(),
            name: "staging".into(),
            tile_ids: vec!["dead".into()],
        },
    ]);

    // A non-empty tab is refused without force.
    let err = dispatch(&ctx, "close_tab", &json!({"tabId": "t2"})).unwrap_err();
    assert!(err.contains("close its terminals first"), "got: {err}");

    // close_terminal drops the tile from its tab (tmux kill is idempotent on
    // an already-gone session, so this exercises the registry path headlessly).
    dispatch(&ctx, "close_terminal", &json!({"sessionId": "dead"})).unwrap();
    let t2 = ctx
        .tab_registry()
        .snapshot()
        .into_iter()
        .find(|t| t.id == "t2")
        .unwrap();
    assert!(t2.tile_ids.is_empty(), "the closed tile left its tab");

    // The emptied tab closes headlessly (by name here - id also works).
    let v = dispatch(&ctx, "close_tab", &json!({"tabName": "staging"})).unwrap();
    assert_eq!(v["accepted"], "close_tab");
    assert_eq!(v["tabId"], "t2");
    assert!(ctx.tab_registry().snapshot().iter().all(|t| t.id != "t2"));

    // The LAST tab is never closed.
    let err = dispatch(&ctx, "close_tab", &json!({"tabId": "t1"})).unwrap_err();
    assert!(err.contains("last tab"), "got: {err}");
}

#[test]
fn placement_falls_back_when_the_target_tab_vanished() {
    // The tab-closed-during-spawn race, at the placement primitive: the tab
    // resolved before the tmux spawn may be gone by placement time. The tile
    // must ALWAYS land in the registry - active tab first, else first tab -
    // and the actual tab id is returned.
    let ctx = test_ctx("t");
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "t1".into(),
            name: "Workspace 1".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "t2".into(),
            name: "Workspace 2".into(),
            tile_ids: vec![],
        },
    ]);
    assert!(ctx.tab_registry().set_active_tab("t2"));

    // Target vanished -> falls back to the ACTIVE tab.
    let placed = ctx
        .tab_registry()
        .place_tile_with_fallback("tile-a", Some("closed-mid-spawn"));
    assert_eq!(placed.as_deref(), Some("t2"));
    // Target vanished AND no active pointer -> first tab.
    ctx.tab_registry().replace(vec![TabRecord {
        id: "only".into(),
        name: "Solo".into(),
        tile_ids: vec![],
    }]);
    let placed = ctx
        .tab_registry()
        .place_tile_with_fallback("tile-b", Some("also-gone"));
    assert_eq!(placed.as_deref(), Some("only"));
    let snap = ctx.tab_registry().snapshot();
    assert_eq!(snap[0].tile_ids, vec!["tile-b"]);
    // Empty registry -> unplaced (None), the only case a tile stays out.
    ctx.tab_registry().replace(vec![]);
    assert_eq!(
        ctx.tab_registry()
            .place_tile_with_fallback("tile-c", Some("x")),
        None
    );
}

#[test]
fn spawn_survives_a_concurrent_close_of_its_target_tab() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // Dispatch-level tab-closed-during-spawn race: close_tab races the spawn's
    // resolve->tmux->place window. Whichever side wins, the invariant holds:
    // the spawned session ends up in EXACTLY ONE registry tab, and the
    // response's tabId names that tab (fallback placement is reflected).
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink);
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "keep".into(),
            name: "Workspace 1".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "doomed".into(),
            name: "staging".into(),
            tile_ids: vec![],
        },
    ]);
    assert!(ctx.tab_registry().set_active_tab("keep"));
    let spawn_started = std::env::temp_dir().join(format!(
        "t-hub-spawn-race-{}",
        uuid::Uuid::new_v4().simple()
    ));

    let closer = {
        let ctx = ctx.clone();
        let spawn_started = spawn_started.clone();
        std::thread::spawn(move || {
            // Wait until the pane command proves spawn passed strict tab
            // validation. This targets the intended resolve->place race
            // without allowing the close to invalidate the request before
            // spawn_terminal begins.
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if spawn_started.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(spawn_started.exists(), "spawn pane never signaled startup");
            // Either outcome is legal: the close wins (spawn falls back to
            // "keep") or the placement wins (close refuses the non-empty tab).
            let _ = dispatch(&ctx, "close_tab", &json!({"tabId": "doomed"}));
        })
    };
    let startup_command = format!("touch {}", spawn_started.display());
    let v = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "tabId": "doomed",
            "startupCommand": startup_command,
        }),
    )
    .unwrap();
    closer.join().unwrap();
    let _ = std::fs::remove_file(spawn_started);

    let id = v["id"].as_str().unwrap().to_string();
    let placed_tab = v["tabId"].as_str().expect("always placed").to_string();
    assert_eq!(v["placed"], true);
    let owners: Vec<String> = ctx
        .tab_registry()
        .snapshot()
        .into_iter()
        .filter(|t| t.tile_ids.iter().any(|x| x == &id))
        .map(|t| t.id)
        .collect();
    assert_eq!(owners, vec![placed_tab], "tile in exactly the reported tab");

    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
}

#[test]
fn back_to_back_close_tab_keeps_the_active_pointer_valid() {
    // A second close (or a close racing a focus) must never leave the
    // registry's activeTabId pointing at a deleted tab: removal + pointer
    // fixup are atomic under the registry lock, and focus_tab's validate+set
    // is a single atomic operation.
    let ctx = test_ctx("t");
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "a".into(),
            name: "A".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "b".into(),
            name: "B".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "c".into(),
            name: "C".into(),
            tile_ids: vec![],
        },
    ]);
    assert!(ctx.tab_registry().set_active_tab("c"));

    let active_is_valid = |ctx: &ControlContext| {
        let snap = ctx.tab_registry().snapshot_full();
        let active = snap.active_tab_id.expect("active pointer set");
        assert!(
            snap.tabs.iter().any(|t| t.id == active),
            "active '{active}' must reference an existing tab; tabs: {:?}",
            snap.tabs.iter().map(|t| t.id.clone()).collect::<Vec<_>>()
        );
    };

    // Close the ACTIVE tab, then immediately close the tab the pointer
    // healed onto - the pointer must stay valid after each step.
    dispatch(&ctx, "close_tab", &json!({"tabId": "c"})).unwrap();
    active_is_valid(&ctx);
    let healed = ctx.tab_registry().snapshot_full().active_tab_id.unwrap();
    dispatch(&ctx, "close_tab", &json!({"tabId": healed})).unwrap();
    active_is_valid(&ctx);

    // focus_tab on the now-deleted tab fails cleanly, pointer untouched.
    let err = dispatch(&ctx, "focus_tab", &json!({"tabId": "c"})).unwrap_err();
    assert!(err.contains("unknown tabId"), "got: {err}");
    active_is_valid(&ctx);

    // Concurrent closes from a 3-tab registry: whichever interleaving wins,
    // the surviving pointer references a live tab.
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "a".into(),
            name: "A".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "b".into(),
            name: "B".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "c".into(),
            name: "C".into(),
            tile_ids: vec![],
        },
    ]);
    assert!(ctx.tab_registry().set_active_tab("b"));
    let t1 = {
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = ctx.tab_registry().remove_tab("b", false);
        })
    };
    let t2 = {
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = ctx.tab_registry().remove_tab("c", false);
        })
    };
    t1.join().unwrap();
    t2.join().unwrap();
    active_is_valid(&ctx);
}

#[test]
fn spawn_terminal_default_placement_is_the_active_tab_without_switching() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // No tabName/tabId: the tile lands in the tab the USER has active (per the
    // registry mirror) - matching the "+" menu - and never switches it.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [
                {"id": "t1", "name": "Workspace 1", "tileIds": []},
                {"id": "t2", "name": "Workspace 2", "tileIds": []},
            ], "activeTabId": "t2"}),
    )
    .unwrap();

    let v = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).unwrap();
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["tabId"], "t2", "default placement is the active tab");
    let snap = ctx.tab_registry().snapshot_full();
    assert_eq!(snap.active_tab_id.as_deref(), Some("t2"), "focus untouched");
    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
}

#[test]
fn report_workspace_tabs_replaces_the_registry() {
    // The frontend's up-sync (via the Tauri command, exercised here directly on
    // the shared registry) makes list_tabs reflect the live UI, including
    // UI-created tabs and real tile membership.
    let ctx = test_ctx("t");
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "t1".into(),
            name: "Main".into(),
            tile_ids: vec!["a".into(), "b".into()],
        },
        TabRecord {
            id: "t2".into(),
            name: "Side".into(),
            tile_ids: vec![],
        },
    ]);
    let tabs = dispatch(&ctx, "list_tabs", &Value::Null).unwrap();
    assert_eq!(tabs["count"], 3);
    assert_eq!(tabs["tabs"][0]["id"], "t1");
    assert_eq!(tabs["tabs"][0]["tileIds"][1], "b");
    assert_eq!(tabs["tabs"][1]["name"], "Side");
    assert_eq!(tabs["tabs"][2]["kind"], "captain");
    assert_eq!(tabs["tabs"][2]["name"], CAPTAIN_WORKSPACE_NAME);
}

#[test]
fn terminal_inventory_prunes_only_the_registry_revision_it_observed() {
    let tabs = TabRegistry::new();
    tabs.replace(vec![TabRecord {
        id: "work".into(),
        name: "Workspace".into(),
        tile_ids: vec!["live".into(), "gone".into()],
    }]);
    let observed_seq = tabs.snapshot_full().seq;
    let live = std::collections::HashSet::from(["live".to_string()]);

    let pruned = tabs
        .prune_gone_tiles_if_seq(observed_seq, &live)
        .expect("unchanged registry should converge to terminal inventory");
    assert_eq!(pruned.tabs[0].tile_ids, vec!["live"]);

    tabs.replace(vec![TabRecord {
        id: "work".into(),
        name: "Workspace".into(),
        tile_ids: vec!["live".into(), "new".into()],
    }]);
    assert!(tabs.prune_gone_tiles_if_seq(pruned.seq, &live).is_none());
    assert_eq!(tabs.snapshot()[0].tile_ids, vec!["live", "new"]);
}

#[test]
fn create_worktree_named_placement_reuses_a_tab_by_name() {
    // TASK C: a create_worktree with a tabName that already exists resolves to
    // the SAME tab id (no duplicate), and the forward carries that id so the
    // frontend places the tile deterministically, not into the focused tab.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    // Seed an existing tab named "control-surface".
    ctx.tab_registry().replace(vec![TabRecord {
        id: "existing-tab".into(),
        name: "control-surface".into(),
        tile_ids: vec![],
    }]);
    // A create_worktree targeting that name should reuse `existing-tab`. We
    // resolve the tab BEFORE git runs by calling the registry directly for the
    // assertion (git::worktree_add needs a real repo, out of scope for a unit
    // test), mirroring the handler's own resolution.
    assert_eq!(
        ctx.tab_registry().id_for_name("control-surface"),
        Some("existing-tab".to_string())
    );
}

#[test]
fn report_workspace_tabs_replaces_the_registry_for_list_tabs() {
    // T12: the socket twin of the Tauri report command - the native client's
    // half of the registry mirror.
    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [
            {"id": "t1", "name": "Workspace 1", "tileIds": ["aa", "bb"]},
            {"id": "t2", "name": "ops", "tileIds": []},
        ]}),
    )
    .unwrap();
    assert_eq!(v["reported"], 2);

    let tabs = dispatch(&ctx, "list_tabs", &json!({})).unwrap();
    assert_eq!(tabs["count"], 3);
    assert_eq!(tabs["tabs"][0]["id"], "t1");
    assert_eq!(tabs["tabs"][0]["tileIds"], json!(["aa", "bb"]));
    assert_eq!(tabs["tabs"][1]["name"], "ops");
    assert_eq!(tabs["tabs"][2]["id"], CAPTAIN_WORKSPACE_ID);

    // A report may not erase the last Work Workspace. The reserved Captain
    // Workspace is not a usable canvas for ordinary terminals.
    let err = dispatch(&ctx, "report_workspace_tabs", &json!({"tabs": []})).unwrap_err();
    assert!(err.contains("at least one Work Workspace"), "got: {err}");
    assert_eq!(dispatch(&ctx, "list_tabs", &json!({})).unwrap()["count"], 3);

    // Malformed shapes are a clear error, not a partial replace.
    let err = dispatch(&ctx, "report_workspace_tabs", &json!({})).unwrap_err();
    assert!(err.contains("requires a 'tabs'"), "got: {err}");
    let err = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [{"name": 7}]}),
    )
    .unwrap_err();
    assert!(err.contains("bad 'tabs' shape"), "got: {err}");
}

#[test]
fn prune_tab_drops_the_tab_but_keeps_the_claim() {
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-1", Some("alpha"), vec!["tab-1".into(), "tab-2".into()])
        .unwrap();
    assert!(reg.prune_tab("tab-1").unwrap());
    let snap = reg.snapshot();
    assert_eq!(
        snap.captains[0].workspace_tab_ids,
        vec!["tab-2".to_string()]
    );
    assert!(
        !reg.prune_tab("tab-1").unwrap(),
        "already-pruned tab must not bump the revision"
    );
    assert!(reg.prune_tab("tab-2").unwrap());
    // Zero controlled tabs is a valid claim state.
    assert_eq!(reg.snapshot().captains.len(), 1);
}

#[test]
fn close_tab_persistence_failure_preserves_both_registries_and_projection() {
    let path = captains_tmp("close-tab-transaction");
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    captains
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![
        TabRecord {
            id: "work-a".into(),
            name: "Work A".into(),
            tile_ids: Vec::new(),
        },
        TabRecord {
            id: "work-b".into(),
            name: "Work B".into(),
            tile_ids: Vec::new(),
        },
    ]);
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let context = test_ctx("close-tab-transaction")
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs))
        .with_apply_sink(sink.clone());
    let before_captains = captains.snapshot();
    let before_tabs = tabs.snapshot_full();
    captains.fail_next_persist("close tab prune persistence failure");

    let error = dispatch(&context, "close_tab", &json!({"tabId": "work-a"})).unwrap_err();
    assert!(error.contains("close tab prune persistence failure"));
    assert_eq!(captains.snapshot().captains, before_captains.captains);
    assert_eq!(captains.snapshot().seq, before_captains.seq);
    assert_eq!(
        serde_json::to_value(tabs.snapshot_full().tabs).unwrap(),
        serde_json::to_value(before_tabs.tabs).unwrap()
    );
    assert_eq!(tabs.snapshot_full().seq, before_tabs.seq);
    assert!(sink.calls.lock().unwrap().is_empty());

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn report_workspace_tabs_prunes_closed_tabs_from_captains() {
    // The PRIMARY UI tab-close path is report_workspace_tabs (the webview
    // reports its new tab set), NOT the socket close_tab. A tab dropped from
    // the report must leave every captain's workspaceTabIds and forward a
    // captains snapshot - else it lingers as a phantom controlled-workspace.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "t1".into(),
            name: "Main".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "t2".into(),
            name: "Side".into(),
            tile_ids: vec![],
        },
    ]);
    ctx.captains
        .claim_test("cap-1", Some("alpha"), vec!["t1".into(), "t2".into()])
        .unwrap();

    // Report a tab set WITHOUT t2 (the user closed it): t2 is pruned from the
    // captain, and a sync_captains forward carries the pruned snapshot.
    dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [{"id": "t1", "name": "Main", "tileIds": []}]}),
    )
    .unwrap();
    assert_eq!(
        ctx.captains.snapshot().captains[0].workspace_tab_ids,
        vec!["t1".to_string()],
    );
    let calls = sink.calls.lock().unwrap();
    assert!(
        calls.iter().any(|(c, a)| c == "sync_captains"
            && a["sync"]["captains"][0]["workspaceTabIds"] == json!(["t1"])),
        "a sync_captains forward must carry the pruned workspaceTabIds"
    );
}

#[test]
fn close_tab_prunes_captain_workspace_ownership() {
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "tab-1".into(),
            name: "Main".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "tab-2".into(),
            name: "Side".into(),
            tile_ids: vec![],
        },
    ]);
    ctx.captains
        .claim_test("cap-1", Some("alpha"), vec!["tab-2".into()])
        .unwrap();

    dispatch(&ctx, "close_tab", &json!({"tabId": "tab-2"})).unwrap();
    let snap = ctx.captains.snapshot();
    assert_eq!(snap.captains[0].workspace_tab_ids, Vec::<String>::new());
    // The prune rode a sync_captains forward ahead of the close_tab apply.
    let calls = sink.calls.lock().unwrap();
    let names: Vec<&str> = calls.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(names, ["sync_captains", "close_tab"]);
}
