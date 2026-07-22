#![allow(dead_code)]

//! End-to-end proof of the real MCP binary, control listener, and tmux path.
//!
//! The control listener runs in a helper process. Closing that process is the
//! explicit listener shutdown boundary, so a failed assertion cannot leave an
//! accept loop or request handler alive in the integration-test process.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use t_hub_lib::control;
use t_hub_protocol::JournalEventType;

const MCP_IO_TIMEOUT: Duration = Duration::from_secs(10);
const PROCESS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const HELPER_READY_TIMEOUT: Duration = Duration::from_secs(10);
const HELPER_LIFETIME: Duration = Duration::from_secs(60);
const FIXTURE_IO_TIMEOUT: Duration = Duration::from_secs(3);
static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

const CONTINUITY_CAPTAIN: &str = "ctcap001";
const CONTINUITY_FOREIGN_CAPTAIN: &str = "frcap001";
const CONTINUITY_CREW: &str = "ctcrew01";
const CONTINUITY_AGENT: &str = "ctagent1";
const CONTINUITY_FOREIGN_CREW: &str = "frcrew01";
const CONTINUITY_CORTANA: &str = "cort0001";
const CONTINUITY_SHIP_ADMIN: &str = "shadm001";
const CONTINUITY_FLEET_ADMIN: &str = "fladm001";
const CONTINUITY_DEAD_CAPTAIN: &str = "dead0001";
const CONTINUITY_DUPLICATE_CAPTAIN: &str = "dupe0001";

const RETIRED_POWDER_TOOLS: [&str; 4] = [
    "append_crew_powder_work_log",
    "read_crew_powder_evidence",
    "review_crew_powder_criterion",
    "complete_crew_powder",
];

#[allow(dead_code)]
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
        Self::spawn_with_home(
            bin,
            handshake_file,
            tmux_socket,
            endpoint,
            session_token,
            None,
        )
    }

    fn spawn_with_home(
        bin: &Path,
        handshake_file: &Path,
        tmux_socket: &str,
        endpoint: Option<(&str, &str)>,
        session_token: Option<&str>,
        home: Option<&Path>,
    ) -> Self {
        Self::spawn_with_environment(
            bin,
            handshake_file,
            tmux_socket,
            endpoint,
            session_token,
            home,
            &[],
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_with_environment(
        bin: &Path,
        handshake_file: &Path,
        tmux_socket: &str,
        endpoint: Option<(&str, &str)>,
        session_token: Option<&str>,
        home: Option<&Path>,
        environment: &[(&str, &str)],
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
        if let Some(home) = home {
            command.env("HOME", home);
        }
        command.envs(environment.iter().copied());
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
    read_token: String,
    tmux_socket: String,
    continuity_fixture: bool,
    lease_ttl_secs: Option<String>,
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
        Self::spawn_with_fixture(tmux_socket, false, None)
    }

    fn spawn_continuity(tmux_socket: &str) -> Self {
        Self::spawn_with_fixture(tmux_socket, true, Some("1"))
    }

    fn spawn_bridge_continuity(tmux_socket: &str) -> Self {
        Self::spawn_with_fixture(tmux_socket, true, Some("90"))
    }

    fn spawn_with_fixture(
        tmux_socket: &str,
        continuity_fixture: bool,
        lease_ttl_secs: Option<&str>,
    ) -> Self {
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
        let child = Self::launch_helper(
            tmux_socket,
            continuity_fixture,
            &handshake_file,
            &stop_file,
            &auth_file,
            &seed_file,
            &seed_ready_file,
            &powder_state_file,
            temp_dir,
            lease_ttl_secs,
        );
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
            read_token: String::new(),
            tmux_socket: tmux_socket.to_string(),
            continuity_fixture,
            lease_ttl_secs: lease_ttl_secs.map(str::to_string),
        };
        process.wait_until_ready();
        process
    }

    #[allow(clippy::too_many_arguments)]
    fn launch_helper(
        tmux_socket: &str,
        continuity_fixture: bool,
        handshake_file: &Path,
        stop_file: &Path,
        auth_file: &Path,
        seed_file: &Path,
        seed_ready_file: &Path,
        powder_state_file: &Path,
        temp_dir: &Path,
        lease_ttl_secs: Option<&str>,
    ) -> Child {
        let mut command = Command::new(std::env::current_exe().expect("current test executable"));
        command
            .args([
                "--exact",
                "mcp_control_helper",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("T_HUB_MCP_CONTROL_HELPER", "1")
            .env("T_HUB_CONTROL_FILE", handshake_file)
            .env("T_HUB_MCP_CONTROL_STOP_FILE", stop_file)
            .env("T_HUB_MCP_CONTROL_AUTH_FILE", auth_file)
            .env("T_HUB_MCP_CONTROL_SEED_FILE", seed_file)
            .env("T_HUB_MCP_CONTROL_SEED_READY_FILE", seed_ready_file)
            .env("T_HUB_MCP_POWDER_STATE_FILE", powder_state_file)
            .env("T_HUB_TMUX_SOCKET", tmux_socket)
            .env("T_HUB_INBOX_DIR", temp_dir.join("inbox"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        if continuity_fixture {
            command
                .env("T_HUB_MCP_CONTINUITY_FIXTURE", "1")
                .env("T_HUB_MCP_CONTINUITY_DIR", temp_dir);
        }
        if let Some(lease_ttl_secs) = lease_ttl_secs {
            command.env("T_HUB_CONTROL_LEASE_TTL_SECS", lease_ttl_secs);
        }
        command.spawn().expect("spawn control helper process")
    }

    fn wait_until_ready(&mut self) {
        let deadline = Instant::now() + HELPER_READY_TIMEOUT;
        loop {
            if let Some(auth) = fs::read(&self.auth_file)
                .ok()
                .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
            {
                self.addr = auth["addr"].as_str().expect("helper addr").to_string();
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

    fn restart(&mut self) -> Result<(), String> {
        fs::write(&self.stop_file, b"stop\n")
            .map_err(|error| format!("signal control helper restart: {error}"))?;
        if let Some(child) = self.child.as_mut() {
            stop_child(child)?;
        }
        self.child.take();
        let old_addr = self
            .addr
            .parse::<SocketAddr>()
            .map_err(|error| format!("parse control helper address before restart: {error}"))?;
        if TcpStream::connect_timeout(&old_addr, Duration::from_millis(100)).is_ok() {
            return Err("old control listener remained reachable across restart".into());
        }
        for path in [&self.stop_file, &self.auth_file] {
            if path.exists() {
                fs::remove_file(path).map_err(|error| {
                    format!("remove restart marker '{}': {error}", path.display())
                })?;
            }
        }
        self.child = Some(Self::launch_helper(
            &self.tmux_socket,
            self.continuity_fixture,
            &self.handshake_file,
            &self.stop_file,
            &self.auth_file,
            &self.seed_file,
            &self.seed_ready_file,
            &self.powder_state_file,
            &self.temp_dir,
            self.lease_ttl_secs.as_deref(),
        ));
        self.wait_until_ready();
        Ok(())
    }

    fn fixture_auth(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.auth_file).expect("read helper auth fixture"))
            .expect("parse helper auth fixture")
    }
}

impl Drop for ControlProc {
    fn drop(&mut self) {
        if self.shutdown().is_err() {
            std::process::abort();
        }
    }
}

struct BridgeWedgeProxy {
    addr: SocketAddr,
    wedged: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl BridgeWedgeProxy {
    fn start(target: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind bridge wedge proxy");
        listener
            .set_nonblocking(true)
            .expect("make bridge wedge proxy nonblocking");
        let addr = listener.local_addr().expect("bridge wedge proxy address");
        let wedged = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_wedged = Arc::clone(&wedged);
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut client, _)) if thread_wedged.load(Ordering::Acquire) => {
                        let _ = client.set_read_timeout(Some(FIXTURE_IO_TIMEOUT));
                        let mut request = Vec::new();
                        let _ = BufReader::new(&mut client).read_until(b'\n', &mut request);
                        while !thread_stop.load(Ordering::Acquire) {
                            thread::sleep(Duration::from_millis(10));
                        }
                    }
                    Ok((client, _)) => forward_control_connection(client, target),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            wedged,
            stop,
            thread: Some(thread),
        }
    }

    fn wedge(&self) {
        self.wedged.store(true, Ordering::Release);
    }
}

fn forward_control_connection(mut client: TcpStream, target: SocketAddr) {
    let _ = client.set_read_timeout(Some(FIXTURE_IO_TIMEOUT));
    let _ = client.set_write_timeout(Some(FIXTURE_IO_TIMEOUT));
    let Ok(mut backend) = TcpStream::connect_timeout(&target, FIXTURE_IO_TIMEOUT) else {
        return;
    };
    let _ = backend.set_read_timeout(Some(FIXTURE_IO_TIMEOUT));
    let _ = backend.set_write_timeout(Some(FIXTURE_IO_TIMEOUT));
    let mut request = Vec::new();
    if BufReader::new(&mut client)
        .read_until(b'\n', &mut request)
        .is_err()
    {
        return;
    }
    if backend.write_all(&request).is_err() {
        return;
    }
    let mut response = Vec::new();
    if BufReader::new(&mut backend)
        .read_until(b'\n', &mut response)
        .is_ok()
    {
        let _ = client.write_all(&response);
        let _ = client.flush();
    }
}

impl Drop for BridgeWedgeProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(100));
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join bridge wedge proxy");
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
    criterion_review_posts: Vec<Value>,
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
            ("GET", "/readyz") => (200, json!({"ok": true, "schema_version": 18})),
            ("GET", "/api/v1/routes") => (200, powder_capability_routes()),
            ("GET", "/api/v1/cards/mcp-owned-card") => (200, powder_card_evidence(&state)),
            ("GET", "/api/v1/runs/mcp-owned-run") => (200, powder_run_evidence(&state)),
            ("POST", "/api/v1/cards/mcp-owned-card/runs/mcp-owned-run/work-log") => {
                state.append_posts.push(body.clone());
                (
                    200,
                    powder_operation_outcome(
                        "work_log_append",
                        &body,
                        json!({
                            "schema_version": "powder.work_log_entry.v1",
                            "id": "work-log-mcp",
                            "card_id": "mcp-owned-card",
                            "actor": body["agent"],
                            "agent": body["agent"],
                            "model": body["model"],
                            "reasoning": body["reasoning"],
                            "harness": body["harness"],
                            "run_id": "mcp-owned-run",
                            "body": body["body"],
                            "created_at": 11,
                            "updated_at": 11
                        }),
                    ),
                )
            }
            ("POST", "/api/v1/cards/mcp-owned-card/runs/mcp-owned-run/criteria/review") => {
                state.criterion_review_posts.push(body.clone());
                (
                    200,
                    powder_operation_outcome(
                        "criterion_review",
                        &body,
                        json!({
                            "id": "review-mcp",
                            "operation_id": body["operation_id"],
                            "card_id": "mcp-owned-card",
                            "run_id": "mcp-owned-run",
                            "criterion_index": body["criterion"],
                            "criterion_id": body["criterion_id"],
                            "criterion_text": "MCP dispatcher criterion",
                            "decision": body["decision"],
                            "reviewer": "mcp-captain",
                            "reviewer_identity": "actor-t-hub",
                            "proof": body["proof"],
                            "supersedes_review_id": "review-initial",
                            "created_at": 124
                        }),
                    ),
                )
            }
            ("POST", "/api/v1/cards/mcp-owned-card/runs/mcp-owned-run/complete") => {
                state.completion_posts.push(body.clone());
                state.completed = true;
                state.proof = body["proof"].as_str().map(str::to_string);
                (
                    200,
                    powder_operation_outcome(
                        "completion",
                        &body,
                        json!({
                            "schema_version": "powder.run_bound_completion.v1",
                            "card_id": "mcp-owned-card",
                            "run_id": "mcp-owned-run",
                            "operation_id": body["operation_id"],
                            "status": "done",
                            "proof": body["proof"],
                            "criterion_proofs": body["criterion_proofs"],
                            "updated_at": 126,
                            "audit_event_id": "audit-mcp-operation"
                        }),
                    ),
                )
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

fn powder_capability_routes() -> Value {
    json!([
        {
            "method": "POST",
            "path": "/api/v1/cards/{id}/runs/{run_id}/work-log",
            "body_shape": "{\"operation_id\":\"stable-id\",\"agent\":\"...\",\"body\":\"...\"}"
        },
        {
            "method": "POST",
            "path": "/api/v1/cards/{id}/runs/{run_id}/criteria/review",
            "body_shape": "{\"operation_id\":\"stable-id\",\"criterion\":0,\"criterion_id\":\"...\",\"decision\":\"approved\"}"
        },
        {
            "method": "POST",
            "path": "/api/v1/cards/{id}/runs/{run_id}/complete",
            "body_shape": "{\"operation_id\":\"stable-id\",\"proof\":null,\"criterion_proofs\":null}"
        },
        {
            "method": "GET",
            "path": "/api/v1/operations/{id}",
            "body_shape": null
        }
    ])
}

fn powder_operation_outcome(kind: &str, body: &Value, result: Value) -> Value {
    json!({
        "schema_version": "powder.operation_status.v1",
        "operation_id": body["operation_id"],
        "state": "succeeded",
        "request_digest": powder_operation_request_digest(kind, body),
        "kind": kind,
        "target_card_id": "mcp-owned-card",
        "expected_run_id": "mcp-owned-run",
        "result": result,
        "failure": null,
        "audit_event_id": "audit-mcp-operation",
        "created_at": 125,
        "updated_at": 126,
        "expires_at": 600
    })
}

fn powder_operation_request_digest(kind: &str, body: &Value) -> String {
    let criterion_index = body["criterion"].as_u64().map(|value| value.to_string());
    let criterion_proofs = body["criterion_proofs"].as_array().map(|proofs| {
        serde_json::to_string(
            &proofs
                .iter()
                .map(|proof| {
                    json!({
                        "criterion": proof["criterion"],
                        "url": proof["url"],
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap()
    });
    let payload = match kind {
        "work_log_append" => vec![
            ("agent", body["agent"].as_str()),
            ("model", body["model"].as_str()),
            ("reasoning", body["reasoning"].as_str()),
            ("harness", body["harness"].as_str()),
            ("body", body["body"].as_str()),
        ],
        "criterion_review" => vec![
            ("criterion_index", criterion_index.as_deref()),
            ("criterion_id", body["criterion_id"].as_str()),
            ("decision", body["decision"].as_str()),
            ("proof", body["proof"].as_str()),
        ],
        "completion" => vec![
            ("proof", body["proof"].as_str()),
            ("criterion_proofs", criterion_proofs.as_deref()),
        ],
        _ => panic!("unsupported Powder operation kind"),
    };
    let mut hasher = Sha256::new();
    for (name, value) in [
        ("schema", Some("powder.operation_request.v1")),
        ("kind", Some(kind)),
        ("target_type", Some("card")),
        ("target", Some("mcp-owned-card")),
        ("authority", Some("actor-t-hub")),
        ("expected_run", Some("mcp-owned-run")),
    ]
    .into_iter()
    .chain(payload)
    {
        hasher.update(u32::try_from(name.len()).unwrap().to_be_bytes());
        hasher.update(name.as_bytes());
        match value {
            Some(value) => {
                hasher.update(u32::try_from(value.len()).unwrap().to_be_bytes());
                hasher.update(value.as_bytes());
            }
            None => hasher.update(u32::MAX.to_be_bytes()),
        }
    }
    format!("sha256:{:x}", hasher.finalize())
}

const MCP_CRITERION_ID: &str =
    "powder.criterion.v1:sha256:1977e92d087253639c224379af040d1b1b59714c0062b8fe6ab134abee4eaf5c:0";

fn powder_criterion(state: &PowderFixtureState) -> Value {
    let proof_links = state
        .completion_posts
        .last()
        .and_then(|post| post["criterion_proofs"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|proof| proof["url"].as_str())
        .map(|url| {
            json!({
                "url": url,
                "actor": "mcp-captain",
                "created_at": 126
            })
        })
        .collect::<Vec<_>>();
    json!({
        "text": "MCP dispatcher criterion",
        "checked_by": "mcp-captain",
        "checked_at": 123,
        "proof_links": proof_links
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
        "criteria": [powder_criterion(state)]
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
                "run_id": "mcp-owned-run",
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
        "work_log_total": state.append_posts.len(),
        "current_run_criteria": if state.completed {
            Vec::<Value>::new()
        } else {
            vec![powder_run_criterion()]
        }
    })
}

fn powder_run_evidence(state: &PowderFixtureState) -> Value {
    json!({
        "run": powder_run(state),
        "card": powder_card(state),
        "activities": [],
        "activities_total": 0,
        "links": [],
        "links_total": 0,
        "criteria": [powder_run_criterion()]
    })
}

fn powder_run_criterion() -> Value {
    json!({
        "criterion_index": 0,
        "criterion_id": MCP_CRITERION_ID,
        "criterion_text": "MCP dispatcher criterion",
        "review": {
            "id": "review-initial",
            "operation_id": "review-operation-initial",
            "card_id": "mcp-owned-card",
            "run_id": "mcp-owned-run",
            "criterion_index": 0,
            "criterion_id": MCP_CRITERION_ID,
            "criterion_text": "MCP dispatcher criterion",
            "decision": "approved",
            "reviewer": "mcp-captain",
            "reviewer_identity": "actor-t-hub",
            "proof": "initial review proof",
            "supersedes_review_id": null,
            "created_at": 123
        }
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
    let continuity_fixture = std::env::var("T_HUB_MCP_CONTINUITY_FIXTURE").as_deref() == Ok("1");
    let (registry, identities, delegated_admin, fixture_sessions) = if continuity_fixture {
        continuity_control_fixture()
    } else {
        (
            Arc::new(control::CaptainsRegistry::new()),
            Arc::new(control::IdentityStore::ephemeral()),
            Arc::new(t_hub_lib::delegated_admin::DelegatedAdminStore::ephemeral()),
            Value::Null,
        )
    };
    let powder_state_file =
        PathBuf::from(std::env::var_os("T_HUB_MCP_POWDER_STATE_FILE").expect("Powder state file"));
    let mut powder_server = PowderFixtureServer::start(powder_state_file);
    let profile_file = powder_server_profile_file(powder_server.addr);
    std::env::set_var("T_HUB_POWDER_PROFILES_FILE", &profile_file);
    let context =
        control::ControlContext::with_shared_supervisor(status, supervisor, token.clone())
            .with_read_token(read_token.clone())
            .with_captains_registry(registry.clone())
            .with_identity_store(identities)
            .with_delegated_admin(delegated_admin)
            .with_durable_inbox()
            .with_apply_sink(Arc::new(NoopApplySink));
    let handshake = control::start(context).expect("control listener starts");
    assert_eq!(handshake.local_control_token, token);
    fs::write(
        std::env::var_os("T_HUB_MCP_CONTROL_AUTH_FILE").expect("control helper auth file"),
        serde_json::to_vec(&json!({
            "addr": handshake.addr,
            "readToken": handshake.read_token,
            "sessions": fixture_sessions,
        }))
        .expect("serialize control helper auth"),
    )
    .expect("write control helper auth");
    protect_fixture_file(&PathBuf::from(
        std::env::var_os("T_HUB_MCP_CONTROL_AUTH_FILE").expect("control helper auth file"),
    ));
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

fn protect_fixture_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("protect credential-bearing test fixture");
    }
}

fn continuity_control_fixture() -> (
    Arc<control::CaptainsRegistry>,
    Arc<control::IdentityStore>,
    Arc<t_hub_lib::delegated_admin::DelegatedAdminStore>,
    Value,
) {
    let root = PathBuf::from(
        std::env::var_os("T_HUB_MCP_CONTINUITY_DIR").expect("continuity fixture directory"),
    );
    let captains_path = root.join("captains.json");
    let identities_path = root.join("identities.json");
    let grants_path = root.join("delegated-admin.json");
    let credentials_path = root.join("continuity-credentials.json");
    if credentials_path.exists() {
        return (
            Arc::new(control::CaptainsRegistry::load(captains_path)),
            Arc::new(control::IdentityStore::load(identities_path)),
            Arc::new(
                t_hub_lib::delegated_admin::DelegatedAdminStore::load(grants_path)
                    .expect("reload delegated admin fixture"),
            ),
            serde_json::from_slice(
                &fs::read(credentials_path).expect("read continuity credentials"),
            )
            .expect("parse continuity credentials"),
        );
    }

    let registry = control::CaptainsRegistry::load(captains_path.clone());
    for (project_id, root_path) in [
        ("continuity-project", "/tmp/continuity-project"),
        (
            "continuity-foreign-project",
            "/tmp/continuity-foreign-project",
        ),
        ("continuity-dead-project", "/tmp/continuity-dead-project"),
        (
            "continuity-duplicate-project",
            "/tmp/continuity-duplicate-project",
        ),
    ] {
        registry
            .upsert_project(control::ProjectRecord {
                project_id: project_id.into(),
                name: project_id.into(),
                repo_root: root_path.into(),
                root_path: Some(root_path.into()),
                vcs_capability: Some("none".into()),
                git_main_root: None,
                remote_url: None,
                default_branch: Some("main".into()),
                powder: None,
                created_at: 1,
                updated_at: 1,
            })
            .expect("seed continuity project");
    }
    for (terminal, ship, project) in [
        (CONTINUITY_CAPTAIN, "continuity-ship", "continuity-project"),
        (
            CONTINUITY_FOREIGN_CAPTAIN,
            "continuity-foreign-ship",
            "continuity-foreign-project",
        ),
        (
            CONTINUITY_DEAD_CAPTAIN,
            "continuity-dead-ship",
            "continuity-dead-project",
        ),
        (
            CONTINUITY_DUPLICATE_CAPTAIN,
            "continuity-duplicate-ship",
            "continuity-duplicate-project",
        ),
    ] {
        registry
            .claim_provider(
                terminal,
                Some(ship),
                control::FleetRole::Captain,
                Some("codex"),
                None,
                vec![],
                &|_| false,
                &|_| panic!("fresh fixture claim does not inspect Crew"),
            )
            .expect("seed continuity Captain");
        registry
            .bind_ship_context(ship, project, "Continuity E2E", "codex")
            .expect("bind continuity Project");
    }
    for crew in [
        CONTINUITY_CREW,
        CONTINUITY_SHIP_ADMIN,
        CONTINUITY_FLEET_ADMIN,
    ] {
        registry
            .record_crew(CONTINUITY_CAPTAIN, crew)
            .expect("seed owned continuity Crew");
    }
    registry
        .record_crew(CONTINUITY_FOREIGN_CAPTAIN, CONTINUITY_FOREIGN_CREW)
        .expect("seed foreign continuity Crew");
    registry
        .claim_provider(
            CONTINUITY_CORTANA,
            None,
            control::FleetRole::Cortana,
            Some("codex"),
            None,
            vec![],
            &|_| false,
            &|_| panic!("fresh Cortana claim does not inspect Crew"),
        )
        .expect("seed continuity Cortana");

    let identities = control::IdentityStore::load(identities_path.clone());
    let captain = identities
        .mint_and_bind(
            control::SessionIdentityRole::Captain,
            Some("continuity-ship".into()),
            CONTINUITY_CAPTAIN,
        )
        .expect("mint continuity Captain");
    let foreign = identities
        .mint_and_bind(
            control::SessionIdentityRole::Captain,
            Some("continuity-foreign-ship".into()),
            CONTINUITY_FOREIGN_CAPTAIN,
        )
        .expect("mint foreign Captain");
    let crew = identities
        .mint_and_bind(
            control::SessionIdentityRole::Crew,
            Some("continuity-ship".into()),
            CONTINUITY_CREW,
        )
        .expect("mint continuity Crew");
    let foreign_crew = identities
        .mint_and_bind(
            control::SessionIdentityRole::Crew,
            Some("continuity-foreign-ship".into()),
            CONTINUITY_FOREIGN_CREW,
        )
        .expect("mint foreign Crew");
    let cortana = identities
        .mint_and_bind(
            control::SessionIdentityRole::Cortana,
            None,
            CONTINUITY_CORTANA,
        )
        .expect("mint continuity Cortana");
    let ship_admin = identities
        .mint_and_bind(
            control::SessionIdentityRole::Crew,
            Some("continuity-ship".into()),
            CONTINUITY_SHIP_ADMIN,
        )
        .expect("mint Ship Admin Crew");
    let fleet_admin = identities
        .mint_and_bind(
            control::SessionIdentityRole::Crew,
            Some("continuity-ship".into()),
            CONTINUITY_FLEET_ADMIN,
        )
        .expect("mint Fleet Admin Crew");
    let dead = identities
        .mint_and_bind(
            control::SessionIdentityRole::Captain,
            Some("continuity-dead-ship".into()),
            CONTINUITY_DEAD_CAPTAIN,
        )
        .expect("mint dead Captain fixture");
    let duplicate = identities
        .mint_and_bind(
            control::SessionIdentityRole::Captain,
            Some("continuity-duplicate-ship".into()),
            CONTINUITY_DUPLICATE_CAPTAIN,
        )
        .expect("mint duplicate Captain fixture");
    identities
        .mint_and_bind(
            control::SessionIdentityRole::Captain,
            Some("continuity-duplicate-ship".into()),
            CONTINUITY_DUPLICATE_CAPTAIN,
        )
        .expect("mint second duplicate identity");
    let revoked = identities
        .mint_and_bind(
            control::SessionIdentityRole::Captain,
            Some("continuity-ship".into()),
            "continuity-revoked",
        )
        .expect("mint revoked fixture");
    identities
        .revoke(&revoked.id)
        .expect("revoke fixture identity");
    let removed = identities
        .mint_and_bind(
            control::SessionIdentityRole::Captain,
            Some("continuity-ship".into()),
            "continuity-removed",
        )
        .expect("mint removed fixture");
    identities
        .retire(&removed.id)
        .expect("retire fixture identity");

    let mut snapshot = registry.snapshot();
    snapshot.agent_sessions.push(
        serde_json::from_value(json!({
            "agentSessionId": CONTINUITY_AGENT,
            "captainSessionId": CONTINUITY_CAPTAIN,
            "projectId": "continuity-project",
            "assignment": "Original continuity Assignment",
            "directory": "/tmp/continuity-project",
            "harness": "codex",
            "provider": "codex",
            "runtimeState": "starting",
            "workStage": "assigned",
            "admissionPurpose": "ordinary",
            "createdAt": 1,
            "updatedAt": 1
        }))
        .expect("seed continuity agent session"),
    );
    snapshot.cortana = control::CortanaDurableIdentity {
        identity_id: Some(cortana.id.clone()),
        generation: 1,
        terminal_id: Some(CONTINUITY_CORTANA.into()),
        harness: Some("codex".into()),
        provider_session_id: None,
        conversation_id: None,
        checkpoint: None,
        owner: Some(control::CortanaManagedOwnerToken {
            version: 2,
            unit_name: format!("t-hub-{}.scope", "a".repeat(32)),
            invocation_id: "b".repeat(32),
            cgroup_path: format!(
                "/user.slice/user-1000.slice/user@1000.service/app.slice/t-hub-{}.scope",
                "a".repeat(32)
            ),
            cgroup_inode: 1,
            launcher_pid: 100,
            launcher_start_ticks: 200,
            launch_nonce: "a".repeat(32),
            tools: control::CortanaManagedSystemTools {
                python: control::CortanaExecutableIdentity {
                    path: "/usr/bin/python3.12".into(),
                    device: 1,
                    inode: 3,
                },
                systemctl: control::CortanaExecutableIdentity {
                    path: "/usr/bin/systemctl".into(),
                    device: 1,
                    inode: 1,
                },
                systemd_run: control::CortanaExecutableIdentity {
                    path: "/usr/bin/systemd-run".into(),
                    device: 1,
                    inode: 2,
                },
            },
            tmux: control::CortanaOrphanEffectIdentity {
                tmux_session_id: 1,
                tmux_session_created: 1,
                tmux_window_id: 1,
                tmux_pane_id: 1,
                pane_pid: 100,
                pane_start_ticks: 200,
                pane_process_group_id: 100,
                pane_process_session_id: 100,
                foreground_pid: 100,
                foreground_start_ticks: 200,
                foreground_process_group_id: 100,
                foreground_process_session_id: 100,
            },
        }),
        managed_launch: None,
        legacy_quarantine: None,
        legacy_orphan_provenance: None,
        recovery: control::CortanaRecoveryState::Healthy {
            operation_id: "continuity-e2e".into(),
            verified_at: 1,
        },
    };
    fs::write(
        &captains_path,
        serde_json::to_vec_pretty(&snapshot).expect("serialize continuity Fleet"),
    )
    .expect("persist authoritative Cortana fixture");

    let sessions = json!({
        "captain": captain.secret,
        "foreignCaptain": foreign.secret,
        "crew": crew.secret,
        "foreignCrew": foreign_crew.secret,
        "cortana": cortana.secret,
        "shipAdmin": ship_admin.secret,
        "fleetAdmin": fleet_admin.secret,
        "deadCaptain": dead.secret,
        "duplicateCaptain": duplicate.secret,
        "revoked": revoked.secret,
        "removed": removed.secret,
    });
    fs::write(
        &credentials_path,
        serde_json::to_vec(&sessions).expect("serialize continuity credentials"),
    )
    .expect("persist isolated continuity test credentials");
    protect_fixture_file(&credentials_path);

    (
        Arc::new(control::CaptainsRegistry::load(captains_path)),
        Arc::new(control::IdentityStore::load(identities_path)),
        Arc::new(
            t_hub_lib::delegated_admin::DelegatedAdminStore::load(grants_path)
                .expect("load continuity delegated admin store"),
        ),
        sessions,
    )
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
                    "operationIdentity": "actor-t-hub",
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
                root_path: Some(format!("/tmp/mcp-{fixture}-project")),
                vcs_capability: Some("none".into()),
                git_main_root: None,
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
            "agent": "powder-agent",
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

fn raw_control_call(addr: &str, token: &str, session: &str, command: &str, args: Value) -> Value {
    let mut stream = TcpStream::connect(addr).expect("connect raw control fixture");
    stream
        .set_read_timeout(Some(FIXTURE_IO_TIMEOUT))
        .expect("set raw control read timeout");
    let request = json!({
        "token": token,
        "session": session,
        "command": command,
        "args": args,
    });
    stream
        .write_all(&serde_json::to_vec(&request).expect("serialize raw control request"))
        .and_then(|_| stream.write_all(b"\n"))
        .expect("write raw control request");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read raw control response");
    serde_json::from_str(response.trim()).expect("parse raw control response")
}

fn current_handshake(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read current control handshake"))
        .expect("parse current control handshake")
}

#[cfg(unix)]
fn install_test_powershell_bridge(directory: &Path) -> (PathBuf, PathBuf) {
    let bin_dir = directory.join("bridge-bin");
    let capture_file = directory.join("bridge-capture.json");
    fs::create_dir_all(&bin_dir).expect("create test PowerShell bridge directory");
    let bridge = bin_dir.join("powershell.exe");
    fs::write(
        &bridge,
        "#!/bin/sh\nexec \"$T_HUB_TEST_BRIDGE_HELPER_EXE\" --exact \
         mcp_powershell_bridge_helper --ignored --nocapture\n",
    )
    .expect("write test PowerShell bridge");
    let mut permissions = fs::metadata(&bridge)
        .expect("stat test PowerShell bridge")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&bridge, permissions).expect("make test PowerShell bridge executable");
    (bin_dir, capture_file)
}

#[test]
#[ignore = "PowerShell-shaped bridge subprocess helper"]
fn mcp_powershell_bridge_helper() {
    if std::env::var("T_HUB_TEST_POWERSHELL_BRIDGE").as_deref() != Ok("1") {
        return;
    }
    let wire = std::env::var("THUB_REBIND_REQUEST").expect("bridge request");
    let request: Value = serde_json::from_str(&wire).expect("parse bridge request");
    let target = std::env::var("T_HUB_TEST_BRIDGE_TARGET")
        .expect("bridge target")
        .parse::<SocketAddr>()
        .expect("parse bridge target");
    let mut stream =
        TcpStream::connect_timeout(&target, FIXTURE_IO_TIMEOUT).expect("connect bridge target");
    stream
        .set_read_timeout(Some(FIXTURE_IO_TIMEOUT))
        .expect("set bridge read timeout");
    stream
        .write_all(wire.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .expect("write bridge request");
    let mut response_line = String::new();
    BufReader::new(stream)
        .read_line(&mut response_line)
        .expect("read bridge response");
    let response: Value =
        serde_json::from_str(response_line.trim_end()).expect("parse bridge response");
    let token = request["token"].as_str().unwrap_or_default();
    let session = request["session"].as_str().unwrap_or_default();
    let capture = json!({
        "command": request["command"],
        "hasToken": !token.is_empty(),
        "hasSession": !session.is_empty(),
        "tokenDigest": format!("{:x}", Sha256::digest(token.as_bytes())),
        "sessionDigest": format!("{:x}", Sha256::digest(session.as_bytes())),
        "responseOk": response["ok"],
        "responseResult": response["result"],
    });
    fs::write(
        std::env::var_os("T_HUB_TEST_BRIDGE_CAPTURE").expect("bridge capture path"),
        serde_json::to_vec(&capture).expect("serialize bridge capture"),
    )
    .expect("write bridge capture");
    println!("{}", response_line.trim_end());
}

fn add_tmux_fixture_session(socket: &str, terminal_id: &str) {
    let target = format!("th_{terminal_id}");
    let output = bounded_tmux_output(socket, &["new-session", "-d", "-s", &target, "sleep 300"])
        .expect("start continuity tmux session");
    assert!(
        output.status.success(),
        "failed to start {target}: {}",
        command_failure(&output)
    );
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

// Historical integration fixture retained for reference only. Retired Powder
// operations must never be exercised by the active MCP E2E suite.
#[cfg(any())]
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
        json!({
            "operationId": "mcp-anonymous-work-log",
            "message": "anonymous must not append"
        }),
    );
    assert!(tool_error_text(&anonymous_append).contains(
        "unauthorized: 'append_crew_powder_work_log' requires the control capability (this token is read-only)"
    ));
    let anonymous_complete = call_tool(
        &anonymous,
        103,
        "complete_crew_powder",
        json!({
            "crewSessionId": "missing",
            "operationId": "mcp-anonymous-completion",
            "proof": "anonymous must not complete",
            "criterionProofs": []
        }),
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
        json!({
            "operationId": "mcp-work-log-operation",
            "message": "real MCP append sentinel"
        }),
    );
    assert_eq!(append["result"]["isError"], false, "{append}");
    let append_data = tool_structured(&append);
    assert_eq!(append_data["accepted"], "append_crew_powder_work_log");
    assert_eq!(append_data["crewSessionId"], owned_crew);
    assert_eq!(append_data["cardId"], "mcp-owned-card");
    assert_eq!(append_data["runId"], "mcp-owned-run");
    assert_eq!(append_data["messageBytes"], 24);
    assert_eq!(append_data["mutationState"], "committed");
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
    assert_eq!(
        crew_read_data["card"]["workLog"][0]["agent"],
        "powder-agent"
    );
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
        json!({
            "crewSessionId": owned_crew,
            "operationId": "mcp-read-only-completion",
            "proof": "read token must not complete",
            "criterionProofs": []
        }),
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
        json!({
            "crewSessionId": owned_crew,
            "operationId": "mcp-foreign-completion",
            "proof": "foreign must not complete",
            "criterionProofs": []
        }),
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
        json!({
            "crewSessionId": foreign_crew,
            "operationId": "mcp-wrong-ship-completion",
            "proof": "wrong ship",
            "criterionProofs": []
        }),
    );
    assert!(tool_error_text(&captain_foreign_complete).contains("requires the same-ship Captain"));
    let criterion_review = call_tool(
        &owned_captain_mcp,
        164,
        "review_crew_powder_criterion",
        json!({
            "crewSessionId": owned_crew,
            "operationId": "mcp-criterion-operation",
            "criterion": 0,
            "criterionId": MCP_CRITERION_ID,
            "decision": "approved",
            "proof": "real MCP criterion sentinel",
            "expectedReviewerIdentity": "actor-mcp-owned-captain"
        }),
    );
    let criterion_review_data = tool_structured(&criterion_review);
    assert_eq!(
        criterion_review_data["accepted"],
        "review_crew_powder_criterion"
    );
    assert_eq!(criterion_review_data["runId"], "mcp-owned-run");
    assert_eq!(criterion_review_data["mutationState"], "committed");
    let completion = call_tool(
        &owned_captain_mcp,
        165,
        "complete_crew_powder",
        json!({
            "crewSessionId": owned_crew,
            "operationId": "mcp-completion-operation",
            "proof": "real MCP completion sentinel",
            "criterionProofs": [{
                "criterion": 0,
                "criterionId": MCP_CRITERION_ID,
                "url": "https://example.test/mcp-completion-proof"
            }]
        }),
    );
    if completion["result"]["isError"] != false {
        let fixture = fs::read_to_string(&control.powder_state_file)
            .unwrap_or_else(|error| format!("fixture state unavailable: {error}"));
        panic!("completion failed: {completion}; Powder fixture: {fixture}");
    }
    let completion_data = tool_structured(&completion);
    assert_eq!(completion_data["accepted"], "complete_crew_powder");
    assert_eq!(completion_data["crewSessionId"], owned_crew);
    assert_eq!(completion_data["cardId"], "mcp-owned-card");
    assert_eq!(completion_data["runId"], "mcp-owned-run");
    assert_eq!(completion_data["cardStatus"], "done");
    assert_eq!(completion_data["runState"], "complete");
    assert_eq!(completion_data["mutationState"], "committed");
    owned_captain_mcp
        .shutdown()
        .expect("stop owned Captain MCP");

    let fixture: Value = serde_json::from_slice(
        &fs::read(&control.powder_state_file).expect("read Powder fixture sentinel"),
    )
    .expect("parse Powder fixture sentinel");
    assert_eq!(fixture["appendPosts"].as_array().unwrap().len(), 1);
    assert_eq!(fixture["criterionReviewPosts"].as_array().unwrap().len(), 1);
    assert_eq!(fixture["completionPosts"].as_array().unwrap().len(), 1);
    assert_eq!(fixture["appendPosts"][0]["agent"], "powder-agent");
    assert_eq!(
        fixture["appendPosts"][0]["operation_id"],
        "mcp-work-log-operation"
    );
    assert!(fixture["appendPosts"][0].get("run_id").is_none());
    assert_eq!(
        fixture["appendPosts"][0]["body"],
        "real MCP append sentinel"
    );
    assert_eq!(
        fixture["criterionReviewPosts"][0]["operation_id"],
        "mcp-criterion-operation"
    );
    assert_eq!(
        fixture["completionPosts"][0]["proof"],
        "real MCP completion sentinel"
    );
    assert_eq!(
        fixture["completionPosts"][0]["operation_id"],
        "mcp-completion-operation"
    );
    assert_eq!(fixture["completed"], true);
    assert!(fixture["requestPaths"]
        .as_array()
        .unwrap()
        .iter()
        .all(|path| !path.as_str().unwrap().contains("mcp-foreign")));
    for required_get in [
        "GET /readyz",
        "GET /api/v1/routes",
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

#[cfg(unix)]
#[test]
fn captain_bridge_wedge_recovers_through_production_mcp_process_path() {
    let bin = locate_mcp_binary();
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let tmux_socket = format!("t-hub-bridge-{}-{test_id}", std::process::id());
    let mut tmux_guard =
        TmuxServerGuard::start(tmux_socket.clone(), format!("th_{CONTINUITY_CAPTAIN}"))
            .expect("start bridge tmux server");
    let mut control = ControlProc::spawn_bridge_continuity(&tmux_socket);
    let session = control.fixture_auth()["sessions"]["captain"]
        .as_str()
        .expect("bridge Captain session")
        .to_string();
    let backend_addr = control
        .addr
        .parse::<SocketAddr>()
        .expect("bridge backend address");
    let proxy = BridgeWedgeProxy::start(backend_addr);

    let mut handshake = current_handshake(&control.handshake_file);
    handshake["addr"] = Value::String(proxy.addr.to_string());
    fs::write(
        &control.handshake_file,
        serde_json::to_vec(&handshake).expect("serialize proxied control handshake"),
    )
    .expect("publish proxied control handshake");

    let (bridge_bin, capture_file) = install_test_powershell_bridge(&control.temp_dir);
    let path = format!(
        "{}:{}",
        bridge_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let backend = backend_addr.to_string();
    let capture = capture_file.to_string_lossy().into_owned();
    let bridge_helper_exe = std::env::current_exe()
        .expect("bridge helper executable")
        .to_string_lossy()
        .into_owned();
    let environment = [
        ("PATH", path.as_str()),
        ("WSL_DISTRO_NAME", "T-Hub-Bridge-E2E"),
        ("T_HUB_TEST_POWERSHELL_BRIDGE", "1"),
        ("T_HUB_TEST_BRIDGE_HELPER_EXE", bridge_helper_exe.as_str()),
        ("T_HUB_TEST_BRIDGE_TARGET", backend.as_str()),
        ("T_HUB_TEST_BRIDGE_CAPTURE", capture.as_str()),
    ];
    let mut captain = McpProc::spawn_with_environment(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        None,
        Some(&session),
        None,
        &environment,
    );
    initialize_mcp(&captain, 300);

    let before_wedge = call_tool(&captain, 301, "my_capability", Value::Null);
    assert_eq!(before_wedge["result"]["isError"], false, "{before_wedge}");
    assert_eq!(
        before_wedge["result"]["structuredContent"]["capability"],
        "control"
    );

    proxy.wedge();
    let recovered = call_tool(
        &captain,
        302,
        "new_tab",
        json!({"name": "Mutate after bridge recovery"}),
    );

    wait_for_path(&capture_file, "PowerShell bridge capture");
    let capture_body = fs::read_to_string(&capture_file).expect("read PowerShell bridge capture");
    let bridge_capture: Value =
        serde_json::from_str(&capture_body).expect("parse PowerShell bridge capture");
    let session_digest = format!("{:x}", Sha256::digest(session.as_bytes()));
    let read_token_digest = format!("{:x}", Sha256::digest(control.read_token.as_bytes()));
    assert_eq!(bridge_capture["command"], "rebind_control");
    assert_eq!(bridge_capture["hasToken"], true);
    assert_eq!(bridge_capture["hasSession"], true);
    assert_eq!(bridge_capture["sessionDigest"], session_digest);
    assert_ne!(bridge_capture["tokenDigest"], read_token_digest);
    assert_eq!(bridge_capture["responseOk"], true, "{bridge_capture}");
    assert_eq!(bridge_capture["responseResult"]["rebound"], true);
    assert_eq!(bridge_capture["responseResult"]["tokensRotated"], false);
    assert!(!capture_body.contains(&session));
    assert!(!capture_body.contains(&control.read_token));
    assert_eq!(recovered["result"]["isError"], false, "{recovered}");

    let healed = current_handshake(&control.handshake_file);
    assert_ne!(healed["addr"], proxy.addr.to_string());
    assert_ne!(healed["addr"], backend_addr.to_string());

    captain.shutdown().expect("stop bridge MCP process");
    drop(proxy);
    tmux_guard.shutdown().expect("stop bridge tmux server");
    control.shutdown().expect("stop bridge control helper");
}

#[test]
fn captain_control_continuity_process_merge_gate() {
    let bin = locate_mcp_binary();
    let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let tmux_socket = format!("t-hub-continuity-{}-{test_id}", std::process::id());
    let mut tmux_guard =
        TmuxServerGuard::start(tmux_socket.clone(), format!("th_{CONTINUITY_CAPTAIN}"))
            .expect("start continuity tmux server");
    for terminal in [
        CONTINUITY_FOREIGN_CAPTAIN,
        CONTINUITY_CREW,
        CONTINUITY_FOREIGN_CREW,
        CONTINUITY_CORTANA,
        CONTINUITY_SHIP_ADMIN,
        CONTINUITY_FLEET_ADMIN,
        CONTINUITY_DUPLICATE_CAPTAIN,
    ] {
        add_tmux_fixture_session(&tmux_socket, terminal);
    }
    let mut control = ControlProc::spawn_continuity(&tmux_socket);
    let first_auth = control.fixture_auth();
    let sessions = &first_auth["sessions"];

    let shadow_home = control.temp_dir.join("wsl-home");
    let shadow_dir = shadow_home.join(".t-hub");
    fs::create_dir_all(&shadow_dir).expect("create stale WSL shadow directory");
    fs::write(
        shadow_dir.join("control.json"),
        serde_json::to_vec(&json!({
            "addr": "127.0.0.1:9",
            "token": "stale-shadow-token",
            "readToken": "stale-shadow-token",
        }))
        .unwrap(),
    )
    .expect("write stale WSL shadow handshake");

    let mut captain = McpProc::spawn_with_home(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        None,
        sessions["captain"].as_str(),
        Some(&shadow_home),
    );
    initialize_mcp(&captain, 200);
    let catalog = captain.request(json!({ "jsonrpc": "2.0", "id": 199, "method": "tools/list" }));
    let catalog_tools = catalog["result"]["tools"].as_array().unwrap();
    for required in ["dispatch_preflight", "agent_followup"] {
        assert!(
            catalog_tools.iter().any(|tool| tool["name"] == required),
            "continuity tools/list missing {required}"
        );
    }
    let initial_mutation = call_tool(
        &captain,
        201,
        "new_tab",
        json!({"name": "Stable discovery"}),
    );
    assert_eq!(
        initial_mutation["result"]["isError"], false,
        "{initial_mutation}"
    );

    let before_rebind = current_handshake(&control.handshake_file);
    let lease_response = raw_control_call(
        before_rebind["addr"].as_str().unwrap(),
        before_rebind["token"].as_str().unwrap(),
        sessions["captain"].as_str().unwrap(),
        "renew_captain_control_lease",
        Value::Null,
    );
    assert_eq!(lease_response["ok"], true, "lease acquisition failed");
    let scoped_lease = lease_response["result"]["lease"]
        .as_str()
        .expect("scoped lease in memory")
        .to_string();
    let bridge_rebind = raw_control_call(
        before_rebind["addr"].as_str().unwrap(),
        &scoped_lease,
        sessions["captain"].as_str().unwrap(),
        "rebind_control",
        Value::Null,
    );
    assert_eq!(
        bridge_rebind["ok"], true,
        "scoped bridge-shaped rebind failed"
    );
    let after_rebind = current_handshake(&control.handshake_file);
    assert_ne!(after_rebind["addr"], before_rebind["addr"]);
    assert_eq!(after_rebind["token"], before_rebind["token"]);
    let rebound_mutation = call_tool(&captain, 202, "new_tab", json!({"name": "Port rebound"}));
    assert_eq!(
        rebound_mutation["result"]["isError"], false,
        "{rebound_mutation}"
    );

    thread::sleep(Duration::from_millis(1_100));
    let expired = raw_control_call(
        after_rebind["addr"].as_str().unwrap(),
        &scoped_lease,
        sessions["captain"].as_str().unwrap(),
        "new_tab",
        json!({"name": "Expired lease must fail"}),
    );
    assert_eq!(expired["ok"], false, "expired scoped lease remained usable");
    let renewed_after_expiry = call_tool(
        &captain,
        203,
        "new_tab",
        json!({"name": "Renewed after expiry"}),
    );
    assert_eq!(
        renewed_after_expiry["result"]["isError"], false,
        "{renewed_after_expiry}"
    );

    let followup_args = json!({
        "requestId": "continuity-followup-1",
        "captainSessionId": CONTINUITY_CAPTAIN,
        "shipSlug": "continuity-ship",
        "projectId": "continuity-project",
        "agentSessionId": CONTINUITY_AGENT,
        "message": "Continue the durable continuity proof."
    });
    let followup = call_tool(&captain, 205, "agent_followup", followup_args.clone());
    assert_eq!(followup["result"]["isError"], false, "{followup}");
    let followup_data = tool_structured(&followup);
    assert_eq!(followup_data["messageSeq"], 0);
    assert_eq!(followup_data["assignmentChanged"], false);
    let persisted: Value = serde_json::from_slice(
        &fs::read(control.temp_dir.join("captains.json")).expect("read continuity registry"),
    )
    .expect("parse continuity registry");
    assert_eq!(
        persisted["agentSessions"][0]["assignment"],
        "Original continuity Assignment"
    );

    let ship_appointment = call_tool(
        &captain,
        204,
        "appoint_admin",
        json!({
            "actorSessionId": CONTINUITY_SHIP_ADMIN,
            "role": "shipAdmin",
            "permittedOperations": ["maintainSession"]
        }),
    );
    assert_eq!(
        ship_appointment["result"]["isError"], false,
        "{ship_appointment}"
    );

    let mut cortana = McpProc::spawn(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        None,
        sessions["cortana"].as_str(),
    );
    initialize_mcp(&cortana, 210);
    let cortana_mutation = call_tool(&cortana, 211, "new_tab", json!({"name": "Cortana scoped"}));
    assert_eq!(
        cortana_mutation["result"]["isError"], false,
        "{cortana_mutation}"
    );
    let fleet_appointment = call_tool(
        &cortana,
        212,
        "appoint_admin",
        json!({
            "actorSessionId": CONTINUITY_FLEET_ADMIN,
            "role": "fleetAdmin",
            "permittedOperations": ["maintainFleetResource"]
        }),
    );
    assert_eq!(
        fleet_appointment["result"]["isError"], false,
        "{fleet_appointment}"
    );

    let mut ship_admin = McpProc::spawn(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        None,
        sessions["shipAdmin"].as_str(),
    );
    initialize_mcp(&ship_admin, 220);
    let ship_allowed = call_tool(
        &ship_admin,
        221,
        "execute_admin_operation",
        json!({
            "operation": "maintainSession",
            "target": {"kind": "session", "sessionId": CONTINUITY_CREW}
        }),
    );
    assert_eq!(ship_allowed["result"]["isError"], false, "{ship_allowed}");
    let ship_foreign = call_tool(
        &ship_admin,
        222,
        "execute_admin_operation",
        json!({
            "operation": "maintainSession",
            "target": {"kind": "session", "sessionId": CONTINUITY_FOREIGN_CREW}
        }),
    );
    assert!(tool_error_text(&ship_foreign).contains("scope"));

    let mut fleet_admin = McpProc::spawn(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        None,
        sessions["fleetAdmin"].as_str(),
    );
    initialize_mcp(&fleet_admin, 230);
    let fleet_allowed = call_tool(
        &fleet_admin,
        231,
        "execute_admin_operation",
        json!({
            "operation": "maintainFleetResource",
            "target": {"kind": "fleet"}
        }),
    );
    assert_eq!(fleet_allowed["result"]["isError"], false, "{fleet_allowed}");
    let fleet_wrong_operation = call_tool(
        &fleet_admin,
        232,
        "execute_admin_operation",
        json!({
            "operation": "maintainSession",
            "target": {"kind": "session", "sessionId": CONTINUITY_CREW}
        }),
    );
    assert!(tool_error_text(&fleet_wrong_operation).contains("operationNotGranted"));

    let mut crew = McpProc::spawn(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        None,
        sessions["crew"].as_str(),
    );
    initialize_mcp(&crew, 240);
    let crew_control_request = call_tool(
        &crew,
        241,
        "spawn_terminal",
        json!({"cwd": "/tmp", "capability": "control"}),
    );
    assert!(tool_error_text(&crew_control_request).contains("control_reauthentication_required"));
    let crew_mutation = call_tool(&crew, 242, "new_tab", json!({"name": "Crew escape"}));
    assert!(tool_error_text(&crew_mutation).contains("control_reauthentication_required"));

    let mut foreign = McpProc::spawn(
        &bin,
        &control.handshake_file,
        &tmux_socket,
        None,
        sessions["foreignCaptain"].as_str(),
    );
    initialize_mcp(&foreign, 250);
    let foreign_watch = call_tool(
        &foreign,
        251,
        "watch_fleet",
        json!({"orchestratorSessionId": CONTINUITY_CAPTAIN, "scope": "all"}),
    );
    assert!(tool_error_text(&foreign_watch).contains("own or same-ship watch"));
    let foreign_followup = call_tool(
        &foreign,
        252,
        "agent_followup",
        json!({
            "requestId": "continuity-foreign-followup",
            "captainSessionId": CONTINUITY_CAPTAIN,
            "shipSlug": "continuity-ship",
            "projectId": "continuity-project",
            "agentSessionId": CONTINUITY_AGENT,
            "message": "This foreign instruction must be refused."
        }),
    );
    assert!(tool_error_text(&foreign_followup).contains("exact active owning Captain"));

    let old_read_token = first_auth["readToken"].as_str().unwrap().to_string();
    let pre_restart_handshake = current_handshake(&control.handshake_file);
    control.restart().expect("restart continuity control app");
    let restarted_auth = control.fixture_auth();
    assert!(
        restarted_auth["readToken"] != old_read_token,
        "rotating read credential did not change across app restart"
    );
    let post_restart_handshake = current_handshake(&control.handshake_file);
    assert!(
        post_restart_handshake["instance_id"] != pre_restart_handshake["instance_id"],
        "listener instance identity did not change across app restart"
    );
    let post_restart = call_tool(
        &captain,
        260,
        "new_tab",
        json!({"name": "Same MCP after app restart"}),
    );
    assert_eq!(post_restart["result"]["isError"], false, "{post_restart}");
    let followup_replay = call_tool(&captain, 262, "agent_followup", followup_args);
    assert_eq!(
        followup_replay["result"]["isError"], false,
        "{followup_replay}"
    );
    let followup_replay_data = tool_structured(&followup_replay);
    assert_eq!(followup_replay_data["messageSeq"], 0);
    assert_eq!(followup_replay_data["idempotentReplay"], true);
    let changed_scope_replay = call_tool(
        &captain,
        263,
        "agent_followup",
        json!({
            "requestId": "continuity-followup-1",
            "captainSessionId": CONTINUITY_CAPTAIN,
            "shipSlug": "continuity-ship",
            "projectId": "continuity-project",
            "agentSessionId": CONTINUITY_AGENT,
            "message": "Continue the durable continuity proof.",
            "replacementAssignment": "Changed replay must not mutate"
        }),
    );
    assert!(tool_error_text(&changed_scope_replay).contains("different"));
    let persisted_after_conflict: Value = serde_json::from_slice(
        &fs::read(control.temp_dir.join("captains.json")).expect("read continuity registry"),
    )
    .expect("parse continuity registry");
    assert_eq!(
        persisted_after_conflict["agentSessions"][0]["assignment"],
        "Original continuity Assignment"
    );
    let admin_post_restart = call_tool(
        &ship_admin,
        261,
        "execute_admin_operation",
        json!({
            "operation": "maintainSession",
            "target": {"kind": "session", "sessionId": CONTINUITY_CREW}
        }),
    );
    assert_eq!(
        admin_post_restart["result"]["isError"], false,
        "{admin_post_restart}"
    );

    for (id, label, secret_key, expected) in [
        (270, "dead", "deadCaptain", "not alive"),
        (271, "duplicate", "duplicateCaptain", "ambiguous"),
        (272, "revoked", "revoked", "could not be verified"),
        (273, "removed", "removed", "could not be verified"),
    ] {
        let mut denied = McpProc::spawn(
            &bin,
            &control.handshake_file,
            &tmux_socket,
            None,
            restarted_auth["sessions"][secret_key].as_str(),
        );
        initialize_mcp(&denied, id);
        let response = call_tool(
            &denied,
            id + 100,
            "new_tab",
            json!({"name": format!("{label} must not mutate")}),
        );
        assert!(
            tool_error_text(&response).contains(expected),
            "{label} refusal was unexpected: {response}"
        );
        denied.shutdown().expect("stop denied identity MCP");
    }

    let release = call_tool(
        &captain,
        280,
        "release_captain",
        json!({"captainSessionId": CONTINUITY_CAPTAIN}),
    );
    assert_eq!(release["result"]["isError"], false, "{release}");
    let released_mutation = call_tool(
        &captain,
        281,
        "new_tab",
        json!({"name": "Released Captain must not mutate"}),
    );
    assert!(tool_error_text(&released_mutation).contains("no active scoped mutation authority"));

    for process in [
        &mut fleet_admin,
        &mut ship_admin,
        &mut cortana,
        &mut foreign,
        &mut crew,
        &mut captain,
    ] {
        process.shutdown().expect("stop continuity MCP process");
    }
    control.shutdown().expect("stop continuity control helper");
    tmux_guard
        .shutdown()
        .expect("stop continuity tmux fixtures");
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
    for name in RETIRED_POWDER_TOOLS {
        assert!(
            tools.iter().all(|tool| tool["name"] != name),
            "retired Powder tool {name} must not be advertised"
        );
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
