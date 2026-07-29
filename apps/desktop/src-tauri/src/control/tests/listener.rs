use super::*;

fn attach_serial_guard() -> std::sync::MutexGuard<'static, ()> {
    ATTACH_TEST_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Stand up the REAL accept loop (`serve`, not per-connection `handle_conn`)
/// on an ephemeral loopback port. The thread parks in accept for the process
/// lifetime, exactly like the `control_probe_server` example.
fn spawn_attach_listener(ctx: ControlContext) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    std::thread::spawn(move || serve(listener, ctx, stop));
    addr
}

/// Round-trip a no-I/O `get_theme` against `addr`; returns true iff the listener
/// accepted, handled, and wrote back a response line. Short timeouts so a
/// refused/retired port returns false fast instead of hanging the test. Any
/// response (even the theme "not wired" error) proves the serve path is live.
fn listener_serves(addr: &str) -> bool {
    use std::io::{BufRead, BufReader, Write};
    let sock: std::net::SocketAddr = match addr.parse() {
        Ok(s) => s,
        Err(_) => return false,
    };
    let stream = match TcpStream::connect_timeout(&sock, Duration::from_millis(300)) {
        Ok(s) => s,
        Err(_) => return false,
    };
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return false,
    };
    let req = json!({"token": "secret", "command": "get_theme", "args": {}, "v": 1}).to_string();
    if writeln!(writer, "{req}").is_err() {
        return false;
    }
    let mut line = String::new();
    matches!(BufReader::new(stream).read_line(&mut line), Ok(n) if n > 0)
}

fn listener_discovery_proof(addr: &str, nonce: &str) -> Option<Value> {
    use std::io::{BufRead, BufReader, Write};
    let socket: std::net::SocketAddr = addr.parse().ok()?;
    let stream = TcpStream::connect_timeout(&socket, Duration::from_millis(300)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut writer = stream.try_clone().ok()?;
    let request = json!({
        "token": "read-secret",
        "session": "",
        "command": "control_discovery_proof",
        "args": {"nonce": nonce},
        "v": PROTOCOL_VERSION,
    });
    writeln!(writer, "{request}").ok()?;
    let mut line = String::new();
    if BufReader::new(stream).read_line(&mut line).ok()? == 0 {
        return None;
    }
    serde_json::from_str::<Value>(&line)
        .ok()?
        .get("result")
        .cloned()
}

/// Poll `cond` until it holds or `budget` elapses (short sleeps).
fn wait_until(budget: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

/// RELAY-WEDGE SELF-HEAL (cause 2): `rebind_control` binds a fresh port, atomically
/// rewrites control.json (tokens KEPT), serves on the new port, retires the old
/// listener, and rate-limits back-to-back rebinds. (The WSL relay wedge itself is
/// unreproducible in-process - this proves the app-side rebind mechanics the client
/// bridge triggers; see the PR for the honest E2E limits.)
#[test]
fn rebind_control_moves_listener_rewrites_json_and_rate_limits() {
    // Unique temp control.json for this test; handshake_path() honors this env.
    let cj = std::env::temp_dir().join(format!(
        "t-hub-rebind-{}-{}.json",
        std::process::id(),
        REBIND_TEST_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _control_file = ControlFileEnv::set(&cj);
    let _ = std::fs::remove_file(&cj);

    // Stand up an initial loopback listener + serve loop, like `start`: bind, set
    // addr on the ctx, register the stop flag in the rebind controller.
    let mut ctx = test_ctx("secret");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind initial");
    let old_addr = listener.local_addr().unwrap().to_string();
    ctx.addr = old_addr.clone();
    let old_generation = ctx.listener_generation.fetch_add(1, Ordering::AcqRel) + 1;
    ctx.bound_listener_generation = old_generation;
    let stop = Arc::new(AtomicBool::new(false));
    ctx.rebind.set_initial_stop(stop.clone());
    {
        let serve_ctx = ctx.clone();
        let serve_stop = stop.clone();
        std::thread::spawn(move || serve(listener, serve_ctx, serve_stop));
    }
    assert!(
        wait_until(Duration::from_secs(2), || listener_serves(&old_addr)),
        "the initial listener should serve before a rebind"
    );
    let old_proof = listener_discovery_proof(&old_addr, "old-listener-proof").unwrap();
    assert_eq!(old_proof["listenerAddr"], old_addr);
    assert_eq!(old_proof["listenerGeneration"], old_generation);

    // WRITE-token gated: rebind_control is Organization tier (control token only).
    assert_eq!(required_tier("rebind_control"), CommandTier::Organization);

    // Rebind.
    let resp = rebind_control(&ctx).expect("rebind ok");
    assert_eq!(resp["rebound"], true);
    assert_eq!(resp["tokensRotated"], false);
    let new_addr = resp["addr"].as_str().unwrap().to_string();
    assert_ne!(new_addr, old_addr, "rebind must move to a fresh port");

    // control.json now names the fresh addr (atomic rewrite), tokens KEPT (a
    // rebind is transport recovery, never a key rotation). Under item-3's default-ON
    // hardening the PUBLISHED token is the read token ("read-secret") - still the
    // SAME read token, not a rotated one - and the full token stays off disk; the
    // frontend keeps full control via the in-process local_control_token.
    let written: Value =
        serde_json::from_slice(&std::fs::read(&cj).expect("read control.json")).unwrap();
    assert_eq!(written["addr"], json!(new_addr));
    assert_eq!(
        written["token"],
        json!("read-secret"),
        "the published token must be the KEPT read token (harden default-ON), not rotated"
    );
    assert_ne!(
        written["token"],
        json!("secret"),
        "the full token must NOT reach disk"
    );

    // The NEW listener serves; the OLD one is retired (stops accepting).
    assert!(
        wait_until(Duration::from_secs(2), || listener_serves(&new_addr)),
        "the fresh listener should serve after a rebind"
    );
    let new_proof = listener_discovery_proof(&new_addr, "new-listener-proof").unwrap();
    assert_eq!(new_proof["listenerAddr"], new_addr);
    assert_eq!(
        new_proof["listenerGeneration"],
        written["listener_generation"]
    );
    assert_ne!(
        new_proof["listenerGeneration"],
        old_proof["listenerGeneration"]
    );
    assert!(
        wait_until(Duration::from_secs(3), || !listener_serves(&old_addr)),
        "the old listener should stop accepting after a rebind"
    );

    // A second immediate rebind is rate-limited with a clear cooldown message.
    let err = rebind_control(&ctx).unwrap_err();
    assert!(
        err.contains("rate-limited"),
        "a back-to-back rebind must be refused: {err}"
    );

    // Cleanup: retire the fresh listener so we leak neither a thread nor state.
    // `_control_file` restores the ambient discovery path when it drops.
    if let Some(s) = ctx.rebind.lock().current_stop.take() {
        s.store(true, Ordering::Release);
    }
    wake_accept(&new_addr);
    let _ = std::fs::remove_file(&cj);
}

#[test]
fn failed_rebind_publication_preserves_old_bound_proof_and_retires_unpublished_generation() {
    let root = std::env::temp_dir().join(format!(
        "t-hub-rebind-publish-fail-{}-{}",
        std::process::id(),
        REBIND_TEST_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let blocked_handshake = root.join("control.json");
    std::fs::create_dir_all(&blocked_handshake).unwrap();
    let _control_file = ControlFileEnv::set(&blocked_handshake);

    let mut ctx = test_ctx("secret");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let old_addr = listener.local_addr().unwrap().to_string();
    ctx.addr = old_addr.clone();
    let old_generation = ctx.listener_generation.fetch_add(1, Ordering::AcqRel) + 1;
    ctx.bound_listener_generation = old_generation;
    let stop = Arc::new(AtomicBool::new(false));
    ctx.rebind.set_initial_stop(stop.clone());
    {
        let serve_ctx = ctx.clone();
        let serve_stop = stop.clone();
        std::thread::spawn(move || serve(listener, serve_ctx, serve_stop));
    }
    assert!(wait_until(Duration::from_secs(2), || {
        listener_discovery_proof(&old_addr, "before-failed-publish").is_some()
    }));

    let error = rebind_control(&ctx).unwrap_err();
    assert!(error.contains("failed to publish control.json"));
    let unpublished_addr = error
        .split("fresh port ")
        .nth(1)
        .and_then(|tail| tail.split(" but failed").next())
        .unwrap()
        .to_string();
    assert_eq!(
        ctx.listener_generation.load(Ordering::Acquire),
        old_generation + 1,
        "the failed publication consumes its reserved generation"
    );
    let old_proof =
        listener_discovery_proof(&old_addr, "after-failed-publish").expect("old remains live");
    assert_eq!(old_proof["listenerAddr"], old_addr);
    assert_eq!(old_proof["listenerGeneration"], old_generation);
    assert!(
        wait_until(Duration::from_secs(2), || listener_discovery_proof(
            &unpublished_addr,
            "unpublished"
        )
        .is_none()),
        "the unpublished generation must not remain available for validation"
    );

    stop.store(true, Ordering::Release);
    wake_accept(&old_addr);
    // `_control_file` restores the ambient discovery path when it drops.
    let _ = std::fs::remove_dir_all(root);
}

/// A disposable real tmux session for attach tests; returns (id, tmux name).
fn churn_tmux_session(tag: &str) -> (String, String) {
    let id = format!(
        "s27{tag}{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let target = format!("th_{id}");
    let _ = tmux::kill_session(&target);
    tmux::new_session_with_env(&target, "/tmp", None, &[]).expect("spawn churn tmux session");
    (id, target)
}

/// A disposable churn tmux session that is ALWAYS killed on drop - including
/// when an assertion panics mid-test - so the attach suite can NEVER leak a
/// `th_s27*` session onto the socket. That leak is exactly what produced the
/// 13 `th_s27churn*` ghosts in the incident: a failing run of the churn test
/// left its sessions behind, and the app's post-restart adopt path then choked
/// on the debris. Paired with the `cfg(test)` socket isolation in `tmux.rs`
/// (THIS crate's test sessions live on `t-hub-test`, never the live `t-hub`
/// socket), this makes a leak from the attach suite both unable-to-hit-the-live
/// -app AND self-cleaning. (Other producers isolate separately - see the SCOPE
/// note on `tmux::SOCKET_NAME`.)
struct ChurnSession {
    id: String,
    target: String,
}

impl ChurnSession {
    fn new(tag: &str) -> Self {
        let (id, target) = churn_tmux_session(tag);
        Self { id, target }
    }
}

impl Drop for ChurnSession {
    fn drop(&mut self) {
        let _ = tmux::kill_session(&self.target);
    }
}

/// Send a v1 `attach_pty` request line on `stream`.
fn send_attach_request(stream: &mut TcpStream, token: &str, session_id: &str) {
    let mut frame = serde_json::to_vec(&json!({
        "token": token,
        "command": ATTACH_PTY_COMMAND,
        "args": { "sessionId": session_id, "cols": 80, "rows": 24 },
    }))
    .unwrap();
    frame.push(b'\n');
    stream.write_all(&frame).expect("write attach_pty request");
}

/// Send a v1 `{"write":"<b64>"}` input frame (keystrokes) on `stream`.
fn send_write_frame(stream: &mut TcpStream, keys: &str) {
    let mut frame = serde_json::to_vec(&json!({ "write": STANDARD.encode(keys) })).unwrap();
    frame.push(b'\n');
    stream.write_all(&frame).expect("write input frame");
}

/// Read one newline-delimited JSON frame; panics on EOF (caller expects one).
fn read_json_frame(reader: &mut BufReader<TcpStream>) -> Value {
    let mut line = String::new();
    let n = reader.read_line(&mut line).expect("read frame");
    assert!(n > 0, "connection closed before the expected frame");
    serde_json::from_str(line.trim()).expect("frame is JSON")
}

/// THE s27 regression: N clients die abruptly at every stage of the attach
/// lifecycle - before speaking, mid-request, pre-seed, post-seed via RST,
/// and the incident's exact shape: a client that starts a firehose, stops
/// draining, and silently HOLDS its socket (the un-reaped CLOSE_WAIT
/// forwarders that wedged the live server's new-attach path). The server
/// must reap every forwarder on its own and keep serving fresh attaches.
#[test]
fn attach_path_survives_abrupt_client_churn() {
    let _serial = attach_serial_guard();
    eventually(
        "forwarder table to drain before the test",
        Duration::from_secs(10),
        || attach_forwarder_count() == 0,
    );

    let mut ctx = test_ctx("churn-secret");
    ctx.idle_timeout = Duration::from_millis(500);
    ctx.attach_write_timeout = Duration::from_millis(300);
    let addr = spawn_attach_listener(ctx);
    let conns_baseline = ACTIVE_CONNS.load(Ordering::Relaxed);

    // Drop-guarded: the session is killed even if any assertion below panics.
    let churn = ChurnSession::new("churn");
    let id = churn.id.clone();
    let target = churn.target.clone();

    // (a) Dies before speaking: reaped by the idle read timeout.
    drop(TcpStream::connect(addr).expect("connect"));
    // (b) Dies mid-request-line (no newline ever arrives).
    {
        let mut s = TcpStream::connect(addr).expect("connect");
        s.write_all(b"{\"token\":\"churn-secret\",\"comm").unwrap();
        drop(s);
    }
    // (c) Attaches to a MISSING session and dies without reading the refusal.
    {
        let mut s = TcpStream::connect(addr).expect("connect");
        send_attach_request(&mut s, "churn-secret", "s27-definitely-absent");
        drop(s);
    }
    // (d) Dies between the request and the seed (FIN lands mid-seed), x3.
    for _ in 0..3 {
        let mut s = TcpStream::connect(addr).expect("connect");
        send_attach_request(&mut s, "churn-secret", &id);
        drop(s);
    }
    // (e) Reads the seed, then dies with an abrupt RST (SO_LINGER 0), x3.
    for _ in 0..3 {
        let s = TcpStream::connect(addr).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut w = s.try_clone().unwrap();
        send_attach_request(&mut w, "churn-secret", &id);
        let mut reader = BufReader::new(s);
        let seed = read_json_frame(&mut reader);
        assert!(
            seed.get("scrollback").is_some(),
            "expected a seed, got {seed}"
        );
        socket2::SockRef::from(reader.get_ref())
            .set_linger(Some(Duration::from_secs(0)))
            .unwrap();
        // Dropping both clones now closes the socket -> RST, not FIN.
    }

    // (f) The incident wedge: a tiny-receive-buffer client attaches, starts a
    // firehose, stops reading, and HOLDS the socket open in silence. ~13 MB of
    // output against a 4 KiB client window and a <=4 MiB kernel send buffer
    // guarantees the forwarder's sink write blocks; the write timeout must
    // then tear the whole forwarder down while the client still holds its end.
    let wedge = {
        let sock =
            socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
        sock.set_recv_buffer_size(4096).unwrap();
        sock.connect(&addr.into()).expect("connect wedge client");
        TcpStream::from(sock)
    };
    wedge
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut wedge_writer = wedge.try_clone().unwrap();
    send_attach_request(&mut wedge_writer, "churn-secret", &id);
    let mut wedge_reader = BufReader::new(wedge);
    let seed = read_json_frame(&mut wedge_reader);
    assert!(
        seed.get("scrollback").is_some(),
        "expected a seed, got {seed}"
    );
    send_write_frame(&mut wedge_writer, "yes S27-FIREHOSE | head -n 1000000\n");
    // Do NOT read, do NOT close. The server must reap the forwarder on its
    // own; every earlier case drains here too (EOF/RST paths are fast).
    eventually(
        "forwarder teardown while the wedged client still holds its socket",
        Duration::from_secs(20),
        || attach_forwarder_count() == 0,
    );

    // The forwarder timeout proves the wedged socket was reaped, but it does
    // not stop the firehose command running inside tmux. Under full-suite CPU
    // load that command can still fill the fresh client's receive window and
    // trip the deliberately tiny 300 ms server write timeout before this test
    // starts reading. Return the shared pane to a quiet prompt and observe a
    // marker there before testing recovery, so this assertion measures attach
    // health rather than a race with the previous client's output workload.
    tmux::send_keys(&target, &["C-c"]).expect("interrupt churn firehose");
    tmux::send_text(&target, "printf S27_FIREHOSE_STOPPED", true)
        .expect("write quiet-shell marker");
    eventually("churn firehose to stop", Duration::from_secs(10), || {
        tmux::capture_pane_text(&target, 100)
            .map(|text| text.contains("S27_FIREHOSE_STOPPED"))
            .unwrap_or(false)
    });

    // A FRESH attach must now succeed end to end - the exact operation that
    // failed for every client in the incident.
    let fresh = TcpStream::connect(addr).expect("connect fresh client");
    fresh
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut fresh_writer = fresh.try_clone().unwrap();
    send_attach_request(&mut fresh_writer, "churn-secret", &id);
    let mut fresh_reader = BufReader::new(fresh);
    let seed = read_json_frame(&mut fresh_reader);
    assert!(
        seed.get("scrollback").is_some(),
        "fresh attach after churn must get a seed, got {seed}"
    );
    send_write_frame(&mut fresh_writer, "echo S27_CHURN_OK\n");
    let mut seen = String::new();
    let sentinel_deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !seen.contains("S27_CHURN_OK") {
        assert!(
            std::time::Instant::now() < sentinel_deadline,
            "sentinel never arrived on the fresh attach; saw: {seen:?}"
        );
        let mut line = String::new();
        let n = fresh_reader.read_line(&mut line).expect("read out frame");
        assert!(n > 0, "server closed the fresh attach early; saw: {seen:?}");
        let v: Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(b64) = v.get("out").and_then(|x| x.as_str()) {
            if let Ok(bytes) = STANDARD.decode(b64) {
                seen.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    }

    // Teardown: with every client gone, BOTH tables return to baseline - no
    // leaked forwarder slot, no leaked connection slot.
    drop(fresh_reader);
    drop(fresh_writer);
    drop(wedge_reader);
    drop(wedge_writer);
    let _ = tmux::kill_session(&target);
    eventually(
        "forwarder table back to baseline",
        Duration::from_secs(10),
        || attach_forwarder_count() == 0,
    );
    eventually(
        "connection handlers to drain",
        Duration::from_secs(10),
        || ACTIVE_CONNS.load(Ordering::Relaxed) <= conns_baseline,
    );
}

/// The defensive forwarder-table bound: at the cap a new attach is refused
/// with a clear error (not a silent close), and a released slot makes the
/// attach path serviceable again.
#[test]
fn attach_forwarder_cap_refuses_then_recovers() {
    let _serial = attach_serial_guard();
    eventually(
        "forwarder table to drain before the test",
        Duration::from_secs(10),
        || attach_forwarder_count() == 0,
    );

    let mut ctx = test_ctx("cap-secret");
    ctx.idle_timeout = Duration::from_millis(500);
    ctx.attach_write_timeout = Duration::from_secs(2);
    ctx.max_attach_forwarders = 1;
    let addr = spawn_attach_listener(ctx);

    let churn = ChurnSession::new("cap");
    let id = churn.id.clone();
    let target = churn.target.clone();

    // First attach fills the size-1 table; reading the seed proves the slot
    // is held (the guard is acquired before the seed is written).
    let first = TcpStream::connect(addr).expect("connect");
    first
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut first_writer = first.try_clone().unwrap();
    send_attach_request(&mut first_writer, "cap-secret", &id);
    let mut first_reader = BufReader::new(first);
    assert_eq!(
        read_json_frame(&mut first_reader)["scrollback"],
        "",
        "attach must not replay a second copy of the tmux screen"
    );
    assert_eq!(attach_forwarder_count(), 1);

    // Second attach: refused with an actionable error, then closed.
    let second = TcpStream::connect(addr).expect("connect");
    second
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut second_writer = second.try_clone().unwrap();
    send_attach_request(&mut second_writer, "cap-secret", &id);
    let mut second_reader = BufReader::new(second);
    let refusal = read_json_frame(&mut second_reader);
    assert_eq!(refusal["ok"], false, "expected a refusal, got {refusal}");
    assert!(
        refusal["error"]
            .as_str()
            .unwrap()
            .contains("forwarder table is full"),
        "got: {refusal}"
    );
    let mut rest = String::new();
    assert_eq!(
        second_reader
            .read_line(&mut rest)
            .expect("read after refusal"),
        0,
        "the refused connection must be closed, not parked"
    );

    // Release the slot; the table must drain without any explicit detach call.
    drop(first_reader);
    drop(first_writer);
    eventually(
        "slot release after client disconnect",
        Duration::from_secs(10),
        || attach_forwarder_count() == 0,
    );

    // And the attach path is serviceable again.
    let third = TcpStream::connect(addr).expect("connect");
    third
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut third_writer = third.try_clone().unwrap();
    send_attach_request(&mut third_writer, "cap-secret", &id);
    let mut third_reader = BufReader::new(third);
    assert!(
        read_json_frame(&mut third_reader)
            .get("scrollback")
            .is_some(),
        "attach must succeed once the table drained"
    );

    drop(third_reader);
    drop(third_writer);
    let _ = tmux::kill_session(&target);
    eventually(
        "forwarder table drained at test end",
        Duration::from_secs(10),
        || attach_forwarder_count() == 0,
    );
}

/// THE s27 idle-leak regression: a client attached to an IDLE terminal that
/// stops draining and then vanishes WITHOUT a clean close (no FIN reaches the
/// server's input read) must still be reaped. The forwarder only ever noticed
/// a dead client when it had real output to write; an idle terminal produces
/// none, so the write path never fired and the forwarder parked forever on the
/// silent PTY read - leaking the slot and, at scale, wedging the table so new
/// cockpit tiles could not attach. The sibling churn test above never catches
/// this because every one of its clients either closes (FIN/RST -> the input
/// read unblocks) or drives a firehose (the sink write blocks -> write
/// timeout); only a SILENT idle client exercises the gap. The periodic idle
/// keepalive must now force the stalled client to surface (its socket buffers
/// fill, the attach write timeout fires) so the forwarder reaps on its own.
#[test]
fn attach_reaps_idle_terminal_with_stalled_client() {
    let _serial = attach_serial_guard();
    eventually(
        "forwarder table to drain before the test",
        Duration::from_secs(10),
        || attach_forwarder_count() == 0,
    );

    let mut ctx = test_ctx("idle-secret");
    ctx.idle_timeout = Duration::from_millis(500);
    ctx.attach_write_timeout = Duration::from_millis(300);
    // A short keepalive so the idle liveness probe fires within the test window
    // (production drives seconds). Without the probe an idle forwarder never
    // writes, so a stalled client is never noticed and the slot leaks forever.
    ctx.attach_keepalive_interval = Duration::from_millis(50);
    let addr = spawn_attach_listener(ctx);
    let conns_baseline = ACTIVE_CONNS.load(Ordering::Relaxed);

    let churn = ChurnSession::new("idle");
    let id = churn.id.clone();
    let target = churn.target.clone();

    // A tiny-receive-buffer client attaches to an IDLE session, reads the seed,
    // then STOPS reading and holds the socket in silence - the idle analogue of
    // the firehose wedge (case f above), but with no output to force the issue.
    // Only the idle keepalive can fill the small buffer and trip the write
    // timeout; without it this forwarder never reaps.
    let stalled = {
        let sock =
            socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
        sock.set_recv_buffer_size(4096).unwrap();
        sock.connect(&addr.into()).expect("connect stalled client");
        TcpStream::from(sock)
    };
    stalled
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut stalled_writer = stalled.try_clone().unwrap();
    send_attach_request(&mut stalled_writer, "idle-secret", &id);
    let mut stalled_reader = BufReader::new(stalled);
    let seed = read_json_frame(&mut stalled_reader);
    assert!(
        seed.get("scrollback").is_some(),
        "expected a seed, got {seed}"
    );
    assert_eq!(attach_forwarder_count(), 1, "forwarder up after attach");

    // Do NOT read, do NOT close: the client is gone but its socket lingers. The
    // server must reap this idle forwarder on its own, driven by the keepalive.
    eventually(
        "idle-terminal forwarder reaps a stalled client via the keepalive probe",
        Duration::from_secs(15),
        || attach_forwarder_count() == 0,
    );

    // Hold the client until AFTER the assertion so the reap is proven to be
    // driven by the server's probe, not by the socket finally closing.
    drop(stalled_reader);
    drop(stalled_writer);
    let _ = tmux::kill_session(&target);
    eventually(
        "connection handlers to drain",
        Duration::from_secs(10),
        || ACTIVE_CONNS.load(Ordering::Relaxed) <= conns_baseline,
    );
}
