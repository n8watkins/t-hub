//! End-to-end proof of the MCP path on this dev box (PRD §9.6).
//!
//! This exercises the **real** pieces together, with no Tauri GUI:
//!   1. a real [`Supervisor`] seeded with hook events + a real [`StatusBridge`]
//!      with an ingested statusline snapshot;
//!   2. a real `t-hub` control listener ([`t_hub_lib::control::start`]) on a
//!      loopback port, with the handshake written to a temp file;
//!   3. a real tmux session (`th_*` on a per-process ISOLATED socket, never the
//!      live `t-hub`) so `list_terminals` has something to report;
//!   4. the real compiled `t-hub-mcp` binary, spawned as a subprocess and
//!      driven over its stdin/stdout with genuine MCP JSON-RPC.
//!
//! It then asserts the full JSON round-trip for `initialize`, `tools/list`, and
//! several `tools/call`s (a Read tool, a search, a status read, and the gated
//! process-changing tool), proving: MCP binary → control channel → app dispatch
//! → back.
//!
//! The test is resilient: if tmux isn't available it still runs every non-tmux
//! assertion; if the `t-hub-mcp` binary can't be located it fails with a clear
//! message telling you to `cargo build -p t-hub-mcp` first.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};

use t_hub_lib::control;

// The crate's internal types are reachable because we seed them through the
// public test constructor; we only need the protocol event enum + the two
// bridges, which are re-exported on the lib's public surface used by control.
use t_hub_protocol::JournalEventType;

/// Locate the compiled `t-hub-mcp` binary. The integration test binary lives
/// in `target/<profile>/deps/`, so the sibling binary is `../t-hub-mcp`.
fn locate_mcp_binary() -> PathBuf {
    let mut dir = std::env::current_exe().expect("current_exe");
    dir.pop(); // drop the test binary filename → .../deps
    if dir.ends_with("deps") {
        dir.pop(); // → .../<profile>
    }
    let candidate = dir.join(if cfg!(windows) {
        "t-hub-mcp.exe"
    } else {
        "t-hub-mcp"
    });
    assert!(
        candidate.exists(),
        "t-hub-mcp binary not found at {} — run `cargo build -p t-hub-mcp` first \
         (the e2e test spawns the real binary)",
        candidate.display()
    );
    candidate
}

/// A thin driver around the spawned MCP subprocess: write one JSON-RPC request
/// line, read one JSON-RPC response line.
struct McpProc {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl McpProc {
    fn spawn(bin: &PathBuf, handshake_file: &PathBuf) -> Self {
        Self::spawn_with_session(bin, handshake_file, None)
    }

    fn spawn_with_session(
        bin: &PathBuf,
        handshake_file: &PathBuf,
        session_token: Option<&str>,
    ) -> Self {
        let mut command = Command::new(bin);
        command
            .env("T_HUB_CONTROL_FILE", handshake_file)
            .env_remove("T_HUB_CONTROL_ADDR")
            .env_remove("T_HUB_CONTROL_TOKEN")
            .env_remove("T_HUB_SESSION_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(token) = session_token {
            command.env("T_HUB_SESSION_TOKEN", token);
        }
        let mut child = command.spawn().expect("spawn t-hub-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    /// Send a request and read the next response line as JSON. Prints the
    /// request/response pair so `cargo test -- --nocapture` shows the real wire
    /// transcript (the human-readable proof evidence).
    fn request(&mut self, value: Value) -> Value {
        let req_line = serde_json::to_string(&value).unwrap();
        println!("→ {req_line}");
        let mut line = req_line.into_bytes();
        line.push(b'\n');
        self.stdin.write_all(&line).expect("write to mcp stdin");
        self.stdin.flush().unwrap();
        let mut resp = String::new();
        let n = self.stdout.read_line(&mut resp).expect("read mcp stdout");
        assert!(n > 0, "t-hub-mcp closed stdout without responding");
        println!("← {}", resp.trim_end());
        serde_json::from_str(resp.trim_end())
            .unwrap_or_else(|e| panic!("non-JSON response {resp:?}: {e}"))
    }

    /// Send a notification (no response expected).
    fn notify(&mut self, value: Value) {
        let mut line = serde_json::to_vec(&value).unwrap();
        line.push(b'\n');
        self.stdin.write_all(&line).unwrap();
        self.stdin.flush().unwrap();
    }
}

struct MockControl {
    handshake_file: PathBuf,
    requests: Receiver<Value>,
    server: thread::JoinHandle<()>,
}

impl MockControl {
    fn start<F>(expected_calls: usize, handler: F) -> Self
    where
        F: Fn(&Value) -> Value + Send + 'static,
    {
        Self::start_with_token(expected_calls, "mock-read-capability", handler)
    }

    fn start_with_token<F>(expected_calls: usize, token: &str, handler: F) -> Self
    where
        F: Fn(&Value) -> Value + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock control listener");
        let addr = listener.local_addr().expect("mock control address");
        let tmp = std::env::temp_dir().join(format!(
            "th-mcp-powder-contract-{}-{}",
            std::process::id(),
            addr.port()
        ));
        fs::create_dir_all(&tmp).expect("create mock control directory");
        let handshake_file = tmp.join("control.json");
        fs::write(
            &handshake_file,
            serde_json::to_vec(&json!({
                "addr": addr.to_string(),
                "token": token
            }))
            .unwrap(),
        )
        .expect("write mock control handshake");

        let (tx, requests) = mpsc::channel();
        let server = thread::spawn(move || {
            for _ in 0..expected_calls {
                let (mut stream, _) = listener.accept().expect("accept MCP control call");
                let mut line = String::new();
                BufReader::new(stream.try_clone().expect("clone mock control stream"))
                    .read_line(&mut line)
                    .expect("read MCP control request");
                let request: Value = serde_json::from_str(&line).expect("control request JSON");
                let response = handler(&request);
                tx.send(request).expect("publish MCP control request");
                let mut encoded = serde_json::to_vec(&response).expect("control response JSON");
                encoded.push(b'\n');
                stream
                    .write_all(&encoded)
                    .expect("write MCP control response");
            }
        });

        Self {
            handshake_file,
            requests,
            server,
        }
    }

    fn finish(self, expected_calls: usize) -> Vec<Value> {
        let requests: Vec<Value> = (0..expected_calls)
            .map(|_| self.requests.recv().expect("receive MCP control request"))
            .collect();
        self.server.join().expect("join mock control server");
        if let Some(tmp) = self.handshake_file.parent() {
            let _ = fs::remove_dir_all(tmp);
        }
        requests
    }
}

impl Drop for McpProc {
    fn drop(&mut self) {
        // Closing stdin makes the server loop hit EOF and exit; then reap it.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Helper to pull the structured tool-call payload out of an MCP `tools/call`
/// success response.
fn tool_structured(resp: &Value) -> &Value {
    assert!(
        resp.get("error").is_none(),
        "tools/call returned a transport error: {resp}"
    );
    &resp["result"]["structuredContent"]
}

fn tool_error_text<'a>(resp: &'a Value, operation: &str) -> &'a str {
    assert!(
        resp.get("error").is_none(),
        "{operation} returned an MCP transport error: {resp}"
    );
    assert_eq!(
        resp["result"]["isError"], true,
        "{operation} must fail closed without a bound Crew/Captain identity: {resp}"
    );
    assert!(
        resp["result"]["structuredContent"].is_null(),
        "{operation} errors must not masquerade as structured success: {resp}"
    );
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{operation} must return structured MCP error content: {resp}"))
}

fn assert_authorization_tool_error(resp: &Value, operation: &str, expected: &str) {
    let message = tool_error_text(resp, operation);
    assert_eq!(message, expected, "{operation} authorization error");
    assert!(
        !message.contains("not exposed over the control channel"),
        "{operation} must not pass because its control command is absent: {message}"
    );
}

#[test]
fn end_to_end_mcp_round_trip() {
    let bin = locate_mcp_binary();

    // ISOLATED SOCKET: point this whole test (the in-process control listener that
    // runs the tmux ops, the spawned t-hub-mcp which inherits the env, and the
    // make/kill helpers below) at a per-process socket, NEVER the live `t-hub` a
    // running app drives. Set BEFORE anything resolves `tmux::socket()` (the first
    // list_terminals). Cleaned up at the end alongside T_HUB_CONTROL_FILE.
    let tmux_socket = format!("t-hub-mcpe2e-{}", std::process::id());
    std::env::set_var("T_HUB_TMUX_SOCKET", &tmux_socket);

    // --- 1. Seed real supervision + status state -------------------------
    let supervisor = Arc::new(Mutex::new(t_hub_lib::supervision_for_test()));
    {
        let mut s = supervisor.lock();
        // An orchestrator with a running subagent, then Stop → WaitingOnSubagents.
        s.ingest(
            Some("sess-e2e"),
            None,
            None,
            None,
            JournalEventType::SessionStart,
            1,
        );
        s.ingest(
            Some("sess-e2e"),
            None,
            None,
            None,
            JournalEventType::UserPromptSubmit,
            2,
        );
        s.ingest(
            Some("sess-e2e"),
            Some("agent-1"),
            Some("general-purpose"),
            None,
            JournalEventType::SubagentStart,
            3,
        );
        s.ingest(
            Some("sess-e2e"),
            None,
            None,
            None,
            JournalEventType::Stop,
            4,
        );
    }

    let status = Arc::new(t_hub_lib::status_bridge_for_test());
    status.ingest(
        "sess-e2e",
        &json!({ "context_window": { "used_percentage": 42.0 } }),
        1000,
    );

    // --- 2. Start the real control listener ------------------------------
    let token = format!("e2e-token-{}", std::process::id());
    let tmp = std::env::temp_dir().join(format!("th-mcp-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let handshake_file = tmp.join("control.json");
    std::env::set_var("T_HUB_CONTROL_FILE", &handshake_file);

    let ctx = control::ControlContext::with_shared_supervisor(
        status.clone(),
        supervisor.clone(),
        token.clone(),
    );
    let handshake = control::start(ctx).expect("control listener starts");
    assert_eq!(handshake.token, token);
    // Give the accept loop a beat to be ready.
    std::thread::sleep(Duration::from_millis(100));

    // --- 3. A real tmux session so list_terminals reports something ------
    let tmux_session = format!("th_e2e{}", std::process::id() % 100000);
    let tmux_ok = make_tmux_session(&tmux_socket, &tmux_session);
    // Drop-guard: the session is killed even if an assertion below panics, so a
    // failure can never leak an `th_e2e*` session (belt on top of the explicit
    // cleanup at the end).
    let _session_guard = TmuxSessionGuard {
        socket: tmux_socket.clone(),
        name: tmux_session.clone(),
    };

    // --- 4. Spawn the real t-hub-mcp binary + drive it -----------------
    let mut mcp = McpProc::spawn(&bin, &handshake_file);

    // initialize
    let init = mcp.request(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2024-11-05", "capabilities": {} }
    }));
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "t-hub-mcp");
    assert!(init["result"]["capabilities"]["tools"].is_object());

    // initialized notification (no response)
    mcp.notify(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    // tools/list
    let list = mcp.request(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "list_terminals",
        "get_status",
        "supervision_tree",
        "wsl_health",
        "search_files",
        "list_tabs",
        "spawn_terminal",
        "append_crew_powder_work_log",
        "read_crew_powder_evidence",
        "complete_crew_powder",
        "get_theme",
    ] {
        assert!(names.contains(&expected), "tools/list missing {expected}");
    }
    for (name, tier) in [
        ("append_crew_powder_work_log", "organization"),
        ("read_crew_powder_evidence", "read"),
        ("complete_crew_powder", "organization"),
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("tools/list missing {name}"));
        assert_eq!(tool["annotations"]["t-hubTier"], tier, "{name}");
        assert_eq!(
            tool["annotations"]["confirmationRequired"], false,
            "{name} must reach role-bound backend authorization"
        );
        assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{name}");
        for forbidden in [
            "cardId",
            "runId",
            "profile",
            "connectionProfile",
            "endpoint",
            "repository",
            "credential",
        ] {
            assert!(
                tool["inputSchema"]["properties"].get(forbidden).is_none(),
                "{name} must not expose {forbidden} substitution"
            );
        }
    }

    // tools/call → wsl_health (a Read tool that always works)
    let health = mcp.request(json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "wsl_health", "arguments": {} }
    }));
    let h = tool_structured(&health);
    assert_eq!(health["result"]["isError"], false);
    assert!(h["metrics"]["capturedAtMs"].is_u64() || h["metrics"]["capturedAtMs"].is_number());
    // Supervision was seeded with one session.
    assert_eq!(h["supervisedSessions"], 1);

    // tools/call → get_status for the seeded session
    let st = mcp.request(json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": { "name": "get_status", "arguments": { "sessionId": "sess-e2e" } }
    }));
    let stc = tool_structured(&st);
    assert_eq!(stc["sessionId"], "sess-e2e");
    assert_eq!(stc["status"], "waitingOnSubagents");
    assert_eq!(stc["snapshot"]["contextUsedPct"], 42.0);

    // tools/call → supervision_tree for the seeded session
    let tree = mcp.request(json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": { "name": "supervision_tree", "arguments": { "sessionId": "sess-e2e" } }
    }));
    let tc = tool_structured(&tree);
    assert_eq!(tc["sessionId"], "sess-e2e");
    assert_eq!(tc["status"], "waitingOnSubagents");
    assert_eq!(tc["children"].as_array().unwrap().len(), 1);
    assert_eq!(tc["children"][0]["agentId"], "agent-1");

    // tools/call → search_files against this very repo's src-tauri tree
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let search = mcp.request(json!({
        "jsonrpc": "2.0", "id": 6, "method": "tools/call",
        "params": {
            "name": "search_files",
            "arguments": { "root": manifest.to_string_lossy(), "query": "control", "limit": 10 }
        }
    }));
    let sc = tool_structured(&search);
    let hits = sc["hits"].as_array().unwrap();
    assert!(
        hits.iter().any(|h| h["relPath"]
            .as_str()
            .map(|p| p.contains("control"))
            .unwrap_or(false)),
        "search_files should find a control* file; got {hits:?}"
    );

    // tools/call → list_terminals (asserts the tmux-backed session if tmux ran)
    let terms = mcp.request(json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "name": "list_terminals", "arguments": {} }
    }));
    let tcount = tool_structured(&terms);
    assert!(tcount["terminals"].is_array());
    if tmux_ok {
        let ids: Vec<&str> = tcount["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["tmuxSession"].as_str())
            .collect();
        assert!(
            ids.iter().any(|s| *s == tmux_session),
            "list_terminals should include {tmux_session}; got {ids:?}"
        );
    }

    // tools/call → spawn_terminal: functional (#17) but routed through the UI
    // ApplySink. This e2e listener is HEADLESS (no apply sink), so there is no UI
    // to adopt the tile and the spawn is refused with a clear "no UI" error rather
    // than creating an untracked tmux session. It still comes back as a tool error
    // (isError), just no longer the old "gated off" refusal.
    let spawn = mcp.request(json!({
        "jsonrpc": "2.0", "id": 8, "method": "tools/call",
        "params": { "name": "spawn_terminal", "arguments": { "cwd": "/tmp" } }
    }));
    assert_eq!(
        spawn["result"]["isError"], true,
        "spawn_terminal without a UI must refuse, got {spawn}"
    );
    let msg = spawn["result"]["content"][0]["text"].as_str().unwrap();
    assert!(msg.contains("no UI"), "refusal message: {msg}");

    // --- cleanup ---------------------------------------------------------
    if tmux_ok {
        kill_tmux_session(&tmux_socket, &tmux_session);
    }
    drop(mcp);
    let _ = std::fs::remove_dir_all(&tmp);
    std::env::remove_var("T_HUB_CONTROL_FILE");
    std::env::remove_var("T_HUB_TMUX_SOCKET");
}

#[test]
fn powder_authorization_errors_are_specific_and_preserve_bound_identity() {
    let bin = locate_mcp_binary();
    let read_expected_calls = 7;
    let read_mock = MockControl::start(read_expected_calls, |request| {
        let command = request["command"].as_str().unwrap();
        let session = request["session"].as_str().unwrap();
        let args = &request["args"];
        assert_eq!(request["token"], "mock-read-capability");
        let error = match (session, command) {
            ("", "append_crew_powder_work_log" | "complete_crew_powder") => format!(
                "unauthorized: '{command}' requires the control capability (this token is read-only)"
            ),
            ("", "read_crew_powder_evidence") => {
                "acl: read_crew_powder_evidence requires a valid Crew or Captain T_HUB_SESSION_TOKEN"
                    .to_string()
            }
            ("owned-crew-token", "append_crew_powder_work_log") => {
                "powder unavailable after Crew-self authorization: test sentinel".to_string()
            }
            ("owned-crew-token", "read_crew_powder_evidence")
                if args.get("crewSessionId").is_some() =>
            {
                "acl: Crew callers cannot select another Crew identity".to_string()
            }
            ("owned-crew-token", "read_crew_powder_evidence") => {
                "powder unavailable after Crew-self authorization: test sentinel".to_string()
            }
            ("owned-crew-token", "complete_crew_powder") => {
                "unauthorized: 'complete_crew_powder' requires the control capability (this token is read-only)"
                    .to_string()
            }
            _ => panic!("unexpected authorization contract request: {request}"),
        };
        json!({ "ok": false, "error": error })
    });

    let mut anonymous = McpProc::spawn(&bin, &read_mock.handshake_file);
    let append_without_capability = anonymous.request(json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": "append_crew_powder_work_log",
            "arguments": { "message": "bounded evidence" }
        }
    }));
    assert_authorization_tool_error(
        &append_without_capability,
        "append_crew_powder_work_log",
        "unauthorized: 'append_crew_powder_work_log' requires the control capability (this token is read-only)",
    );
    let complete_without_capability = anonymous.request(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {
            "name": "complete_crew_powder",
            "arguments": { "crewSessionId": "owned-crew", "proof": "tests pass" }
        }
    }));
    assert_authorization_tool_error(
        &complete_without_capability,
        "complete_crew_powder",
        "unauthorized: 'complete_crew_powder' requires the control capability (this token is read-only)",
    );
    let read_without_identity = anonymous.request(json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {
            "name": "read_crew_powder_evidence",
            "arguments": { "limit": 20 }
        }
    }));
    assert_authorization_tool_error(
        &read_without_identity,
        "read_crew_powder_evidence",
        "acl: read_crew_powder_evidence requires a valid Crew or Captain T_HUB_SESSION_TOKEN",
    );

    let mut crew =
        McpProc::spawn_with_session(&bin, &read_mock.handshake_file, Some("owned-crew-token"));
    let owned_append = crew.request(json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {
            "name": "append_crew_powder_work_log",
            "arguments": { "message": "bounded evidence" }
        }
    }));
    assert_eq!(
        tool_error_text(&owned_append, "append_crew_powder_work_log"),
        "powder unavailable after Crew-self authorization: test sentinel"
    );
    let owned_read = crew.request(json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": {
            "name": "read_crew_powder_evidence",
            "arguments": { "limit": 20 }
        }
    }));
    assert_eq!(
        tool_error_text(&owned_read, "read_crew_powder_evidence"),
        "powder unavailable after Crew-self authorization: test sentinel"
    );
    let foreign_read = crew.request(json!({
        "jsonrpc": "2.0", "id": 6, "method": "tools/call",
        "params": {
            "name": "read_crew_powder_evidence",
            "arguments": { "crewSessionId": "foreign-crew", "limit": 20 }
        }
    }));
    assert_authorization_tool_error(
        &foreign_read,
        "read_crew_powder_evidence",
        "acl: Crew callers cannot select another Crew identity",
    );
    let crew_completion = crew.request(json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {
            "name": "complete_crew_powder",
            "arguments": { "crewSessionId": "owned-crew", "proof": "tests pass" }
        }
    }));
    assert_authorization_tool_error(
        &crew_completion,
        "complete_crew_powder",
        "unauthorized: 'complete_crew_powder' requires the control capability (this token is read-only)",
    );

    drop(anonymous);
    drop(crew);
    let read_requests = read_mock.finish(read_expected_calls);
    assert_eq!(read_requests[0]["session"], "");
    assert_eq!(read_requests[1]["command"], "complete_crew_powder");
    assert_eq!(read_requests[2]["command"], "read_crew_powder_evidence");
    assert_eq!(read_requests[3]["session"], "owned-crew-token");
    assert_eq!(
        read_requests[3]["args"],
        json!({ "message": "bounded evidence" })
    );
    assert!(read_requests[4]["args"].get("crewSessionId").is_none());
    assert_eq!(read_requests[5]["args"]["crewSessionId"], "foreign-crew");
    assert_eq!(read_requests[6]["session"], "owned-crew-token");

    let control_expected_calls = 2;
    let control_mock = MockControl::start_with_token(
        control_expected_calls,
        "mock-control-capability",
        |request| {
            assert_eq!(request["token"], "mock-control-capability");
            assert_eq!(request["session"], "owning-captain-token");
            assert_eq!(request["command"], "complete_crew_powder");
            let error = if request["args"]["crewSessionId"] == "owned-crew" {
                "powder unavailable after Captain authorization: test sentinel"
            } else {
                "acl: Crew session is not owned by the calling Captain"
            };
            json!({ "ok": false, "error": error })
        },
    );
    let mut captain = McpProc::spawn_with_session(
        &bin,
        &control_mock.handshake_file,
        Some("owning-captain-token"),
    );
    let owned_completion = captain.request(json!({
        "jsonrpc": "2.0", "id": 8, "method": "tools/call",
        "params": {
            "name": "complete_crew_powder",
            "arguments": { "crewSessionId": "owned-crew", "proof": "tests pass" }
        }
    }));
    assert_eq!(
        tool_error_text(&owned_completion, "complete_crew_powder"),
        "powder unavailable after Captain authorization: test sentinel"
    );
    let foreign_completion = captain.request(json!({
        "jsonrpc": "2.0", "id": 9, "method": "tools/call",
        "params": {
            "name": "complete_crew_powder",
            "arguments": { "crewSessionId": "foreign-crew", "proof": "tests pass" }
        }
    }));
    assert_authorization_tool_error(
        &foreign_completion,
        "complete_crew_powder",
        "acl: Crew session is not owned by the calling Captain",
    );

    drop(captain);
    let control_requests = control_mock.finish(control_expected_calls);
    assert_eq!(control_requests[0]["args"]["crewSessionId"], "owned-crew");
    assert_eq!(control_requests[1]["args"]["crewSessionId"], "foreign-crew");
}

#[test]
fn powder_backend_validation_errors_survive_the_mcp_adapter() {
    let bin = locate_mcp_binary();
    let forbidden_fields = [
        "cardId",
        "runId",
        "profile",
        "endpoint",
        "repository",
        "credential",
    ];
    let expected_calls = 4 + forbidden_fields.len();
    let mock =
        MockControl::start_with_token(expected_calls, "mock-control-capability", |request| {
            assert_eq!(request["token"], "mock-control-capability");
            let command = request["command"].as_str().unwrap();
            let args = &request["args"];
            let error = if let Some(field) = [
                "cardId",
                "runId",
                "profile",
                "endpoint",
                "repository",
                "credential",
            ]
            .into_iter()
            .find(|field| args.get(field).is_some())
            {
                format!("invalid arguments: forbidden authority field '{field}'")
            } else {
                let (field, maximum) = match command {
                    "append_crew_powder_work_log" => ("message", 16 * 1024),
                    "complete_crew_powder" => ("proof", 4096),
                    _ => panic!("unexpected validation contract request: {request}"),
                };
                let value = args[field].as_str().unwrap();
                if value.trim().is_empty() {
                    format!("invalid arguments: {field} must not be empty")
                } else if value.len() > maximum {
                    format!("invalid arguments: {field} exceeds its UTF-8 byte limit")
                } else {
                    panic!("validation contract accepted invalid input: {request}");
                }
            };
            json!({ "ok": false, "error": error })
        });
    let mut crew =
        McpProc::spawn_with_session(&bin, &mock.handshake_file, Some("owned-crew-token"));
    let mut captain =
        McpProc::spawn_with_session(&bin, &mock.handshake_file, Some("owning-captain-token"));

    for (id, message, expected) in [
        (1, "é".repeat(8193), "message exceeds its UTF-8 byte limit"),
        (2, " \t\n ".to_string(), "message must not be empty"),
    ] {
        let response = crew.request(json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {
                "name": "append_crew_powder_work_log",
                "arguments": { "message": message }
            }
        }));
        assert!(
            tool_error_text(&response, "append_crew_powder_work_log").contains(expected),
            "response: {response}"
        );
    }
    for (id, proof, expected) in [
        (3, "é".repeat(2049), "proof exceeds its UTF-8 byte limit"),
        (4, " \t\n ".to_string(), "proof must not be empty"),
    ] {
        let response = captain.request(json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {
                "name": "complete_crew_powder",
                "arguments": { "crewSessionId": "owned-crew", "proof": proof }
            }
        }));
        assert!(
            tool_error_text(&response, "complete_crew_powder").contains(expected),
            "response: {response}"
        );
    }
    for (offset, field) in forbidden_fields.into_iter().enumerate() {
        let mut arguments = serde_json::Map::new();
        arguments.insert("limit".to_string(), json!(20));
        arguments.insert(field.to_string(), json!("substitution"));
        let response = crew.request(json!({
            "jsonrpc": "2.0", "id": 5 + offset, "method": "tools/call",
            "params": {
                "name": "read_crew_powder_evidence",
                "arguments": Value::Object(arguments)
            }
        }));
        let message = tool_error_text(&response, "read_crew_powder_evidence");
        assert_eq!(
            message,
            format!("invalid arguments: forbidden authority field '{field}'")
        );
    }

    drop(crew);
    drop(captain);
    let requests = mock.finish(expected_calls);
    assert_eq!(
        requests[0]["args"]["message"].as_str().unwrap().len(),
        16 * 1024 + 2
    );
    assert_eq!(requests[2]["args"]["proof"].as_str().unwrap().len(), 4098);
}

/// Kills its session on drop - including on a panicking assertion - so this E2E
/// can never leak an `th_e2e*` session.
struct TmuxSessionGuard {
    socket: String,
    name: String,
}
impl Drop for TmuxSessionGuard {
    fn drop(&mut self) {
        kill_tmux_session(&self.socket, &self.name);
    }
}

/// Create a detached tmux session on the ISOLATED test socket (never the live
/// `t-hub`). Returns false (and the test skips tmux-specific asserts) if tmux
/// isn't usable here.
fn make_tmux_session(socket: &str, name: &str) -> bool {
    Command::new("tmux")
        .args(["-L", socket, "new-session", "-d", "-s", name, "sleep 300"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn kill_tmux_session(socket: &str, name: &str) {
    let _ = Command::new("tmux")
        .args(["-L", socket, "kill-session", "-t", name])
        .status();
}
