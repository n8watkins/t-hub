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

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

/// The wire protocol version this client speaks. The app advertises its own in
/// the handshake file; a mismatch is surfaced as [`ControlError::Protocol`].
pub const PROTOCOL_VERSION: u32 = 2;

/// A resolved, authenticated control endpoint.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub addr: String,
    pub token: String,
    handshake_path: PathBuf,
    env_pinned: bool,
}

/// A control-channel failure, classified so the CLI can pick a stable exit code
/// (see the `exit` module in `main.rs`).
#[derive(Debug)]
pub enum ControlError {
    /// Discovery or connect failed — the app is not reachable (exit 3).
    AppDown(String),
    /// The app answered `ok:false` (exit 4, or 5 when the message is a gate).
    Server(String),
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
}

/// Resolve the control endpoint: env overrides first, then the handshake file.
///
/// Returns a classified error (never panics) when the app isn't running / the
/// handshake file is missing / the wire version disagrees.
pub fn resolve_endpoint() -> Result<Endpoint, ControlError> {
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
                    });
                }
            }
            return Ok(Endpoint {
                addr,
                token,
                handshake_path: path,
                env_pinned: true,
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
    match call_once(ep, command, &args) {
        Err(ControlError::AppDown(first_error)) => {
            let refreshed = refresh_endpoint(ep)?;
            if refreshed.addr == ep.addr {
                return Err(ControlError::AppDown(first_error));
            }
            call_once(&refreshed, command, &args)
        }
        result => result,
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
    })
}

fn call_once(ep: &Endpoint, command: &str, args: &Value) -> Result<Value, ControlError> {
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

    let stream = TcpStream::connect(&ep.addr).map_err(|e| {
        ControlError::AppDown(format!(
            "failed to connect to T-Hub control channel {}: {e}",
            ep.addr
        ))
    })?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));

    let mut writer = stream
        .try_clone()
        .map_err(|e| ControlError::AppDown(format!("failed to clone control stream: {e}")))?;
    let mut line =
        serde_json::to_vec(&request).map_err(|e| ControlError::Protocol(e.to_string()))?;
    line.push(b'\n');
    writer
        .write_all(&line)
        .map_err(|e| ControlError::AppDown(format!("failed to send control request: {e}")))?;
    writer
        .flush()
        .map_err(|e| ControlError::AppDown(format!("failed to flush control request: {e}")))?;

    let mut reader = BufReader::new(stream);
    let mut resp_line = String::new();
    let n = reader
        .read_line(&mut resp_line)
        .map_err(|e| ControlError::AppDown(format!("failed to read control response: {e}")))?;
    if n == 0 {
        return Err(ControlError::AppDown(
            "T-Hub control channel closed without responding".to_string(),
        ));
    }

    let resp: Response = serde_json::from_str(resp_line.trim_end()).map_err(|e| {
        ControlError::Protocol(format!(
            "malformed control response: {e} (raw: {})",
            resp_line.trim_end()
        ))
    })?;

    if resp.ok {
        Ok(resp.result.unwrap_or(Value::Null))
    } else {
        Err(ControlError::Server(resp.error.unwrap_or_else(|| {
            "control command failed (no error message)".to_string()
        })))
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

    fn handshake_file(body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "th-control-test-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("thread")
        ));
        std::fs::write(&path, body).unwrap();
        path
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
}
