//! End-to-end proof of the real MCP binary, control listener, and tmux path.
//!
//! The control listener runs in a helper process. Closing that process is the
//! explicit listener shutdown boundary, so a failed assertion cannot leave an
//! accept loop or request handler alive in the integration-test process.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
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
const HELPER_LIFETIME: Duration = Duration::from_secs(60);
const FIXTURE_IO_TIMEOUT: Duration = Duration::from_secs(3);
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

fn wait_for_child(child: &mut Child, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => return Err("child process did not exit before deadline".into()),
            Err(error) => return Err(format!("poll child process: {error}")),
        }
    }
}

fn stop_child(child: &mut Child) -> Result<(), String> {
    if wait_for_child(child, PROCESS_SHUTDOWN_TIMEOUT).is_ok() {
        return Ok(());
    }
    match child.kill() {
        Ok(()) => {}
        Err(_error) if child.try_wait().ok().flatten().is_some() => return Ok(()),
        Err(error) => return Err(format!("hard-kill child process: {error}")),
    }
    wait_for_child(child, PROCESS_SHUTDOWN_TIMEOUT)
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
    fn spawn(
        bin: &Path,
        handshake_file: &Path,
        tmux_socket: &str,
        endpoint: Option<(&str, &str)>,
        session_token: Option<&str>,
    ) -> Self {
        let mut command = Command::new(bin);
        command
            .env("T_HUB_CONTROL_FILE", handshake_file)
            .env("T_HUB_TMUX_SOCKET", tmux_socket)
            .env_remove("T_HUB_CONTROL_ADDR")
            .env_remove("T_HUB_CONTROL_TOKEN")
            .env_remove("T_HUB_SESSION_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some((addr, token)) = endpoint {
            command
                .env("T_HUB_CONTROL_ADDR", addr)
                .env("T_HUB_CONTROL_TOKEN", token);
        }
        if let Some(session_token) = session_token {
            command.env("T_HUB_SESSION_TOKEN", session_token);
        }
        let mut child = command.spawn().expect("spawn t-hub-mcp");
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

    fn shutdown(&mut self) -> Result<(), String> {
        if self.child.is_none() && self.writer.is_none() && self.reader.is_none() {
            return Ok(());
        }
        let _ = self.stdin_tx.send(StdinCommand::Shutdown);
        if let Some(child) = self.child.as_mut() {
            stop_child(child)?;
        }
        self.child.take();
        self.finish_thread(true)?;
        self.finish_thread(false)?;
        Ok(())
    }

    fn finish_thread(&mut self, writer: bool) -> Result<(), String> {
        let (done, handle) = if writer {
            (&self.writer_done, &mut self.writer)
        } else {
            (&self.reader_done, &mut self.reader)
        };
        done.recv_timeout(PROCESS_SHUTDOWN_TIMEOUT).map_err(|_| {
            format!(
                "MCP {} thread did not stop",
                if writer { "writer" } else { "reader" }
            )
        })?;
        if let Some(handle) = handle.take() {
            handle.join().map_err(|_| {
                format!(
                    "MCP {} thread panicked",
                    if writer { "writer" } else { "reader" }
                )
            })?;
        }
        Ok(())
    }
}

impl Drop for McpProc {
    fn drop(&mut self) {
        if self.shutdown().is_err() {
            std::process::abort();
        }
    }
}

struct ControlProc {
    child: Option<Child>,
    temp_dir: PathBuf,
    handshake_file: PathBuf,
    stop_file: PathBuf,
    auth_file: PathBuf,
    seed_file: PathBuf,
    seed_ready_file: PathBuf,
    powder_state_file: PathBuf,
    addr: String,
    control_token: String,
    read_token: String,
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
        let auth_file = temp_dir.join("auth.json");
        let seed_file = temp_dir.join("seed.json");
        let seed_ready_file = temp_dir.join("seed-ready");
        let powder_state_file = temp_dir.join("powder-state.json");
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
            .env("T_HUB_MCP_CONTROL_AUTH_FILE", &auth_file)
            .env("T_HUB_MCP_CONTROL_SEED_FILE", &seed_file)
            .env("T_HUB_MCP_CONTROL_SEED_READY_FILE", &seed_ready_file)
            .env("T_HUB_MCP_POWDER_STATE_FILE", &powder_state_file)
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
            auth_file,
            seed_file,
            seed_ready_file,
            powder_state_file,
            addr: String::new(),
            control_token: String::new(),
            read_token: String::new(),
        };
        process.wait_until_ready();
        process
    }

    fn wait_until_ready(&mut self) {
        let deadline = Instant::now() + HELPER_READY_TIMEOUT;
        loop {
            if let Some(auth) = fs::read(&self.auth_file)
                .ok()
                .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
            {
                self.addr = auth["addr"].as_str().expect("helper addr").to_string();
                self.control_token = auth["controlToken"]
                    .as_str()
                    .expect("helper control token")
                    .to_string();
                self.read_token = auth["readToken"]
                    .as_str()
                    .expect("helper read token")
                    .to_string();
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

    fn shutdown(&mut self) -> Result<(), String> {
        if self.child.is_none() {
            return Ok(());
        }
        fs::write(&self.stop_file, b"stop\n")
            .map_err(|error| format!("signal control helper: {error}"))?;
        if let Some(child) = self.child.as_mut() {
            stop_child(child)?;
        }
        self.child.take();
        let addr = self
            .addr
            .parse::<SocketAddr>()
            .map_err(|error| format!("parse helper address: {error}"))?;
        if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            return Err("control listener remained reachable after helper exit".into());
        }
        fs::remove_dir_all(&self.temp_dir)
            .map_err(|error| format!("remove control helper temp directory: {error}"))?;
        if self.temp_dir.exists() {
            return Err("control helper temp directory remained after removal".into());
        }
        Ok(())
    }
}

impl Drop for ControlProc {
    fn drop(&mut self) {
        if self.shutdown().is_err() {
            std::process::abort();
        }
    }
}

struct NoopApplySink;

impl control::ApplySink for NoopApplySink {
    fn apply(&self, _command: &str, _args: &Value) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PowderFixtureState {
    completed: bool,
    proof: Option<String>,
    append_posts: Vec<Value>,
    completion_posts: Vec<Value>,
    request_paths: Vec<String>,
}

struct PowderFixtureServer {
    addr: SocketAddr,
    stop: Arc<std::sync::atomic::AtomicBool>,
    done: Receiver<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl PowderFixtureServer {
    fn start(state_file: PathBuf) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind Powder fixture");
        listener
            .set_nonblocking(true)
            .expect("set Powder fixture nonblocking");
        let addr = listener.local_addr().expect("Powder fixture address");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_stop = stop.clone();
        let (done_tx, done) = mpsc::channel();
        let thread = thread::spawn(move || {
            let state = Arc::new(std::sync::Mutex::new(PowderFixtureState::default()));
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((_stream, _)) if thread_stop.load(Ordering::Acquire) => break,
                    Ok((stream, _)) => handle_powder_fixture_request(stream, &state, &state_file),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept Powder fixture request: {error}"),
                }
            }
            write_powder_fixture_state(&state_file, &state.lock().unwrap());
            let _ = done_tx.send(());
        });
        Self {
            addr,
            stop,
            done,
            thread: Some(thread),
        }
    }

    fn shutdown(&mut self) -> Result<(), String> {
        if self.thread.is_none() {
            return Ok(());
        }
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(100));
        self.done
            .recv_timeout(PROCESS_SHUTDOWN_TIMEOUT)
            .map_err(|_| "Powder fixture thread did not stop".to_string())?;
        self.thread
            .take()
            .expect("Powder fixture thread")
            .join()
            .map_err(|_| "Powder fixture thread panicked".to_string())?;
        Ok(())
    }
}

impl Drop for PowderFixtureServer {
    fn drop(&mut self) {
        if self.shutdown().is_err() {
            std::process::abort();
        }
    }
}

fn write_powder_fixture_state(path: &Path, state: &PowderFixtureState) {
    fs::write(
        path,
        serde_json::to_vec(state).expect("serialize Powder fixture state"),
    )
    .expect("write Powder fixture state");
}

fn handle_powder_fixture_request(
    mut stream: TcpStream,
    state: &Arc<std::sync::Mutex<PowderFixtureState>>,
    state_file: &Path,
) {
    stream
        .set_read_timeout(Some(FIXTURE_IO_TIMEOUT))
        .expect("set Powder fixture read timeout");
    stream
        .set_write_timeout(Some(FIXTURE_IO_TIMEOUT))
        .expect("set Powder fixture write timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("clone Powder fixture stream"));
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .expect("read Powder fixture request line");
    if request_line.is_empty() {
        return;
    }
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        reader
            .read_line(&mut header)
            .expect("read Powder fixture header");
        if header == "\r\n" || header.is_empty() {
            break;
        }
        if let Some(length) = header
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .and_then(|value| value.trim().parse::<usize>().ok())
        {
            content_length = length;
        }
    }
    assert!(
        content_length <= 32 * 1024,
        "Powder fixture request too large"
    );
    let mut body = vec![0; content_length];
    reader
        .read_exact(&mut body)
        .expect("read Powder fixture request body");
    let body = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).expect("parse Powder fixture request body")
    };
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default();
    let path = request_parts.next().unwrap_or_default();
    let (status, response) = {
        let mut state = state.lock().unwrap();
        state.request_paths.push(format!("{method} {path}"));
        let result = match (method, path) {
            ("GET", "/api/v1/cards/mcp-owned-card") => (200, powder_card_evidence(&state)),
            ("GET", "/api/v1/runs/mcp-owned-run") => (200, powder_run_evidence(&state)),
            ("POST", "/api/v1/cards/mcp-owned-card/work-log") => {
                state.append_posts.push(body.clone());
                (
                    200,
                    json!({
                        "card_id": "mcp-owned-card",
                        "agent": body["agent"],
                        "model": body["model"],
                        "reasoning": body["reasoning"],
                        "harness": body["harness"],
                        "run_id": body["run_id"],
                        "body": body["body"],
                        "created_at": 11
                    }),
                )
            }
            ("POST", "/api/v1/cards/mcp-owned-card/complete") => {
                state.completion_posts.push(body.clone());
                state.completed = true;
                state.proof = body["proof"].as_str().map(str::to_string);
                (200, powder_card(&state))
            }
            _ => (404, json!({"error": "unexpected fixture route"})),
        };
        write_powder_fixture_state(state_file, &state);
        result
    };
    let response = response.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        if status == 200 { "OK" } else { "Not Found" },
        response.len(),
        response
    )
    .expect("write Powder fixture response");
    stream.flush().expect("flush Powder fixture response");
}

fn powder_criterion() -> Value {
    json!({
        "text": "MCP dispatcher criterion",
        "checked_by": "mcp-captain",
        "checked_at": 123,
        "proof_links": []
    })
}

fn powder_run(state: &PowderFixtureState) -> Value {
    json!({
        "id": "mcp-owned-run",
        "card_id": "mcp-owned-card",
        "state": if state.completed { "complete" } else { "active" },
        "agent": "powder-agent",
        "proof": state.proof,
        "claim_expires_at": if state.completed { 0 } else { 100 },
        "created_at": 1,
        "updated_at": 2
    })
}

fn powder_card(state: &PowderFixtureState) -> Value {
    json!({
        "id": "mcp-owned-card",
        "title": "MCP dispatcher sentinel",
        "status": if state.completed { "done" } else { "running" },
        "repo": "t-hub",
        "updated_at": 2,
        "claim": if state.completed { Value::Null } else { json!({
            "run_id": "mcp-owned-run",
            "agent": "powder-agent",
            "expires_at": 100
        }) },
        "criteria": [powder_criterion()]
    })
}

fn powder_card_evidence(state: &PowderFixtureState) -> Value {
    let work_log = state
        .append_posts
        .iter()
        .enumerate()
        .map(|(index, post)| {
            json!({
                "card_id": "mcp-owned-card",
                "agent": post["agent"],
                "model": post["model"],
                "reasoning": post["reasoning"],
                "harness": post["harness"],
                "run_id": post["run_id"],
                "body": post["body"],
                "created_at": 11 + index as i64
            })
        })
        .collect::<Vec<_>>();
    json!({
        "card": powder_card(state),
        "runs": [powder_run(state)],
        "runs_total": 1,
        "work_log": work_log,
        "work_log_total": state.append_posts.len()
    })
}

fn powder_run_evidence(state: &PowderFixtureState) -> Value {
    json!({
        "run": powder_run(state),
        "card": powder_card(state),
        "activities": [],
        "activities_total": 0,
        "links": [],
        "links_total": 0
    })
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
    let read_token = format!("e2e-read-token-{}", std::process::id());
    let registry = Arc::new(control::CaptainsRegistry::new());
    let powder_state_file =
        PathBuf::from(std::env::var_os("T_HUB_MCP_POWDER_STATE_FILE").expect("Powder state file"));
    let mut powder_server = PowderFixtureServer::start(powder_state_file);
    let profile_file = powder_server_profile_file(powder_server.addr);
    std::env::set_var("T_HUB_POWDER_PROFILES_FILE", &profile_file);
    let context =
        control::ControlContext::with_shared_supervisor(status, supervisor, token.clone())
            .with_read_token(read_token.clone())
            .with_captains_registry(registry.clone())
            .with_apply_sink(Arc::new(NoopApplySink));
    let handshake = control::start(context).expect("control listener starts");
    assert_eq!(handshake.local_control_token, token);
    fs::write(
        std::env::var_os("T_HUB_MCP_CONTROL_AUTH_FILE").expect("control helper auth file"),
        serde_json::to_vec(&json!({
            "addr": handshake.addr,
            "controlToken": handshake.local_control_token,
            "readToken": handshake.read_token,
        }))
        .expect("serialize control helper auth"),
    )
    .expect("write control helper auth");
    let stop_file = PathBuf::from(
        std::env::var_os("T_HUB_MCP_CONTROL_STOP_FILE").expect("control helper stop file"),
    );
    let seed_file = PathBuf::from(
        std::env::var_os("T_HUB_MCP_CONTROL_SEED_FILE").expect("control helper seed file"),
    );
    let seed_ready_file = PathBuf::from(
        std::env::var_os("T_HUB_MCP_CONTROL_SEED_READY_FILE")
            .expect("control helper seed ready file"),
    );
    let deadline = Instant::now() + HELPER_LIFETIME;
    let mut seeded = false;
    while !stop_file.exists() && Instant::now() < deadline {
        if !seeded {
            if let Ok(body) = fs::read(&seed_file) {
                let seed: Value = serde_json::from_slice(&body).expect("parse registry seed");
                seed_powder_registry(&registry, &seed);
                fs::write(&seed_ready_file, b"ready\n").expect("write seed ready marker");
                seeded = true;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    powder_server.shutdown().expect("stop Powder fixture");
    let _ = fs::remove_file(profile_file);
}

fn powder_server_profile_file(addr: SocketAddr) -> PathBuf {
    let path =
        PathBuf::from(std::env::var_os("T_HUB_MCP_POWDER_STATE_FILE").expect("Powder state file"))
            .with_file_name("powder-profiles.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "schemaVersion": 1,
            "profiles": {
                "mcp-e2e-powder": {
                    "baseUrl": format!("http://{addr}"),
                    "agentName": "powder-agent",
                    "apiKey": "mcp-e2e-key"
                }
            }
        }))
        .expect("serialize Powder profile"),
    )
    .expect("write Powder profile");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("protect Powder profile fixture");
    }
    path
}

fn seed_powder_registry(registry: &control::CaptainsRegistry, seed: &Value) {
    for fixture in ["owned", "foreign"] {
        let captain = seed[format!("{fixture}Captain")]
            .as_str()
            .expect("seed Captain tile");
        let crew = seed[format!("{fixture}Crew")]
            .as_str()
            .expect("seed Crew tile");
        let ship = format!("mcp-{fixture}-ship");
        let project_id = format!("mcp-{fixture}-project");
        let card_id = format!("mcp-{fixture}-card");
        let run_id = format!("mcp-{fixture}-run");
        registry
            .upsert_project(control::ProjectRecord {
                project_id: project_id.clone(),
                name: format!("MCP {fixture} project"),
                repo_root: format!("/tmp/mcp-{fixture}-project"),
                remote_url: None,
                default_branch: Some("main".into()),
                powder: Some(control::PowderProjectBinding {
                    connection_profile: "mcp-e2e-powder".into(),
                    repository: "t-hub".into(),
                    event_cursor: 0,
                }),
                created_at: 0,
                updated_at: 0,
            })
            .expect("register Powder fixture project");
        registry
            .claim_provider(
                captain,
                Some(&ship),
                control::FleetRole::Captain,
                Some("codex"),
                Some(&format!("mcp-{fixture}-thread")),
                vec![],
                &|_| false,
                &|_| panic!("crew liveness is unused for a fresh claim"),
            )
            .expect("claim fixture Captain");
        registry
            .bind_ship_context(captain, &project_id, "MCP E2E fixture", "codex")
            .or_else(|_| registry.bind_ship_context(&ship, &project_id, "MCP E2E fixture", "codex"))
            .expect("bind fixture Captain project");
        registry
            .record_crew(captain, crew)
            .expect("record fixture Crew");
        let powder_work = serde_json::from_value(json!({
            "cardId": card_id,
            "runId": run_id,
            "claimExpiresAt": 100,
            "state": { "kind": "active" }
        }))
        .expect("deserialize cross-version Powder work binding");
        registry
            .bind_crew_context(
                captain,
                crew,
                "Exercise Powder MCP lifecycle",
                "codex",
                Some("/tmp"),
                Some("feat/mcp-e2e"),
                powder_work,
            )
            .expect("bind fixture Crew Powder work");
    }
}

fn tool_structured(response: &Value) -> &Value {
    assert!(
        response.get("error").is_none(),
        "tools/call returned a transport error: {response}"
    );
    &response["result"]["structuredContent"]
}

fn assert_response_id(response: &Value, id: u64) {
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], id);
}

fn initialize_mcp(mcp: &McpProc, id: u64) {
    let response = mcp.request(json!({
        "jsonrpc": "2.0", "id": id, "method": "initialize",
        "params": { "protocolVersion": "2024-11-05", "capabilities": {} }
    }));
    assert_response_id(&response, id);
    assert_eq!(response["result"]["serverInfo"]["name"], "t-hub-mcp");
    mcp.notify(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
}

fn call_tool(mcp: &McpProc, id: u64, name: &str, arguments: Value) -> Value {
    let response = mcp.request(json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    }));
    assert_response_id(&response, id);
    response
}

fn tool_error_text(response: &Value) -> &str {
    assert_eq!(
        response["result"]["isError"], true,
        "expected tool error: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool error text")
}

fn spawn_fixture_terminal(mcp: &McpProc, id: u64, capability: &str) -> String {
    let response = call_tool(
        mcp,
        id,
        "spawn_terminal",
        json!({
            "cwd": "/tmp",
            "startupCommand": "sleep 300",
            "capability": capability
        }),
    );
    let result = tool_structured(&response);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(result["accepted"], "spawn_terminal");
    result["id"]
        .as_str()
        .expect("spawned terminal id")
        .to_string()
}

fn tmux_session_token(socket: &str, terminal_id: &str) -> String {
    let target = format!("th_{terminal_id}");
    let output = bounded_tmux_output(
        socket,
        &["show-environment", "-t", &target, "T_HUB_SESSION_TOKEN"],
    )
    .expect("read tmux session identity");
    assert!(output.status.success(), "tmux session identity unavailable");
    String::from_utf8(output.stdout)
        .expect("tmux session identity UTF-8")
        .trim()
        .strip_prefix("T_HUB_SESSION_TOKEN=")
        .expect("tmux session token assignment")
        .to_string()
}

fn wait_for_path(path: &Path, label: &str) {
    let deadline = Instant::now() + HELPER_READY_TIMEOUT;
    while !path.exists() {
        assert!(Instant::now() < deadline, "{label} deadline exceeded");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn powder_tools_reach_real_authenticated_dispatcher() {
    let bin = locate_mcp_binary();
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let tmux_socket = format!("t-hub-mcp-powder-{}-{test_id}", std::process::id());
    let mut control = ControlProc::spawn(&tmux_socket);
    let endpoint = (&control.addr[..], &control.read_token[..]);

    let mut anonymous = McpProc::spawn(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        Some(endpoint),
        None,
    );
    initialize_mcp(&anonymous, 100);
    let anonymous_read = call_tool(
        &anonymous,
        101,
        "read_crew_powder_evidence",
        json!({"limit": 20}),
    );
    let anonymous_read_error = tool_error_text(&anonymous_read);
    if (anonymous_read_error.contains("unknown control command")
        || anonymous_read_error.contains("is not exposed over the control channel"))
        && std::env::var("T_HUB_ALLOW_MISSING_POWDER_CONTROL").as_deref() == Ok("1")
    {
        anonymous.shutdown().expect("stop anonymous MCP");
        control.shutdown().expect("stop control helper");
        return;
    }
    assert!(
        anonymous_read_error.contains(
            "acl: 'read_crew_powder_evidence' requires a valid Crew or Captain T_HUB_SESSION_TOKEN"
        ),
        "unexpected anonymous evidence refusal: {anonymous_read_error}"
    );
    let anonymous_append = call_tool(
        &anonymous,
        102,
        "append_crew_powder_work_log",
        json!({"message": "anonymous must not append"}),
    );
    assert!(tool_error_text(&anonymous_append).contains(
        "unauthorized: 'append_crew_powder_work_log' requires the control capability (this token is read-only)"
    ));
    let anonymous_complete = call_tool(
        &anonymous,
        103,
        "complete_crew_powder",
        json!({"crewSessionId": "missing", "proof": "anonymous must not complete"}),
    );
    assert!(tool_error_text(&anonymous_complete).contains(
        "unauthorized: 'complete_crew_powder' requires the control capability (this token is read-only)"
    ));
    anonymous.shutdown().expect("stop anonymous MCP");

    let mut tmux_guard = TmuxServerGuard::start(
        tmux_socket.clone(),
        format!("th_mcp_guard_{}", std::process::id()),
    )
    .expect("start dedicated Powder tmux server");
    let mut bootstrap = McpProc::spawn(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        Some((&control.addr, &control.control_token)),
        None,
    );
    initialize_mcp(&bootstrap, 110);
    let owned_captain = spawn_fixture_terminal(&bootstrap, 111, "control");
    let owned_crew = spawn_fixture_terminal(&bootstrap, 112, "read");
    let foreign_captain = spawn_fixture_terminal(&bootstrap, 113, "control");
    let foreign_crew = spawn_fixture_terminal(&bootstrap, 114, "read");
    let owned_captain_token = tmux_session_token(&tmux_socket, &owned_captain);
    let owned_crew_token = tmux_session_token(&tmux_socket, &owned_crew);
    let foreign_captain_token = tmux_session_token(&tmux_socket, &foreign_captain);
    let foreign_crew_token = tmux_session_token(&tmux_socket, &foreign_crew);
    fs::write(
        &control.seed_file,
        serde_json::to_vec(&json!({
            "ownedCaptain": owned_captain,
            "ownedCrew": owned_crew,
            "foreignCaptain": foreign_captain,
            "foreignCrew": foreign_crew,
        }))
        .expect("serialize registry seed"),
    )
    .expect("write registry seed");
    wait_for_path(&control.seed_ready_file, "registry seed");
    bootstrap.shutdown().expect("stop bootstrap MCP");

    let mut owned_crew_mcp = McpProc::spawn(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        Some((&control.addr, &control.read_token)),
        Some(&owned_crew_token),
    );
    initialize_mcp(&owned_crew_mcp, 120);
    let append = call_tool(
        &owned_crew_mcp,
        121,
        "append_crew_powder_work_log",
        json!({"message": "real MCP append sentinel"}),
    );
    let append_data = tool_structured(&append);
    assert_eq!(append_data["accepted"], "append_crew_powder_work_log");
    assert_eq!(append_data["crewSessionId"], owned_crew);
    assert_eq!(append_data["cardId"], "mcp-owned-card");
    assert_eq!(append_data["runId"], "mcp-owned-run");
    assert_eq!(append_data["messageBytes"], 24);
    let crew_read = call_tool(
        &owned_crew_mcp,
        122,
        "read_crew_powder_evidence",
        json!({"limit": 20}),
    );
    let crew_read_data = tool_structured(&crew_read);
    assert_eq!(crew_read_data["accepted"], "read_crew_powder_evidence");
    assert_eq!(crew_read_data["crewSessionId"], owned_crew);
    assert_eq!(crew_read_data["card"]["cardId"], "mcp-owned-card");
    assert_eq!(crew_read_data["card"]["title"], "MCP dispatcher sentinel");
    assert_eq!(crew_read_data["run"]["run"]["runId"], "mcp-owned-run");
    assert_eq!(
        crew_read_data["card"]["workLog"][0]["body"],
        "real MCP append sentinel"
    );
    assert_eq!(crew_read_data["card"]["workLog"][0]["agent"], owned_crew);
    let crew_foreign = call_tool(
        &owned_crew_mcp,
        123,
        "read_crew_powder_evidence",
        json!({"crewSessionId": foreign_crew}),
    );
    assert!(tool_error_text(&crew_foreign).contains("requires the same-ship Captain"));
    owned_crew_mcp.shutdown().expect("stop owned Crew MCP");

    let mut foreign_crew_mcp = McpProc::spawn(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        Some((&control.addr, &control.read_token)),
        Some(&foreign_crew_token),
    );
    initialize_mcp(&foreign_crew_mcp, 130);
    let foreign_owned = call_tool(
        &foreign_crew_mcp,
        131,
        "read_crew_powder_evidence",
        json!({"crewSessionId": owned_crew}),
    );
    assert!(tool_error_text(&foreign_owned).contains("requires the same-ship Captain"));
    foreign_crew_mcp.shutdown().expect("stop foreign Crew MCP");

    let mut read_only_captain = McpProc::spawn(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        Some((&control.addr, &control.read_token)),
        Some(&owned_captain_token),
    );
    initialize_mcp(&read_only_captain, 140);
    let read_only_completion = call_tool(
        &read_only_captain,
        141,
        "complete_crew_powder",
        json!({"crewSessionId": owned_crew, "proof": "read token must not complete"}),
    );
    assert!(tool_error_text(&read_only_completion).contains(
        "unauthorized: 'complete_crew_powder' requires the control capability (this token is read-only)"
    ));
    read_only_captain
        .shutdown()
        .expect("stop read-only Captain MCP");

    let mut foreign_captain_mcp = McpProc::spawn(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        Some((&control.addr, &control.control_token)),
        Some(&foreign_captain_token),
    );
    initialize_mcp(&foreign_captain_mcp, 150);
    let foreign_completion = call_tool(
        &foreign_captain_mcp,
        151,
        "complete_crew_powder",
        json!({"crewSessionId": owned_crew, "proof": "foreign must not complete"}),
    );
    assert!(tool_error_text(&foreign_completion).contains("requires the same-ship Captain"));
    foreign_captain_mcp
        .shutdown()
        .expect("stop foreign Captain MCP");

    let mut owned_captain_mcp = McpProc::spawn(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        Some((&control.addr, &control.control_token)),
        Some(&owned_captain_token),
    );
    initialize_mcp(&owned_captain_mcp, 160);
    let captain_read = call_tool(
        &owned_captain_mcp,
        161,
        "read_crew_powder_evidence",
        json!({"crewSessionId": owned_crew, "limit": 20}),
    );
    assert_eq!(tool_structured(&captain_read)["crewSessionId"], owned_crew);
    let captain_foreign_read = call_tool(
        &owned_captain_mcp,
        162,
        "read_crew_powder_evidence",
        json!({"crewSessionId": foreign_crew}),
    );
    assert!(tool_error_text(&captain_foreign_read).contains("requires the same-ship Captain"));
    let captain_foreign_complete = call_tool(
        &owned_captain_mcp,
        163,
        "complete_crew_powder",
        json!({"crewSessionId": foreign_crew, "proof": "wrong ship"}),
    );
    assert!(tool_error_text(&captain_foreign_complete).contains("requires the same-ship Captain"));
    let completion = call_tool(
        &owned_captain_mcp,
        164,
        "complete_crew_powder",
        json!({"crewSessionId": owned_crew, "proof": "real MCP completion sentinel"}),
    );
    let completion_data = tool_structured(&completion);
    assert_eq!(completion_data["accepted"], "complete_crew_powder");
    assert_eq!(completion_data["crewSessionId"], owned_crew);
    assert_eq!(completion_data["cardId"], "mcp-owned-card");
    assert_eq!(completion_data["runId"], "mcp-owned-run");
    assert_eq!(completion_data["cardStatus"], "done");
    assert_eq!(completion_data["runState"], "complete");
    owned_captain_mcp
        .shutdown()
        .expect("stop owned Captain MCP");

    let fixture: Value = serde_json::from_slice(
        &fs::read(&control.powder_state_file).expect("read Powder fixture sentinel"),
    )
    .expect("parse Powder fixture sentinel");
    assert_eq!(fixture["appendPosts"].as_array().unwrap().len(), 1);
    assert_eq!(fixture["completionPosts"].as_array().unwrap().len(), 1);
    assert_eq!(fixture["appendPosts"][0]["agent"], owned_crew);
    assert_eq!(fixture["appendPosts"][0]["run_id"], "mcp-owned-run");
    assert_eq!(
        fixture["appendPosts"][0]["body"],
        "real MCP append sentinel"
    );
    assert_eq!(
        fixture["completionPosts"][0]["proof"],
        "real MCP completion sentinel"
    );
    assert_eq!(fixture["completed"], true);
    assert!(fixture["requestPaths"]
        .as_array()
        .unwrap()
        .iter()
        .all(|path| !path.as_str().unwrap().contains("mcp-foreign")));
    for required_get in [
        "GET /api/v1/cards/mcp-owned-card",
        "GET /api/v1/runs/mcp-owned-run",
    ] {
        assert!(
            fixture["requestPaths"]
                .as_array()
                .unwrap()
                .iter()
                .any(|path| path == required_get),
            "Powder fixture did not receive {required_get}"
        );
    }

    tmux_guard.shutdown().expect("stop Powder tmux fixtures");
    control.shutdown().expect("stop Powder control helper");
}

#[test]
fn end_to_end_mcp_round_trip() {
    let bin = locate_mcp_binary();
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let tmux_socket = format!("t-hub-mcpe2e-{}-{test_id}", std::process::id());
    let mut control = ControlProc::spawn(&tmux_socket);
    let control_temp_dir = control.temp_dir.clone();

    let tmux_session = format!("th_e2e{}", std::process::id() % 100000);
    let mut tmux_guard = TmuxServerGuard::start(tmux_socket.clone(), tmux_session.clone())
        .expect("start dedicated baseline tmux server");
    let mut mcp = McpProc::spawn(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        Some((&control.addr, &control.read_token)),
        None,
    );

    let init = mcp.request(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2024-11-05", "capabilities": {} }
    }));
    assert_response_id(&init, 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "t-hub-mcp");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    mcp.notify(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    let list = mcp.request(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    assert_response_id(&list, 2);
    let tools = list["result"]["tools"].as_array().expect("MCP tools array");
    for required in [
        "wsl_health",
        "get_status",
        "supervision_tree",
        "search_files",
        "list_tabs",
        "list_terminals",
        "get_theme",
        "spawn_terminal",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == required),
            "tools/list missing baseline tool {required}"
        );
    }
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
    assert_response_id(&health, 3);
    let health_data = tool_structured(&health);
    assert_eq!(health["result"]["isError"], false);
    assert_eq!(health_data["supervisedSessions"], 1);
    assert!(health_data["metrics"]["capturedAtMs"].as_u64().unwrap_or(0) > 0);

    let status = mcp.request(json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": { "name": "get_status", "arguments": { "sessionId": "sess-e2e" } }
    }));
    assert_response_id(&status, 4);
    let status_data = tool_structured(&status);
    assert_eq!(status_data["sessionId"], "sess-e2e");
    assert_eq!(status_data["resolvedSessionId"], "sess-e2e");
    assert_eq!(status_data["status"], "waitingOnSubagents");
    assert_eq!(status_data["snapshot"]["contextUsedPct"], 42.0);

    let tree = mcp.request(json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": { "name": "supervision_tree", "arguments": { "sessionId": "sess-e2e" } }
    }));
    assert_response_id(&tree, 5);
    let tree_data = tool_structured(&tree);
    assert_eq!(tree_data["sessionId"], "sess-e2e");
    assert_eq!(tree_data["status"], "waitingOnSubagents");
    assert_eq!(tree_data["children"].as_array().unwrap().len(), 1);
    assert_eq!(tree_data["children"][0]["agentId"], "agent-1");
    assert_eq!(tree_data["outstandingTasks"], 0);

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let search = mcp.request(json!({
        "jsonrpc": "2.0", "id": 6, "method": "tools/call",
        "params": {
            "name": "search_files",
            "arguments": { "root": manifest.to_string_lossy(), "query": "control", "limit": 10 }
        }
    }));
    assert_response_id(&search, 6);
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
    assert_response_id(&terminals, 7);
    let terminal_data = tool_structured(&terminals);
    assert!(terminal_data["terminals"].is_array());
    assert!(terminal_data["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|terminal| terminal["tmuxSession"] == tmux_session));

    let spawn = mcp.request(json!({
        "jsonrpc": "2.0", "id": 8, "method": "tools/call",
        "params": { "name": "spawn_terminal", "arguments": { "cwd": "/tmp" } }
    }));
    assert_response_id(&spawn, 8);
    assert_eq!(spawn["result"]["isError"], true);
    assert!(spawn["result"]["content"][0]["text"]
        .as_str()
        .expect("spawn refusal")
        .contains("requires the control capability (this token is read-only)"));

    mcp.shutdown().expect("stop MCP process");
    tmux_guard.shutdown().expect("stop tmux fixture");
    control.shutdown().expect("stop control helper");
    assert!(
        !control_temp_dir.exists(),
        "control helper temporary directory was not removed"
    );
}

struct TmuxServerGuard {
    socket: String,
    socket_path: Option<PathBuf>,
    active: bool,
}

impl TmuxServerGuard {
    fn start(socket: String, initial_session: String) -> Result<Self, String> {
        match probe_tmux_server(&socket) {
            TmuxServerProbe::Absent => {}
            TmuxServerProbe::Present => {
                return Err(format!("dedicated tmux server {socket} already exists"));
            }
            TmuxServerProbe::Failed(error) => {
                return Err(format!("probe dedicated tmux server {socket}: {error}"));
            }
        }
        let mut guard = Self {
            socket,
            socket_path: None,
            active: true,
        };
        let output = bounded_tmux_output(
            &guard.socket,
            &["new-session", "-d", "-s", &initial_session, "sleep 300"],
        )?;
        if !output.status.success() {
            return Err(format!(
                "start dedicated tmux server {}: {}",
                guard.socket,
                command_failure(&output)
            ));
        }
        match probe_tmux_server(&guard.socket) {
            TmuxServerProbe::Present => {}
            TmuxServerProbe::Absent => {
                return Err(format!(
                    "dedicated tmux server {} disappeared after start",
                    guard.socket
                ));
            }
            TmuxServerProbe::Failed(error) => {
                return Err(format!(
                    "verify dedicated tmux server {}: {error}",
                    guard.socket
                ));
            }
        }
        let output =
            bounded_tmux_output(&guard.socket, &["display-message", "-p", "#{socket_path}"])?;
        if !output.status.success() {
            return Err(format!(
                "resolve dedicated tmux socket {}: {}",
                guard.socket,
                command_failure(&output)
            ));
        }
        let socket_path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
        if socket_path.as_os_str().is_empty() || !socket_path.exists() {
            return Err(format!(
                "dedicated tmux socket path for {} was not present",
                guard.socket
            ));
        }
        guard.socket_path = Some(socket_path);
        Ok(guard)
    }

    fn shutdown(&mut self) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        let mut last_probe_failure = None;
        for _attempt in 0..3 {
            match probe_tmux_server(&self.socket) {
                TmuxServerProbe::Present => {
                    if let Err(error) = kill_tmux_server(&self.socket) {
                        last_probe_failure = Some(error);
                    }
                }
                TmuxServerProbe::Absent => {
                    if let Some(path) = self.socket_path.as_ref().filter(|path| path.exists()) {
                        fs::remove_file(path)
                            .map_err(|error| format!("remove stale tmux socket: {error}"))?;
                    }
                    if self.socket_path.as_ref().is_some_and(|path| path.exists()) {
                        return Err(format!(
                            "dedicated tmux socket for {} remained after server absence",
                            self.socket
                        ));
                    }
                    match probe_tmux_server(&self.socket) {
                        TmuxServerProbe::Absent => {
                            self.active = false;
                            return Ok(());
                        }
                        TmuxServerProbe::Present => {}
                        TmuxServerProbe::Failed(error) => {
                            last_probe_failure = Some(error);
                        }
                    }
                }
                TmuxServerProbe::Failed(error) => {
                    last_probe_failure = Some(error);
                    let _ = kill_tmux_server(&self.socket);
                }
            }
        }
        Err(format!(
            "dedicated tmux server {} was not confirmed absent{}",
            self.socket,
            last_probe_failure
                .map(|error| format!("; last probe failure: {error}"))
                .unwrap_or_default()
        ))
    }
}

impl Drop for TmuxServerGuard {
    fn drop(&mut self) {
        if self.shutdown().is_err() {
            std::process::abort();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TmuxServerProbe {
    Present,
    Absent,
    Failed(String),
}

fn bounded_tmux_output(socket: &str, args: &[&str]) -> Result<Output, String> {
    let output = Command::new("timeout")
        .args([
            "--signal=TERM",
            "--kill-after=1s",
            "3s",
            "tmux",
            "-L",
            socket,
        ])
        .args(args)
        .output()
        .map_err(|error| format!("spawn bounded tmux command: {error}"))?;
    if matches!(output.status.code(), Some(124 | 137)) {
        return Err(format!(
            "tmux command exceeded its timeout: tmux -L {socket} {}",
            args.join(" ")
        ));
    }
    Ok(output)
}

fn kill_tmux_server(socket: &str) -> Result<(), String> {
    let output = bounded_tmux_output(socket, &["kill-server"])?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "kill dedicated tmux server {socket}: {}",
        command_failure(&output)
    ))
}

fn probe_tmux_server(socket: &str) -> TmuxServerProbe {
    let output = match bounded_tmux_output(socket, &["list-sessions"]) {
        Ok(output) => output,
        Err(error) => return TmuxServerProbe::Failed(error),
    };
    if output.status.success() {
        return TmuxServerProbe::Present;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let connection_absent = stderr.contains("error connecting to")
        && (stderr.contains("No such file or directory") || stderr.contains("Connection refused"));
    if stderr.contains("no server running on") || connection_absent {
        TmuxServerProbe::Absent
    } else {
        TmuxServerProbe::Failed(command_failure(&output))
    }
}

fn command_failure(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("exit status {}", output.status)
    } else {
        format!("exit status {}: {stderr}", output.status)
    }
}
