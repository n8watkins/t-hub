//! Minimal, self-contained client for T-Hub's loopback control channel.
//!
//! This is the Rust port of `scripts/probes/t1_lib.py` (+ `t1_subscribe.py`):
//! discover the running app via the handshake file (or env overrides), open a
//! short-lived TCP connection, authenticate with the per-launch token, send one
//! NDJSON request line and read one NDJSON response line. It has NO dependency
//! on apps/desktop — `th` is a pure client of the same wire the MCP server uses.
//!
//! Discovery mirrors the MCP server (`t-hub-mcp/src/control_client.rs`): an
//! injected address and token are tried first, while the current handshake file
//! supplies a rotated address after an application restart.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

/// The wire protocol version this client speaks. The app advertises its own in
/// the handshake file; a mismatch is surfaced as [`ControlError::Protocol`].
pub const PROTOCOL_VERSION: u32 = 2;

/// One ordinary control call, including endpoint discovery time, stale-endpoint
/// invalidation, and one retry, must finish within this wall-clock budget.
const CONTROL_DEADLINE: Duration = Duration::from_secs(10);
const LONG_ORCHESTRATION_TIMEOUT: Duration = Duration::from_secs(120);

/// A connected endpoint gets only a short slice of the overall budget. This is
/// what prevents an inherited port that accepts but never answers from consuming
/// the entire recovery window before the current handshake is tried.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);

/// Every control client accepts at most 1 MiB before the NDJSON response newline.
/// This bounds memory, parsing work, and any structured error derived from a peer.
const MAX_RESPONSE_FRAME_BYTES: usize = 1024 * 1024;

/// A resolved, authenticated control endpoint.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub addr: String,
    pub token: String,
    handshake_path: PathBuf,
    env_pinned: bool,
    discovery_elapsed: Duration,
}

/// A control-channel failure, classified so the CLI can pick a stable exit code
/// (see the `exit` module in `main.rs`).
#[derive(Debug)]
pub enum ControlError {
    /// Discovery or connect failed — the app is not reachable (exit 3).
    AppDown(String),
    /// The app answered `ok:false` (exit 4, or 5 when the message is a gate).
    Server {
        message: String,
        retryable: bool,
        details: Option<Value>,
    },
    /// Malformed frame, or a handshake protocol-version mismatch (exit 6).
    Protocol(String),
}

/// The on-disk handshake the app writes. We need `addr` + `token`; the optional
/// `protocol_version` lets us fail fast on a mismatch.
#[derive(Debug, Deserialize)]
struct Handshake {
    addr: String,
    token: String,
    #[serde(default)]
    protocol_version: Option<u32>,
}

/// The app's response envelope: `{ok, result?, error?}`.
#[derive(Debug, Deserialize)]
struct Response {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
    #[serde(rename = "errorKind", default)]
    error_kind: Option<String>,
    #[serde(rename = "errorDetails", default)]
    error_details: Option<Value>,
    #[serde(default)]
    retryable: bool,
}

/// Resolve the control endpoint: env overrides first, then the handshake file.
///
/// Returns a classified error (never panics) when the app isn't running / the
/// handshake file is missing / the wire version disagrees.
pub fn resolve_endpoint() -> Result<Endpoint, ControlError> {
    let started = Instant::now();
    let path = handshake_path();
    // 1. Explicit env override (addr + token). No version metadata to check.
    if let (Ok(addr), Ok(token)) = (
        std::env::var("T_HUB_CONTROL_ADDR"),
        std::env::var("T_HUB_CONTROL_TOKEN"),
    ) {
        if !addr.is_empty() && !token.is_empty() {
            // A T-Hub terminal inherits this pair when it is created. The app
            // rotates its port on restart, so prefer a newer handshake address
            // when the published token proves it belongs to the same authority.
            if let Ok(handshake) = read_handshake(&path) {
                validate_protocol(&handshake)?;
                if handshake.token == token && handshake.addr != addr {
                    return Ok(Endpoint {
                        addr: handshake.addr,
                        token,
                        handshake_path: path,
                        env_pinned: true,
                        discovery_elapsed: started.elapsed(),
                    });
                }
            }
            return Ok(Endpoint {
                addr,
                token,
                handshake_path: path,
                env_pinned: true,
                discovery_elapsed: started.elapsed(),
            });
        }
    }

    // 2. The handshake file the running app wrote.
    let hs = read_handshake(&path)?;
    validate_protocol(&hs)?;
    Ok(Endpoint {
        addr: hs.addr,
        token: hs.token,
        handshake_path: path,
        env_pinned: false,
        discovery_elapsed: started.elapsed(),
    })
}

fn read_handshake(path: &PathBuf) -> Result<Handshake, ControlError> {
    let body = std::fs::read_to_string(path).map_err(|e| {
        ControlError::AppDown(format!(
            "T-Hub control channel not found at {} ({e}).\n\
             Is the T-Hub app running? (set T_HUB_CONTROL_ADDR + T_HUB_CONTROL_TOKEN to override.)",
            path.display()
        ))
    })?;
    serde_json::from_str(&body).map_err(|e| {
        ControlError::AppDown(format!(
            "malformed control handshake at {}: {e}",
            path.display()
        ))
    })
}

fn validate_protocol(handshake: &Handshake) -> Result<(), ControlError> {
    if let Some(version) = handshake.protocol_version {
        if version != PROTOCOL_VERSION {
            return Err(ControlError::Protocol(format!(
                "control protocol mismatch: app speaks v{version}, this `th` speaks v{PROTOCOL_VERSION}. \
                 Update `th` (or the app) so both agree."
            )));
        }
    }
    Ok(())
}

/// The handshake file path: `$T_HUB_CONTROL_FILE`, else `~/.t-hub/control.json`.
fn handshake_path() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_CONTROL_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("control.json")
}

/// Forward one command to the app and return its `result` JSON, or a classified
/// error. The app's own error string is preserved verbatim (so gating /
/// confirmation messages surface unchanged).
pub fn call(ep: &Endpoint, command: &str, args: Value) -> Result<Value, ControlError> {
    let deadline = match command {
        "commission_captain" | "dispatch_crew" | "history_resume" | "reconcile_cortana"
        | "start_agent" => LONG_ORCHESTRATION_TIMEOUT,
        _ => CONTROL_DEADLINE,
    };
    call_with_deadline(ep, command, &args, deadline, ATTEMPT_TIMEOUT)
}

fn call_with_deadline(
    ep: &Endpoint,
    command: &str,
    args: &Value,
    overall: Duration,
    attempt_timeout: Duration,
) -> Result<Value, ControlError> {
    let remaining = overall.saturating_sub(ep.discovery_elapsed);
    let deadline = Instant::now() + remaining;
    if remaining.is_zero() {
        return Err(timeout_error(command, 0, "discovery", overall));
    }

    match call_once(ep, command, args, deadline, attempt_timeout) {
        Ok(value) => Ok(value),
        Err(CallFailure::Server {
            message,
            retryable,
            details,
        }) => Err(ControlError::Server {
            message,
            retryable,
            details,
        }),
        Err(CallFailure::Protocol(message)) => Err(ControlError::Protocol(message)),
        Err(first) => {
            if Instant::now() >= deadline {
                return Err(timeout_error(command, 1, first.stage(), overall));
            }
            let refreshed = refresh_endpoint(ep)?;
            if refreshed.addr == ep.addr {
                return Err(first.into_control_error(command, 1, false, overall));
            }
            match call_once(&refreshed, command, args, deadline, attempt_timeout) {
                Ok(value) => Ok(value),
                Err(CallFailure::Server {
                    message,
                    retryable,
                    details,
                }) => Err(ControlError::Server {
                    message,
                    retryable,
                    details,
                }),
                Err(CallFailure::Protocol(message)) => Err(ControlError::Protocol(message)),
                Err(second) if Instant::now() >= deadline || second.is_timeout() => {
                    Err(timeout_error(command, 2, second.stage(), overall))
                }
                Err(second) => Err(second.into_control_error(command, 2, true, overall)),
            }
        }
    }
}

fn refresh_endpoint(ep: &Endpoint) -> Result<Endpoint, ControlError> {
    let handshake = read_handshake(&ep.handshake_path)?;
    validate_protocol(&handshake)?;
    Ok(Endpoint {
        addr: handshake.addr,
        token: if ep.env_pinned {
            ep.token.clone()
        } else {
            handshake.token
        },
        handshake_path: ep.handshake_path.clone(),
        env_pinned: ep.env_pinned,
        discovery_elapsed: ep.discovery_elapsed,
    })
}

enum CallFailure {
    Transport(&'static str),
    Timeout(&'static str),
    Server {
        message: String,
        retryable: bool,
        details: Option<Value>,
    },
    Protocol(String),
}

impl CallFailure {
    fn stage(&self) -> &'static str {
        match self {
            CallFailure::Transport(stage) | CallFailure::Timeout(stage) => stage,
            CallFailure::Server { .. } => "server",
            CallFailure::Protocol(_) => "protocol",
        }
    }

    fn is_timeout(&self) -> bool {
        matches!(self, CallFailure::Timeout(_))
    }

    fn into_control_error(
        self,
        command: &str,
        attempts: u8,
        endpoint_replaced: bool,
        overall: Duration,
    ) -> ControlError {
        match self {
            CallFailure::Timeout(stage) => timeout_error(command, attempts, stage, overall),
            CallFailure::Transport(stage) => ControlError::AppDown(format!(
                "control_unavailable: command '{command}' failed during {stage} after {attempts} attempt(s); endpoint_replaced={endpoint_replaced}"
            )),
            CallFailure::Server { message, retryable, details } => {
                ControlError::Server { message, retryable, details }
            }
            CallFailure::Protocol(message) => ControlError::Protocol(message),
        }
    }
}

fn timeout_error(command: &str, attempts: u8, stage: &str, overall: Duration) -> ControlError {
    ControlError::AppDown(format!(
        "control_timeout: command '{command}' failed within its {}s recovery deadline during {stage} after {attempts} attempt(s); retry_state=exhausted",
        overall.as_secs()
    ))
}

fn remaining(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

fn call_once(
    ep: &Endpoint,
    command: &str,
    args: &Value,
    deadline: Instant,
    attempt_timeout: Duration,
) -> Result<Value, CallFailure> {
    // Comms-plane Phase 3: present the caller session's PER-SESSION token
    // (`T_HUB_SESSION_TOKEN`) alongside the tier `token` so `th` run inside a spawned
    // session is bound to that session's identity by the plane ACLs (a crew's `th send`
    // is ship-gated like its MCP writes). Absent for a host/human context - the server
    // then treats it as the trusted control-token host (cross-ship ACL fails open).
    let session = std::env::var("T_HUB_SESSION_TOKEN").unwrap_or_default();
    let request = serde_json::json!({
        "token": ep.token,
        "session": session,
        "command": command,
        "args": args,
        "v": PROTOCOL_VERSION,
    });

    let socket: SocketAddr = ep
        .addr
        .parse()
        .map_err(|_| CallFailure::Protocol("malformed control endpoint address".to_string()))?;
    let connect_budget = remaining(deadline)
        .map(|left| left.min(attempt_timeout))
        .filter(|budget| !budget.is_zero())
        .ok_or(CallFailure::Timeout("connect"))?;
    let stream = TcpStream::connect_timeout(&socket, connect_budget).map_err(|e| {
        if matches!(
            e.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            CallFailure::Timeout("connect")
        } else {
            CallFailure::Transport("connect")
        }
    })?;
    let io_budget = remaining(deadline)
        .map(|left| left.min(attempt_timeout))
        .filter(|budget| !budget.is_zero())
        .ok_or(CallFailure::Timeout("write"))?;
    let _ = stream.set_write_timeout(Some(io_budget));

    let mut writer = stream
        .try_clone()
        .map_err(|_| CallFailure::Transport("stream setup"))?;
    let mut line =
        serde_json::to_vec(&request).map_err(|e| CallFailure::Protocol(e.to_string()))?;
    line.push(b'\n');
    writer.write_all(&line).map_err(|e| {
        if matches!(
            e.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            CallFailure::Timeout("write")
        } else {
            CallFailure::Transport("write")
        }
    })?;
    writer.flush().map_err(|e| {
        if matches!(
            e.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            CallFailure::Timeout("write")
        } else {
            CallFailure::Transport("write")
        }
    })?;

    stream
        .set_nonblocking(true)
        .map_err(|_| CallFailure::Transport("stream setup"))?;
    let mut response = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut next_probe = Instant::now() + attempt_timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(CallFailure::Timeout("read"));
        }
        if now >= next_probe {
            if refresh_endpoint(ep)
                .is_ok_and(|fresh| fresh.addr != ep.addr || fresh.token != ep.token)
            {
                return Err(CallFailure::Timeout("read"));
            }
            next_probe = now + attempt_timeout;
        }
        match (&stream).read(&mut chunk) {
            Ok(0) if response.is_empty() => return Err(CallFailure::Transport("read")),
            Ok(0) => {
                return Err(CallFailure::Protocol(
                    "control_protocol: unterminated response frame".to_string(),
                ));
            }
            Ok(n) => {
                let received = &chunk[..n];
                let frame_bytes = received
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .unwrap_or(received.len());
                if response.len().saturating_add(frame_bytes) > MAX_RESPONSE_FRAME_BYTES {
                    return Err(CallFailure::Protocol(format!(
                        "control_protocol: response frame exceeds {MAX_RESPONSE_FRAME_BYTES}-byte limit"
                    )));
                }
                response.extend_from_slice(&received[..frame_bytes]);
                if frame_bytes < received.len() {
                    break;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                let wake_at = deadline.min(next_probe);
                std::thread::sleep(
                    wake_at
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10)),
                );
            }
            Err(_) => return Err(CallFailure::Transport("read")),
        }
    }

    let resp_line = String::from_utf8(response).map_err(|_| {
        CallFailure::Protocol("control_protocol: response frame was not UTF-8".to_string())
    })?;

    let resp: Response = serde_json::from_str(resp_line.trim_end())
        .map_err(|e| CallFailure::Protocol(format!("control_protocol: malformed response: {e}")))?;

    if resp.ok {
        Ok(resp.result.unwrap_or(Value::Null))
    } else {
        Err(CallFailure::Server {
            message: match resp.error_kind.as_deref() {
                Some("powder_retired") => format!(
                    "powder_retired:{}",
                    resp.error
                        .unwrap_or_else(|| "Powder operation is retired".to_string())
                ),
                _ => resp
                    .error
                    .unwrap_or_else(|| "control command failed (no error message)".to_string()),
            },
            retryable: resp.retryable,
            details: resp.error_details,
        })
    }
}

/// Subscribe to the app's event stream (port of `t1_subscribe.py`). Opens a
/// dedicated long-lived connection, sends `__subscribe_events`, and calls
/// `on_frame` for the ack and every subsequent NDJSON frame until EOF/error.
/// Runs until the connection closes (Ctrl-C the process to stop early).
pub fn subscribe<F: FnMut(Value)>(ep: &Endpoint, mut on_frame: F) -> Result<(), ControlError> {
    let stream = TcpStream::connect(&ep.addr).map_err(|e| {
        ControlError::AppDown(format!(
            "failed to connect to T-Hub control channel {}: {e}",
            ep.addr
        ))
    })?;
    let mut writer = stream
        .try_clone()
        .map_err(|e| ControlError::AppDown(format!("failed to clone control stream: {e}")))?;
    let request = serde_json::json!({
        "token": ep.token,
        "command": "__subscribe_events",
        "args": {},
        "v": PROTOCOL_VERSION,
    });
    let mut line =
        serde_json::to_vec(&request).map_err(|e| ControlError::Protocol(e.to_string()))?;
    line.push(b'\n');
    writer
        .write_all(&line)
        .map_err(|e| ControlError::AppDown(format!("failed to send subscribe request: {e}")))?;
    writer
        .flush()
        .map_err(|e| ControlError::AppDown(format!("failed to flush subscribe request: {e}")))?;

    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line =
            line.map_err(|e| ControlError::AppDown(format!("event stream read error: {e}")))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(v) => on_frame(v),
            Err(_) => on_frame(serde_json::json!({ "raw": line })),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::process::Command;
    use std::thread;

    #[test]
    fn timeout_message_uses_the_selected_command_budget() {
        let ControlError::AppDown(message) =
            timeout_error("history_resume", 1, "read", LONG_ORCHESTRATION_TIMEOUT)
        else {
            panic!("timeout must remain an app-down error");
        };
        assert!(message.contains("120s recovery deadline"));
    }

    fn handshake_file(body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "th-control-test-{}-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("thread"),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&path, body).unwrap();
        path
    }

    fn handshake_for(addr: &str) -> PathBuf {
        handshake_file(&format!(
            r#"{{"addr":"{addr}","token":"published-read","protocol_version":2}}"#
        ))
    }

    fn responding_server(result: Value) -> String {
        delayed_server(result, Duration::ZERO)
    }

    fn delayed_server(result: Value, delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            thread::sleep(delay);
            let mut writer = stream;
            serde_json::to_writer(
                &mut writer,
                &serde_json::json!({"ok": true, "result": result}),
            )
            .unwrap();
            writer.write_all(b"\n").unwrap();
        });
        addr
    }

    fn silent_server(hold: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            let mut reader = BufReader::new(stream);
            reader.read_line(&mut request).unwrap();
            thread::sleep(hold);
        });
        addr
    }

    fn closing_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream).read_line(&mut request).unwrap();
        });
        addr
    }

    fn trickle_server(interval: Duration, writes: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let mut writer = stream;
            for _ in 0..writes {
                if writer.write_all(b"{").is_err() || writer.flush().is_err() {
                    break;
                }
                thread::sleep(interval);
            }
        });
        addr
    }

    fn raw_response_server(response: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let mut writer = stream;
            let _ = writer.write_all(&response);
            let _ = writer.flush();
        });
        addr
    }

    #[test]
    fn raw_wire_error_details_survive_cli_control_adapter() {
        let details = serde_json::json!({
            "code": "git_required",
            "operation": "baseline",
            "capability": "git",
            "action": "initialize_git"
        });
        let response = serde_json::to_vec(&serde_json::json!({
            "ok": false,
            "error": "Git capability is required for baseline",
            "errorKind": "git_required",
            "errorDetails": details,
            "retryable": false
        }))
        .unwrap();
        let mut response = response;
        response.push(b'\n');
        let addr = raw_response_server(response);
        let endpoint = Endpoint {
            addr,
            token: "control-token".into(),
            handshake_path: PathBuf::from("/nonexistent/handshake.json"),
            env_pinned: true,
            discovery_elapsed: Duration::ZERO,
        };

        let error = call_with_deadline(
            &endpoint,
            "baseline",
            &Value::Null,
            Duration::from_secs(1),
            Duration::from_millis(250),
        )
        .unwrap_err();

        match error {
            ControlError::Server {
                message,
                retryable,
                details: actual,
            } => {
                assert_eq!(message, "Git capability is required for baseline");
                assert!(!retryable);
                assert_eq!(actual, Some(details));
            }
            other => panic!("expected structured server error, got {other:?}"),
        }
    }

    fn exact_limit_response() -> Vec<u8> {
        let mut response = br#"{"ok":true,"result":null}"#.to_vec();
        response.resize(MAX_RESPONSE_FRAME_BYTES, b' ');
        response.push(b'\n');
        response
    }

    fn dead_addr() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        addr
    }

    fn inherited_endpoint(addr: String, handshake_path: PathBuf) -> Endpoint {
        Endpoint {
            addr,
            token: "inherited-control".to_string(),
            handshake_path,
            env_pinned: true,
            discovery_elapsed: Duration::ZERO,
        }
    }

    fn cli_binary() -> PathBuf {
        if let Some(path) = option_env!("CARGO_BIN_EXE_th") {
            return PathBuf::from(path);
        }
        let test_exe = std::env::current_exe().unwrap();
        let debug_dir = test_exe.parent().and_then(|path| path.parent()).unwrap();
        let name = if cfg!(windows) { "th.exe" } else { "th" };
        let binary = debug_dir.join(name);
        assert!(
            binary.is_file(),
            "CLI process binary missing at {}; run `cargo build` before this focused test",
            binary.display()
        );
        binary
    }

    fn run_cli_process(addr: &str, token: &str, handshake: &PathBuf) -> std::process::Output {
        Command::new(cli_binary())
            .args(["tabs", "--json"])
            .env("T_HUB_CONTROL_ADDR", addr)
            .env("T_HUB_CONTROL_TOKEN", token)
            .env("T_HUB_CONTROL_FILE", handshake)
            .output()
            .unwrap()
    }

    #[test]
    fn refreshed_endpoint_adopts_rotated_address_and_keeps_pinned_token() {
        let path =
            handshake_file(r#"{"addr":"127.0.0.1:62000","token":"read","protocol_version":2}"#);
        let stale = Endpoint {
            addr: "127.0.0.1:61000".to_string(),
            token: "control".to_string(),
            handshake_path: path.clone(),
            env_pinned: true,
            discovery_elapsed: Duration::ZERO,
        };

        let refreshed = refresh_endpoint(&stale).unwrap();
        assert_eq!(refreshed.addr, "127.0.0.1:62000");
        assert_eq!(refreshed.token, "control");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn refreshed_file_endpoint_adopts_rotated_token() {
        let path =
            handshake_file(r#"{"addr":"127.0.0.1:62000","token":"fresh","protocol_version":2}"#);
        let stale = Endpoint {
            addr: "127.0.0.1:61000".to_string(),
            token: "stale".to_string(),
            handshake_path: path.clone(),
            env_pinned: false,
            discovery_elapsed: Duration::ZERO,
        };

        let refreshed = refresh_endpoint(&stale).unwrap();
        assert_eq!(refreshed.token, "fresh");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn protocol_mismatch_remains_a_structured_protocol_error() {
        let handshake = Handshake {
            addr: "127.0.0.1:62000".to_string(),
            token: "read".to_string(),
            protocol_version: Some(1),
        };
        assert!(matches!(
            validate_protocol(&handshake),
            Err(ControlError::Protocol(_))
        ));
    }

    #[test]
    fn refused_connect_recovers_through_the_current_endpoint() {
        let fresh = responding_server(serde_json::json!({"recovered": true}));
        let path = handshake_for(&fresh);
        let stale = inherited_endpoint(dead_addr(), path.clone());

        let value = call_with_deadline(
            &stale,
            "wsl_health",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(60),
        )
        .unwrap();
        assert_eq!(value["recovered"], true);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn connected_but_silent_inherited_port_gets_only_one_attempt_slice() {
        let stale_addr = silent_server(Duration::from_millis(180));
        let fresh = responding_server(serde_json::json!({"tabs": []}));
        let path = handshake_for(&fresh);
        let stale = inherited_endpoint(stale_addr, path.clone());
        let started = Instant::now();

        let value = call_with_deadline(
            &stale,
            "list_tabs",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(40),
        )
        .unwrap();

        assert_eq!(value["tabs"], serde_json::json!([]));
        assert!(
            started.elapsed() < Duration::from_millis(150),
            "recovery must not inherit a second full timeout window"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn response_loss_invalidates_the_endpoint_and_recovers_once() {
        let stale_addr = closing_server();
        let fresh = responding_server(serde_json::json!({"terminals": []}));
        let path = handshake_for(&fresh);
        let stale = inherited_endpoint(stale_addr, path.clone());

        let value = call_with_deadline(
            &stale,
            "list_terminals",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(60),
        )
        .unwrap();
        assert_eq!(value["terminals"], serde_json::json!([]));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn current_endpoint_succeeds_without_retry() {
        let addr = responding_server(serde_json::json!({"healthy": true}));
        let path = handshake_for(&addr);
        let endpoint = inherited_endpoint(addr, path.clone());

        let value = call_with_deadline(
            &endpoint,
            "wsl_health",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(60),
        )
        .unwrap();
        assert_eq!(value["healthy"], true);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn healthy_response_can_outlive_attempt_slice_within_overall_deadline() {
        let addr = delayed_server(
            serde_json::json!({"usage": "ready"}),
            Duration::from_millis(90),
        );
        let path = handshake_for(&addr);
        let endpoint = inherited_endpoint(addr, path.clone());

        let value = call_with_deadline(
            &endpoint,
            "codex_usage",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(40),
        )
        .unwrap();
        assert_eq!(value["usage"], "ready");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn partial_frame_trickle_cannot_bypass_absolute_deadline() {
        let addr = trickle_server(Duration::from_millis(10), 30);
        let path = handshake_for(&addr);
        let endpoint = inherited_endpoint(addr, path.clone());
        let started = Instant::now();

        let error = call_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_millis(70),
            Duration::from_millis(20),
        )
        .unwrap_err();
        assert!(matches!(error, ControlError::AppDown(message) if
            message.contains("control_timeout")));
        assert!(started.elapsed() < Duration::from_millis(150));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn exact_limit_response_frame_is_accepted() {
        let addr = raw_response_server(exact_limit_response());
        let path = handshake_for(&addr);
        let endpoint = inherited_endpoint(addr, path.clone());

        let value = call_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .unwrap();
        assert_eq!(value, Value::Null);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn over_limit_response_frame_is_bounded_and_credential_safe() {
        let secret = "oversized-server-token-must-not-leak";
        let mut response = vec![b'x'; MAX_RESPONSE_FRAME_BYTES];
        response.extend_from_slice(secret.as_bytes());
        response.push(b'\n');
        let addr = raw_response_server(response);
        let path = handshake_for(&addr);
        let endpoint = inherited_endpoint(addr.clone(), path.clone());

        let error = call_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .unwrap_err();
        let ControlError::Protocol(message) = error else {
            panic!("oversized frame must be a protocol error");
        };
        assert!(message.contains("response frame exceeds"));
        assert!(!message.contains(secret));
        assert!(!message.contains("inherited-control"));
        assert!(!message.contains(&addr));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unterminated_response_frame_is_a_safe_protocol_error() {
        let secret = "unterminated-server-token-must-not-leak";
        let addr = raw_response_server(format!("{{\"ok\":true,\"{secret}\":").into_bytes());
        let path = handshake_for(&addr);
        let endpoint = inherited_endpoint(addr, path.clone());

        let error = call_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .unwrap_err();
        let ControlError::Protocol(message) = error else {
            panic!("unterminated frame must be a protocol error");
        };
        assert!(message.contains("unterminated response frame"));
        assert!(!message.contains(secret));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn malformed_response_frame_does_not_echo_peer_content() {
        let secret = "malformed-server-token-must-not-leak";
        let addr = raw_response_server(format!("{{not-json:{secret}}}\n").into_bytes());
        let path = handshake_for(&addr);
        let endpoint = inherited_endpoint(addr, path.clone());

        let error = call_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .unwrap_err();
        let ControlError::Protocol(message) = error else {
            panic!("malformed frame must be a protocol error");
        };
        assert!(message.contains("malformed response"));
        assert!(!message.contains(secret));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn stale_discovery_time_exhausts_the_budget_before_connect() {
        let path = handshake_for("127.0.0.1:1");
        let mut endpoint = inherited_endpoint("127.0.0.1:1".to_string(), path.clone());
        endpoint.discovery_elapsed = Duration::from_millis(251);

        let error = call_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(60),
        )
        .unwrap_err();
        assert!(matches!(error, ControlError::AppDown(message) if
            message.contains("control_timeout") && message.contains("discovery")));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn budget_exhaustion_is_bounded_classified_and_credential_safe() {
        let addr = silent_server(Duration::from_millis(180));
        let path = handshake_for(&addr);
        let endpoint = inherited_endpoint(addr.clone(), path.clone());
        let started = Instant::now();

        let error = call_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_millis(70),
            Duration::from_millis(60),
        )
        .unwrap_err();
        let ControlError::AppDown(message) = error else {
            panic!("timeout must remain in the CLI app-down exit taxonomy");
        };
        assert!(
            message.contains("control_timeout"),
            "expected timeout classification, got: {message}"
        );
        assert!(message.contains("retry_state=exhausted"));
        assert!(!message.contains(&addr));
        assert!(!message.contains("inherited-control"));
        assert!(started.elapsed() < Duration::from_millis(150));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn process_contract_covers_stale_recovery_and_json_timeout_envelope() {
        let stale_addr = silent_server(Duration::from_secs(5));
        let fresh = responding_server(serde_json::json!([]));
        let replacement_path = handshake_for(&fresh);
        let started = Instant::now();

        let recovered = run_cli_process(&stale_addr, "inherited-control", &replacement_path);
        let recovered_json: Value = serde_json::from_slice(&recovered.stdout).unwrap();
        assert!(recovered.status.success(), "output: {recovered:?}");
        assert_eq!(recovered_json["ok"], true);
        assert_eq!(recovered_json["command"], "tabs");
        assert_eq!(recovered_json["error"], Value::Null);
        assert!(recovered.stderr.is_empty(), "stderr: {recovered:?}");
        assert!(started.elapsed() < Duration::from_secs(4));
        let _ = std::fs::remove_file(replacement_path);

        let silent_addr = silent_server(CONTROL_DEADLINE + Duration::from_secs(2));
        let timeout_path = handshake_for(&silent_addr);
        let timed_out = run_cli_process(&silent_addr, "inherited-control", &timeout_path);
        let timeout_json: Value = serde_json::from_slice(&timed_out.stdout).unwrap();
        assert_eq!(timed_out.status.code(), Some(3), "output: {timed_out:?}");
        assert_eq!(timeout_json["ok"], false);
        assert_eq!(timeout_json["command"], "tabs");
        assert_eq!(timeout_json["data"], Value::Null);
        assert_eq!(timeout_json["error"]["code"], 3);
        assert_eq!(timeout_json["error"]["kind"], "app_down");
        let message = timeout_json["error"]["message"].as_str().unwrap();
        assert!(message.contains("control_timeout"));
        assert!(message.contains("retry_state=exhausted"));
        assert!(!message.contains(&silent_addr));
        assert!(!message.contains("inherited-control"));
        assert!(timed_out.stderr.is_empty(), "stderr: {timed_out:?}");
        let _ = std::fs::remove_file(timeout_path);
    }

    #[test]
    fn process_contract_bounds_oversized_malformed_response_output() {
        let secret = "oversized-server-token-must-not-leak";
        let mut response = vec![b'x'; 2 * 1024 * 1024];
        response.extend_from_slice(secret.as_bytes());
        response.push(b'\n');
        let addr = raw_response_server(response);
        let path = handshake_for(&addr);
        let started = Instant::now();

        let output = run_cli_process(&addr, "inherited-control", &path);
        let elapsed = started.elapsed();
        assert!(
            output.stdout.len() <= 1024,
            "oversized response escaped output bound: elapsed={elapsed:?}, stdout_bytes={}, stderr_bytes={}",
            output.stdout.len(),
            output.stderr.len()
        );
        assert_eq!(output.status.code(), Some(6), "output: {output:?}");
        assert!(output.stderr.is_empty(), "stderr: {output:?}");
        assert!(elapsed < Duration::from_secs(2), "elapsed: {elapsed:?}");
        let output_json: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output_json["ok"], false);
        assert_eq!(output_json["command"], "tabs");
        assert_eq!(output_json["data"], Value::Null);
        assert_eq!(output_json["error"]["code"], 6);
        assert_eq!(output_json["error"]["kind"], "protocol");
        let message = output_json["error"]["message"].as_str().unwrap();
        assert!(message.contains("response frame exceeds"));
        assert!(!message.contains(secret));
        assert!(!message.contains("inherited-control"));
        assert!(!message.contains(&addr));
        let _ = std::fs::remove_file(path);
    }
}
