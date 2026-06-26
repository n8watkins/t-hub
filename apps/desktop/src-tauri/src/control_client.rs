//! Client-side transport for the control channel (server-split M1).
//!
//! ## Why this exists
//! M1 ("decouple locally", see `docs/SERVER-SPLIT-AND-ROADMAP.md` §6) routes the
//! GUI↔backend traffic for **server-owned state** through the loopback control
//! socket ([`crate::control`]) instead of in-process Tauri `invoke`/`emit`. The
//! webview's JS cannot open a raw TCP socket, so the socket I/O lives here in the
//! Rust shell: the frontend calls the thin [`control_request`] command (a request
//! frame written to the socket, response awaited off it) and subscribes to a
//! forwarder thread that re-emits the socket's event stream into the webview.
//!
//! On localhost this round-trips through the OS loopback TCP stack — the SAME wire
//! M2 stretches to a remote host. When the server goes remote, only [`endpoint`]'s
//! addr changes; nothing here does.
//!
//! Boundary: [`crate::control`] stays Tauri-free (it is the server half); this
//! module is the Tauri-aware client half (the `#[tauri::command]` + `AppHandle`
//! event re-emit). They meet at the NDJSON wire protocol and the shared
//! [`crate::control::EventFanout`].

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::agent::EventEmitter;
use crate::control::{self, EventFanout};

/// How long to wait for the loopback connect / a response line before giving up.
/// Generous for a same-host round-trip; M2 may widen this for a remote server.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// The Tauri event channel the forwarder re-emits each socket event frame on. The
/// frontend's control-event hub (`src/ipc/controlClient.ts`) subscribes to it and
/// fans out by inner `channel`. Distinct from the raw `session://status` etc. so a
/// migrated channel has exactly one source (the socket), never a double with the
/// still-live in-process Tauri emit.
pub const CONTROL_EVENT: &str = "control://event";

/// Where + how the client transport reaches the control listener: the bound
/// loopback address and the per-launch auth token. Managed in Tauri state so the
/// [`control_request`] command can find the socket. Local-only in M1; M2 swaps the
/// addr for a remote/Tailscale one (and §8's auth-beyond-loopback gates that).
#[derive(Clone)]
pub struct ControlEndpoint {
    pub addr: String,
    pub token: String,
}

/// Open a one-shot connection to the control listener, send one request frame, and
/// await one response line. Connections are short-lived by design (the listener is
/// built for one MCP round-trip per connection); pooling is a later M1 widening.
fn request(addr: &str, token: &str, command: &str, args: &Value) -> Result<Value, String> {
    let socket: SocketAddr = addr
        .parse()
        .map_err(|e| format!("control_request: bad control addr {addr:?}: {e}"))?;
    let stream = TcpStream::connect_timeout(&socket, IO_TIMEOUT)
        .map_err(|e| format!("control_request: connect to {addr} failed: {e}"))?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok();

    let mut writer = stream
        .try_clone()
        .map_err(|e| format!("control_request: clone stream failed: {e}"))?;
    let mut frame = serde_json::to_vec(&json!({
        "token": token,
        "command": command,
        "args": args,
        "v": control::PROTOCOL_VERSION,
    }))
    .map_err(|e| format!("control_request: serialize request failed: {e}"))?;
    frame.push(b'\n');
    writer
        .write_all(&frame)
        .and_then(|()| writer.flush())
        .map_err(|e| format!("control_request: write '{command}' failed: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .map_err(|e| format!("control_request: read response for '{command}' failed: {e}"))?;
    if n == 0 {
        return Err(format!(
            "control_request: connection closed before a response for '{command}'"
        ));
    }
    let resp: Value = serde_json::from_str(line.trim())
        .map_err(|e| format!("control_request: malformed response for '{command}': {e}"))?;
    if resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("control_request: unknown error")
            .to_string())
    }
}

/// Thin frontend transport command: round-trip ONE control command over the
/// loopback socket and return its `result` (or the dispatcher's error). The
/// frontend's `ipc/controlClient.ts` wraps this; migrated `client05`/`client`
/// wrappers call it instead of a direct in-process `invoke`.
#[tauri::command]
pub fn control_request(
    endpoint: tauri::State<'_, ControlEndpoint>,
    command: String,
    args: Option<Value>,
) -> Result<Value, String> {
    request(
        &endpoint.addr,
        &endpoint.token,
        &command,
        &args.unwrap_or(Value::Null),
    )
}

/// An [`EventEmitter`] that writes every backend event to the control socket's
/// [`EventFanout`] (i.e. out over the wire to subscribed clients). Composed with
/// the in-process `TauriEmitter` via [`TeeEmitter`] so events go to BOTH the local
/// webview (un-migrated channels) and the socket (migrated channels) — letting M1
/// flip channels onto the wire one at a time with zero risk to the rest.
pub struct SocketEmitter {
    fanout: Arc<EventFanout>,
}

impl SocketEmitter {
    pub fn new(fanout: Arc<EventFanout>) -> Self {
        Self { fanout }
    }
}

impl EventEmitter for SocketEmitter {
    fn emit_json(&self, channel: &str, payload: &Value) {
        self.fanout.emit_event(channel, payload);
    }
}

/// Forward every emit to two sinks. Used to run the existing [`crate::agent::TauriEmitter`]
/// and the new [`SocketEmitter`] side by side during the M1 migration.
pub struct TeeEmitter {
    a: Arc<dyn EventEmitter>,
    b: Arc<dyn EventEmitter>,
}

impl TeeEmitter {
    pub fn new(a: Arc<dyn EventEmitter>, b: Arc<dyn EventEmitter>) -> Self {
        Self { a, b }
    }
}

impl EventEmitter for TeeEmitter {
    fn emit_json(&self, channel: &str, payload: &Value) {
        self.a.emit_json(channel, payload);
        self.b.emit_json(channel, payload);
    }
}

/// Spawn the background forwarder: connect to the control listener as an event
/// subscriber, read `{event,payload}` frames off the socket, and re-emit each into
/// the webview as [`CONTROL_EVENT`]. This is the receive half of the event wire —
/// in M1 it loops back to the same process; in M2 the same loop points at a remote
/// server. Reconnects with a small backoff so a dropped/late listener self-heals.
pub fn spawn_event_forwarder(app: AppHandle, addr: String, token: String) {
    std::thread::Builder::new()
        .name("t-hub-control-forward".into())
        .spawn(move || {
            // EXPONENTIAL BACKOFF on reconnect (M2b): a live connection that ENDS
            // (Ok — server closed/restarted) retries promptly and resets the backoff;
            // a connect/transport FAILURE (Err — server down/unreachable) backs off
            // 250ms→10s so we don't hammer a down remote server. In M1/loopback the
            // listener never dies, so this loop parks inside `forward_once`.
            // (Jitter is unnecessary for one client; add it for M4's many clients.)
            let mut backoff = Duration::from_millis(250);
            let max_backoff = Duration::from_secs(10);
            // A connection that lived at least this long is "healthy" — its end is a
            // server close/restart, so retry promptly + reset. A shorter Ok (accepted
            // then dropped immediately — e.g. the remote's peer gate rejected our
            // source IP) is treated like a failure so we back off instead of
            // tight-looping at ~4 Hz against a remote that keeps closing us.
            let healthy_after = Duration::from_secs(1);
            loop {
                let started = std::time::Instant::now();
                let result = forward_once(&app, &addr, &token);
                let lived = started.elapsed();
                match result {
                    Ok(()) if lived >= healthy_after => {
                        backoff = Duration::from_millis(250);
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    Ok(()) => {
                        std::thread::sleep(backoff);
                        backoff = (backoff * 2).min(max_backoff);
                    }
                    Err(e) => {
                        eprintln!(
                            "t-hub-control: event forwarder reconnect failed: {e} (retry in {backoff:?})"
                        );
                        std::thread::sleep(backoff);
                        backoff = (backoff * 2).min(max_backoff);
                    }
                }
            }
        })
        .ok();
}

/// One subscribe-and-stream cycle. Connects, sends the subscribe request, then
/// blocks re-emitting each event frame until the connection drops (returns Ok on a
/// clean EOF, Err on a transport error — both trigger a reconnect).
fn forward_once(app: &AppHandle, addr: &str, token: &str) -> Result<(), String> {
    let socket: SocketAddr = addr
        .parse()
        .map_err(|e| format!("bad control addr {addr:?}: {e}"))?;
    let stream = TcpStream::connect_timeout(&socket, IO_TIMEOUT)
        .map_err(|e| format!("connect to {addr} failed: {e}"))?;
    let mut writer = stream
        .try_clone()
        .map_err(|e| format!("clone stream failed: {e}"))?;
    let mut frame = serde_json::to_vec(&json!({
        "token": token,
        "command": control::SUBSCRIBE_COMMAND,
        "args": {},
        "v": control::PROTOCOL_VERSION,
    }))
    .map_err(|e| e.to_string())?;
    frame.push(b'\n');
    writer
        .write_all(&frame)
        .and_then(|()| writer.flush())
        .map_err(|e| format!("subscribe write failed: {e}"))?;

    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line.map_err(|e| format!("read frame failed: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let frame: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue, // skip a malformed frame rather than tearing down
        };
        // A subscribe REJECTION ({"ok":false,error}) — bad token or a protocol
        // version mismatch (M2b) — surfaces here. Return it (with the server's
        // message) so the reconnect loop logs a clear cause and backs off, instead
        // of silently skipping it and parking until the idle timeout.
        if frame.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            let msg = frame
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("subscribe rejected");
            return Err(format!("subscribe rejected: {msg}"));
        }
        // The subscribe ack ({"ok":true,...}) carries no "event"; skip it.
        let Some(channel) = frame.get("event").and_then(|v| v.as_str()) else {
            continue;
        };
        let payload = frame.get("payload").cloned().unwrap_or(Value::Null);
        if let Err(e) = app.emit(CONTROL_EVENT, json!({ "channel": channel, "payload": payload })) {
            eprintln!("t-hub-control: re-emit of {channel} failed: {e}");
        }
    }
    Ok(())
}

/// Manage the [`ControlEndpoint`] and start the event forwarder once the control
/// listener is bound. Called from `setup()` right after `control::start` returns
/// the handshake (addr+token), so both halves of the wire share one source of
/// truth and the forwarder connects to an already-accepting listener.
pub fn install(app: &AppHandle, handshake: &control::ControlHandshake) {
    // Server-split M2b: a REMOTE-client override. When T_HUB_REMOTE_ADDR (+
    // T_HUB_REMOTE_TOKEN) is set, this GUI is a THIN CLIENT to a remote server —
    // control_request, the event forwarder, AND the terminal RemotePty (all read
    // ControlEndpoint) target the remote control socket instead of this machine's
    // loopback. Unset (the default) ⇒ the local loopback server, exactly as today.
    let (addr, token) = match (
        std::env::var("T_HUB_REMOTE_ADDR").ok().filter(|a| !a.is_empty()),
        std::env::var("T_HUB_REMOTE_TOKEN").ok().filter(|t| !t.is_empty()),
    ) {
        (Some(addr), Some(token)) => {
            eprintln!(
                "t-hub: REMOTE client mode — control endpoint = {addr} (T_HUB_REMOTE_ADDR); \
                 tiles + events + reads target the remote server"
            );
            (addr, token)
        }
        _ => (handshake.addr.clone(), handshake.token.clone()),
    };
    app.manage(ControlEndpoint {
        addr: addr.clone(),
        token: token.clone(),
    });
    spawn_event_forwarder(app.clone(), addr, token);
}
