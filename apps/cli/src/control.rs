//! Minimal, self-contained client for T-Hub's loopback control channel.
//!
//! This is the Rust port of `scripts/probes/t1_lib.py` (+ `t1_subscribe.py`):
//! discover the running app via the handshake file (or env overrides), open a
//! short-lived TCP connection, authenticate with the per-launch token, send one
//! NDJSON request line and read one NDJSON response line. It has NO dependency
//! on apps/desktop — `th` is a pure client of the same wire the MCP server uses.
//!
//! Discovery order mirrors the MCP server (`t-hub-mcp/src/control_client.rs`):
//!   1. `$T_HUB_CONTROL_ADDR` + `$T_HUB_CONTROL_TOKEN` (pin the endpoint).
//!   2. `$T_HUB_CONTROL_FILE`, else `~/.t-hub/control.json` (the handshake file).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

/// A resolved, authenticated control endpoint.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub addr: String,
    pub token: String,
}

/// The on-disk handshake the app writes. We only need `addr` + `token`.
#[derive(Debug, Deserialize)]
struct Handshake {
    addr: String,
    token: String,
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
/// Returns a readable error (never panics) when the app isn't running / the
/// handshake file is missing, so the CLI can surface "T-Hub is not running".
pub fn resolve_endpoint() -> Result<Endpoint, String> {
    // 1. Explicit env override (addr + token).
    if let (Ok(addr), Ok(token)) = (
        std::env::var("T_HUB_CONTROL_ADDR"),
        std::env::var("T_HUB_CONTROL_TOKEN"),
    ) {
        if !addr.is_empty() && !token.is_empty() {
            return Ok(Endpoint { addr, token });
        }
    }

    // 2. The handshake file the running app wrote.
    let path = handshake_path();
    let body = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "T-Hub control channel not found at {} ({e}).\n\
             Is the T-Hub app running? (set T_HUB_CONTROL_ADDR + T_HUB_CONTROL_TOKEN to override.)",
            path.display()
        )
    })?;
    let hs: Handshake = serde_json::from_str(&body)
        .map_err(|e| format!("malformed control handshake at {}: {e}", path.display()))?;
    Ok(Endpoint {
        addr: hs.addr,
        token: hs.token,
    })
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

/// Forward one command to the app and return its `result` JSON, or the app's
/// error string verbatim (so gating / confirmation messages surface unchanged).
pub fn call(ep: &Endpoint, command: &str, args: Value) -> Result<Value, String> {
    let request = serde_json::json!({
        "token": ep.token,
        "command": command,
        "args": args,
        "v": 1,
    });

    let stream = TcpStream::connect(&ep.addr)
        .map_err(|e| format!("failed to connect to T-Hub control channel {}: {e}", ep.addr))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));

    let mut writer = stream
        .try_clone()
        .map_err(|e| format!("failed to clone control stream: {e}"))?;
    let mut line = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    line.push(b'\n');
    writer
        .write_all(&line)
        .map_err(|e| format!("failed to send control request: {e}"))?;
    writer
        .flush()
        .map_err(|e| format!("failed to flush control request: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut resp_line = String::new();
    let n = reader
        .read_line(&mut resp_line)
        .map_err(|e| format!("failed to read control response: {e}"))?;
    if n == 0 {
        return Err("T-Hub control channel closed without responding".to_string());
    }

    let resp: Response = serde_json::from_str(resp_line.trim_end())
        .map_err(|e| format!("malformed control response: {e} (raw: {})", resp_line.trim_end()))?;

    if resp.ok {
        Ok(resp.result.unwrap_or(Value::Null))
    } else {
        Err(resp
            .error
            .unwrap_or_else(|| "control command failed (no error message)".to_string()))
    }
}

/// Subscribe to the app's event stream (port of `t1_subscribe.py`). Opens a
/// dedicated long-lived connection, sends `__subscribe_events`, and calls
/// `on_frame` for the ack and every subsequent NDJSON frame until EOF/error.
/// Runs until the connection closes (Ctrl-C the process to stop early).
pub fn subscribe<F: FnMut(Value)>(ep: &Endpoint, mut on_frame: F) -> Result<(), String> {
    let stream = TcpStream::connect(&ep.addr)
        .map_err(|e| format!("failed to connect to T-Hub control channel {}: {e}", ep.addr))?;
    let mut writer = stream
        .try_clone()
        .map_err(|e| format!("failed to clone control stream: {e}"))?;
    let request = serde_json::json!({
        "token": ep.token,
        "command": "__subscribe_events",
        "args": {},
        "v": 1,
    });
    let mut line = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    line.push(b'\n');
    writer
        .write_all(&line)
        .map_err(|e| format!("failed to send subscribe request: {e}"))?;
    writer
        .flush()
        .map_err(|e| format!("failed to flush subscribe request: {e}"))?;

    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line.map_err(|e| format!("event stream read error: {e}"))?;
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
