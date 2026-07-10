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

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
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
        let mut child = Command::new(bin)
            // Point the binary's discovery at our temp handshake file so it
            // connects to the listener this test started.
            .env("T_HUB_CONTROL_FILE", handshake_file)
            // Make sure no stray addr/token override leaks in from the harness.
            .env_remove("T_HUB_CONTROL_ADDR")
            .env_remove("T_HUB_CONTROL_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn t-hub-mcp");
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
        s.ingest(Some("sess-e2e"), None, None, JournalEventType::SessionStart, 1);
        s.ingest(Some("sess-e2e"), None, None, JournalEventType::UserPromptSubmit, 2);
        s.ingest(
            Some("sess-e2e"),
            Some("agent-1"),
            Some("general-purpose"),
            JournalEventType::SubagentStart,
            3,
        );
        s.ingest(Some("sess-e2e"), None, None, JournalEventType::Stop, 4);
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
        "get_theme",
    ] {
        assert!(names.contains(&expected), "tools/list missing {expected}");
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
