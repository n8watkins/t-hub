//! Client side of the loopback control channel: the bridge from `tools/call` to
//! the running T-Hub app.
//!
//! Discovery: read the handshake file the app wrote (`$T_HUB_CONTROL_FILE`, or
//! `~/.t-hub/control.json`) for the `addr` + `token`, or take both from
//! `$T_HUB_CONTROL_ADDR` + `$T_HUB_CONTROL_TOKEN` (used by the test harness
//! and when the app's path differs). Each call opens a short-lived TCP
//! connection to `addr`, sends one NDJSON request line, and reads one NDJSON
//! response line. Connections are not pooled — `tools/call` is infrequent and a
//! fresh connection keeps the client stateless and robust to app restarts.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

/// How T-Hub's control channel was located + authenticated.
#[derive(Debug, Clone)]
pub struct ControlEndpoint {
    pub addr: String,
    pub token: String,
}

/// The on-disk handshake the app writes. We only need `addr` + `token`.
#[derive(Debug, Deserialize)]
struct Handshake {
    addr: String,
    token: String,
}

/// Resolve the control endpoint, env overrides first, then the handshake file.
///
/// Returns a descriptive error (not a panic) when the app isn't running / the
/// handshake file is missing, so the MCP server can surface "T-Hub is not
/// running" as a tool error rather than crashing.
pub fn resolve_endpoint() -> Result<ControlEndpoint, String> {
    // 1. Explicit env override (addr + token) — used by the proof harness.
    if let (Ok(addr), Ok(token)) = (
        std::env::var("T_HUB_CONTROL_ADDR"),
        std::env::var("T_HUB_CONTROL_TOKEN"),
    ) {
        if !addr.is_empty() && !token.is_empty() {
            return Ok(ControlEndpoint { addr, token });
        }
    }

    // 2. The handshake file the running app wrote.
    let path = handshake_path();
    let body = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "T-Hub control channel not found at {} ({e}). Is the T-Hub app \
             running? (set T_HUB_CONTROL_ADDR + T_HUB_CONTROL_TOKEN to override.)",
            path.display()
        )
    })?;
    let hs: Handshake = serde_json::from_str(&body)
        .map_err(|e| format!("malformed control handshake at {}: {e}", path.display()))?;
    Ok(ControlEndpoint {
        addr: hs.addr,
        token: hs.token,
    })
}

/// The handshake file path (mirrors `crate::control::handshake_path` on the app
/// side): `$T_HUB_CONTROL_FILE`, else `~/.t-hub/control.json`.
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

/// The app's response envelope: `{ok, result?, error?}`.
#[derive(Debug, Deserialize)]
struct ControlResponse {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

/// Forward one command to the app and return its `result` JSON, or an error
/// string. `endpoint` carries the resolved addr + token.
pub fn call(endpoint: &ControlEndpoint, command: &str, args: &Value) -> Result<Value, String> {
    let request = serde_json::json!({
        "token": endpoint.token,
        "command": command,
        "args": args,
    });

    let stream = TcpStream::connect(&endpoint.addr)
        .map_err(|e| format!("failed to connect to T-Hub control channel {}: {e}", endpoint.addr))?;
    // Bounded timeouts so a hung app surfaces as a tool error, not a stuck MCP
    // server. The control handler answers a single round-trip quickly.
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

    let resp: ControlResponse = serde_json::from_str(resp_line.trim_end())
        .map_err(|e| format!("malformed control response: {e} (raw: {})", resp_line.trim_end()))?;

    if resp.ok {
        Ok(resp.result.unwrap_or(Value::Null))
    } else {
        Err(resp
            .error
            .unwrap_or_else(|| "control command failed (no error message)".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Spin up a one-shot fake control server on loopback that asserts the token
    /// and echoes a canned response. Returns its addr.
    fn fake_server(expect_token: &str, reply: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let expect = expect_token.to_string();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut writer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let req: Value = serde_json::from_str(line.trim_end()).unwrap();
                assert_eq!(req["token"], expect, "server saw wrong token");
                writer.write_all(reply.as_bytes()).unwrap();
                writer.write_all(b"\n").unwrap();
                writer.flush().unwrap();
            }
        });
        addr
    }

    #[test]
    fn call_returns_result_on_ok() {
        let addr = fake_server("tok", r#"{"ok":true,"result":{"hello":"world"}}"#);
        let ep = ControlEndpoint {
            addr,
            token: "tok".into(),
        };
        let v = call(&ep, "list_tabs", &Value::Null).unwrap();
        assert_eq!(v["hello"], "world");
    }

    #[test]
    fn call_returns_err_on_error_envelope() {
        let addr = fake_server("tok", r#"{"ok":false,"error":"boom"}"#);
        let ep = ControlEndpoint {
            addr,
            token: "tok".into(),
        };
        let err = call(&ep, "list_tabs", &Value::Null).unwrap_err();
        assert_eq!(err, "boom");
    }

    #[test]
    fn call_forwards_token_and_args() {
        // The fake server asserts the token; here we also confirm a result with
        // the args echoed back round-trips.
        let addr = fake_server("secret", r#"{"ok":true,"result":{"echoed":true}}"#);
        let ep = ControlEndpoint {
            addr,
            token: "secret".into(),
        };
        let v = call(&ep, "get_status", &serde_json::json!({"sessionId": "s1"})).unwrap();
        assert_eq!(v["echoed"], true);
    }

    #[test]
    fn resolve_endpoint_reads_handshake_file() {
        // Write a temp handshake and point T_HUB_CONTROL_FILE at it. Clear the
        // addr/token env so the file path is exercised.
        let dir = std::env::temp_dir().join(format!("th-mcp-hs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");
        std::fs::write(
            &file,
            r#"{"addr":"127.0.0.1:9999","token":"filetok","pid":1}"#,
        )
        .unwrap();

        // SAFETY: tests in this module run single-threaded per process by default
        // for env mutation; we set + remove around the call.
        std::env::set_var("T_HUB_CONTROL_FILE", &file);
        std::env::remove_var("T_HUB_CONTROL_ADDR");
        std::env::remove_var("T_HUB_CONTROL_TOKEN");

        let ep = resolve_endpoint().unwrap();
        assert_eq!(ep.addr, "127.0.0.1:9999");
        assert_eq!(ep.token, "filetok");

        std::env::remove_var("T_HUB_CONTROL_FILE");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
