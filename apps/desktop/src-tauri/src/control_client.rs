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

use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::agent::EventEmitter;
use crate::control::{self, EventFanout};

/// One ordinary request, including connect, write, read, endpoint invalidation,
/// and one retry, must finish within this wall-clock budget.
const CONTROL_DEADLINE: Duration = Duration::from_secs(10);

/// A stale endpoint gets only a short slice before recovery checks the current
/// handshake. Long orchestration responses are exempt from this slice but remain
/// bounded by their single overall deadline.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);

/// Every control client accepts at most 1 MiB before the NDJSON response newline.
/// This bounds memory, parsing work, and any structured error derived from a peer.
const MAX_RESPONSE_FRAME_BYTES: usize = 1024 * 1024;

/// Commissioning, dispatch, and Cortana recovery cross bounded git, tmux, and
/// harness-start operations. Their response window must outlive the server's
/// normal request phase so the client receives the authoritative result instead
/// of abandoning a mutation that is still running.
const LONG_ORCHESTRATION_TIMEOUT: Duration = Duration::from_secs(120);

fn response_timeout_for_command(command: &str) -> Duration {
    match command {
        "commission_captain" | "dispatch_crew" | "history_resume" | "reconcile_cortana"
        | "start_agent" => LONG_ORCHESTRATION_TIMEOUT,
        _ => CONTROL_DEADLINE,
    }
}

/// The Tauri event channel the forwarder re-emits each socket event frame on. The
/// frontend's control-event hub (`src/ipc/controlClient.ts`) subscribes to it and
/// fans out by inner `channel`. Distinct from the raw `session://status` etc. so a
/// migrated channel has exactly one source (the socket), never a double with the
/// still-live in-process Tauri emit.
pub const CONTROL_EVENT: &str = "control://event";

/// Where + how the client transport reaches the control listener: the bound
/// loopback address and the per-launch auth token. Managed in Tauri state (as an
/// `Arc`) so the [`control_request`] command, the event forwarder, and terminal
/// attach all share ONE endpoint. Local-only in M1; M2 swaps the addr for a
/// remote/Tailscale one (and §8's auth-beyond-loopback gates that).
///
/// RELAY-WEDGE SELF-HEAL (PR #50): the `addr` is interior-mutable because the
/// server can rotate its listener port under us - `rebind_control` moves the
/// listener to a fresh port to escape a wedged WSL loopback relay flow and rewrites
/// `control.json`. The app's OWN client half lives on healthy Windows loopback (the
/// wedge never touches it), so without a way to pick up the new port an automatic
/// rebind would strand the local GUI's command channel + terminal attach on the
/// retired port until a human restart - defeating the whole "no restart" goal.
/// [`refresh_addr`](Self::refresh_addr) re-reads the fresh addr from the local
/// handshake on a transport failure (mirroring PR #38's external-client behavior),
/// keeping the cached FULL-power token (a rebind never rotates tokens).
pub struct ControlEndpoint {
    addr: RwLock<String>,
    token: String,
    /// In-process-only origin proof. Empty for remote thin-client endpoints.
    host_token: String,
    /// Local handshake file to re-read the rotated addr from. `None` in REMOTE
    /// thin-client mode (there is no local rebind to track).
    refresh_path: Option<PathBuf>,
}

impl ControlEndpoint {
    /// The current control address (follows a rebind once [`refresh_addr`] adopts it).
    pub fn addr(&self) -> String {
        self.addr.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The auth token. Fixed for the launch: a rebind is transport recovery, not a
    /// credential rotation, so the token the frontend already holds stays valid.
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn host_token(&self) -> &str {
        &self.host_token
    }

    /// After a transport failure, re-read the LOCAL handshake for a rotated addr (the
    /// self-heal's `rebind_control` moves the listener + rewrites `control.json`).
    /// Returns `Some(new_addr)` iff the addr CHANGED, updating the shared cache so
    /// every consumer (command channel, forwarder, attach) follows to the new port.
    /// `None` when there is no refresh source (remote mode), the file is unreadable,
    /// or the addr is unchanged (a genuine failure that a re-resolve won't fix).
    ///
    /// The published token is deliberately IGNORED: under Phase 3 hardening
    /// `control.json` publishes only the read token, so adopting it would drop the
    /// frontend to read-only and break attach. The cached full token still authorizes
    /// the fresh port because the rebind kept it.
    pub fn refresh_addr(&self) -> Option<String> {
        let mut cur = self.addr.write().unwrap_or_else(|e| e.into_inner());
        let fresh = self.discovered_addr()?;
        if *cur != fresh {
            *cur = fresh.clone();
            Some(fresh)
        } else {
            None
        }
    }

    fn discovered_addr(&self) -> Option<String> {
        let path = self.refresh_path.as_ref()?;
        let body = std::fs::read_to_string(path).ok()?;
        let hs: Value = serde_json::from_str(&body).ok()?;
        hs.get("addr")
            .and_then(|value| value.as_str())
            .map(str::to_string)
    }

    /// Adopt an address observed while using `attempted` only if shared state still
    /// points at that attempted address. A different current address means another
    /// request or the event forwarder already observed a newer rotation.
    fn adopt_addr_after(&self, attempted: &str, observed: String) -> String {
        let mut current = self.addr.write().unwrap_or_else(|e| e.into_inner());
        if *current == attempted {
            *current = observed;
        }
        current.clone()
    }

    fn refresh_addr_after(&self, attempted: &str) -> Option<String> {
        let observed = self.discovered_addr()?;
        let current = self.adopt_addr_after(attempted, observed);
        (current != attempted).then_some(current)
    }
}

/// Open a one-shot connection to the control listener, send one request frame, and
/// await one response line. Connections are short-lived by design (the listener is
/// built for one MCP round-trip per connection); pooling is a later M1 widening.
fn request(
    endpoint: &ControlEndpoint,
    command: &str,
    args: &Value,
) -> Result<Value, ControlRequestError> {
    request_with_deadline(
        endpoint,
        command,
        args,
        response_timeout_for_command(command),
        ATTEMPT_TIMEOUT,
    )
}

#[derive(Debug)]
enum RequestError {
    Transport(&'static str),
    Timeout(&'static str),
    EndpointChanged(String),
    App {
        message: String,
        retryable: bool,
        kind: Option<String>,
        details: Option<Value>,
    },
    Protocol(String),
}

impl RequestError {
    fn stage(&self) -> &'static str {
        match self {
            RequestError::Transport(stage) | RequestError::Timeout(stage) => stage,
            RequestError::EndpointChanged(_) => "endpoint refresh",
            RequestError::App { .. } => "server",
            RequestError::Protocol(_) => "protocol",
        }
    }

    fn retryable(&self) -> bool {
        matches!(
            self,
            RequestError::Transport(_)
                | RequestError::Timeout(_)
                | RequestError::EndpointChanged(_)
        )
    }
}

fn request_with_deadline(
    endpoint: &ControlEndpoint,
    command: &str,
    args: &Value,
    overall: Duration,
    attempt_timeout: Duration,
) -> Result<Value, ControlRequestError> {
    let deadline = Instant::now() + overall;
    let first_addr = endpoint.addr();
    match request_once(
        &first_addr,
        endpoint.token(),
        endpoint.host_token(),
        command,
        args,
        deadline,
        attempt_timeout,
        Some(endpoint),
    ) {
        Ok(value) => Ok(value),
        Err(RequestError::App {
            message,
            retryable,
            kind,
            details,
        }) => Err(ControlRequestError {
            message,
            retryable,
            kind,
            details,
        }),
        Err(RequestError::Protocol(message)) => Err(ControlRequestError::message(message)),
        Err(first) if first.retryable() => {
            if Instant::now() >= deadline {
                return Err(ControlRequestError::retryable_message(timeout_message(
                    command,
                    1,
                    first.stage(),
                    overall,
                )));
            }
            let fresh = match &first {
                RequestError::EndpointChanged(observed) => {
                    endpoint.adopt_addr_after(&first_addr, observed.clone())
                }
                _ => {
                    let Some(fresh) = endpoint.refresh_addr_after(&first_addr) else {
                        return Err(ControlRequestError::retryable_message(failure_message(
                            command, 1, &first, false, overall,
                        )));
                    };
                    fresh
                }
            };
            match request_once(
                &fresh,
                endpoint.token(),
                endpoint.host_token(),
                command,
                args,
                deadline,
                attempt_timeout,
                Some(endpoint),
            ) {
                Ok(value) => Ok(value),
                Err(RequestError::App {
                    message,
                    retryable,
                    kind,
                    details,
                }) => Err(ControlRequestError {
                    message,
                    retryable,
                    kind,
                    details,
                }),
                Err(RequestError::Protocol(message)) => Err(ControlRequestError::message(message)),
                Err(second) => Err(ControlRequestError::retryable_message(failure_message(
                    command, 2, &second, true, overall,
                ))),
            }
        }
        Err(_) => unreachable!("app and protocol errors returned above"),
    }
}

fn failure_message(
    command: &str,
    attempts: u8,
    error: &RequestError,
    endpoint_replaced: bool,
    overall: Duration,
) -> String {
    if matches!(error, RequestError::Timeout(_)) {
        return timeout_message(command, attempts, error.stage(), overall);
    }
    let stage = error.stage();
    format!(
        "control_unavailable: command '{command}' failed during {stage} after {attempts} attempt(s); endpoint_replaced={endpoint_replaced}"
    )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRequestError {
    message: String,
    retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
}

impl ControlRequestError {
    fn message(message: String) -> Self {
        Self::retryable_message_with(message, false)
    }

    fn retryable_message(message: String) -> Self {
        Self::retryable_message_with(message, true)
    }

    fn retryable_message_with(message: String, retryable: bool) -> Self {
        Self {
            message,
            retryable,
            kind: None,
            details: None,
        }
    }
}

impl fmt::Display for ControlRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

fn timeout_message(command: &str, attempts: u8, stage: &str, overall: Duration) -> String {
    format!(
        "control_timeout: command '{command}' failed within its {}s recovery deadline during {stage} after {attempts} attempt(s); retry_state=exhausted",
        overall.as_secs()
    )
}

fn remaining(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

fn request_once(
    addr: &str,
    token: &str,
    host_token: &str,
    command: &str,
    args: &Value,
    deadline: Instant,
    attempt_timeout: Duration,
    refresh: Option<&ControlEndpoint>,
) -> Result<Value, RequestError> {
    let socket: SocketAddr = addr.parse().map_err(|_| {
        RequestError::Protocol("control_protocol: malformed endpoint address".into())
    })?;
    let connect_budget = remaining(deadline)
        .map(|left| left.min(attempt_timeout))
        .filter(|budget| !budget.is_zero())
        .ok_or(RequestError::Timeout("connect"))?;
    let stream = TcpStream::connect_timeout(&socket, connect_budget).map_err(|e| {
        if matches!(
            e.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            RequestError::Timeout("connect")
        } else {
            RequestError::Transport("connect")
        }
    })?;
    let io_budget = remaining(deadline)
        .map(|left| left.min(attempt_timeout))
        .filter(|budget| !budget.is_zero())
        .ok_or(RequestError::Timeout("write"))?;
    stream.set_write_timeout(Some(io_budget)).ok();

    let mut writer = stream
        .try_clone()
        .map_err(|_| RequestError::Transport("stream setup"))?;
    let mut frame = serde_json::to_vec(&json!({
        "token": token,
        "host": host_token,
        "command": command,
        "args": args,
        "v": control::PROTOCOL_VERSION,
    }))
    .map_err(|e| RequestError::Protocol(format!("control_protocol: serialize failed: {e}")))?;
    frame.push(b'\n');
    writer
        .write_all(&frame)
        .and_then(|()| writer.flush())
        .map_err(|e| {
            if matches!(
                e.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) {
                RequestError::Timeout("write")
            } else {
                RequestError::Transport("write")
            }
        })?;

    stream
        .set_nonblocking(true)
        .map_err(|_| RequestError::Transport("stream setup"))?;
    let mut response = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut next_probe = Instant::now() + attempt_timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(RequestError::Timeout("read"));
        }
        if now >= next_probe {
            if let Some(fresh) = refresh
                .and_then(ControlEndpoint::discovered_addr)
                .filter(|fresh| fresh != addr)
            {
                return Err(RequestError::EndpointChanged(fresh));
            }
            next_probe = now + attempt_timeout;
        }
        match (&stream).read(&mut chunk) {
            Ok(0) if response.is_empty() => return Err(RequestError::Transport("read")),
            Ok(0) => {
                return Err(RequestError::Protocol(
                    "control_protocol: unterminated response frame".into(),
                ));
            }
            Ok(n) => {
                let received = &chunk[..n];
                let frame_bytes = received
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .unwrap_or(received.len());
                if response.len().saturating_add(frame_bytes) > MAX_RESPONSE_FRAME_BYTES {
                    return Err(RequestError::Protocol(format!(
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
            Err(_) => return Err(RequestError::Transport("read")),
        }
    }
    let line = String::from_utf8(response).map_err(|_| {
        RequestError::Protocol("control_protocol: response frame was not UTF-8".into())
    })?;
    let resp: Value = serde_json::from_str(line.trim()).map_err(|e| {
        RequestError::Protocol(format!("control_protocol: malformed response: {e}"))
    })?;
    if resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(RequestError::App {
            message: resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("control_request: unknown error")
                .to_string(),
            retryable: resp
                .get("retryable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            kind: resp
                .get("errorKind")
                .and_then(Value::as_str)
                .map(str::to_owned),
            details: resp.get("errorDetails").cloned(),
        })
    }
}

/// Thin frontend transport command: round-trip ONE control command over the
/// loopback socket and return its `result` (or the dispatcher's error). The
/// frontend's `ipc/controlClient.ts` wraps this; migrated `client05`/`client`
/// wrappers call it instead of a direct in-process `invoke`.
#[tauri::command]
pub async fn control_request(
    endpoint: tauri::State<'_, Arc<ControlEndpoint>>,
    command: String,
    args: Option<Value>,
) -> Result<Value, ControlRequestError> {
    // ASYNC + spawn_blocking — `request` does a BLOCKING socket round-trip whose
    // duration is the backend command's runtime. control_request is the transport
    // for recent/git/usage/codex/files; as a SYNC command it ran on the main UI
    // thread, so a slow backend op (a flaky ~4s `claude -p /usage`, a stalling
    // `\\wsl.localhost\` recent read) FROZE the whole window for that whole time —
    // the sporadic "Not Responding" / Alt-Tab-ghost hang (the watchdog caught 2-4s
    // main-thread blocks with near-zero emits, ruling out emit volume). Running it
    // on the blocking pool keeps the main thread pumping. (Frontend unchanged —
    // `invoke` already returns a promise.)
    let ep = endpoint.inner().clone();
    let args = args.unwrap_or(Value::Null);
    tauri::async_runtime::spawn_blocking(move || request(&ep, &command, &args))
        .await
        .map_err(|error| ControlRequestError {
            message: format!("control_request: task join failed: {error}"),
            retryable: true,
            kind: None,
            details: None,
        })?
}

/// The production [`EventEmitter`]: writes every backend event to the control
/// socket's [`EventFanout`] (out over the wire to subscribed clients). The local
/// event forwarder (see [`spawn_event_forwarder`]) subscribes and re-emits each
/// frame into the webview as a single `control://event` envelope, which the
/// frontend demuxes by channel. This is the SOLE bridge-event sink: the M1
/// migration moved every channel onto this wire, so there is no longer a parallel
/// in-process Tauri emit (the old TeeEmitter/TauriEmitter dual-leg is gone).
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

/// Spawn the background forwarder: connect to the control listener as an event
/// subscriber, read `{event,payload}` frames off the socket, and re-emit each into
/// the webview as [`CONTROL_EVENT`]. This is the receive half of the event wire —
/// in M1 it loops back to the same process; in M2 the same loop points at a remote
/// server. Reconnects with a small backoff so a dropped/late listener self-heals.
pub fn spawn_event_forwarder(app: AppHandle, endpoint: Arc<ControlEndpoint>) {
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
                // Read the (possibly rebound) addr each cycle so a reconnect follows a
                // rotated listener port. Existing subscriptions survive a rebind (the
                // fanout is shared across the server's listeners), so this only matters
                // once the connection actually drops and must reconnect.
                let attempted_addr = endpoint.addr();
                let result = forward_once(&app, &attempted_addr, endpoint.token());
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
                        // A reconnect failure may be the retired old port after a
                        // rebind: re-read control.json so the NEXT cycle targets the
                        // fresh addr (relay-wedge self-heal).
                        endpoint.refresh_addr_after(&attempted_addr);
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
    let stream = TcpStream::connect_timeout(&socket, CONTROL_DEADLINE)
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
        crate::hangwatch::note_emit(); // count toward the main-thread emit-rate watchdog
        if let Err(e) = app.emit(
            CONTROL_EVENT,
            json!({ "channel": channel, "payload": payload }),
        ) {
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
    let remote = (
        std::env::var("T_HUB_REMOTE_ADDR")
            .ok()
            .filter(|a| !a.is_empty()),
        std::env::var("T_HUB_REMOTE_TOKEN")
            .ok()
            .filter(|t| !t.is_empty()),
    );
    if matches!(remote, (Some(_), Some(_))) {
        // Log only in the remote branch; `resolve_endpoint` stays pure/testable.
        if let (Some(addr), _) = &remote {
            eprintln!(
                "t-hub: REMOTE client mode — control endpoint = {addr} (T_HUB_REMOTE_ADDR); \
                 tiles + events + reads target the remote server"
            );
        }
    }
    let is_remote = matches!(remote, (Some(_), Some(_)));
    let (addr, token) = resolve_endpoint(handshake, remote);
    // LOCAL mode: track the local handshake so a rebind's rotated addr is picked up.
    // REMOTE thin-client mode: no local rebind to follow, so no refresh source.
    let refresh_path = if is_remote {
        None
    } else {
        Some(control::handshake_path())
    };
    // item-3: the least-privilege read token for the local UI spawn path. Only
    // meaningful in local mode (remote has no read/control split); empty otherwise.
    let host_token = if is_remote {
        String::new()
    } else {
        handshake.local_host_token.clone()
    };
    let endpoint = Arc::new(ControlEndpoint {
        addr: RwLock::new(addr),
        token,
        host_token,
        refresh_path,
    });
    app.manage(endpoint.clone());
    spawn_event_forwarder(app.clone(), endpoint);
}

/// Pick the (addr, token) the LOCAL webview authenticates the control socket with.
///
/// Two cases, kept pure so they are directly unit-testable:
/// - REMOTE thin-client (`T_HUB_REMOTE_ADDR` + `T_HUB_REMOTE_TOKEN` both set): use
///   the remote addr + remote token verbatim; this GUI talks to another host's
///   server and must present that host's credential.
/// - LOCAL loopback (the default): use the handshake addr and the FULL-power
///   `local_control_token`, NOT the published `token`. Under Phase 3 hardening
///   (`T_HUB_CONTROL_HARDEN=1`) the published `token` is only the read token, so
///   authenticating the app's own frontend with it would drop the webview to
///   read-only and break terminal attach. The full token travels solely in this
///   in-process handshake struct (never in `control.json`), so the trusted local
///   frontend keeps full control regardless of hardening.
fn resolve_endpoint(
    handshake: &control::ControlHandshake,
    remote: (Option<String>, Option<String>),
) -> (String, String) {
    match remote {
        (Some(addr), Some(token)) => (addr, token),
        _ => (
            handshake.addr.clone(),
            handshake.local_control_token.clone(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{ControlHandshake, PROTOCOL_VERSION};
    use std::net::TcpListener;
    use std::thread;

    /// A handshake as `control::start` builds it under Phase 3 hardening: the
    /// published `token` is the READ token, while `local_control_token` carries the
    /// full-power control token for the trusted in-process frontend.
    fn hardened_handshake() -> ControlHandshake {
        ControlHandshake {
            addr: "127.0.0.1:5000".into(),
            token: "read-only".into(),
            read_token: "read-only".into(),
            pid: 1,
            protocol_version: PROTOCOL_VERSION,
            instance_id: "instance".into(),
            listener_generation: 1,
            published_at: 123,
            local_control_token: "full-control".into(),
            local_host_token: "host-only".into(),
        }
    }

    fn temp_handshake(addr: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "t-hub-desktop-client-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&path, serde_json::to_vec(&json!({"addr": addr})).unwrap()).unwrap();
        path
    }

    fn test_endpoint(addr: String, refresh_path: Option<PathBuf>) -> ControlEndpoint {
        ControlEndpoint {
            addr: RwLock::new(addr),
            token: "full-control".into(),
            host_token: "host-only".into(),
            refresh_path,
        }
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
            serde_json::to_writer(&mut writer, &json!({"ok": true, "result": result})).unwrap();
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

    #[test]
    fn local_arm_authenticates_with_the_full_control_token() {
        // The whole Phase-3-safety fix: with no remote override, the local webview
        // takes its credential from `local_control_token` (full power), NOT the
        // published `token` (read-only under hardening). This is what keeps terminal
        // attach working when hardening is enabled.
        let hs = hardened_handshake();
        let (addr, token) = resolve_endpoint(&hs, (None, None));
        assert_eq!(addr, "127.0.0.1:5000");
        assert_eq!(
            token, "full-control",
            "local frontend must get the FULL token"
        );
        assert_ne!(
            token, hs.token,
            "must NOT use the published (read-only) token"
        );
    }

    #[test]
    fn remote_arm_uses_the_remote_token_verbatim() {
        // A remote thin client must keep presenting its remote credential; the fix
        // does not disturb the T_HUB_REMOTE_* override branch.
        let hs = hardened_handshake();
        let (addr, token) = resolve_endpoint(
            &hs,
            (Some("10.0.0.9:8787".into()), Some("remote-secret".into())),
        );
        assert_eq!(addr, "10.0.0.9:8787");
        assert_eq!(token, "remote-secret");
    }

    #[test]
    fn partial_remote_env_falls_back_to_local() {
        // Only one of the two remote vars set ⇒ not remote; use the local full token.
        let hs = hardened_handshake();
        let (_, token) = resolve_endpoint(&hs, (Some("10.0.0.9:8787".into()), None));
        assert_eq!(token, "full-control");
        let (_, token) = resolve_endpoint(&hs, (None, Some("remote-secret".into())));
        assert_eq!(token, "full-control");
    }

    #[test]
    fn commissioning_gets_a_longer_response_window_without_widening_normal_reads() {
        assert_eq!(response_timeout_for_command("list_tabs"), CONTROL_DEADLINE);
        assert_eq!(
            response_timeout_for_command("codex_usage"),
            CONTROL_DEADLINE
        );
        assert_eq!(response_timeout_for_command("unknown"), CONTROL_DEADLINE);
        assert_eq!(
            response_timeout_for_command("commission_captain"),
            LONG_ORCHESTRATION_TIMEOUT
        );
        assert_eq!(
            response_timeout_for_command("dispatch_crew"),
            LONG_ORCHESTRATION_TIMEOUT
        );
        assert_eq!(
            response_timeout_for_command("reconcile_cortana"),
            LONG_ORCHESTRATION_TIMEOUT
        );
        assert_eq!(
            response_timeout_for_command("history_resume"),
            LONG_ORCHESTRATION_TIMEOUT
        );
    }

    #[test]
    fn delayed_orchestration_error_reaches_the_client_before_its_response_window() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request_line)
                .unwrap();
            assert!(request_line.contains("commission_captain"));
            thread::sleep(Duration::from_millis(60));
            let mut writer = stream;
            writer
                .write_all(b"{\"ok\":false,\"error\":\"commissioning failed after rollback\"}\n")
                .unwrap();
        });

        let error = request_once(
            &addr,
            "token",
            "host",
            "commission_captain",
            &json!({}),
            Instant::now() + Duration::from_millis(250),
            Duration::from_millis(50),
            None,
        )
        .unwrap_err();
        server.join().unwrap();
        assert!(
            matches!(error, RequestError::App { message, retryable: false, .. } if
            message == "commissioning failed after rollback")
        );
    }

    #[test]
    fn retryable_backend_error_remains_structured_for_the_webview() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request_line)
                .unwrap();
            let mut writer = stream;
            writer
                .write_all(
                    b"{\"ok\":false,\"error\":\"history_resume_failed: placement uncertain\",\"retryable\":true}\n",
                )
                .unwrap();
        });

        let error = request_with_deadline(
            &test_endpoint(addr, None),
            "history_resume",
            &json!({"requestId": "request-one"}),
            Duration::from_millis(250),
            Duration::from_millis(50),
        )
        .unwrap_err();
        server.join().unwrap();

        let structured = error;
        assert!(structured.retryable);
        assert_eq!(
            structured.message,
            "history_resume_failed: placement uncertain"
        );
    }

    #[test]
    fn native_control_response_errors_survive_the_tauri_bridge_losslessly() {
        let fixture: Value =
            serde_json::from_str(include_str!("../fixtures/native-control-error-bridge.json"))
                .unwrap();
        for response in fixture.as_object().unwrap().values() {
            let command = response
                .get("details")
                .and_then(|details| details["operation"].as_str())
                .unwrap_or("register_project");
            let kind = response.get("kind").and_then(Value::as_str);
            let details = response.get("details").cloned();
            let message = response["message"].as_str().unwrap();
            let expected_retryable = response["retryable"].as_bool().unwrap();
            let mut response = json!({
                "ok": false,
                "error": message,
                "retryable": expected_retryable,
            });
            if let Some(kind) = kind {
                response["errorKind"] = json!(kind);
            }
            if let Some(details) = details.as_ref() {
                response["errorDetails"] = details.clone();
            }
            let endpoint = test_endpoint(
                raw_response_server(format!("{response}\n").into_bytes()),
                None,
            );
            let error = request_with_deadline(
                &endpoint,
                command,
                &json!({}),
                Duration::from_secs(2),
                Duration::from_millis(100),
            )
            .unwrap_err();

            assert_eq!(error.message, message);
            assert_eq!(error.retryable, expected_retryable);
            assert_eq!(error.kind.as_deref(), kind);
            assert_eq!(error.details, details);
            let mut expected = json!({
                "message": message,
                "retryable": expected_retryable,
            });
            if let Some(kind) = kind {
                expected["kind"] = json!(kind);
            }
            if let Some(details) = details {
                expected["details"] = details;
            }
            assert_eq!(serde_json::to_value(&error).unwrap(), expected);
        }
    }

    #[test]
    fn absent_optional_native_error_fields_keep_the_legacy_bridge_shape() {
        let endpoint = test_endpoint(
            raw_response_server(
                b"{\"ok\":false,\"error\":\"validation failed\",\"retryable\":false}\n".to_vec(),
            ),
            None,
        );
        let error = request_with_deadline(
            &endpoint,
            "register_project",
            &json!({}),
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"message": "validation failed", "retryable": false})
        );
    }

    #[test]
    fn malformed_native_response_is_a_nonstructured_bridge_error() {
        let endpoint = test_endpoint(raw_response_server(b"{not-json}\n".to_vec()), None);
        let error = request_with_deadline(
            &endpoint,
            "list_dir",
            &json!({}),
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.message.contains("malformed response"));
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"message": error.message, "retryable": false})
        );
    }

    #[test]
    fn retryable_transport_failure_is_structured_without_string_encoding() {
        let endpoint = test_endpoint(dead_addr(), None);
        let error = request_with_deadline(
            &endpoint,
            "list_dir",
            &json!({}),
            Duration::from_millis(100),
            Duration::from_millis(40),
        )
        .unwrap_err();
        assert!(error.retryable);
        assert!(error.message.contains("control_unavailable"));
        assert_eq!(error.kind, None);
        assert_eq!(error.details, None);
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"message": error.message, "retryable": true})
        );
    }

    #[test]
    fn refused_connect_recovers_through_replacement_endpoint() {
        let fresh = responding_server(json!({"healthy": true}));
        let path = temp_handshake(&fresh);
        let endpoint = test_endpoint(dead_addr(), Some(path.clone()));

        let value = request_with_deadline(
            &endpoint,
            "wsl_health",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(60),
        )
        .unwrap();
        assert_eq!(value["healthy"], true);
        assert_eq!(endpoint.addr(), fresh);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn connected_but_silent_inherited_port_recovers_promptly() {
        let stale = silent_server(Duration::from_millis(180));
        let fresh = responding_server(json!({"tabs": []}));
        let path = temp_handshake(&fresh);
        let endpoint = test_endpoint(stale, Some(path.clone()));
        let started = Instant::now();

        let value = request_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(40),
        )
        .unwrap();
        assert_eq!(value["tabs"], json!([]));
        assert!(started.elapsed() < Duration::from_millis(150));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn response_loss_invalidates_endpoint_and_recovers() {
        let stale = closing_server();
        let fresh = responding_server(json!({"terminals": []}));
        let path = temp_handshake(&fresh);
        let endpoint = test_endpoint(stale, Some(path.clone()));

        let value = request_with_deadline(
            &endpoint,
            "list_terminals",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(60),
        )
        .unwrap();
        assert_eq!(value["terminals"], json!([]));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn current_endpoint_succeeds_without_refresh() {
        let addr = responding_server(json!({"capabilities": ["read"]}));
        let endpoint = test_endpoint(addr, None);
        let value = request_with_deadline(
            &endpoint,
            "capabilities",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(60),
        )
        .unwrap();
        assert_eq!(value["capabilities"], json!(["read"]));
    }

    #[test]
    fn healthy_response_can_outlive_attempt_slice_within_overall_deadline() {
        let addr = delayed_server(json!({"usage": "ready"}), Duration::from_millis(90));
        let endpoint = test_endpoint(addr, None);

        let value = request_with_deadline(
            &endpoint,
            "codex_usage",
            &Value::Null,
            Duration::from_millis(250),
            Duration::from_millis(40),
        )
        .unwrap();
        assert_eq!(value["usage"], "ready");
    }

    #[test]
    fn partial_frame_trickle_cannot_bypass_absolute_deadline() {
        let addr = trickle_server(Duration::from_millis(10), 30);
        let endpoint = test_endpoint(addr, None);
        let started = Instant::now();

        let error = request_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_millis(70),
            Duration::from_millis(20),
        )
        .unwrap_err();
        assert!(error.message.contains("control_timeout"), "error: {error}");
        assert!(started.elapsed() < Duration::from_millis(150));
    }

    #[test]
    fn exact_limit_response_frame_is_accepted() {
        let addr = raw_response_server(exact_limit_response());
        let endpoint = test_endpoint(addr, None);

        let value = request_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .unwrap();
        assert_eq!(value, Value::Null);
    }

    #[test]
    fn over_limit_response_frame_is_bounded_and_credential_safe() {
        let secret = "oversized-server-token-must-not-leak";
        let mut response = vec![b'x'; MAX_RESPONSE_FRAME_BYTES];
        response.extend_from_slice(secret.as_bytes());
        response.push(b'\n');
        let addr = raw_response_server(response);
        let endpoint = test_endpoint(addr.clone(), None);

        let error = request_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.message.contains("response frame exceeds"));
        assert!(!error.message.contains(secret));
        assert!(!error.message.contains("full-control"));
        assert!(!error.message.contains("host-only"));
        assert!(!error.message.contains(&addr));
    }

    #[test]
    fn unterminated_response_frame_is_a_safe_protocol_error() {
        let secret = "unterminated-server-token-must-not-leak";
        let addr = raw_response_server(format!("{{\"ok\":true,\"{secret}\":").into_bytes());
        let endpoint = test_endpoint(addr, None);

        let error = request_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.message.contains("unterminated response frame"));
        assert!(!error.message.contains(secret));
    }

    #[test]
    fn malformed_response_frame_does_not_echo_peer_content() {
        let secret = "malformed-server-token-must-not-leak";
        let addr = raw_response_server(format!("{{not-json:{secret}}}\n").into_bytes());
        let endpoint = test_endpoint(addr, None);

        let error = request_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.message.contains("malformed response"));
        assert!(!error.message.contains(secret));
    }

    #[test]
    fn budget_exhaustion_is_classified_and_does_not_leak_credentials() {
        let addr = silent_server(Duration::from_millis(180));
        let endpoint = test_endpoint(addr.clone(), None);
        let error = request_with_deadline(
            &endpoint,
            "list_tabs",
            &Value::Null,
            Duration::from_millis(70),
            Duration::from_millis(60),
        )
        .unwrap_err();
        assert!(error.message.contains("control_timeout"), "error: {error}");
        assert!(error.message.contains("retry_state=exhausted"));
        assert!(!error.message.contains(&addr));
        assert!(!error.message.contains("full-control"));
        assert!(!error.message.contains("host-only"));
    }

    /// F1 REGRESSION (PR #50 fix round): the app's OWN client must follow the server
    /// to a rotated port after a `rebind_control`, or an automatic relay-wedge heal
    /// would strand the local GUI's command channel + terminal attach on the retired
    /// port until a human restart. This exercises the exact re-resolve seam the
    /// reviewer flagged as untested: after the server rewrites control.json with a
    /// fresh addr, `refresh_addr` adopts it (keeping the full token) and every
    /// consumer's `addr()` follows.
    #[test]
    fn refresh_addr_adopts_a_rotated_port_from_the_local_handshake() {
        let cj = std::env::temp_dir().join(format!(
            "t-hub-f1-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(
            &cj,
            serde_json::to_vec(&json!({
                "addr": "127.0.0.1:6001",
                "token": "read-only",          // published token (ignored on refresh)
                "read_token": "read-only",
                "pid": 1,
                "protocolVersion": control::PROTOCOL_VERSION,
            }))
            .unwrap(),
        )
        .unwrap();

        // Start on the OLD port with the FULL token; refresh source is the handshake.
        let ep = ControlEndpoint {
            addr: RwLock::new("127.0.0.1:6000".into()),
            token: "full-control".into(),
            host_token: "host".into(),
            refresh_path: Some(cj.clone()),
        };
        assert_eq!(ep.addr(), "127.0.0.1:6000");

        // A rebind rotated the port: refresh adopts the fresh addr, keeps the token.
        assert_eq!(ep.refresh_addr().as_deref(), Some("127.0.0.1:6001"));
        assert_eq!(
            ep.addr(),
            "127.0.0.1:6001",
            "consumers must follow the new port"
        );
        assert_eq!(
            ep.token(),
            "full-control",
            "the full token must NOT be dropped"
        );

        // Idempotent: a second refresh with no further rotation reports no change.
        assert_eq!(ep.refresh_addr(), None);

        // Remote thin-client mode (no refresh source) never re-resolves.
        let remote_ep = ControlEndpoint {
            addr: RwLock::new("10.0.0.9:8787".into()),
            token: "remote-secret".into(),
            host_token: String::new(),
            refresh_path: None,
        };
        assert_eq!(remote_ep.refresh_addr(), None);
        assert_eq!(remote_ep.addr(), "10.0.0.9:8787");

        let _ = std::fs::remove_file(&cj);
    }

    #[test]
    fn concurrent_older_rotation_observation_cannot_replace_newer_endpoint() {
        let endpoint = Arc::new(test_endpoint("127.0.0.1:6000".into(), None));
        let older_endpoint = endpoint.clone();
        let (captured_tx, captured_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let older = thread::spawn(move || {
            let attempted = "127.0.0.1:6000";
            let observed = "127.0.0.1:6001".to_string();
            captured_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            older_endpoint.adopt_addr_after(attempted, observed)
        });

        captured_rx.recv().unwrap();
        assert_eq!(
            endpoint.adopt_addr_after("127.0.0.1:6000", "127.0.0.1:6002".into()),
            "127.0.0.1:6002"
        );
        release_tx.send(()).unwrap();

        assert_eq!(older.join().unwrap(), "127.0.0.1:6002");
        assert_eq!(endpoint.addr(), "127.0.0.1:6002");
    }
}
