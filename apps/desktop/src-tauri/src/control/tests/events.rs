use super::*;

#[test]
fn organization_actions_are_accepted_and_audited() {
    // No apply sink (headless): accepted + audited, but not applied.
    // focus_session and a targetId-only move_tile (within-tab reorder) stay
    // legacy pass-through forwards.
    let ctx = test_ctx("t");
    for (cmd, args) in [
        ("focus_session", json!({"sessionId": "s1"})),
        ("move_tile", json!({"terminalId": "t1", "targetId": "t2"})),
    ] {
        let v = dispatch(&ctx, cmd, &args).unwrap();
        assert_eq!(v["accepted"], cmd);
        assert_eq!(v["audited"], true);
        assert_eq!(v["applied"], false);
    }
    // Headless-org: registry-first mutations are STRICT - an unknown target
    // is a hard error, not the old silent accept-then-lose.
    for (cmd, args) in [
        ("move_tile", json!({"terminalId": "t1", "tabId": "nope"})),
        ("rename_tab", json!({"tabId": "nope", "name": "x"})),
        ("close_tab", json!({"tabId": "nope"})),
    ] {
        let err = dispatch(&ctx, cmd, &args).unwrap_err();
        assert!(err.contains("unknown"), "{cmd}: {err}");
    }
}

#[test]
fn organization_actions_are_forwarded_and_applied_with_a_sink() {
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "tab-1".into(),
            name: "Main".into(),
            tile_ids: vec!["term-1".into()],
        },
        TabRecord {
            id: "tab-2".into(),
            name: "Side".into(),
            tile_ids: vec![],
        },
    ]);

    for (cmd, args) in [
        ("focus_session", json!({"sessionId": "term-1"})),
        (
            "move_tile",
            json!({"terminalId": "term-1", "tabId": "tab-2"}),
        ),
        ("rename_tab", json!({"tabId": "tab-2", "name": "Ops"})),
    ] {
        let v = dispatch(&ctx, cmd, &args).unwrap();
        assert_eq!(v["accepted"], cmd);
        assert_eq!(v["audited"], true);
        // With a sink wired, the action is forwarded to the UI and applied.
        assert_eq!(v["applied"], true, "expected applied:true for {cmd}");
    }

    // Every Organization-tier command reached the sink, in order, with args.
    let calls = sink.calls.lock().unwrap();
    let names: Vec<&str> = calls.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(names, ["focus_session", "move_tile", "rename_tab"]);
    assert_eq!(calls[0].1, json!({"sessionId": "term-1"}));

    // Headless-org: registry-first forwards carry the authoritative snapshot
    // (`sync.seq` / `sync.tabs`) so the UI renders FROM the registry - the
    // move is visible in the snapshot even before any UI report.
    let sync = &calls[1].1["sync"];
    assert!(sync["seq"].as_u64().unwrap() >= 1);
    let tabs = sync["tabs"].as_array().unwrap();
    let tab2 = tabs.iter().find(|t| t["id"] == "tab-2").unwrap();
    assert_eq!(tab2["tileIds"], json!(["term-1"]));
    assert_eq!(calls[2].1["name"], "Ops");
}

/// SERVE-PATH WEDGE REGRESSION: a subscriber that stops draining its socket
/// must not stall an UNRELATED fanout operation. This reproduces the control
/// wedge in the small: `emit_event` used to hold the `subs` registry lock
/// across every blocking per-subscriber `write_all`, so a single stuck client
/// (its send buffer full) parked the lock for the full 5s `SO_SNDTIMEO` - and
/// with it every `register`/`unregister`/`subscriber_count` and every other
/// emit. Here a background emit blocks writing to a never-draining subscriber
/// while the main thread times a `register` + `subscriber_count`; with the lock
/// held across the write those calls block ~5s (the test's 3s bound trips),
/// and with the snapshot-then-write-unlocked fix they return immediately.
#[test]
fn stuck_subscriber_does_not_stall_registry_ops() {
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    let fanout = Arc::new(EventFanout::new());

    // A "stuck" subscriber: a real loopback socket whose CLIENT end never
    // reads. We shrink both buffers so a modest frame overflows the send path
    // and the emit's `write_all` blocks (until the 5s subscriber write timeout
    // register() installs). The client MUST stay alive and unread for the
    // duration, so we hold it in scope and never touch it.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let stuck_client = TcpStream::connect(addr).expect("connect stuck client");
    {
        let cref = socket2::SockRef::from(&stuck_client);
        let _ = cref.set_recv_buffer_size(1024);
    }
    let (stuck_server, _) = listener.accept().expect("accept stuck server");
    {
        let sref = socket2::SockRef::from(&stuck_server);
        let _ = sref.set_send_buffer_size(1024);
    }
    fanout.register(stuck_server);

    // Background emit: a payload comfortably larger than the shrunk buffers so
    // the write to the stuck subscriber blocks rather than completing.
    let emit_fanout = Arc::clone(&fanout);
    let emitter = std::thread::spawn(move || {
        let big = "x".repeat(4 * 1024 * 1024);
        emit_fanout.emit_event("control://wedge-test", &json!({ "blob": big }));
    });

    // Let the emit get into its blocking write (and, on the buggy code, take
    // and hold the registry lock). This delay is OUTSIDE the measured window.
    std::thread::sleep(Duration::from_millis(300));

    // The unrelated registry ops. On the pre-fix code these block on the
    // `subs` lock the stuck emit holds for ~5s; with the fix the lock is free.
    let healthy_listener = TcpListener::bind("127.0.0.1:0").expect("bind healthy");
    let healthy_addr = healthy_listener.local_addr().unwrap();
    let _healthy_client = TcpStream::connect(healthy_addr).expect("connect healthy");
    let (healthy_server, _) = healthy_listener.accept().expect("accept healthy");

    let started = Instant::now();
    let id = fanout.register(healthy_server);
    let count = fanout.subscriber_count();
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(3),
        "registry ops stalled behind a stuck subscriber's emit write ({elapsed:?}); \
             the subs lock is being held across the blocking socket write"
    );
    assert!(count >= 1, "the healthy subscriber should be registered");
    let _ = id;

    // The stuck subscriber's write eventually times out (5s SO_SNDTIMEO) and
    // the emit thread returns; join so the test owns no leaked thread. Keep the
    // stuck client alive until here so the connection never closes early.
    emitter.join().expect("emit thread joins");
    drop(stuck_client);
}

#[test]
fn apply_forwards_are_broadcast_to_event_subscribers() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // T12: every accepted Organization forward ALSO reaches event
    // subscribers on `control://apply`, while the webview sink keeps
    // receiving exactly what it always did.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let fanout = Arc::new(EventFanout::new());
    let ctx = test_ctx("t")
        .with_apply_sink(sink.clone())
        .with_event_fanout(fanout.clone());
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    let mut reader = subscribe_test_reader(&fanout);

    // A plain organization apply: broadcast mirrors the sink call.
    let v = dispatch(&ctx, "focus_tab", &json!({"tabId": "tab-1"})).unwrap();
    assert_eq!(v["applied"], true);
    let frame = read_event_frame(&mut reader);
    assert_eq!(frame["event"], APPLY_EVENT_CHANNEL);
    assert_eq!(frame["payload"]["command"], "focus_tab");
    assert_eq!(frame["payload"]["args"], json!({"tabId": "tab-1"}));

    // new_tab: the broadcast carries the SAME core-minted id the caller got.
    let v = dispatch(&ctx, "new_tab", &json!({"name": "Logs"})).unwrap();
    let frame = read_event_frame(&mut reader);
    assert_eq!(frame["payload"]["command"], "new_tab");
    assert_eq!(frame["payload"]["args"]["id"], v["tabId"]);
    assert_eq!(frame["payload"]["args"]["name"], "Logs");

    // spawn_terminal: the server spawns + places (headless-org), so sink AND
    // subscribers both hear the forward WITH the real id + registry snapshot.
    let v = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp", "name": "n"})).unwrap();
    assert_eq!(v["accepted"], "spawn_terminal");
    let spawned_id = v["id"].as_str().unwrap().to_string();
    let frame = read_event_frame(&mut reader);
    assert_eq!(frame["payload"]["command"], "spawn_terminal");
    assert_eq!(frame["payload"]["args"]["cwd"], "/tmp");
    assert_eq!(frame["payload"]["args"]["id"], json!(spawned_id));
    assert!(frame["payload"]["args"]["sync"]["seq"].as_u64().is_some());

    // remove_worktree fails before either the sink or subscribers receive a
    // detach forward.
    let err = dispatch(
        &ctx,
        "remove_worktree",
        &json!({"repoRoot": "/r", "worktreePath": "/r/wt"}),
    )
    .unwrap_err();
    assert_eq!(err, git::WORKTREE_REMOVAL_UNAVAILABLE);
    assert_no_event(&mut reader);

    // The sink saw every forward, unchanged by the broadcast riding along.
    let names: Vec<String> = sink
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(c, _)| c.clone())
        .collect();
    assert_eq!(names, ["focus_tab", "new_tab", "spawn_terminal"]);

    // Reap the real session the spawn created.
    dispatch(&ctx, "close_terminal", &json!({"sessionId": spawned_id})).unwrap();
}

#[test]
fn forward_without_sink_counts_event_subscribers_as_delivery() {
    // T12: a headless server fronting the native cockpit has no ApplySink;
    // reaching an event subscriber is what "applied" means there.
    let fanout = Arc::new(EventFanout::new());
    let ctx = test_ctx("t").with_event_fanout(fanout.clone());
    ctx.tab_registry().replace(vec![TabRecord {
        id: "x".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    let mut reader = subscribe_test_reader(&fanout);

    let v = dispatch(&ctx, "rename_tab", &json!({"tabId": "x", "name": "ops"})).unwrap();
    assert_eq!(
        v["applied"], true,
        "subscriber delivery counts without a sink"
    );
    let frame = read_event_frame(&mut reader);
    assert_eq!(frame["payload"]["command"], "rename_tab");
    // (Sink-less AND subscriber-less stays applied:false - covered by
    // `organization_actions_are_accepted_and_audited`.)
}

#[test]
fn event_fanout_streams_a_frame_to_a_subscriber() {
    // server-split M1: a registered subscriber receives each backend event as a
    // newline-delimited `{event,payload}` frame; unregister drops it. Uses a
    // real loopback socket pair but is deterministic (no disconnect-timing
    // races — we assert the explicit unregister, not write-error pruning).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let client = TcpStream::connect(addr).unwrap();
    let (server, _) = listener.accept().unwrap();

    let fanout = EventFanout::new();
    let id = fanout.register(server);
    assert_eq!(fanout.subscriber_count(), 1);

    fanout.emit_event(
        "session://status",
        &json!({ "sessionId": "s1", "status": "working" }),
    );

    let mut reader = BufReader::new(&client);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let frame: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(frame["event"], "session://status");
    assert_eq!(frame["payload"]["sessionId"], "s1");
    assert_eq!(frame["payload"]["status"], "working");

    fanout.unregister(id);
    assert_eq!(fanout.subscriber_count(), 0);
}
