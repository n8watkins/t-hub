//! Client side of the loopback control channel: the bridge from `tools/call` to
//! the running T-Hub app.
//!
//! Discovery: read the handshake file the app wrote (`$T_HUB_CONTROL_FILE`, or
//! `~/.t-hub/control.json`) for the `addr` + `token`, or take both from
//! `$T_HUB_CONTROL_ADDR` + `$T_HUB_CONTROL_TOKEN` (used by the proof harness and
//! when the app's path differs). These inputs are captured once into a
//! [`Discovery`] value at startup so the rest of the crate resolves endpoints
//! from an injected config rather than process-global env (which keeps the tests
//! hermetic under parallel execution). Each call opens a short-lived TCP
//! connection to `addr`, sends one NDJSON request line, and reads one NDJSON
//! response line. Connections are not pooled - `tools/call` is infrequent and a
//! fresh connection keeps the client stateless and robust to app restarts.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

/// Per-attempt socket read timeout. A response that has not started arriving
/// within this window bounces to the overall-deadline retry loop (the command may
/// have been accepted and its response is merely late), rather than surfacing an
/// ambiguous error on the first idle.
const READ_TIMEOUT_PER_ATTEMPT: Duration = Duration::from_secs(10);

/// Overall deadline for reading one response before the round-trip is declared a
/// transport failure. Retrying WouldBlock up to here (instead of failing at the
/// first 15s idle, as before) directly fixes the ask-#2 client leg: a briefly
/// busy/wedged app still gets its late response delivered.
const READ_OVERALL_DEADLINE: Duration = Duration::from_secs(45);

/// How long to keep resolving an AMBIGUOUS spawn-class failure via
/// `get_request_status` (polling while the original is still in flight) before
/// giving up and telling the caller to poll it themselves.
const AMBIGUOUS_RESOLVE_DEADLINE: Duration = Duration::from_secs(30);

/// Spawn-class commands whose retries must dedup via a client `requestId`
/// (mirrors the app-side `is_idempotent_command`).
fn is_idempotent_command(command: &str) -> bool {
    matches!(command, "spawn_terminal" | "create_worktree")
}

/// Mint a process-unique idempotency key without pulling in a uuid/rng dependency
/// (this crate is deliberately dependency-light). pid + a monotonic nanosecond
/// clock + a per-process counter is unique enough to key one launch's spawn
/// retries, which is all the server-side cache needs.
fn new_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("mcp-{}-{}-{}", std::process::id(), nanos, n)
}

/// Ensure an idempotent command's args carry a `requestId`, returning the
/// (possibly augmented) args and the id to reuse for every retry of this call.
/// A non-idempotent command is passed through untouched with `None`.
fn ensure_request_id(command: &str, args: &Value) -> (Value, Option<String>) {
    if !is_idempotent_command(command) {
        return (args.clone(), None);
    }
    // Respect a caller-supplied id (e.g. the probe harness), else mint one.
    if let Some(existing) = args
        .get("requestId")
        .or_else(|| args.get("request_id"))
        .and_then(Value::as_str)
    {
        return (args.clone(), Some(existing.to_string()));
    }
    let id = new_request_id();
    let mut augmented = args.clone();
    match &mut augmented {
        Value::Object(map) => {
            map.insert("requestId".to_string(), Value::String(id.clone()));
        }
        // A non-object args (null / scalar): wrap into an object carrying the id.
        _ => {
            augmented = serde_json::json!({ "requestId": id });
        }
    }
    (augmented, Some(id))
}

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

/// The inputs used to locate the control channel, captured up front so that
/// resolution is a pure function of its fields rather than of process-global
/// environment variables. Production builds construct this once with
/// [`Discovery::from_env`]; tests construct it directly, which keeps them
/// hermetic (no shared `T_HUB_CONTROL_*` env mutation that could race across
/// threads when the suite runs in parallel).
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    /// Explicit control address override (`$T_HUB_CONTROL_ADDR`).
    pub addr: Option<String>,
    /// Explicit control token override (`$T_HUB_CONTROL_TOKEN`).
    pub token: Option<String>,
    /// Handshake file path override (`$T_HUB_CONTROL_FILE`); when `None`,
    /// resolution falls back to `~/.t-hub/control.json`.
    pub file: Option<PathBuf>,
    /// Home directory used to derive the default handshake path. When `None`,
    /// it is read from `$HOME`/`$USERPROFILE` at resolution time. Tests set this
    /// to keep the default-path branch off the real environment.
    pub home: Option<PathBuf>,
}

impl Discovery {
    /// Capture discovery inputs from the environment (the production path).
    /// Reading env once, here, means the rest of the crate never touches
    /// process-global state.
    pub fn from_env() -> Self {
        let non_empty = |v: String| if v.is_empty() { None } else { Some(v) };
        Discovery {
            addr: std::env::var("T_HUB_CONTROL_ADDR").ok().and_then(non_empty),
            token: std::env::var("T_HUB_CONTROL_TOKEN")
                .ok()
                .and_then(non_empty),
            file: std::env::var_os("T_HUB_CONTROL_FILE").map(PathBuf::from),
            // Resolved lazily in `handshake_path` so the default branch still
            // honors the live environment in production.
            home: None,
        }
    }

    /// Resolve the control endpoint, explicit addr+token override first, then
    /// the handshake file.
    ///
    /// Returns a descriptive error (not a panic) when the app isn't running /
    /// the handshake file is missing, so the MCP server can surface "T-Hub is
    /// not running" as a tool error rather than crashing.
    pub fn resolve(&self) -> Result<ControlEndpoint, String> {
        // 1. Explicit addr + token override - used by the proof harness and the
        //    app's spawn-tree env injection (`T_HUB_CONTROL_ADDR` +
        //    `T_HUB_CONTROL_TOKEN`). The fast path while the app stays bound; if it
        //    later goes stale (restart onto a new port), `resolve_and_call` falls
        //    back to `resolve_from_file` instead of re-trusting this pair.
        if let (Some(addr), Some(token)) = (&self.addr, &self.token) {
            if !addr.is_empty() && !token.is_empty() {
                return Ok(ControlEndpoint {
                    addr: addr.clone(),
                    token: token.clone(),
                });
            }
        }

        // 2. The handshake file the running app wrote.
        self.resolve_from_file()
    }

    /// Read the endpoint from the handshake file ONLY, ignoring any
    /// `$T_HUB_CONTROL_ADDR`/`$T_HUB_CONTROL_TOKEN` override.
    ///
    /// This is the recovery path after a transport failure: the app rebinds to a
    /// fresh ephemeral port on every restart and rewrites control.json, but a
    /// session's MCP captured the old addr+token in its env at spawn time. Once
    /// that env pin points at a dead port, the live endpoint lives only in the
    /// file - so we re-read it here rather than re-trusting the stale env pair.
    pub fn resolve_from_file(&self) -> Result<ControlEndpoint, String> {
        let path = self.handshake_path();
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

    /// The handshake file path (mirrors `crate::control::handshake_path` on the
    /// app side): the `file` override, else `<home>/.t-hub/control.json`.
    fn handshake_path(&self) -> PathBuf {
        if let Some(p) = &self.file {
            return p.clone();
        }
        let home = self
            .home
            .clone()
            .or_else(|| {
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".t-hub").join("control.json")
    }
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

/// Why a single control round-trip failed, so the retry layer can tell a moved
/// endpoint apart from a command the app deliberately rejected.
enum CallError {
    /// Transport-level: connect refused/timed out, or the stream died (or spoke
    /// garbage) mid round-trip. A restarted app on a new ephemeral port looks
    /// exactly like this, so the caller re-reads control.json and retries.
    Transport(String),
    /// The app answered and rejected the command (bad token, unknown command,
    /// governor refusal). A different endpoint won't change the verdict.
    App(String),
}

impl CallError {
    fn into_message(self) -> String {
        match self {
            CallError::Transport(m) | CallError::App(m) => m,
        }
    }
}

/// Resolve the control endpoint and run one command, transparently recovering
/// from an app restart.
///
/// The app rebinds to a fresh ephemeral port on every launch and rewrites
/// control.json, but a session's MCP captured the old addr+token in its env at
/// spawn time (see `elevation_env` on the app side). So when the resolved
/// endpoint is dead (a transport failure), we re-read control.json - dropping
/// the stale env pair entirely - and retry once against the addr+token the
/// running app just wrote, instead of wrongly concluding "T-Hub is down".
/// App-level rejections are returned verbatim (a new endpoint won't change them).
pub fn resolve_and_call(
    discovery: &Discovery,
    command: &str,
    args: &Value,
) -> Result<Value, String> {
    // Idempotency (ask #1): a spawn-class command carries a `requestId` so every
    // retry below dedups server-side (a retry never double-applies; a completed
    // outcome is replayed). The SAME id is reused for the initial call and every
    // recovery path.
    let (args, request_id) = ensure_request_id(command, args);
    let endpoint = discovery.resolve()?;
    match call_classified(&endpoint, command, &args) {
        Ok(v) => Ok(v),
        Err(CallError::App(msg)) => Err(msg),
        Err(CallError::Transport(first)) => {
            // The endpoint we tried is unreachable/unresponsive. If control.json now
            // names a *different* endpoint (the app restarted onto a new port, so
            // our env pin went stale), prefer the freshly-read addr+token.
            let fresh = discovery
                .resolve_from_file()
                .ok()
                .filter(|f| f.addr != endpoint.addr || f.token != endpoint.token);

            // For a spawn-class command with a requestId the transport failure is
            // AMBIGUOUS: the command may have applied server-side before the
            // response leg died (Incident A/B/D). Resolve it authoritatively via
            // get_request_status instead of blindly retrying (which historically
            // made the duplicate) or failing (which lost the ghost).
            if let Some(id) = &request_id {
                let ep = fresh.unwrap_or(endpoint);
                return resolve_ambiguous_request(&ep, command, &args, id, first);
            }

            // Non-idempotent command: keep the existing single restart-recovery
            // retry against a genuinely-different endpoint, else surface the error.
            match fresh {
                Some(f) => call_classified(&f, command, &args).map_err(CallError::into_message),
                None => Err(first),
            }
        }
    }
}

/// A socket read timeout / would-block surfaces as `WouldBlock` (unix SO_RCVTIMEO)
/// or `TimedOut` (windows). Both mean "no data yet", not a dead transport.
fn is_would_block(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// Resolve an ambiguous spawn-class transport failure (ask #1/#2): the command was
/// possibly accepted but its response leg failed. Query `get_request_status` for
/// the SAME `request_id` and act on the authoritative answer:
///   - completed(ok)   -> return the original result (the apply happened once)
///   - completed(err)   -> return that error (it ran and failed; no ghost)
///   - inFlight         -> poll until it resolves or the deadline, then hand the
///                         caller the requestId to poll themselves
///   - unknown          -> it never landed under this id: safe to re-run ONCE (the
///                         same requestId keeps that retry idempotent)
/// If the status channel itself stays unreachable, we surface the original error.
fn resolve_ambiguous_request(
    endpoint: &ControlEndpoint,
    command: &str,
    args: &Value,
    request_id: &str,
    first_err: String,
) -> Result<Value, String> {
    let deadline = Instant::now() + AMBIGUOUS_RESOLVE_DEADLINE;
    let status_args = serde_json::json!({ "requestId": request_id });
    loop {
        match call_classified(endpoint, "get_request_status", &status_args) {
            Ok(v) => match v.get("status").and_then(Value::as_str) {
                Some("completed") => {
                    if v.get("ok").and_then(Value::as_bool) == Some(true) {
                        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                    }
                    return Err(v
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("control command failed (no error message)")
                        .to_string());
                }
                Some("inFlight") => {
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "{first_err}; the request was accepted (requestId '{request_id}') \
                             but is still in flight - poll get_request_status for its outcome"
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                // "unknown" (or a server that answered oddly): the command never
                // landed under this id, so re-running it once is safe + idempotent.
                _ => {
                    return call_classified(endpoint, command, args)
                        .map_err(CallError::into_message);
                }
            },
            // The app answered but rejected the STATUS query itself - most likely an
            // older app that predates get_request_status (no server-side cache, so
            // no idempotency guarantee). Don't guess: surface the original error.
            Err(CallError::App(_)) => return Err(first_err),
            // The channel is still unreachable: keep trying to reach the status
            // endpoint until the deadline, else give up with the original error.
            Err(CallError::Transport(_)) => {
                if Instant::now() >= deadline {
                    return Err(first_err);
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

/// Forward one command to the app and return its `result` JSON, or an error
/// string - the single-shot primitive used by the crate's tests. Production code
/// goes through [`resolve_and_call`], which adds the restart-recovery retry.
#[cfg(test)]
fn call(endpoint: &ControlEndpoint, command: &str, args: &Value) -> Result<Value, String> {
    call_classified(endpoint, command, args).map_err(CallError::into_message)
}

/// The single round-trip, with its failure classified so [`resolve_and_call`]
/// knows whether re-reading control.json could recover it.
fn call_classified(
    endpoint: &ControlEndpoint,
    command: &str,
    args: &Value,
) -> Result<Value, CallError> {
    let request = serde_json::json!({
        "token": endpoint.token,
        "command": command,
        "args": args,
    });

    let stream = TcpStream::connect(&endpoint.addr).map_err(|e| {
        CallError::Transport(format!(
            "failed to connect to T-Hub control channel {}: {e}",
            endpoint.addr
        ))
    })?;
    // Bounded timeouts so a hung app surfaces as a tool error, not a stuck MCP
    // server. The per-attempt read timeout is short; the read loop below retries
    // WouldBlock up to READ_OVERALL_DEADLINE so a merely-late response (the app was
    // briefly busy/wedged) is still delivered rather than surfaced as an ambiguous
    // failure on the first idle (ask #2, client leg).
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT_PER_ATTEMPT));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));

    let mut writer = stream
        .try_clone()
        .map_err(|e| CallError::Transport(format!("failed to clone control stream: {e}")))?;
    let mut line = serde_json::to_vec(&request).map_err(|e| CallError::Transport(e.to_string()))?;
    line.push(b'\n');
    writer
        .write_all(&line)
        .map_err(|e| CallError::Transport(format!("failed to send control request: {e}")))?;
    writer
        .flush()
        .map_err(|e| CallError::Transport(format!("failed to flush control request: {e}")))?;

    let mut reader = BufReader::new(stream);
    let mut resp_line = String::new();
    let deadline = Instant::now() + READ_OVERALL_DEADLINE;
    loop {
        match reader.read_line(&mut resp_line) {
            Ok(0) => {
                return Err(CallError::Transport(
                    "T-Hub control channel closed without responding".to_string(),
                ));
            }
            // A full line arrived (read_line stops at the newline).
            Ok(_) => break,
            // A per-attempt read timeout: the response is late, not (yet) gone.
            // Keep waiting until the overall deadline before declaring the
            // round-trip ambiguous - the command may already have been accepted.
            Err(e) if is_would_block(&e) => {
                if Instant::now() >= deadline {
                    return Err(CallError::Transport(format!(
                        "failed to read control response: {e} (no response within {}s)",
                        READ_OVERALL_DEADLINE.as_secs()
                    )));
                }
            }
            Err(e) => {
                return Err(CallError::Transport(format!(
                    "failed to read control response: {e}"
                )));
            }
        }
    }

    let resp: ControlResponse = serde_json::from_str(resp_line.trim_end()).map_err(|e| {
        CallError::Transport(format!(
            "malformed control response: {e} (raw: {})",
            resp_line.trim_end()
        ))
    })?;

    if resp.ok {
        Ok(resp.result.unwrap_or(Value::Null))
    } else {
        Err(CallError::App(resp.error.unwrap_or_else(|| {
            "control command failed (no error message)".to_string()
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A fake control server that services a scripted SEQUENCE of connections. For
    /// each entry it accepts one connection, reads the one request line (captured
    /// for assertions), then either writes `Some(reply)` or, on `None`, drops the
    /// connection WITHOUT responding - reproducing a failed response leg (Incident
    /// A/B/D). Returns its addr plus the shared capture of every request seen.
    fn scripted_server(replies: Vec<Option<&'static str>>) -> (String, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let cap = captured.clone();
        std::thread::spawn(move || {
            for reply in replies {
                let Ok((stream, _)) = listener.accept() else { break };
                let mut writer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    if let Ok(v) = serde_json::from_str::<Value>(line.trim_end()) {
                        cap.lock().unwrap().push(v);
                    }
                }
                if let Some(body) = reply {
                    let _ = writer.write_all(body.as_bytes());
                    let _ = writer.write_all(b"\n");
                    let _ = writer.flush();
                }
                // `None`: drop the connection with no response (failed response leg).
            }
        });
        (addr, captured)
    }

    fn discovery_for(addr: String) -> Discovery {
        Discovery {
            addr: Some(addr),
            token: Some("tok".into()),
            // A file that does not exist so the restart-recovery re-read finds
            // nothing fresher and the ambiguity path reuses the same endpoint.
            file: Some(PathBuf::from("/nonexistent/th-control.json")),
            ..Default::default()
        }
    }

    #[test]
    fn spawn_class_call_injects_a_request_id() {
        let (addr, captured) = scripted_server(vec![Some(r#"{"ok":true,"result":{"id":"s"}}"#)]);
        resolve_and_call(&discovery_for(addr), "spawn_terminal", &serde_json::json!({"cwd": "/tmp"}))
            .unwrap();
        let reqs = captured.lock().unwrap();
        assert!(
            reqs[0]["args"]["requestId"].as_str().is_some(),
            "a spawn-class call must carry a requestId: {:?}",
            reqs[0]
        );
    }

    #[test]
    fn non_idempotent_call_does_not_inject_a_request_id() {
        let (addr, captured) = scripted_server(vec![Some(r#"{"ok":true,"result":{}}"#)]);
        resolve_and_call(&discovery_for(addr), "list_tabs", &Value::Null).unwrap();
        let reqs = captured.lock().unwrap();
        assert!(
            reqs[0]["args"].get("requestId").is_none(),
            "a read command must not get a requestId"
        );
    }

    #[test]
    fn ambiguous_response_leg_resolves_to_the_completed_outcome() {
        // The spawn's response leg fails (conn 1 closes with no reply), but the
        // command DID apply. The client resolves it via get_request_status (conn 2)
        // using the SAME requestId, and returns the original result - no duplicate.
        let (addr, captured) = scripted_server(vec![
            None, // spawn_terminal: accepted, response leg dies
            Some(r#"{"ok":true,"result":{"status":"completed","ok":true,"result":{"id":"sess-1"}}}"#),
        ]);
        let v = resolve_and_call(&discovery_for(addr), "spawn_terminal", &serde_json::json!({"cwd": "/tmp"}))
            .unwrap();
        assert_eq!(v["id"], "sess-1", "returns the completed spawn's result");
        let reqs = captured.lock().unwrap();
        let rid = reqs[0]["args"]["requestId"].as_str().unwrap();
        assert_eq!(reqs[1]["command"], "get_request_status");
        assert_eq!(
            reqs[1]["args"]["requestId"].as_str().unwrap(),
            rid,
            "the status query reuses the original requestId"
        );
    }

    #[test]
    fn ambiguous_response_leg_reruns_once_when_status_is_unknown() {
        // The spawn's response leg fails AND the server never saw it (status
        // unknown: it did not land). The client safely re-runs it ONCE with the
        // same requestId, which now succeeds.
        let (addr, captured) = scripted_server(vec![
            None,                                             // spawn 1: response leg dies
            Some(r#"{"ok":true,"result":{"status":"unknown"}}"#), // status: never landed
            Some(r#"{"ok":true,"result":{"id":"sess-2","accepted":"spawn_terminal"}}"#), // retry ok
        ]);
        let v = resolve_and_call(&discovery_for(addr), "spawn_terminal", &serde_json::json!({"cwd": "/tmp"}))
            .unwrap();
        assert_eq!(v["id"], "sess-2");
        let reqs = captured.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[2]["command"], "spawn_terminal");
        assert_eq!(
            reqs[2]["args"]["requestId"], reqs[0]["args"]["requestId"],
            "the re-run reuses the same requestId so it stays idempotent"
        );
    }

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
        // Write a temp handshake and point a Discovery at it. No env mutation:
        // the config is injected, so this stays hermetic under parallel runs.
        let dir = std::env::temp_dir().join(format!("th-mcp-hs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");
        std::fs::write(
            &file,
            r#"{"addr":"127.0.0.1:9999","token":"filetok","pid":1}"#,
        )
        .unwrap();

        let discovery = Discovery {
            file: Some(file),
            ..Default::default()
        };
        let ep = discovery.resolve().unwrap();
        assert_eq!(ep.addr, "127.0.0.1:9999");
        assert_eq!(ep.token, "filetok");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_endpoint_prefers_addr_token_override() {
        let discovery = Discovery {
            addr: Some("127.0.0.1:1".into()),
            token: Some("envtok".into()),
            // File path is ignored when addr+token are present.
            file: Some(PathBuf::from("/nonexistent/control.json")),
            ..Default::default()
        };
        let ep = discovery.resolve().unwrap();
        assert_eq!(ep.addr, "127.0.0.1:1");
        assert_eq!(ep.token, "envtok");
    }

    #[test]
    fn resolve_endpoint_missing_file_is_descriptive_error() {
        let discovery = Discovery {
            file: Some(PathBuf::from("/nonexistent/th-control.json")),
            ..Default::default()
        };
        let err = discovery.resolve().unwrap_err();
        assert!(err.contains("control channel not found"), "err: {err}");
    }

    #[test]
    fn resolve_and_call_recovers_after_app_restart() {
        // Reproduce the real failure: a session's MCP was spawned BEFORE an app
        // restart, so its env pins the now-dead addr+token, while control.json
        // carries the addr+token the restarted app just wrote. (Both change here - // proving the recovery drops the stale env PAIR, not just the addr.)
        let dir = std::env::temp_dir().join(format!("th-mcp-restart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control.json");

        // The "restarted" app: a fresh listener on a new port with a new token,
        // and control.json pointing at it (what the live app wrote on relaunch).
        let live_addr = fake_server("filetok", r#"{"ok":true,"result":{"hello":"world"}}"#);
        std::fs::write(
            &file,
            format!(r#"{{"addr":"{live_addr}","token":"filetok","pid":1}}"#),
        )
        .unwrap();

        // The dead pre-restart endpoint: bind to grab a port, then drop it so
        // connects are refused (the old ephemeral port the app abandoned).
        let dead = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = dead.local_addr().unwrap().to_string();
        drop(dead);

        let discovery = Discovery {
            addr: Some(dead_addr.clone()),
            token: Some("envtok".into()),
            file: Some(file.clone()),
            ..Default::default()
        };

        // Red path: the naive single-shot against the env-pinned endpoint fails,
        // because that port died when the app restarted.
        let stale = discovery.resolve().unwrap();
        assert_eq!(stale.addr, dead_addr, "resolve still prefers the env pin");
        assert!(
            call(&stale, "list_tabs", &Value::Null).is_err(),
            "the dead endpoint must fail to connect"
        );

        // Green path: resolve_and_call drops the stale env pair, re-reads
        // control.json, and reconnects to the live post-restart endpoint+token.
        let v = resolve_and_call(&discovery, "list_tabs", &Value::Null).unwrap();
        assert_eq!(v["hello"], "world");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_and_call_app_error_is_not_retried() {
        // An app that answers with a rejection is NOT a moved endpoint: the error
        // surfaces verbatim without a control.json re-read/retry.
        let addr = fake_server("tok", r#"{"ok":false,"error":"boom"}"#);
        let discovery = Discovery {
            addr: Some(addr),
            token: Some("tok".into()),
            // A file that does not exist: if this path retried on disk it would
            // change the error; asserting "boom" proves it did not.
            file: Some(PathBuf::from("/nonexistent/th-control.json")),
            ..Default::default()
        };
        let err = resolve_and_call(&discovery, "list_tabs", &Value::Null).unwrap_err();
        assert_eq!(err, "boom");
    }
}
