use super::*;

/// The id-namespace bridge: the supervisor keys by the Claude UUID, but callers
/// address a captain by its tile id (`captainSessionId`). `get_status` must
/// resolve tile -> UUID via the status bridge, so a captain's status is no longer
/// a spurious `unknown`. A UUID passed directly is unchanged.
#[test]
fn get_status_resolves_a_captain_tile_id_to_its_claude_uuid() {
    use t_hub_protocol::JournalEventType;
    let supervisor = Arc::new(StdMutex::new(Supervisor::new()));
    supervisor.lock().unwrap().ingest(
        Some("uuid-abc"),
        None,
        None,
        None,
        JournalEventType::SessionStart,
        1,
    );
    let sup_for_closure = supervisor.clone();
    let visitor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync> =
        Arc::new(move |f: &mut dyn FnMut(&Supervisor)| {
            let guard = sup_for_closure.lock().unwrap();
            f(&guard);
        });
    let status = Arc::new(StatusBridge::new());
    // The tile `cap01234` currently hosts Claude session `uuid-abc`.
    status.ingest(
        "uuid-abc",
        &json!({ "cwd": "/p", "tmux_session": "th_cap01234" }),
        1,
    );
    let ctx = ControlContext::new(status, visitor, "t".to_string());

    // Poll by the CAPTAIN tile id -> resolves to the UUID, returns the real status.
    let v = get_status(&ctx, &json!({ "sessionId": "cap01234" })).unwrap();
    assert_eq!(
        v.get("resolvedSessionId").and_then(|x| x.as_str()),
        Some("uuid-abc"),
        "tile id must resolve to the Claude UUID"
    );
    assert_eq!(
        v.get("status").and_then(|x| x.as_str()),
        Some("working"),
        "status must be the real supervisor status, not 'unknown'"
    );
    // A UUID (already a supervisor key) is passed through untouched.
    let v2 = get_status(&ctx, &json!({ "sessionId": "uuid-abc" })).unwrap();
    assert_eq!(
        v2.get("resolvedSessionId").and_then(|x| x.as_str()),
        Some("uuid-abc")
    );
    // A genuinely unknown id still resolves to unknown (no regression).
    let v3 = get_status(&ctx, &json!({ "sessionId": "ghostzzzz" })).unwrap();
    assert_eq!(v3.get("status").and_then(|x| x.as_str()), Some("unknown"));
}

#[test]
fn host_metrics_prefers_the_bridge_and_serializes_snake_case() {
    // A stubbed agent-bridge metrics RPC: the handler must PREFER it over the
    // daemon's local /proc, and serialize snake_case (the frontend wire shape in
    // src/ipc/protocol.ts) — NOT the camelCase `wsl_health` shape.
    let ctx = test_ctx("t").with_metrics(Arc::new(|| {
        Ok(t_hub_protocol::HostMetrics {
            mem_total_kib: 16_000_000,
            mem_available_kib: 8_000_000,
            swap_total_kib: 2_000_000,
            swap_free_kib: 1_500_000,
            cpu_count: 12,
            load_avg: [1.0, 0.5, 0.25],
            process_count: 432,
            distro: Some("Ubuntu 24.04".into()),
            captured_at_ms: 1_700_000_000_000,
        })
    }));
    let v = dispatch(&ctx, "host_metrics", &Value::Null).unwrap();
    assert_eq!(
        v.get("mem_total_kib").and_then(|x| x.as_u64()),
        Some(16_000_000)
    );
    assert_eq!(v.get("cpu_count").and_then(|x| x.as_u64()), Some(12));
    assert_eq!(v.get("process_count").and_then(|x| x.as_u64()), Some(432));
    assert_eq!(
        v.get("distro").and_then(|x| x.as_str()),
        Some("Ubuntu 24.04")
    );
    assert!(
        v.get("memTotalKib").is_none(),
        "must be snake_case, not the camelCase wsl_health shape"
    );
}

#[test]
fn host_metrics_falls_back_when_the_bridge_errors() {
    // Bridge says "not connected". On Linux the daemon's own /proc IS the real
    // host (native-WSL / remote-Linux daemon, or the dev box), so we serve a
    // snake_case snapshot from it. On non-Linux the local /proc would be
    // all-zeros, so we surface the error instead (preserves today's UX).
    let ctx = test_ctx("t").with_metrics(Arc::new(|| Err("not connected".into())));
    let out = dispatch(&ctx, "host_metrics", &Value::Null);
    #[cfg(target_os = "linux")]
    {
        let v = out.expect("linux falls back to local /proc");
        assert!(
            v.get("mem_total_kib").is_some(),
            "snake_case local snapshot"
        );
        assert!(v.get("captured_at_ms").is_some());
    }
    #[cfg(not(target_os = "linux"))]
    {
        assert!(out.unwrap_err().contains("not connected"));
    }
}

#[test]
fn wait_for_status_immediate_match_does_not_time_out() {
    // An empty Supervisor reports `unknown` for any unseen session, so a
    // target of "unknown" matches on the first poll and returns at once.
    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "wait_for_status",
        &json!({"sessionId": "absent", "targetStatus": "unknown"}),
    )
    .unwrap();
    assert_eq!(v["finalStatus"], "unknown");
    assert_eq!(v["timedOut"], false);
}

#[test]
fn wait_for_status_accepts_target_array() {
    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "wait_for_status",
        &json!({"sessionId": "absent", "targetStatus": ["completed", "unknown"]}),
    )
    .unwrap();
    assert_eq!(v["finalStatus"], "unknown");
    assert_eq!(v["timedOut"], false);
}

#[test]
fn wait_for_status_times_out_when_target_never_seen() {
    // A status that never occurs for an unseen session, with a 0ms timeout,
    // returns on the first iteration with timedOut:true.
    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "wait_for_status",
        &json!({"sessionId": "absent", "targetStatus": "completed", "timeoutMs": 0}),
    )
    .unwrap();
    assert_eq!(v["finalStatus"], "unknown");
    assert_eq!(v["timedOut"], true);
}

#[test]
fn wait_for_status_requires_session_and_target() {
    let ctx = test_ctx("t");
    let err = dispatch(
        &ctx,
        "wait_for_status",
        &json!({"targetStatus": "completed"}),
    )
    .unwrap_err();
    assert!(err.contains("sessionId"), "got: {err}");
    let err = dispatch(&ctx, "wait_for_status", &json!({"sessionId": "x"})).unwrap_err();
    assert!(err.contains("targetStatus"), "got: {err}");
}

#[test]
fn get_status_requires_session_id() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "get_status", &Value::Null).unwrap_err();
    assert!(err.contains("sessionId"), "got: {err}");
}

#[test]
fn get_status_returns_unknown_for_unseen_session() {
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "get_status", &json!({"sessionId": "nope"})).unwrap();
    assert_eq!(v["status"], "unknown");
    assert_eq!(v["sessionId"], "nope");
    assert!(v["snapshot"].is_null());
}

#[test]
fn supervision_tree_unknown_session_is_null() {
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "supervision_tree", &json!({"sessionId": "nope"})).unwrap();
    assert!(v.is_null());
}

#[test]
fn supervision_session_ids_returns_an_array() {
    // An empty supervisor reports no sessions — but the command returns a JSON
    // array (not null/error), matching the Tauri command's `Vec<String>`.
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "supervision_session_ids", &Value::Null).unwrap();
    assert!(v.is_array(), "expected an array, got {v:?}");
    assert_eq!(v.as_array().unwrap().len(), 0);
}

#[test]
fn wsl_health_has_metrics_and_supervised_count() {
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "wsl_health", &Value::Null).unwrap();
    assert!(v.get("metrics").is_some());
    assert_eq!(v["supervisedSessions"], 0);
    // The metrics object always carries capturedAtMs + cpuCount.
    assert!(v["metrics"].get("capturedAtMs").is_some());
    assert!(v["metrics"].get("cpuCount").is_some());
}
