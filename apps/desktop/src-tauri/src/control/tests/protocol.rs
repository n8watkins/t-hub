use super::*;

#[test]
fn pty_output_and_probe_ack_frames_cannot_interleave() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let outbound = Arc::new(Mutex::new(server));
    let mut sink = SharedPtyWriter {
        outbound: outbound.clone(),
        buffer: Vec::new(),
    };

    // Simulate the output producer constructing one frame through partial
    // writes while the input path emits an acknowledgement in between.
    sink.write_all(br#"{"out":"YW"#).unwrap();
    {
        let mut writer = outbound.lock().unwrap();
        write_json_line(&mut writer, &json!({ "probeAck": 7 })).unwrap();
    }
    sink.write_all(b"Jj\"}\n").unwrap();
    sink.flush().unwrap();

    let mut reader = BufReader::new(client);
    let mut first = String::new();
    let mut second = String::new();
    reader.read_line(&mut first).unwrap();
    reader.read_line(&mut second).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&first).unwrap(),
        json!({ "probeAck": 7 })
    );
    assert_eq!(
        serde_json::from_str::<Value>(&second).unwrap(),
        json!({ "out": "YWJj" })
    );
}

#[test]
fn control_request_debug_redacts_all_credential_and_argument_values() {
    let request = ControlRequest {
        token: "global-control-secret".into(),
        command: "new_tab".into(),
        args: serde_json::json!({"credential": "argument-secret"}),
        session: "bound-session-secret".into(),
        host: "host-proof-secret".into(),
        v: Some(PROTOCOL_VERSION),
    };

    let diagnostic = format!("{request:?}");
    assert!(diagnostic.contains("ControlRequest"));
    assert!(diagnostic.contains("new_tab"));
    assert!(diagnostic.contains("<redacted>"));
    for secret in [
        "global-control-secret",
        "argument-secret",
        "bound-session-secret",
        "host-proof-secret",
    ] {
        assert!(
            !diagnostic.contains(secret),
            "ControlRequest Debug leaked {secret}"
        );
    }
}

#[test]
fn bad_token_is_rejected_before_dispatch() {
    let ctx = test_ctx("secret");
    let req = ControlRequest {
        token: "wrong".into(),
        command: "list_tabs".into(),
        args: Value::Null,
        session: String::new(),
        host: String::new(),
        v: None,
    };
    let resp = dispatch_authenticated(&ctx, req);
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("unauthorized"));
}

#[test]
fn good_token_dispatches() {
    let ctx = test_ctx("secret");
    let req = ControlRequest {
        token: "secret".into(),
        command: "list_tabs".into(),
        args: Value::Null,
        session: String::new(),
        host: "secret".into(),
        v: None,
    };
    let resp = dispatch_authenticated(&ctx, req);
    assert!(resp.ok, "expected ok, got {:?}", resp.error);
    assert!(resp.result.unwrap().get("tabs").is_some());
}

#[test]
fn unknown_command_is_refused() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "definitely_not_a_command", &Value::Null).unwrap_err();
    assert!(err.contains("not exposed over the control channel"));
}

/// LOW-1 guard (PR-58 review): a retryable control error carries a STRUCTURED
/// `retryable:true` flag on the wire so fleet automation discriminates a wedge
/// from a genuine error WITHOUT substring-matching prose - and the machine marker
/// never leaks into the human text, and the flag is omitted (wire unchanged) for
/// non-retryable errors. Ties a real site (the writer gate) through
/// `retryable_error` → `ControlResponse::err` → serialization. Bypass: drop the
/// `retryable_error` wrapper on the Unknown arm and the `retryable==true` assert
/// trips.
#[test]
fn low1_retryable_errors_carry_a_structured_flag_not_prose() {
    use tmux::SessionLiveness::*;
    // A retryable site (writer gate on Unknown) → structured retryable + clean text.
    let gate_err =
        writer_liveness_gate("send_text", "e05764f5", "th_e05764f5", Unknown).unwrap_err();
    let resp = ControlResponse::err(gate_err);
    assert!(!resp.ok);
    assert!(
        resp.retryable,
        "an Unknown-arm error must be structurally retryable"
    );
    let text = resp.error.as_deref().unwrap_or("");
    assert!(
        !text.contains(RETRYABLE_ERROR_MARKER),
        "the machine marker must be stripped from the wire text; got: {text:?}"
    );
    assert!(
        text.contains("timed out") && text.contains("retry"),
        "human guidance is preserved: {text}"
    );
    // A definitive (Gone) error is NOT retryable.
    let gone_err = writer_liveness_gate("send_text", "e05764f5", "th_e05764f5", Gone).unwrap_err();
    let resp_gone = ControlResponse::err(gone_err);
    assert!(
        !resp_gone.retryable,
        "a definitive 'no such session' must not be flagged retryable"
    );
    // Serialization: `retryable` present only when true (wire unchanged otherwise).
    let j = serde_json::to_value(&resp).unwrap();
    assert_eq!(j.get("retryable").and_then(|v| v.as_bool()), Some(true));
    let j_gone = serde_json::to_value(&resp_gone).unwrap();
    assert!(
        j_gone.get("retryable").is_none(),
        "retryable is omitted when false, so existing consumers see an unchanged wire"
    );
}

/// Live round-trip through dispatch: spawn a real tmux session, type a line
/// via `send_text`, read it back via `read_terminal`, then `close_terminal`.
/// Needs a real tmux on PATH (WSL2 dev shell; not the Windows CI target).
#[test]
fn live_send_read_close_roundtrip() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // The id must honor the production invariant "the id IS the tmux session
    // suffix, capped at 8 chars" (`tmux::target_for_id`) — the previous long
    // `mcp3test<nanos>` id created `th_mcp3test<nanos>` but dispatched
    // against `th_mcp3test`, so send_text hit a session that never existed.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let id = format!("{:08x}", (nanos as u64) & 0xffff_ffff);
    let target = tmux::target_for_id(&id);
    let _ = tmux::kill_session(&target);
    tmux::new_session_with_env(&target, "/tmp", None, &[]).expect("spawn session");
    // Deterministic geometry regardless of what the server's latest client
    // reports (the wedged-2x24 gotcha; see tmux::resize_window_for_tests).
    tmux::resize_window_for_tests(&target, 80, 24).expect("resize session");

    let ctx = test_ctx("t");
    dispatch(
        &ctx,
        "send_text",
        &json!({"sessionId": id, "text": "echo MCP3_ROUNDTRIP_OK", "enter": true}),
    )
    .expect("send_text should succeed");
    std::thread::sleep(std::time::Duration::from_millis(300));

    let v = dispatch(&ctx, "read_terminal", &json!({"sessionId": id})).unwrap();
    assert!(
        v["text"].as_str().unwrap().contains("MCP3_ROUNDTRIP_OK"),
        "read_terminal should show the echoed sentinel; got {v:?}"
    );

    let c = dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    assert_eq!(c["accepted"], "close_terminal");
    assert!(
        !tmux::has_session(&target),
        "session should be gone after close"
    );
}

#[test]
fn idle_connection_is_closed_after_the_read_timeout() {
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};

    // A listener + a context with a SHORT idle timeout. A client that connects
    // and never sends a request must be closed by the server (M2b hardening),
    // not left to park the handler thread forever.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let mut ctx = test_ctx("t");
    ctx.idle_timeout = std::time::Duration::from_millis(200);

    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        // Returns Ok once the idle read times out and the request loop breaks.
        let _ = handle_conn(stream, &ctx);
    });

    // Connect, send NOTHING, then read: the server should close the socket
    // after ~200ms, so the read returns 0 (EOF). The generous 2s client-side
    // timeout only trips if the server FAILED to close us — the regression.
    let mut client = TcpStream::connect(addr).expect("connect");
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let mut buf = [0u8; 16];
    let n = client
        .read(&mut buf)
        .expect("read should return EOF, not time out");
    assert_eq!(n, 0, "server should have closed the idle connection (EOF)");

    server.join().unwrap();
}

#[test]
fn protocol_version_gate_rejects_a_skewed_client() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let ctx = test_ctx("secret");
    // Serve one connection per assertion (each `send` opens + closes one).
    let server = std::thread::spawn(move || {
        for _ in 0..4 {
            let (stream, _) = listener.accept().expect("accept");
            let _ = handle_conn(stream, &ctx);
        }
    });

    // Open a connection, send one frame, read one response line.
    let send = |frame: Value| -> Value {
        let mut s = TcpStream::connect(addr).expect("connect");
        let mut bytes = serde_json::to_vec(&frame).unwrap();
        bytes.push(b'\n');
        s.write_all(&bytes).unwrap();
        let mut reader = BufReader::new(s);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(line.trim()).unwrap()
    };

    // A valid token but a version NEWER than the server speaks is rejected — the
    // gate fires before dispatch, with a clear, actionable message.
    let bad = send(json!({"token": "secret", "command": "list_tabs", "v": 999}));
    assert_eq!(bad["ok"], false);
    assert!(
        bad["error"]
            .as_str()
            .unwrap()
            .contains("protocol version too new"),
        "got: {bad}"
    );

    // The matching version passes the gate and dispatches normally.
    let good = send(json!({"token": "secret", "command": "list_tabs", "v": PROTOCOL_VERSION}));
    assert_eq!(good["ok"], true, "got: {good}");

    // A LOWER version (a v1 client against this v2 server) is still accepted —
    // the protocol is backward-compatible (T13 binary framing is opt-in), so the
    // live webview keeps working unchanged.
    let v1 = send(json!({"token": "secret", "command": "list_tabs", "v": 1}));
    assert_eq!(v1["ok"], true, "got: {v1}");

    // No version field at all stays accepted (backward-compat: the MCP / legacy
    // clients don't advertise one).
    let legacy = send(json!({"token": "secret", "command": "list_tabs"}));
    assert_eq!(legacy["ok"], true, "got: {legacy}");

    server.join().unwrap();
}

#[test]
fn loopback_file_read_bypasses_the_scope() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let ctx = test_ctx("secret");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let _ = handle_conn(stream, &ctx);
    });

    // list_dir on a NON-indexed path: over loopback the peer is trusted, so the
    // #23 scope is bypassed and the listing succeeds. This proves handle_conn
    // tags the 127.0.0.1 peer as loopback -> enforce_scope=false end-to-end (a
    // logic inversion would either over-restrict locally or — worse — fail to
    // restrict a real remote peer; the core's enforce=true path is covered by
    // the files.rs scope test).
    let mut s = TcpStream::connect(addr).expect("connect");
    let tmp = std::env::temp_dir().to_string_lossy().into_owned();
    let frame = json!({"token": "secret", "command": "list_dir", "args": {"path": tmp}});
    let mut bytes = serde_json::to_vec(&frame).unwrap();
    bytes.push(b'\n');
    s.write_all(&bytes).unwrap();
    let mut reader = BufReader::new(s);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let resp: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(
        resp["ok"], true,
        "loopback list_dir should bypass scope: {resp}"
    );
    // Close the client so the server's next read hits EOF and handle_conn
    // returns immediately — otherwise it would park until the idle timeout.
    drop(reader);

    server.join().unwrap();
}

#[test]
fn theme_commands_are_forwarded_by_name() {
    let ctx = test_ctx("t");
    // Forwarded by name; not yet wired ⇒ a clear, theme-specific error (not
    // the generic "unknown command" arm). This proves the forward path.
    for cmd in ["get_theme", "set_theme"] {
        let err = dispatch(&ctx, cmd, &Value::Null).unwrap_err();
        assert!(err.contains("theme command handler"), "got: {err}");
    }
}

#[test]
fn is_allowed_peer_admits_only_loopback_and_tailscale() {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    // Loopback — always.
    assert!(is_allowed_peer(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    assert!(is_allowed_peer(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    // Tailscale CGNAT 100.64.0.0/10 (IPv4).
    assert!(is_allowed_peer(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
    assert!(is_allowed_peer(IpAddr::V4(Ipv4Addr::new(
        100, 127, 255, 254
    ))));
    // Tailscale ULA fd7a:115c::/32 (IPv6).
    assert!(is_allowed_peer(IpAddr::V6(Ipv6Addr::new(
        0xfd7a, 0x115c, 0, 0, 0, 0, 0, 1
    ))));
    // LAN / public — rejected before auth.
    assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))));
    assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))));
    assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    // 100.x OUTSIDE the 64..=127 second octet is NOT Tailscale — rejected.
    assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(100, 0, 0, 1))));
    assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(100, 128, 0, 1))));
    // IPv4-mapped IPv6 (how IPv4 peers arrive on a dual-stack [::] bind): a
    // mapped loopback / tailnet IP is admitted, a mapped public IP rejected.
    assert!(is_allowed_peer("::ffff:127.0.0.1".parse().unwrap()));
    assert!(is_allowed_peer("::ffff:100.64.0.1".parse().unwrap()));
    assert!(!is_allowed_peer("::ffff:8.8.8.8".parse().unwrap()));
}

#[test]
fn handshake_roundtrips_through_json() {
    let h = ControlHandshake {
        addr: "127.0.0.1:5000".into(),
        token: "abc".into(),
        read_token: "rdonly".into(),
        pid: 42,
        protocol_version: PROTOCOL_VERSION,
        instance_id: "instance".into(),
        listener_generation: 1,
        published_at: 123,
        local_control_token: "full".into(),
        local_host_token: "host".into(),
    };
    let s = serde_json::to_string(&h).unwrap();
    let back: ControlHandshake = serde_json::from_str(&s).unwrap();
    assert_eq!(back.addr, "127.0.0.1:5000");
    assert_eq!(back.token, "abc");
    assert_eq!(back.read_token, "rdonly");
    assert_eq!(back.pid, 42);
    assert_eq!(back.protocol_version, PROTOCOL_VERSION);
    // `local_control_token` is in-process-only: it is NEVER serialized, so it
    // does not survive the JSON round-trip and comes back empty (its default).
    assert!(
        !s.contains("local_control_token"),
        "field must not serialize"
    );
    assert!(
        !s.contains("full"),
        "in-process token must not appear in JSON"
    );
    assert_eq!(back.local_control_token, "");
}

#[test]
fn old_handshake_without_read_token_still_parses() {
    // Backward-compat: a control.json written before Phase 2 (no read_token
    // field) must still deserialize - the field defaults to empty.
    let json = r#"{"addr":"127.0.0.1:9","token":"t","pid":1,"protocol_version":2}"#;
    let hs: ControlHandshake = serde_json::from_str(json).unwrap();
    assert_eq!(hs.token, "t");
    assert_eq!(hs.read_token, "");
    // The Phase-3 in-process field is absent from old files and defaults empty.
    assert_eq!(hs.local_control_token, "");
}
