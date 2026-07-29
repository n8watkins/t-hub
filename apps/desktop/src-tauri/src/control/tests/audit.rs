use super::*;

#[test]
fn normal_captain_fanout_burst_not_refused_at_gate() {
    // THE most important test (design spec): a captain fanning out 6 crew in an
    // instant burst must NOT be refused by the fleet gate. With the default
    // burst of 8 the governor admits all six; they fail downstream only because
    // this headless ctx has no UI sink, never because of the budget.
    let dir = std::env::temp_dir().join("t-hub-gate-burst");
    clean_audit(&dir);
    let ctx = test_ctx("burst")
        .with_governor(Arc::new(SpawnGovernor::default()))
        .with_audit(Arc::new(AuditLog::new(dir.clone())));
    for i in 0..6 {
        let resp = dispatch_authenticated(
            &ctx,
            req(
                "burst",
                "spawn_terminal",
                json!({"cwd": "/tmp", "name": format!("crew-{i}")}),
            ),
        );
        let err = resp.error.clone().unwrap_or_default();
        assert!(
            !err.contains("rate limit"),
            "spawn {i} was rate-limited: {err}"
        );
        assert!(
            !err.contains("concurrent-session cap"),
            "spawn {i} hit the concurrent cap: {err}"
        );
    }
    clean_audit(&dir);
}

#[test]
fn socket_process_change_is_refused_before_side_effect_when_audit_sink_fails() {
    use std::io::{BufRead, BufReader, Write};

    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let id = format!("af{:06x}", (nanos as u64) & 0x00ff_ffff);
    let target = tmux::target_for_id(&id);
    let sentinel = std::env::temp_dir().join(format!("t-hub-audit-fail-e2e-{id}"));
    let sink_parent = std::env::temp_dir().join(format!("t-hub-audit-sink-file-{id}"));
    let _ = std::fs::remove_file(&sentinel);
    let _ = std::fs::remove_dir_all(&sink_parent);
    let _ = std::fs::remove_file(&sink_parent);
    std::fs::write(&sink_parent, b"not a directory").unwrap();

    let _ = tmux::kill_session(&target);
    tmux::new_session_with_env(&target, "/tmp", None, &[]).expect("spawn session");
    tmux::resize_window_for_tests(&target, 80, 24).expect("resize session");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let ctx = test_ctx("audit-e2e").with_audit(Arc::new(AuditLog::new(sink_parent.join("audit"))));
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        handle_conn(stream, &ctx).expect("serve request");
    });

    let command = format!("printf AUDIT_SIDE_EFFECT > {}", sentinel.display());
    let frame = json!({
        "token": "audit-e2e",
        "host": "audit-e2e",
        "command": "send_text",
        "args": {
            "sessionId": id,
            "text": command,
            "enter": true
        },
        "v": PROTOCOL_VERSION
    });
    let mut stream = TcpStream::connect(addr).expect("connect");
    let mut bytes = serde_json::to_vec(&frame).unwrap();
    bytes.push(b'\n');
    stream.write_all(&bytes).unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read response");
    let response: Value = serde_json::from_str(line.trim()).unwrap();
    drop(reader);
    server.join().unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !sentinel.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let side_effect_happened = sentinel.exists();
    println!("CONTROL_RESPONSE {response}");
    println!("SIDE_EFFECT_SENTINEL_EXISTS {side_effect_happened}");

    let _ = tmux::kill_session(&target);
    let _ = std::fs::remove_file(&sentinel);
    let _ = std::fs::remove_file(&sink_parent);

    assert_eq!(
        response["ok"], false,
        "a process-changing socket request must fail when its durable audit record cannot be written: {response}"
    );
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("audit sink unavailable")),
        "the refusal must identify the failed audit guarantee: {response}"
    );
    assert!(
        !side_effect_happened,
        "send_text reached the user's shell even though its audit record could not be written"
    );
}

#[test]
fn audit_refusal_releases_idempotency_reservation_for_retry() {
    let sink_parent = std::env::temp_dir().join(format!(
        "t-hub-audit-idempotency-failure-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let _ = std::fs::remove_dir_all(&sink_parent);
    let _ = std::fs::remove_file(&sink_parent);
    std::fs::write(&sink_parent, b"not a directory").unwrap();
    // Stub the live-session evidence. The spawn path gathers it by shelling out
    // to tmux BEFORE the audit gate, so a tmux server that is unreachable (a
    // loaded CI runner, a hostile socket name) refuses the request as
    // `refused-evidence` and this test never reaches the gate it covers.
    let ctx = test_ctx("audit-retry")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_audit(Arc::new(AuditLog::new(sink_parent.join("audit"))));
    let request = || {
        req(
            "audit-retry",
            "spawn_terminal",
            json!({"cwd": "/tmp", "requestId": "audit-retry-id"}),
        )
    };

    let first = dispatch_authenticated(&ctx, request());
    let second = dispatch_authenticated(&ctx, request());

    for response in [first, second] {
        let error = response.error.unwrap_or_default();
        assert!(
            error.contains("audit sink unavailable"),
            "a retry should reach the audit gate again: {error}"
        );
        assert!(
            !error.contains("already in flight"),
            "the failed audit gate leaked its reservation: {error}"
        );
    }
    let _ = std::fs::remove_file(&sink_parent);
}

#[test]
fn audit_refusal_refunds_governor_admission() {
    let broken_sink = || {
        let sink_parent = std::env::temp_dir().join(format!(
            "t-hub-audit-governor-failure-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&sink_parent, b"not a directory").unwrap();
        sink_parent
    };

    let spawn_sink = broken_sink();
    // Stub the live-session evidence: both the dispatch below and the
    // `admit_spawn` refund check gather it from tmux before the audit gate, so
    // an unreachable tmux server refuses as `refused-evidence` and hides the
    // gate behaviour this test covers.
    let spawn_ctx = test_ctx("audit-spawn-refund")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_governor(Arc::new(SpawnGovernor::new(128, 0.0, 1.0)))
        .with_audit(Arc::new(AuditLog::new(spawn_sink.join("audit"))));
    let response = dispatch_authenticated(
        &spawn_ctx,
        req(
            "audit-spawn-refund",
            "spawn_terminal",
            json!({"cwd": "/tmp"}),
        ),
    );
    assert!(response
        .error
        .unwrap_or_default()
        .contains("audit sink unavailable"));
    assert!(
        admit_spawn(&spawn_ctx, SpawnPurpose::Ordinary, 0, None).is_ok(),
        "an audit refusal consumed the sole spawn-rate token"
    );
    let _ = std::fs::remove_file(&spawn_sink);

    let destructive_sink = broken_sink();
    let destructive_ctx = test_ctx("audit-destructive-refund")
        .with_audit(Arc::new(AuditLog::new(destructive_sink.join("audit"))));
    let response = dispatch_authenticated(
        &destructive_ctx,
        req(
            "audit-destructive-refund",
            "send_keys",
            json!({"sessionId": "ghost", "keys": ["C-C"]}),
        ),
    );
    assert!(response
        .error
        .unwrap_or_default()
        .contains("audit sink unavailable"));
    for _ in 0..crate::governor::DESTRUCTIVE_BURST as usize {
        assert!(
            destructive_ctx
                .governor
                .check_destructive(std::time::Instant::now())
                .is_ok(),
            "an audit refusal consumed destructive-rate quota"
        );
    }
    let _ = std::fs::remove_file(&destructive_sink);
}

#[test]
fn audit_integrity_failure_skips_startup_recovery() {
    let sink_parent = std::env::temp_dir().join(format!(
        "t-hub-audit-startup-recovery-failure-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&sink_parent, b"not a directory").unwrap();
    let ctx = test_ctx("audit-startup-recovery")
        .with_audit(Arc::new(AuditLog::new(sink_parent.join("audit"))));

    assert!(
        !recover_pending_fleet_operations_after_audit_check(&ctx),
        "startup recovery ran without a successful integrity check"
    );
    let _ = std::fs::remove_file(&sink_parent);
}

#[test]
fn spawn_rate_limit_refuses_with_exact_message_and_audits() {
    // Burst 1: the first spawn spends the only token; the second is refused with
    // the exact §5 message and recorded as `refused-rate`.
    let dir = std::env::temp_dir().join("t-hub-gate-rate");
    clean_audit(&dir);
    let ctx = test_ctx("rate")
        .with_governor(Arc::new(SpawnGovernor::new(64, 20.0, 1.0)))
        .with_audit(Arc::new(AuditLog::new(dir.clone())));
    let r1 = dispatch_authenticated(&ctx, req("rate", "spawn_terminal", json!({"cwd": "/tmp"})));
    // Governor admitted r1; it fails downstream on the missing UI sink.
    assert!(
        r1.error.clone().unwrap_or_default().contains("no UI"),
        "got: {:?}",
        r1.error
    );
    let r2 = dispatch_authenticated(&ctx, req("rate", "spawn_terminal", json!({"cwd": "/tmp"})));
    assert!(
        r2.error
            .clone()
            .unwrap()
            .contains("spawn rate limit (20/min); retry shortly"),
        "got: {:?}",
        r2.error
    );

    let recs = read_audit(&dir);
    assert_eq!(recs.len(), 2, "expected an allowed + a refused record");
    assert_eq!(recs[0]["decision"], "allowed");
    assert_eq!(recs[0]["command"], "spawn_terminal");
    assert_eq!(recs[1]["decision"], "refused-rate");
    // The hash chain links the refusal to the prior line.
    assert_eq!(recs[1]["prev"], recs[0]["hash"]);
    clean_audit(&dir);
}

#[test]
fn read_tier_is_not_gated_or_audited() {
    // list_terminals is Read tier: it must never touch the governor or the audit
    // log, whether or not tmux is reachable in the test env.
    let dir = std::env::temp_dir().join("t-hub-gate-read");
    clean_audit(&dir);
    let ctx = test_ctx("read").with_audit(Arc::new(AuditLog::new(dir.clone())));
    let _ = dispatch_authenticated(&ctx, req("read", "list_terminals", json!({})));
    assert!(
        read_audit(&dir).is_empty(),
        "a read-tier command was audited"
    );
    clean_audit(&dir);
}

#[test]
fn send_text_is_audited_with_redaction_through_gate() {
    // send_text is process-changing (audited) but NOT rate-limited. The literal
    // text must never reach the audit log - only a length + hash.
    let dir = std::env::temp_dir().join("t-hub-gate-sendtext");
    clean_audit(&dir);
    let ctx = test_ctx("st").with_audit(Arc::new(AuditLog::new(dir.clone())));
    let resp = dispatch_authenticated(
        &ctx,
        req(
            "st",
            "send_text",
            json!({"sessionId": "ghost", "text": "SUPERSECRET", "enter": true}),
        ),
    );
    assert!(!resp.ok); // no such session, but the audit still lands
    let recs = read_audit(&dir);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0]["command"], "send_text");
    assert_eq!(recs[0]["decision"], "allowed");
    assert_eq!(recs[0]["phase"], "authorization");
    assert!(recs[0].get("outcome").is_none());
    let blob = serde_json::to_string(&recs[0]).unwrap();
    assert!(
        !blob.contains("SUPERSECRET"),
        "literal text leaked into audit: {blob}"
    );
    assert_eq!(recs[0]["args"]["textLen"], 11);
    clean_audit(&dir);
}

#[test]
fn bad_token_is_rejected_and_not_audited() {
    // A bad token is rejected before the gate and never audited (no leak of the
    // process-changing surface to an unauthenticated probe).
    let dir = std::env::temp_dir().join("t-hub-gate-badtok");
    clean_audit(&dir);
    let ctx = test_ctx("good").with_audit(Arc::new(AuditLog::new(dir.clone())));
    let resp = dispatch_authenticated(&ctx, req("WRONG", "spawn_terminal", json!({})));
    assert!(resp.error.unwrap().contains("bad control token"));
    assert!(read_audit(&dir).is_empty());
    clean_audit(&dir);
}

#[test]
fn kill_style_send_keys_is_throttled_but_navigation_is_not() {
    // The destructive throttle covers kill-style keys (C-c) but not navigation
    // (Up/Enter) - proven at the classifier the gate uses.
    assert!(keys_are_kill_style(&json!({"keys": ["C-c"]})));
    assert!(keys_are_kill_style(&json!({"keys": ["Up", "C-d"]})));
    assert!(!keys_are_kill_style(&json!({"keys": ["Up", "Enter"]})));
    assert!(!keys_are_kill_style(&json!({"keys": []})));
}

#[test]
fn command_tiers_are_classified() {
    assert_eq!(
        required_tier("spawn_terminal"),
        CommandTier::ProcessChanging
    );
    assert_eq!(
        required_tier("close_terminal"),
        CommandTier::ProcessChanging
    );
    assert_eq!(
        required_tier("history_resume"),
        CommandTier::ProcessChanging
    );
    assert_eq!(
        required_tier("cleanup_worktree_artifacts"),
        CommandTier::ProcessChanging
    );
    for command in ["preview_start", "preview_stop", "preview_restart"] {
        assert_eq!(required_tier(command), CommandTier::ProcessChanging);
    }
    for command in ["preview_select", "preview_refresh", "preview_open"] {
        assert_eq!(required_tier(command), CommandTier::Organization);
    }
    for command in ["preview_discover", "preview_status"] {
        assert_eq!(required_tier(command), CommandTier::Read);
    }
    assert_eq!(required_tier("send_text"), CommandTier::ProcessChanging);
    assert_eq!(required_tier("new_tab"), CommandTier::Organization);
    assert_eq!(required_tier("history_focus"), CommandTier::Organization);
    assert_eq!(required_tier("create_worktree"), CommandTier::Organization);
    assert_eq!(required_tier("remove_worktree"), CommandTier::Organization);
    assert_eq!(required_tier("list_terminals"), CommandTier::Read);
    assert_eq!(required_tier("get_status"), CommandTier::Read);
    assert_eq!(required_tier("history_list"), CommandTier::Organization);
    assert_eq!(required_tier("invalidate_history_cache"), CommandTier::Read);
    // Comms-plane Phase 2 (review H1): `inbox_ack` mutates + compacts durable
    // receipt state, so it must require the control token (Organization) and be
    // audited - NOT fall through to the read tier. `inbox_status` is counts-only
    // and stays Read.
    assert_eq!(required_tier("inbox_ack"), CommandTier::Organization);
    assert_eq!(required_tier("inbox_status"), CommandTier::Read);
}

#[test]
fn refusal_audit_rate_limit_bounds_durable_writes() {
    let dir = std::env::temp_dir().join(format!(
        "t-hub-refusal-audit-rate-{}",
        uuid::Uuid::new_v4().simple()
    ));
    clean_audit(&dir);
    let ctx = test_ctx("refusal-rate").with_audit(Arc::new(AuditLog::new(dir.clone())));
    let attempts = crate::governor::REFUSAL_AUDIT_BURST + 5;

    for _ in 0..attempts {
        let response = dispatch_authenticated(
            &ctx,
            req(
                "read-refusal-rate",
                "send_text",
                json!({"sessionId": "x", "text": "y"}),
            ),
        );
        assert_eq!(
            response.error.as_deref(),
            Some(
                "unauthorized: 'send_text' requires the control capability (this token is read-only)"
            )
        );
    }

    let records = read_audit(&dir);
    assert_eq!(records.len(), crate::governor::REFUSAL_AUDIT_BURST);
    assert!(records
        .iter()
        .all(|record| record["decision"] == "refused-authz"));
    clean_audit(&dir);
}

#[test]
fn control_token_command_audits_token_tier_control() {
    let dir = std::env::temp_dir().join("t-hub-p2-ctltier");
    clean_audit(&dir);
    let ctx = test_ctx("t").with_audit(Arc::new(AuditLog::new(dir.clone())));
    // An Organization command with the control token: allowed, audited control.
    let _ = dispatch_authenticated(&ctx, req("t", "new_tab", json!({"name": "T"})));
    let recs = read_audit(&dir);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0]["tokenTier"], "control");
    assert_eq!(recs[0]["decision"], "allowed");
    clean_audit(&dir);
}

#[test]
fn audit_verify_reports_live_integrity_to_a_read_token() {
    let dir = std::env::temp_dir().join(format!(
        "t-hub-audit-verify-{}",
        uuid::Uuid::new_v4().simple()
    ));
    clean_audit(&dir);
    let ctx = test_ctx("audit-verify").with_audit(Arc::new(AuditLog::new(dir.clone())));

    let mutation = dispatch_authenticated(
        &ctx,
        req(
            "audit-verify",
            "send_text",
            json!({"sessionId": "missing", "text": "hello", "enter": true}),
        ),
    );
    assert!(!mutation.ok, "the target session is intentionally absent");

    let response =
        dispatch_authenticated(&ctx, req("read-audit-verify", "audit_verify", json!({})));
    assert!(response.ok);
    let report = response.result.unwrap();
    println!(
        "AUDIT_VERIFY_RESPONSE {}",
        json!({"ok": true, "result": report})
    );
    assert_eq!(report["ok"], true);
    assert_eq!(report["records"], 1);
    assert_eq!(report["breaks"], json!([]));
    clean_audit(&dir);
}
