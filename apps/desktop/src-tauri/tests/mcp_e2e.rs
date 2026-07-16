//! End-to-end proof of the real MCP binary, control listener, and tmux path.
//!
//! The control listener runs in a helper process. Closing that process is the
//! explicit listener shutdown boundary, so a failed assertion cannot leave an
//! accept loop or request handler alive in the integration-test process.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};
use t_hub_lib::control;
use t_hub_protocol::JournalEventType;

const MCP_IO_TIMEOUT: Duration = Duration::from_secs(10);
const PROCESS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const HELPER_READY_TIMEOUT: Duration = Duration::from_secs(10);
const HELPER_LIFETIME: Duration = Duration::from_secs(30);
static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

const POWDER_TOOLS: [(&str, &str); 3] = [
    ("append_crew_powder_work_log", "organization"),
    ("read_crew_powder_evidence", "read"),
    ("complete_crew_powder", "organization"),
];

const FORBIDDEN_AUTHORITY_FIELDS: [&str; 22] = [
    "card",
    "cardId",
    "card_id",
    "run",
    "runId",
    "run_id",
    "profile",
    "connectionProfile",
    "connection_profile",
    "endpoint",
    "powderEndpoint",
    "powder_endpoint",
    "repository",
    "powderRepository",
    "powder_repository",
    "repo",
    "credential",
    "apiKey",
    "api_key",
    "key",
    "token",
    "secret",
];

fn locate_mcp_binary() -> PathBuf {
    let mut dir = std::env::current_exe().expect("current_exe");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    let candidate = dir.join(if cfg!(windows) {
        "t-hub-mcp.exe"
    } else {
        "t-hub-mcp"
    });
    assert!(
        candidate.exists(),
        "t-hub-mcp binary not found at {}; run `cargo build -p t-hub-mcp` first",
        candidate.display()
    );
    candidate
}

fn wait_for_child(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => return false,
        }
    }
}

fn stop_child(child: &mut Child) {
    if wait_for_child(child, PROCESS_SHUTDOWN_TIMEOUT) {
        return;
    }
    let _ = child.kill();
    let _ = wait_for_child(child, PROCESS_SHUTDOWN_TIMEOUT);
}

enum StdinCommand {
    Write(Vec<u8>, Sender<Result<(), String>>),
    Shutdown,
}

struct McpProc {
    child: Option<Child>,
    stdin_tx: Sender<StdinCommand>,
    responses: Receiver<Result<Value, String>>,
    writer_done: Receiver<()>,
    reader_done: Receiver<()>,
    writer: Option<thread::JoinHandle<()>>,
    reader: Option<thread::JoinHandle<()>>,
}

impl McpProc {
    fn spawn(bin: &Path, handshake_file: &Path, tmux_socket: &str) -> Self {
        let mut child = Command::new(bin)
            .env("T_HUB_CONTROL_FILE", handshake_file)
            .env("T_HUB_TMUX_SOCKET", tmux_socket)
            .env_remove("T_HUB_CONTROL_ADDR")
            .env_remove("T_HUB_CONTROL_TOKEN")
            .env_remove("T_HUB_SESSION_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn t-hub-mcp");
        let mut stdin = child.stdin.take().expect("MCP stdin");
        let stdout = child.stdout.take().expect("MCP stdout");

        let (stdin_tx, stdin_rx) = mpsc::channel();
        let (writer_done_tx, writer_done) = mpsc::channel();
        let writer = thread::spawn(move || {
            while let Ok(command) = stdin_rx.recv() {
                match command {
                    StdinCommand::Write(line, ack) => {
                        let result = stdin
                            .write_all(&line)
                            .and_then(|_| stdin.flush())
                            .map_err(|error| format!("write MCP stdin: {error}"));
                        let _ = ack.send(result);
                    }
                    StdinCommand::Shutdown => break,
                }
            }
            let _ = writer_done_tx.send(());
        });

        let (response_tx, responses) = mpsc::channel();
        let (reader_done_tx, reader_done) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match stdout.read_line(&mut line) {
                    Ok(0) => {
                        let _ = response_tx.send(Err("MCP stdout closed".to_string()));
                        break;
                    }
                    Ok(_) => {
                        let parsed = serde_json::from_str(line.trim_end())
                            .map_err(|error| format!("invalid MCP JSON response: {error}"));
                        if response_tx.send(parsed).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = response_tx.send(Err(format!("read MCP stdout: {error}")));
                        break;
                    }
                }
            }
            let _ = reader_done_tx.send(());
        });

        Self {
            child: Some(child),
            stdin_tx,
            responses,
            writer_done,
            reader_done,
            writer: Some(writer),
            reader: Some(reader),
        }
    }

    fn send_line(&self, mut line: Vec<u8>) {
        line.push(b'\n');
        let (ack_tx, ack_rx) = mpsc::channel();
        self.stdin_tx
            .send(StdinCommand::Write(line, ack_tx))
            .expect("MCP writer thread is available");
        ack_rx
            .recv_timeout(MCP_IO_TIMEOUT)
            .expect("MCP stdin write deadline exceeded")
            .expect("MCP stdin write failed");
    }

    fn request(&self, value: Value) -> Value {
        self.send_line(serde_json::to_vec(&value).expect("serialize MCP request"));
        self.responses
            .recv_timeout(MCP_IO_TIMEOUT)
            .expect("MCP response deadline exceeded")
            .expect("MCP response failed")
    }

    fn notify(&self, value: Value) {
        self.send_line(serde_json::to_vec(&value).expect("serialize MCP notification"));
    }

    fn shutdown(&mut self) {
        if self.child.is_none() {
            return;
        }
        let _ = self.stdin_tx.send(StdinCommand::Shutdown);
        if let Some(child) = self.child.as_mut() {
            stop_child(child);
        }
        self.child.take();
        self.finish_thread(true);
        self.finish_thread(false);
    }

    fn finish_thread(&mut self, writer: bool) {
        let (done, handle) = if writer {
            (&self.writer_done, &mut self.writer)
        } else {
            (&self.reader_done, &mut self.reader)
        };
        if done.recv_timeout(PROCESS_SHUTDOWN_TIMEOUT).is_ok() {
            if let Some(handle) = handle.take() {
                let _ = handle.join();
            }
        } else {
            handle.take();
        }
    }
}

impl Drop for McpProc {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct ControlProc {
    child: Option<Child>,
    temp_dir: PathBuf,
    handshake_file: PathBuf,
    stop_file: PathBuf,
}

struct TempDirGuard(Option<PathBuf>);

impl TempDirGuard {
    fn disarm(mut self) -> PathBuf {
        self.0.take().expect("temporary directory path")
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

impl ControlProc {
    fn spawn(tmux_socket: &str) -> Self {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("th-mcp-e2e-control-{}-{id}", std::process::id()));
        fs::create_dir_all(&temp_dir).expect("create control helper temp directory");
        let temp_guard = TempDirGuard(Some(temp_dir));
        let temp_dir = temp_guard.0.as_ref().unwrap();
        let handshake_file = temp_dir.join("control.json");
        let stop_file = temp_dir.join("stop");
        let child = Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--exact",
                "mcp_control_helper",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("T_HUB_MCP_CONTROL_HELPER", "1")
            .env("T_HUB_CONTROL_FILE", &handshake_file)
            .env("T_HUB_MCP_CONTROL_STOP_FILE", &stop_file)
            .env("T_HUB_TMUX_SOCKET", tmux_socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn control helper process");
        let temp_dir = temp_guard.disarm();
        let mut process = Self {
            child: Some(child),
            temp_dir,
            handshake_file,
            stop_file,
        };
        process.wait_until_ready();
        process
    }

    fn wait_until_ready(&mut self) {
        let deadline = Instant::now() + HELPER_READY_TIMEOUT;
        loop {
            if fs::read(&self.handshake_file)
                .ok()
                .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
                .and_then(|value| {
                    value
                        .get("addr")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .is_some()
            {
                return;
            }
            if let Some(status) = self
                .child
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten())
            {
                panic!("control helper exited before readiness: {status}");
            }
            assert!(
                Instant::now() < deadline,
                "control helper readiness deadline exceeded"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn shutdown(&mut self) {
        let _ = fs::write(&self.stop_file, b"stop\n");
        if let Some(child) = self.child.as_mut() {
            stop_child(child);
        }
        self.child.take();
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

impl Drop for ControlProc {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[test]
#[ignore = "spawned only by end_to_end_mcp_round_trip"]
fn mcp_control_helper() {
    if std::env::var("T_HUB_MCP_CONTROL_HELPER").as_deref() != Ok("1") {
        return;
    }
    let supervisor = Arc::new(Mutex::new(t_hub_lib::supervision_for_test()));
    {
        let mut state = supervisor.lock();
        state.ingest(
            Some("sess-e2e"),
            None,
            None,
            None,
            JournalEventType::SessionStart,
            1,
        );
        state.ingest(
            Some("sess-e2e"),
            None,
            None,
            None,
            JournalEventType::UserPromptSubmit,
            2,
        );
        state.ingest(
            Some("sess-e2e"),
            Some("agent-1"),
            Some("general-purpose"),
            None,
            JournalEventType::SubagentStart,
            3,
        );
        state.ingest(
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
    let token = format!("e2e-token-{}", std::process::id());
    let context =
        control::ControlContext::with_shared_supervisor(status, supervisor, token.clone());
    let handshake = control::start(context).expect("control listener starts");
    assert_eq!(handshake.token, token);
    let stop_file = PathBuf::from(
        std::env::var_os("T_HUB_MCP_CONTROL_STOP_FILE").expect("control helper stop file"),
    );
    let deadline = Instant::now() + HELPER_LIFETIME;
    while !stop_file.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
}

fn tool_structured(response: &Value) -> &Value {
    assert!(
        response.get("error").is_none(),
        "tools/call returned a transport error: {response}"
    );
    &response["result"]["structuredContent"]
}

#[test]
fn end_to_end_mcp_round_trip() {
    let bin = locate_mcp_binary();
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let tmux_socket = format!("t-hub-mcpe2e-{}-{test_id}", std::process::id());
    let mut control = ControlProc::spawn(&tmux_socket);
    let control_temp_dir = control.temp_dir.clone();

    let tmux_session = format!("th_e2e{}", std::process::id() % 100000);
    let tmux_ok = make_tmux_session(&tmux_socket, &tmux_session);
    let mut tmux_guard = TmuxSessionGuard::new(tmux_socket.clone(), tmux_session.clone(), tmux_ok);
    let mut mcp = McpProc::spawn(&bin, &control.handshake_file, &tmux_socket);

    let init = mcp.request(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2024-11-05", "capabilities": {} }
    }));
    assert_eq!(init["result"]["serverInfo"]["name"], "t-hub-mcp");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    mcp.notify(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    let list = mcp.request(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let tools = list["result"]["tools"].as_array().expect("MCP tools array");
    for (name, tier) in POWDER_TOOLS {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("tools/list missing {name}"));
        assert_eq!(tool["annotations"]["t-hubTier"], tier, "{name}");
        assert_eq!(tool["annotations"]["confirmationRequired"], false, "{name}");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{name}");
        for field in FORBIDDEN_AUTHORITY_FIELDS {
            assert!(
                tool["inputSchema"]["properties"].get(field).is_none(),
                "{name} must not expose {field} substitution"
            );
        }
    }

    let health = mcp.request(json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "wsl_health", "arguments": {} }
    }));
    let health_data = tool_structured(&health);
    assert_eq!(health["result"]["isError"], false);
    assert_eq!(health_data["supervisedSessions"], 1);

    let status = mcp.request(json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": { "name": "get_status", "arguments": { "sessionId": "sess-e2e" } }
    }));
    let status_data = tool_structured(&status);
    assert_eq!(status_data["status"], "waitingOnSubagents");
    assert_eq!(status_data["snapshot"]["contextUsedPct"], 42.0);

    let tree = mcp.request(json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": { "name": "supervision_tree", "arguments": { "sessionId": "sess-e2e" } }
    }));
    assert_eq!(tool_structured(&tree)["children"][0]["agentId"], "agent-1");

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let search = mcp.request(json!({
        "jsonrpc": "2.0", "id": 6, "method": "tools/call",
        "params": {
            "name": "search_files",
            "arguments": { "root": manifest.to_string_lossy(), "query": "control", "limit": 10 }
        }
    }));
    assert!(tool_structured(&search)["hits"]
        .as_array()
        .expect("search hits")
        .iter()
        .any(|hit| hit["relPath"]
            .as_str()
            .is_some_and(|path| path.contains("control"))));

    let terminals = mcp.request(json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "name": "list_terminals", "arguments": {} }
    }));
    let terminal_data = tool_structured(&terminals);
    assert!(terminal_data["terminals"].is_array());
    if tmux_ok {
        assert!(terminal_data["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|terminal| terminal["tmuxSession"] == tmux_session));
    }

    let spawn = mcp.request(json!({
        "jsonrpc": "2.0", "id": 8, "method": "tools/call",
        "params": { "name": "spawn_terminal", "arguments": { "cwd": "/tmp" } }
    }));
    assert_eq!(spawn["result"]["isError"], true);
    assert!(spawn["result"]["content"][0]["text"]
        .as_str()
        .expect("spawn refusal")
        .contains("no UI"));

    mcp.shutdown();
    tmux_guard.shutdown();
    control.shutdown();
    assert!(
        !control_temp_dir.exists(),
        "control helper temporary directory was not removed"
    );
    if tmux_ok {
        assert!(
            !tmux_session_exists(&tmux_socket, &tmux_session),
            "tmux test session was not removed"
        );
    }
}

struct TmuxSessionGuard {
    socket: String,
    name: String,
    active: bool,
}

impl TmuxSessionGuard {
    fn new(socket: String, name: String, active: bool) -> Self {
        Self {
            socket,
            name,
            active,
        }
    }

    fn shutdown(&mut self) {
        if self.active {
            kill_tmux_session(&self.socket, &self.name);
            self.active = false;
        }
    }
}

impl Drop for TmuxSessionGuard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn make_tmux_session(socket: &str, name: &str) -> bool {
    Command::new("timeout")
        .args([
            "5s",
            "tmux",
            "-L",
            socket,
            "new-session",
            "-d",
            "-s",
            name,
            "sleep 300",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn kill_tmux_session(socket: &str, name: &str) {
    let _ = Command::new("timeout")
        .args(["5s", "tmux", "-L", socket, "kill-session", "-t", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn tmux_session_exists(socket: &str, name: &str) -> bool {
    Command::new("timeout")
        .args(["5s", "tmux", "-L", socket, "has-session", "-t", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
