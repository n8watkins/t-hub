//! App-side **control listener** — the local control channel the MCP server
//! ([`t-hub-mcp`](../crates/t-hub-mcp)) forwards `tools/call` requests to.
//!
//! ## Why this exists
//! MCP servers are launched by the client (Claude) over stdio, as a separate
//! short-lived process. They cannot share the running T-Hub app's
//! Tauri-managed state in-process. So the MCP binary speaks the MCP protocol on
//! stdio and forwards each `tools/call` to **this** listener over a loopback TCP
//! channel; the listener dispatches by command name against the app's state and
//! returns JSON. The MCP server therefore needs **no compile-time knowledge** of
//! individual commands — dispatch is dynamic, by name (PRD §9.6, §11.2).
//!
//! ## Wire protocol (newline-delimited JSON over loopback TCP)
//! One request object per line, one response object per line:
//! ```text
//! → {"token":"<secret>","command":"list_terminals","args":{}}
//! ← {"ok":true,"result":[ … ]}
//! ```
//! Errors come back as `{"ok":false,"error":"<message>"}`. A request whose token
//! does not match the per-launch secret is rejected before dispatch.
//!
//! ## Discovery + auth
//! On startup we bind `127.0.0.1:0` (an ephemeral port), generate a per-launch
//! token, and write both to a small handshake file (`~/.t-hub/control.json`,
//! mode `0600` on unix). The MCP binary reads that file to learn where to connect
//! and which token to present. `T_HUB_CONTROL_ADDR` + `T_HUB_CONTROL_TOKEN`
//! override discovery for tests / harnesses. Binding to loopback keeps the
//! channel host-local (PRD §11.3: expose only what T-Hub needs).
//!
//! ## Permission tiers (PRD §11.2)
//! Read + Organization tools are dispatched here. Process-changing and
//! destructive tools are **gated**: this listener refuses any command that is not
//! on its allow-list, returning a clear error, so even if a future MCP build
//! advertises a destructive tool the app will not execute it. The MCP tool
//! descriptions additionally mark such tools as confirmation-required.
//!
//! Boundary: this module *reads* the existing command surface (tmux, agent,
//! status, supervision, files) and calls it; it does not change any of it. The
//! `theme` commands are forwarded by name and will light up when the parallel
//! theme track lands the `get_theme`/`set_theme` Tauri commands + a control
//! handler for them; until then they return a clear "not available" error.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::claude::StatusBridge;
use crate::supervision::Supervisor;
use crate::{files, git, pty, tmux};

/// A single control request: a command name + free-form JSON args, authenticated
/// by the per-launch `token`.
#[derive(Debug, Deserialize)]
pub struct ControlRequest {
    /// Per-launch shared secret (see the handshake file).
    #[serde(default)]
    pub token: String,
    /// The command/tool name to dispatch (e.g. `list_terminals`).
    pub command: String,
    /// Command arguments. Shape is per-command; absent ⇒ `null`.
    #[serde(default)]
    pub args: Value,
    /// Wire protocol version the client speaks (server-split M2b). Absent for the
    /// MCP / any legacy client (then unchecked, for backward compatibility); when
    /// present it must equal [`PROTOCOL_VERSION`] or the server rejects the request.
    #[serde(default)]
    pub v: Option<u32>,
}

/// A single control response. `ok` discriminates success (`result`) from failure
/// (`error`), mirroring the `Result<Value, String>` the dispatcher returns.
#[derive(Debug, Serialize)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlResponse {
    fn ok(result: Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(msg.into()),
        }
    }
}

/// The handshake record written so the MCP binary can find + authenticate to the
/// listener. Serialized to `~/.t-hub/control.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ControlHandshake {
    /// `127.0.0.1:<port>` the listener bound to.
    pub addr: String,
    /// Per-launch shared secret the client must present.
    pub token: String,
    /// PID of the app that owns this listener (diagnostics / staleness checks).
    pub pid: u32,
    /// The control wire protocol version this server speaks ([`PROTOCOL_VERSION`]).
    /// A local client (the MCP) can read it to detect a stale binary; defaults to 0
    /// when absent so older handshake readers/files stay parseable.
    #[serde(default)]
    pub protocol_version: u32,
}

/// A sink that delivers an Organization-tier UI mutation to the frontend. The
/// real implementation (wired from `lib.rs`) emits a Tauri `control://apply`
/// event carrying `{command, args}`; the frontend `controlBridge` subscribes and
/// dispatches it into the workspace store. Boxed as a trait object so this module
/// stays free of any `tauri` dependency and the e2e/unit tests can omit it.
pub trait ApplySink: Send + Sync {
    /// Forward an accepted Organization command + its args to the UI. Returns
    /// `Ok(())` if the event was emitted, or an error string the dispatcher
    /// surfaces (the command is still audited regardless).
    fn apply(&self, command: &str, args: &Value) -> Result<(), String>;
}

/// The command name a client sends to switch a control connection into an
/// **event-subscription stream** (server-split M1). Instead of one response, the
/// connection stays open and the server streams `{"event":<channel>,"payload":
/// <value>}` frames (newline-delimited) until the client disconnects. This is the
/// send half of the M1 event wire; the receive half is
/// `control_client::spawn_event_forwarder`.
/// The control wire protocol version (server-split M2b). Bump this on any
/// breaking change to the request/response/event/PTY framing. The server
/// advertises it in the handshake file + the subscribe ack, and rejects a client
/// that advertises a different version (see [`ControlRequest::v`]) — so a remote
/// client running a skewed T-Hub build fails fast with a clear message instead of
/// a cryptic downstream parse error. Clients that send no version stay accepted.
pub const PROTOCOL_VERSION: u32 = 1;

pub const SUBSCRIBE_COMMAND: &str = "__subscribe_events";

/// The command name that switches a control connection into a **PTY stream**
/// (server-split M2a): the connection becomes a full-duplex terminal channel —
/// the server captures scrollback, spawns the PTY-runs-`tmux attach`, streams
/// `{"out":"<b64>"}` frames down, and reads `{"write":"<b64>"}` / `{"resize":
/// {cols,rows}}` frames back up, until the client disconnects (then it detaches —
/// the tmux session survives). Args: `sessionId` (required), `cols`, `rows`.
pub const ATTACH_PTY_COMMAND: &str = "attach_pty";

/// A registry of connected event subscribers. The backend's event emitter
/// (`control_client::SocketEmitter`, installed on the agent bridge) writes each
/// event to every subscriber's socket through [`EventFanout::emit_event`]; a
/// control connection joins the registry via the [`SUBSCRIBE_COMMAND`] handshake
/// in [`handle_conn`]. Cheap to construct empty — the default before any
/// subscriber and in headless tests.
#[derive(Default)]
pub struct EventFanout {
    subs: Mutex<Vec<Subscriber>>,
    next_id: AtomicU64,
}

/// One subscribed connection: the (write half of the) socket plus an id used to
/// prune it on clean disconnect.
struct Subscriber {
    id: u64,
    writer: TcpStream,
}

impl EventFanout {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a subscriber's socket; returns an id for [`unregister`](Self::unregister).
    ///
    /// We set a WRITE TIMEOUT on the subscriber's socket: [`emit_event`](Self::emit_event)
    /// writes to every subscriber while holding `subs`, so without a bound a single
    /// stuck/slow client (its kernel send buffer full) would block the emit — and the
    /// whole journal-consumer path — indefinitely. On loopback the local forwarder
    /// drains promptly so this never fires; it matters the moment M2 binds this wire
    /// to a remote/Tailscale host. On timeout the write errors and `emit_event` prunes
    /// the subscriber, so one wedged client self-heals instead of stalling the rest.
    fn register(&self, writer: TcpStream) -> u64 {
        let _ = writer.set_write_timeout(Some(std::time::Duration::from_secs(5)));
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut subs) = self.subs.lock() {
            subs.push(Subscriber { id, writer });
        }
        id
    }

    /// Drop a subscriber by id (called when its connection closes cleanly). A
    /// subscriber whose socket errors mid-stream is also pruned lazily by the next
    /// [`emit_event`](Self::emit_event), so this is the prompt path, not the only one.
    fn unregister(&self, id: u64) {
        if let Ok(mut subs) = self.subs.lock() {
            subs.retain(|s| s.id != id);
        }
    }

    /// Write one event frame to every subscriber, pruning any whose socket errors
    /// (a disconnected client). Best-effort: a transport failure to one subscriber
    /// never affects another or the emitting (journal-consumption) path. Holding the
    /// lock across the writes serializes emits so frames never interleave on a socket.
    pub fn emit_event(&self, channel: &str, payload: &Value) {
        let mut frame = match serde_json::to_vec(&json!({ "event": channel, "payload": payload })) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("t-hub-control: failed to serialize event {channel}: {e}");
                return;
            }
        };
        frame.push(b'\n');
        if let Ok(mut subs) = self.subs.lock() {
            subs.retain_mut(|s| {
                s.writer
                    .write_all(&frame)
                    .and_then(|()| s.writer.flush())
                    .is_ok()
            });
        }
    }

    /// Number of live subscribers (diagnostics / tests).
    pub fn subscriber_count(&self) -> usize {
        self.subs.lock().map(|s| s.len()).unwrap_or(0)
    }
}

/// The shared state the control dispatcher reads. Holds exactly the handles the
/// Read + Organization tools need.
///
/// Deliberately **not** the Tauri-managed `TerminalManager` / `FileIndexState`
/// (those are non-`Clone`, owned by the app for its lifetime, and only
/// borrowable inside the invoke handler). Instead:
///   - terminal listing is reconstructed from the tmux source of truth (exactly
///     as `commands::list_terminals` treats it — tmux is authoritative);
///   - file search uses its own [`files::FileIndexState`] cache (a cache, so a
///     private one is correct — it just re-walks on first query);
///   - supervision + status are read from the `Arc`-shared bridges in
///     [`crate::AppState`], which *is* `Clone`.
/// Fetch a host-metrics snapshot from the **agent bridge** — i.e. the WSL agent's
/// own `/proc`. On the current Windows-host topology this is the ONLY correct
/// source: the daemon runs in the GUI's Windows process, whose "local `/proc`" is
/// the Windows host (no `/proc` ⇒ zeros), so `host_metrics` must prefer this RPC.
/// `lib.rs` supplies the closure (a clone of the `AgentBridge`); `None` in headless
/// tests/proofs. Returns the bridge's "not connected" error until the agent attaches.
type MetricsFn = Arc<dyn Fn() -> Result<t_hub_protocol::HostMetrics, String> + Send + Sync>;

#[derive(Clone)]
pub struct ControlContext {
    status: Arc<StatusBridge>,
    /// A snapshot accessor over the supervision reducer. Boxed closure so this
    /// module does not need to name the `AgentBridge` internals; the closure
    /// borrows the shared `Mutex<Supervisor>` inside the bridge.
    supervisor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync>,
    /// Private file index cache for control-channel searches.
    files: Arc<files::FileIndexState>,
    /// Sink that forwards Organization-tier UI mutations (`focus_session`,
    /// `move_tile`, `rename_tab`) to the frontend. `None` in headless tests /
    /// proofs (those just audit); `Some` once `lib.rs` wires the `AppHandle`.
    apply_sink: Option<Arc<dyn ApplySink>>,
    /// The event-subscription registry. Backend events fan out to subscribed
    /// connections through this (server-split M1). Default-empty in headless
    /// tests; `lib.rs` shares the same `Arc` with the socket emitter so emits and
    /// subscribers meet here.
    fanout: Arc<EventFanout>,
    /// Fetch host metrics from the agent bridge (the WSL agent's `/proc`). `None`
    /// in headless tests; `lib.rs` wires it from `AgentBridge`. See [`MetricsFn`]
    /// for why this is the canonical source on the Windows-host topology.
    metrics: Option<MetricsFn>,
    /// Idle read timeout for a connection's request phase ([`CONN_READ_TIMEOUT`] by
    /// default). A field (not the bare const) so tests can drive a short timeout
    /// against a real listener; could later carry an operator override.
    idle_timeout: std::time::Duration,
    /// Whether the connection being served is from the LOCAL loopback (same machine,
    /// fully trusted) vs a REMOTE tailnet peer. Set per-connection in `handle_conn`;
    /// `true` by default (tests + the loopback case). Gates the file-read scope (#23):
    /// remote peers are restricted to indexed roots, loopback is unrestricted.
    peer_is_loopback: bool,
    /// The per-launch auth token.
    token: String,
}

impl ControlContext {
    /// Run `f` against the supervision reducer (read-only) via the bridge's lock.
    ///
    /// The visitor type is `FnMut(&mut dyn FnMut(&Supervisor))`, so the inner
    /// closure must be `FnMut`; we move `f` (an `FnOnce`) out of an `Option` on
    /// its single invocation to satisfy that bound. The bridge calls the inner
    /// closure exactly once with the locked `Supervisor`.
    fn with_supervisor<R>(&self, f: impl FnOnce(&Supervisor) -> R) -> R {
        let mut out: Option<R> = None;
        let mut f = Some(f);
        let mut take = |s: &Supervisor| {
            if let Some(f) = f.take() {
                out = Some(f(s));
            }
        };
        (self.supervisor)(&mut take);
        out.expect("supervisor closure always runs")
    }
}

/// Resolve the handshake file path: `$T_HUB_CONTROL_FILE` if set, else
/// `~/.t-hub/control.json` (or the process dir as a last resort).
pub fn handshake_path() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_CONTROL_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("control.json")
}

/// Resolve the persistent server-key file: `$T_HUB_SERVER_KEY_FILE` if set, else
/// `~/.t-hub/server-key`. Mirrors [`handshake_path`] so dev-isolation can point it
/// elsewhere via the env var.
fn key_path() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_SERVER_KEY_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("server-key")
}

/// The PERSISTENT control auth key (server-split M2b): the server's stable identity
/// across restarts, so a remote client paired once need not re-pair each launch.
/// Read from [`key_path`] if present + non-empty; otherwise a fresh UUID is generated
/// and written (best-effort `0600` on unix). On any read/write failure we still
/// return a usable (in-memory) key so the channel always comes up.
pub fn persistent_key() -> String {
    let path = key_path();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let k = existing.trim().to_string();
        if !k.is_empty() {
            return k;
        }
    }
    let key = uuid::Uuid::new_v4().to_string();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&path, &key).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
    }
    key
}

/// Write the handshake file (best-effort `0600` on unix) so the MCP binary can
/// discover the live listener.
fn write_handshake(handshake: &ControlHandshake) -> std::io::Result<()> {
    let path = handshake_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(handshake)?;
    std::fs::write(&path, &body)?;
    // Tighten permissions on unix so another local user can't read the token.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Start the control listener on a background thread.
///
/// Binds `127.0.0.1:0`, writes the handshake file, and serves NDJSON control
/// requests until the process exits. Returns the bound address + token so the
/// caller (and tests) know where it landed. A bind failure is returned to the
/// caller; the app logs it and continues (the control channel is optional, like
/// the agent bridge).
pub fn start(ctx: ControlContext) -> std::io::Result<ControlHandshake> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let handshake = ControlHandshake {
        addr: addr.to_string(),
        token: ctx.token.clone(),
        pid: std::process::id(),
        protocol_version: PROTOCOL_VERSION,
    };
    write_handshake(&handshake)?;

    // Opt-in ADDITIONAL bind for REMOTE access (server-split M2b). GATED — default
    // OFF, so the §8 loopback-only boundary holds unless explicitly enabled. When
    // set, a second listener serves the same dispatch; `handle_conn` restricts peers
    // to loopback + the Tailscale ranges, and the persistent token still gates every
    // request on top of that. A bind failure is logged and never aborts startup.
    if let Some(bind) = resolve_remote_bind() {
        match TcpListener::bind(&bind) {
            Ok(remote_listener) => {
                let remote_addr = remote_listener
                    .local_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|_| bind.clone());
                eprintln!(
                    "t-hub: control listener ALSO bound on {remote_addr} for REMOTE \
                     access (token-gated; loopback + Tailscale peers only)"
                );
                let ctx_remote = ctx.clone();
                std::thread::Builder::new()
                    .name("t-hub-control-remote".into())
                    .spawn(move || serve(remote_listener, ctx_remote))
                    .ok();
            }
            Err(e) => eprintln!("t-hub: remote control bind '{bind}' failed: {e}"),
        }
    }

    std::thread::Builder::new()
        .name("t-hub-control".into())
        .spawn(move || serve(listener, ctx))
        .ok();

    Ok(handshake)
}

/// Resolve the optional REMOTE bind address (M2b), or `None` to stay loopback-only.
/// `T_HUB_CONTROL_BIND=<ip:port>` binds that explicitly; `T_HUB_BIND_TAILSCALE=1`
/// auto-detects the Tailscale IPv4 (`tailscale ip -4`) and binds it on
/// `T_HUB_CONTROL_PORT` (default 8787). Explicit wins. Neither set ⇒ loopback-only.
fn resolve_remote_bind() -> Option<String> {
    if let Ok(a) = std::env::var("T_HUB_CONTROL_BIND") {
        if !a.trim().is_empty() {
            return Some(a.trim().to_string());
        }
    }
    let want_tailscale = std::env::var("T_HUB_BIND_TAILSCALE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if want_tailscale {
        if let Some(ip) = tailscale_ip4() {
            let port = std::env::var("T_HUB_CONTROL_PORT")
                .ok()
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| "8787".to_string());
            return Some(format!("{ip}:{port}"));
        }
        eprintln!(
            "t-hub: T_HUB_BIND_TAILSCALE set but `tailscale ip -4` returned nothing; \
             staying loopback-only"
        );
    }
    None
}

/// Best-effort Tailscale IPv4 via the CLI. `None` if tailscale isn't installed/up.
fn tailscale_ip4() -> Option<String> {
    let out = std::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}

/// Whether a peer IP may use the control channel: loopback always, plus the
/// Tailscale ranges (CGNAT `100.64.0.0/10` for IPv4, ULA `fd7a:115c::/32` for IPv6).
/// Everything else is rejected before auth, so even a `0.0.0.0` bind only ever
/// serves loopback + the tailnet; the token gates dispatch on top of this.
fn is_allowed_peer(ip: std::net::IpAddr) -> bool {
    // Normalize an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) to its IPv4 form —
    // that's how IPv4 peers arrive on a dual-stack (`[::]`) listener. Without this a
    // dual-stack bind would reject the very loopback/tailnet peers it should serve
    // (a mapped public IP still falls through to the rejecting V6 arm, so this never
    // *admits* anything new — it only un-breaks the legitimate mapped cases).
    let ip = match ip {
        std::net::IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(std::net::IpAddr::V4)
            .unwrap_or(std::net::IpAddr::V6(v6)),
        v4 => v4,
    };
    if ip.is_loopback() {
        return true;
    }
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (64..=127).contains(&o[1])
        }
        std::net::IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c
        }
    }
}

/// Accept loop: one short read/serve thread per connection. Connections are
/// expected to be local and short-lived (one MCP `tools/call` round-trip), but we
/// handle multiple lines per connection so a client may pipeline.
/// Max concurrent control connections. Bounds the thread-per-connection DoS surface
/// the M2b network bind opens (a flaky/hostile remote client reconnecting in a tight
/// loop). Generous — normal use is a handful (the MCP, the event forwarder, one per
/// terminal tile); this only trips on runaway connection churn.
const MAX_CONNS: usize = 256;
static ACTIVE_CONNS: AtomicUsize = AtomicUsize::new(0);

/// Idle/read timeout for a control connection's request phase (M2b hardening).
/// A connection that connects and never speaks — or stalls mid-request — would
/// otherwise pin a handler thread indefinitely (up to [`MAX_CONNS`] of them, which
/// wedges the listener). With the opt-in network bind this is a cheap remote DoS;
/// even on loopback it leaks threads on a buggy client. The timeout is CLEARED once
/// a connection enters a long-lived mode (event subscribe-park, PTY attach), which
/// legitimately block on reads for minutes with no client input. Generous: real
/// request/response clients send their line in milliseconds and close on EOF.
const CONN_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// A socket read timeout surfaces as `WouldBlock` (SO_RCVTIMEO on unix) or
/// `TimedOut` (windows). Both mean "idle — close this connection cleanly", not a
/// transport error worth logging or propagating.
fn is_read_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// Decrements the live-connection counter when a connection handler thread exits.
struct ConnGuard;
impl Drop for ConnGuard {
    fn drop(&mut self) {
        ACTIVE_CONNS.fetch_sub(1, Ordering::Relaxed);
    }
}

fn serve(listener: TcpListener, ctx: ControlContext) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                // Connection cap: reject (close) once at the ceiling rather than
                // spawning an unbounded number of handler threads.
                if ACTIVE_CONNS.fetch_add(1, Ordering::Relaxed) >= MAX_CONNS {
                    ACTIVE_CONNS.fetch_sub(1, Ordering::Relaxed);
                    eprintln!(
                        "t-hub-control: connection cap ({MAX_CONNS}) reached; rejecting a connection"
                    );
                    drop(stream);
                    continue;
                }
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let _guard = ConnGuard; // decrements ACTIVE_CONNS on exit
                    if let Err(e) = handle_conn(stream, &ctx) {
                        eprintln!("t-hub-control: connection error: {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("t-hub-control: accept failed: {e}");
            }
        }
    }
}

/// Serve every newline-delimited request on one connection until EOF.
fn handle_conn(stream: TcpStream, ctx: &ControlContext) -> std::io::Result<()> {
    let peer = stream.peer_addr().ok();
    // Restrict peers to loopback + the Tailscale ranges (M2b). With the default
    // loopback-only bind this only ever sees 127.0.0.1; with the opt-in remote bind
    // it admits tailnet peers and rejects everything else BEFORE auth, so a LAN/
    // public peer can't even reach the token check. The token then gates dispatch.
    if let Some(addr) = peer {
        if !is_allowed_peer(addr.ip()) {
            return Ok(());
        }
    }
    // Per-connection view (#23): tag whether the peer is LOOPBACK (same machine =
    // fully trusted) so the file-read handlers can scope a REMOTE tailnet peer to
    // the operator allowlist while leaving the local path unrestricted. Fail closed
    // (treat an un-resolvable peer as remote/scoped). Normalize IPv4-mapped IPv6
    // first (as `is_allowed_peer` does) so a real 127.0.0.1 over a dual-stack bind
    // — arriving as ::ffff:127.0.0.1 — is still recognized as loopback. We
    // clone+shadow `ctx` so the rest of this connection (dispatch included) sees it.
    let mut ctx = ctx.clone();
    ctx.peer_is_loopback = peer
        .map(|a| {
            let ip = match a.ip() {
                std::net::IpAddr::V6(v6) => v6
                    .to_ipv4_mapped()
                    .map(std::net::IpAddr::V4)
                    .unwrap_or(std::net::IpAddr::V6(v6)),
                v4 => v4,
            };
            ip.is_loopback()
        })
        .unwrap_or(false);
    let ctx = &ctx;
    let mut writer = stream.try_clone()?;
    // Read lines manually (not `reader.lines()`) so a connection mode that takes
    // over the rest of the stream (the PTY attach) can be handed `&mut reader`.
    let mut reader = BufReader::new(stream);
    // Bound the request phase with an idle read timeout (M2b hardening): a client
    // that connects but never sends — or stalls mid-line — closes itself rather
    // than parking this thread forever. CLEARED below when the connection becomes
    // a long-lived event/PTY stream (those block on reads for minutes by design).
    reader.get_ref().set_read_timeout(Some(ctx.idle_timeout)).ok();
    // Set once this connection joins the event-subscription registry; used to
    // prune it from the fanout on clean disconnect (loop EOF below).
    let mut subscriber_id: Option<u64> = None;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF: client disconnected.
            Ok(_) => {}
            // Idle past CONN_READ_TIMEOUT: close cleanly (not a real error).
            Err(e) if is_read_timeout(&e) => break,
            Err(e) => return Err(e),
        }
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ControlRequest>(&line) {
            Ok(req) => {
                // Protocol-version gate (M2b hardening): reject a client whose wire
                // version differs from ours with a CLEAR message, instead of letting
                // a version-skewed remote build fail cryptically downstream. A client
                // that sends NO version (the MCP today, any legacy peer) is allowed —
                // so adding the field now is backward-compatible, and enforcement
                // engages only once a peer advertises a mismatching version. The peer
                // is already IP-gated (is_allowed_peer), so echoing our version here
                // leaks nothing the handshake file doesn't already record.
                if let Some(v) = req.v {
                    if v != PROTOCOL_VERSION {
                        write_response(
                            &mut writer,
                            &ControlResponse::err(format!(
                                "protocol version mismatch: server v{PROTOCOL_VERSION}, \
                                 client v{v}; run matching T-Hub builds on both ends"
                            )),
                        )?;
                        continue;
                    }
                }
                // Event-subscription handshake: switch this connection into a one-way
                // event stream. After the ack we send no per-line responses — the
                // fanout owns the socket and the read loop just parks until disconnect.
                if req.command == SUBSCRIBE_COMMAND {
                    if !ct_token_eq(&req.token, &ctx.token) {
                        write_response(
                            &mut writer,
                            &ControlResponse::err("unauthorized: bad control token"),
                        )?;
                        continue;
                    }
                    if subscriber_id.is_none() {
                        // Ack FIRST, then register: so the fanout can never interleave
                        // an event frame with our ack on the same socket. The ack
                        // carries the server version so the forwarder can log a skew.
                        write_response(
                            &mut writer,
                            &ControlResponse::ok(json!({
                                "subscribed": true,
                                "protocolVersion": PROTOCOL_VERSION,
                            })),
                        )?;
                        subscriber_id = Some(ctx.fanout.register(writer.try_clone()?));
                        // This is now a one-way event stream — the client never sends
                        // again, so the read loop must park indefinitely. Drop the idle
                        // timeout (else a quiet stream would self-close every 120s).
                        reader.get_ref().set_read_timeout(None).ok();
                    }
                    // Park: subsequent reads block until the client disconnects.
                } else if req.command == ATTACH_PTY_COMMAND {
                    // PTY stream (M2a): the terminal channel owns the rest of the
                    // connection until the client disconnects.
                    if !ct_token_eq(&req.token, &ctx.token) {
                        write_response(
                            &mut writer,
                            &ControlResponse::err("unauthorized: bad control token"),
                        )?;
                        continue;
                    }
                    // The PTY stream reads {write}/{resize} frames for as long as the
                    // user leaves the tile open — clear the idle timeout so an
                    // untouched terminal isn't force-detached after 120s.
                    reader.get_ref().set_read_timeout(None).ok();
                    serve_pty_attach(&mut writer, &mut reader, &req.args)?;
                    break;
                } else {
                    write_response(&mut writer, &dispatch_authenticated(ctx, req))?;
                }
            }
            Err(e) => write_response(
                &mut writer,
                &ControlResponse::err(format!("malformed control request: {e}")),
            )?,
        }
    }
    if let Some(id) = subscriber_id {
        ctx.fanout.unregister(id);
    }
    Ok(())
}

/// Serve a PTY stream (M2a) on this connection: send the scrollback seed, spawn
/// the PTY-runs-`tmux attach` streaming `{"out"}` frames down (via a clone of the
/// writer — the reader thread owns those writes, so they never interleave with the
/// scrollback we send first), then read `{"write"}`/`{"resize"}` frames from the
/// client until it disconnects, and detach (the tmux session survives).
fn serve_pty_attach(
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    args: &Value,
) -> std::io::Result<()> {
    let session_id = match arg_str(args, "sessionId").or_else(|| arg_str(args, "session_id")) {
        Some(s) => s,
        None => {
            return write_response(
                writer,
                &ControlResponse::err("attach_pty requires a 'sessionId' argument"),
            );
        }
    };
    let tmux_session = tmux_target(&session_id);
    let cols = args.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16;
    let rows = args.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16;

    if !tmux::has_session(&tmux_session) {
        return write_response(
            writer,
            &ControlResponse::err(format!(
                "attach_pty: tmux session {tmux_session} for terminal {session_id} no longer exists"
            )),
        );
    }

    // Scrollback seed (base64), as the opening frame — sent BEFORE the stream
    // starts so the reader thread's output frames never race it.
    let scrollback = tmux::capture_pane(&tmux_session).unwrap_or_default();
    write_json_line(writer, &json!({ "scrollback": STANDARD.encode(&scrollback) }))?;

    // Spawn the PTY streaming output to a clone of this connection.
    let sink = writer.try_clone()?;
    let cwd = std::env::var("HOME").unwrap_or_default();
    let mut handle = match pty::stream_attach_to_sink(&tmux_session, &cwd, cols, rows, Box::new(sink)) {
        Ok(h) => h,
        Err(e) => {
            return write_json_line(writer, &json!({ "error": format!("attach_pty: {e}") }));
        }
    };

    // Drive write/resize frames from the client until it disconnects (EOF).
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break; // client disconnected
        }
        let frame: Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue, // skip a malformed frame rather than tearing down
        };
        if let Some(b64) = frame.get("write").and_then(|v| v.as_str()) {
            if let Ok(bytes) = STANDARD.decode(b64) {
                let _ = handle.write(&bytes);
            }
        } else if let Some(rz) = frame.get("resize") {
            let c = rz.get("cols").and_then(|v| v.as_u64()).unwrap_or(cols as u64) as u16;
            let r = rz.get("rows").and_then(|v| v.as_u64()).unwrap_or(rows as u64) as u16;
            let _ = handle.resize(c, r);
        }
    }
    handle.detach(); // tmux session survives, like close_terminal
    Ok(())
}

/// Write one newline-delimited JSON frame to a stream (best-effort flush). Used by
/// the PTY stream for its scrollback/error frames.
fn write_json_line(writer: &mut TcpStream, frame: &Value) -> std::io::Result<()> {
    let mut body = serde_json::to_vec(frame).unwrap_or_default();
    body.push(b'\n');
    writer.write_all(&body)?;
    writer.flush()
}

/// Write one newline-delimited control response and flush. Shared by the normal
/// request path and the subscribe ack.
fn write_response(writer: &mut TcpStream, resp: &ControlResponse) -> std::io::Result<()> {
    let mut body = serde_json::to_vec(resp).unwrap_or_else(|_| {
        br#"{"ok":false,"error":"failed to serialize response"}"#.to_vec()
    });
    body.push(b'\n');
    writer.write_all(&body)?;
    writer.flush()
}

/// Constant-time token comparison: avoids a timing oracle on the auth token once
/// the channel is network-reachable (M2b). Token length is a fixed-size UUID, so
/// the early length check leaks nothing meaningful.
fn ct_token_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Check the token, then dispatch. A bad token is rejected before any command
/// runs (no information about which commands exist is leaked).
fn dispatch_authenticated(ctx: &ControlContext, req: ControlRequest) -> ControlResponse {
    if !ct_token_eq(&req.token, &ctx.token) {
        return ControlResponse::err("unauthorized: bad control token");
    }
    match dispatch(ctx, &req.command, &req.args) {
        Ok(value) => ControlResponse::ok(value),
        Err(e) => ControlResponse::err(e),
    }
}

/// The set of commands the control channel will execute. Read + Organization
/// tiers (PRD §11.2). Process-changing / destructive commands are intentionally
/// **absent**: they fall through to the "not permitted over the control channel"
/// arm so the app never executes them via MCP, even if a client asks.
///
/// `theme` commands are forwarded by name; until the parallel theme track lands
/// their handlers they return a clear "not yet available" error.
fn dispatch(ctx: &ControlContext, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        // ---- Read tier (PRD §11.2: allowed) --------------------------------
        "list_terminals" => list_terminals(),
        "get_status" => get_status(ctx, args),
        "wait_for_status" => wait_for_status(ctx, args),
        "supervision_tree" => supervision_tree(ctx, args),
        "supervision_session_ids" => supervision_session_ids(ctx),
        "wsl_health" => wsl_health(ctx),
        "recent_sessions" => recent_sessions(),
        "claude_usage" => claude_usage(),
        "codex_usage" => codex_usage(),
        "host_metrics" => host_metrics(ctx),
        "git_info" => git_info(ctx, args),
        "index_project" => index_project(ctx, args),
        "search_files" => search_files(ctx, args),
        "list_dir" => list_dir(ctx, args),
        "read_text_file" => read_text_file(ctx, args),
        "list_tabs" => list_tabs(),
        "read_terminal" | "capture_pane" => read_terminal(args),

        // ---- Organization tier (PRD §11.2: allowed, audited) ---------------
        // These are surfaced by the MCP server and accepted here, but the
        // process-changing subset (spawn) is gated behind the confirmation flag
        // in the MCP tool description AND refused here unless explicitly enabled,
        // so the dev-box proof never spawns/kills anything by accident.
        "focus_session" => organization_apply(ctx, "focus_session", args),
        "move_tile" => organization_apply(ctx, "move_tile", args),
        "rename_tab" => organization_apply(ctx, "rename_tab", args),
        "new_tab" => organization_apply(ctx, "new_tab", args),
        "focus_tab" => organization_apply(ctx, "focus_tab", args),
        "open_file" => open_file(ctx, args),
        // WS-4 git worktrees: create runs git here then forwards the tab+spawn to
        // the UI; remove forwards to the UI so it detaches live tiles BEFORE git
        // tears the dir down (no orphaned processes).
        "create_worktree" => create_worktree(ctx, args),
        "remove_worktree" => remove_worktree(ctx, args),
        // Recent list × made durable: move a project's transcripts out of the
        // scanned catalog into projects-archive (reversible). App-initiated from
        // the sidebar; filesystem-mutating like the worktree ops above.
        "archive_recent_project" => archive_recent_project(args),

        // ---- Process-changing tier (PRD §11.2: confirmation required) ------
        // `spawn_terminal` stays gated off (it would create an untracked tmux
        // session the UI never adopts). The session-targeted process actions —
        // typing into / interrupting / closing an *existing* session — are
        // executed directly against tmux: the MCP tool descriptions mark them
        // CONFIRMATION REQUIRED, which is the user-facing gate, and they only
        // ever act on a `th_*` session the app already owns.
        "spawn_terminal" => gated_process_change("spawn_terminal"),
        "send_text" => send_text(args),
        "send_keys" => send_keys(args),
        "close_terminal" => close_terminal(args),

        // ---- Theme (forwarded by name; parallel track owns the handlers) ----
        "get_theme" | "set_theme" => Err(format!(
            "control: '{command}' is forwarded by name but the theme command \
             handler is not wired in this build yet (parallel theme track)"
        )),

        // ---- Everything else: not permitted over the control channel -------
        other => Err(format!(
            "control: command '{other}' is not exposed over the control channel \
             (process-changing/destructive commands are gated; see PRD §11.2)"
        )),
    }
}

// ---------------------------------------------------------------------------
// Read-tier handlers
// ---------------------------------------------------------------------------

/// `list_terminals`: reconstruct the terminal list from the tmux source of truth
/// on the isolated `t-hub` socket. Mirrors `commands::list_terminals`, minus
/// the in-memory Live/Detached refinement (the control channel does not own the
/// UI's PTY map; everything tmux reports is a live tmux session).
fn list_terminals() -> Result<Value, String> {
    let sessions = tmux::list_sessions().map_err(|e| format!("failed to list tmux sessions: {e}"))?;
    let terminals: Vec<Value> = sessions
        .iter()
        .filter(|s| s.starts_with("th_"))
        .map(|tmux_session| {
            let id = tmux_session
                .strip_prefix("th_")
                .unwrap_or(tmux_session)
                .to_string();
            json!({
                "id": id,
                "tmuxSession": tmux_session,
                "title": tmux_session,
                // tmux owns the live cwd; we do not track it server-side.
                "cwd": "",
                // Source-of-truth listing: present as live tmux-backed sessions.
                "state": "live",
            })
        })
        .collect();
    Ok(json!({ "terminals": terminals, "count": terminals.len() }))
}

/// `get_status`: FR-012 status for one session id (from the supervision reducer)
/// plus the latest statusline snapshot (context %, rate-limit windows) if one
/// has been ingested. `args.sessionId` selects the session.
fn get_status(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("get_status requires a 'sessionId' argument")?;
    let status = ctx.with_supervisor(|s| s.status(&session_id));
    let snapshot = ctx.status.get(&session_id);
    Ok(json!({
        "sessionId": session_id,
        "status": status,
        "snapshot": snapshot,
    }))
}

/// `wait_for_status`: long-poll the supervision reducer until a session reaches a
/// target FR-012 status (or a timeout). The reducer is snapshot-only, but it keeps
/// a bounded **transition log** (see [`Supervisor`]) so this is *edge-capturing*:
/// a status the session merely passes *through* between two 500ms polls (e.g.
/// working→completed→working, or a transient `needsQuestion`) is still observed,
/// instead of being missed and reported as a spurious `timedOut`.
///
/// How it works: we capture the supervisor's `current_seq()` up front, check the
/// current status for an immediate match, then loop — each iteration checks both
/// (a) the *current* status and (b) any logged transition for this session since
/// the last-consumed seq whose status matches a target (advancing the consumed
/// seq as we go). Either hit returns immediately. Each `with_supervisor` call
/// acquires + drops the supervisor mutex, and the 500ms sleep is *outside* the
/// lock, so the reducer keeps advancing (and logging edges) while we wait.
/// Blocking this control connection's thread for up to `timeoutMs` is expected:
/// connections are handled per-connection.
///
/// Args: `sessionId` (required), `targetStatus` (required; a camelCase status
/// string or an array of them — matches any), `timeoutMs` (optional, default
/// 30000). Returns `{ finalStatus, elapsedMs, timedOut }`. Statuses are compared
/// by serializing [`SessionStatus`] to its camelCase string, so the target
/// strings match the `get_status` / IPC representation exactly.
fn wait_for_status(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("wait_for_status requires a 'sessionId' argument")?;
    let targets = parse_target_statuses(args)?;
    // The same targets, resolved once to enum space for the transition-log edge
    // query (`matched_since`). Hoisted out of the loop since it never changes.
    let target_enums = target_statuses(&targets);
    let timeout = std::time::Duration::from_millis(
        args.get("timeoutMs")
            .and_then(|v| v.as_u64())
            .unwrap_or(30000),
    );

    // Watermark: every transition with seq > `consumed` is one we have not yet
    // inspected. Captured before we start waiting, so any edge that lands while we
    // sleep (including a transient status the session passes *through*) is caught
    // on a later iteration. We return on the first match, so this stays fixed.
    let consumed = ctx.with_supervisor(|s| s.current_seq());

    let started = std::time::Instant::now();
    loop {
        // (a) current status, and (b) any transition edge for this session since
        // `consumed` that matches a target — both read under one lock acquisition.
        // We advance `consumed` past every inspected edge so we never re-scan.
        let (status, edge_match) = ctx.with_supervisor(|s| {
            let status = s.status(&session_id);
            let edge = s.matched_since(&session_id, &target_enums, consumed);
            (status, edge)
        });
        let status_str = status_camel(status);
        let elapsed = started.elapsed();

        // An edge we slept through matched a target — report that status as final,
        // even though the *current* status may have already moved on past it. (We
        // return on the first match, so there's no need to advance `consumed`
        // past this edge; the watermark only matters across the no-match sleeps.)
        if let Some((_seq, matched_status)) = edge_match {
            return Ok(json!({
                "finalStatus": status_camel(matched_status),
                "elapsedMs": elapsed.as_millis() as u64,
                "timedOut": false,
            }));
        }
        // The current status matches a target.
        if targets.iter().any(|t| t == &status_str) {
            return Ok(json!({
                "finalStatus": status_str,
                "elapsedMs": elapsed.as_millis() as u64,
                "timedOut": false,
            }));
        }
        if elapsed >= timeout {
            return Ok(json!({
                "finalStatus": status_str,
                "elapsedMs": elapsed.as_millis() as u64,
                "timedOut": true,
            }));
        }
        // Mutex is already released (with_supervisor drops it per call); sleep
        // outside the lock so the reducer keeps advancing while we wait. The log
        // captures any edges the session crosses during this sleep window.
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// Resolve the parsed camelCase target strings back to [`SessionStatus`] values
/// for the transition-log edge query (`matched_since` works in enum space, while
/// the wire targets arrive as strings). Unrecognized strings are dropped — they
/// can never match a real logged status anyway, and the current-status string
/// comparison still covers any exotic value.
fn target_statuses(targets: &[String]) -> Vec<crate::model::SessionStatus> {
    targets
        .iter()
        .filter_map(|t| {
            serde_json::from_value::<crate::model::SessionStatus>(Value::String(t.clone())).ok()
        })
        .collect()
}

/// Serialize a [`SessionStatus`] to its camelCase wire string (e.g. "completed",
/// "needsQuestion"), matching the `get_status` / IPC representation. The enum is
/// `#[serde(rename_all = "camelCase")]`, so it serializes to a bare JSON string.
fn status_camel(status: crate::model::SessionStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Parse `targetStatus` into a non-empty set of camelCase status strings. Accepts
/// a single string or an array of strings (matches any).
fn parse_target_statuses(args: &Value) -> Result<Vec<String>, String> {
    let raw = args
        .get("targetStatus")
        .ok_or("wait_for_status requires a 'targetStatus' argument (string or array of strings)")?;
    let targets: Vec<String> = match raw {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => {
            return Err(
                "wait_for_status 'targetStatus' must be a string or an array of strings".into(),
            )
        }
    };
    if targets.is_empty() {
        return Err("wait_for_status 'targetStatus' must not be empty".into());
    }
    Ok(targets)
}

/// `supervision_session_ids`: every session id the supervision reducer knows.
/// Mirrors the `supervision_session_ids` Tauri command; returns a JSON array of ids
/// (server-split M1 — the supervision/status read surface moves onto the socket).
fn supervision_session_ids(ctx: &ControlContext) -> Result<Value, String> {
    let ids = ctx.with_supervisor(|s| s.session_ids());
    serde_json::to_value(ids).map_err(|e| e.to_string())
}

/// `supervision_tree`: the read-only orchestrator→subagent tree for one session.
/// Returns `null` when the session is unknown (matching the Tauri command).
fn supervision_tree(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("supervision_tree requires a 'sessionId' argument")?;
    let tree = ctx.with_supervisor(|s| s.tree(&session_id));
    Ok(serde_json::to_value(tree).map_err(|e| e.to_string())?)
}

/// `wsl_health`: a compact WSL host snapshot. We synthesize it from the locally
/// observable system (so the read tool works on this dev box without the WSL
/// agent connected) and additionally surface the supervised-session count. The
/// schema mirrors `t_hub_protocol::HostMetrics`.
fn wsl_health(ctx: &ControlContext) -> Result<Value, String> {
    let metrics = collect_host_metrics();
    let supervised = ctx.with_supervisor(|s| s.session_ids().len());
    Ok(json!({
        "metrics": metrics,
        "supervisedSessions": supervised,
    }))
}

/// `recent_sessions` (server-split M3 — first overlay source over the wire): the
/// daemon's recent recallable Claude sessions, so a thin client gets the Recent
/// list remotely. Mirrors the `recent_sessions` Tauri command (same
/// `RecentSession[]` shape), reusing its shared scan cache. When the daemon runs
/// natively in WSL (the M3 endgame) this read is a plain local filesystem walk
/// rather than the `wsl.exe`/UNC hop.
fn recent_sessions() -> Result<Value, String> {
    serde_json::to_value(crate::recent::recent_sessions_cached()).map_err(|e| e.to_string())
}

/// `archive_recent_project`: the Recent list's × made durable. Moves the project
/// at `args.cwd` out of `~/.claude/projects` into `projects-archive` (reversible)
/// so the dismissed project stops appearing in Recent and stops costing scan time.
/// Returns `true` on success.
fn archive_recent_project(args: &Value) -> Result<Value, String> {
    let cwd = args.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    if cwd.is_empty() {
        return Err("archive_recent_project requires a 'cwd'".into());
    }
    crate::recent::archive_project(cwd)?;
    Ok(Value::Bool(true))
}

/// `claude_usage` (server-split M3 overlay source): the daemon's Claude plan usage
/// (`claude -p /usage`, parsed), so a thin client gets the sidebar Usage strip
/// remotely. Mirrors the `claude_usage` Tauri command (same `ClaudeUsage` shape).
/// Runs the `/usage` flow synchronously on this blocking connection thread.
fn claude_usage() -> Result<Value, String> {
    serde_json::to_value(crate::usage::claude_usage_blocking()).map_err(|e| e.to_string())
}

/// `codex_usage` (server-split M3 overlay source): the daemon's Codex plan usage
/// (the newest `~/.codex/logs_*.sqlite` rate-limit row), so a thin client gets the
/// Codex usage strip remotely. Mirrors the `codex_usage` Tauri command (same
/// `CodexUsage` shape). Reads the log DB synchronously on this blocking connection
/// thread.
fn codex_usage() -> Result<Value, String> {
    serde_json::to_value(crate::codex::codex_usage_blocking()).map_err(|e| e.to_string())
}

/// `host_metrics` (server-split M3 overlay source #5): the WSL host's memory / CPU
/// / load / process snapshot for the sidebar health strip, so a thin client gets it
/// remotely. Mirrors the `host_metrics` Tauri command (same snake_case
/// `t_hub_protocol::HostMetrics` shape) — a transport swap, NOT a re-source.
///
/// **Source order matters (the regression trap).** The current topology runs the
/// daemon *in the Windows GUI process*, whose local `/proc` is the Windows host
/// (no `/proc` ⇒ all-zeros). So we PREFER the [`MetricsFn`] agent-bridge RPC (the
/// WSL agent's own `/proc`) — exactly what the in-process Tauri command does today,
/// so flipping the frontend onto this is a no-op locally. We fall back to the
/// daemon's local `/proc` **only on Linux** (`#[cfg(target_os = "linux")]`): that
/// covers the native-WSL / remote-Linux daemon endgame (where local `/proc` IS the
/// real host) and the Linux dev box (a strict improvement — today it shows nothing
/// until the agent connects). On Windows the fallback is compiled out, so we surface
/// the bridge's "not connected" error instead of zeros — preserving today's UX.
fn host_metrics(ctx: &ControlContext) -> Result<Value, String> {
    let bridge_result = match &ctx.metrics {
        Some(fetch) => fetch(),
        None => Err("host_metrics: agent bridge not wired into the control context".to_string()),
    };
    match bridge_result {
        Ok(m) => serde_json::to_value(m).map_err(|e| e.to_string()),
        Err(bridge_err) => {
            #[cfg(target_os = "linux")]
            {
                let _ = bridge_err; // the daemon's own /proc is the real host here
                serde_json::to_value(local_host_metrics()).map_err(|e| e.to_string())
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err(bridge_err)
            }
        }
    }
}

/// Build a snake_case [`t_hub_protocol::HostMetrics`] from the daemon's OWN `/proc`
/// (the M3 fallback when no agent bridge is attached — a native-WSL/Linux daemon).
/// Distinct from [`collect_host_metrics`], which emits the camelCase shape the MCP
/// `wsl_health` tool returns; this one matches the frontend's `host_metrics` wire.
#[cfg(target_os = "linux")]
fn local_host_metrics() -> t_hub_protocol::HostMetrics {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let (mem_total_kib, mem_available_kib, swap_total_kib, swap_free_kib) = read_meminfo();
    t_hub_protocol::HostMetrics {
        mem_total_kib,
        mem_available_kib,
        swap_total_kib,
        swap_free_kib,
        cpu_count: std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(0),
        load_avg: read_loadavg(),
        process_count: count_procs(),
        distro: read_pretty_name(),
        captured_at_ms: now_ms,
    }
}

/// `git_info` (server-split M3 overlay source): git awareness — branch / worktree
/// root / linked-worktree flag / dirty count — for a project cwd, so a thin client
/// gets the Files-panel git header remotely. Mirrors the `git_info` Tauri command
/// (same `GitInfo` shape), reusing its per-cwd TTL cache (the freeze fix). Args:
/// `path` (or `cwd`), the same cwd string the frontend passes.
fn git_info(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let cwd = arg_str(args, "path")
        .or_else(|| arg_str(args, "cwd"))
        .ok_or("git_info requires a 'path' (cwd) argument")?;
    // #27 follow-up: gate the peer-controlled cwd for a REMOTE peer to the operator
    // allowlist — else it leaks whether an arbitrary host path is a git repo + its
    // branch/dirty state. Loopback is unrestricted (scoped_create_path handles the
    // existing cwd; substitute the scoped path so check and use can't diverge).
    let cwd = if ctx.peer_is_loopback {
        cwd
    } else {
        files::scoped_create_path(&cwd, true, files::remote_file_roots())?
            .to_string_lossy()
            .into_owned()
    };
    serde_json::to_value(crate::git::git_info_cached(&cwd)).map_err(|e| e.to_string())
}

/// `index_project` (server-split M3 — the file index, build half): walk `root`,
/// (re)build the control channel's file index, and return its `IndexSummary`
/// (`{root, count}`). Mirrors the `index_project` Tauri command (same shape), so
/// the frontend's warmup flips onto the wire and a thin client indexes the REMOTE
/// tree. Args: `root` (required). Paired with [`search_files`], which reuses the
/// cache this warms (and self-indexes on demand if skipped).
fn index_project(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let root = arg_str(args, "root").ok_or("index_project requires a 'root' argument")?;
    let summary = files::control_index(
        &ctx.files,
        &root,
        !ctx.peer_is_loopback,
        files::remote_file_roots(),
    )?;
    serde_json::to_value(summary).map_err(|e| e.to_string())
}

/// `search_files`: fuzzy basename/path/extension search over a project root,
/// using the control channel's own index cache. Args: `root` (required),
/// `query` (required), `limit` (optional, default 20).
fn search_files(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let root = arg_str(args, "root").ok_or("search_files requires a 'root' argument")?;
    let query = arg_str(args, "query").unwrap_or_default();
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(20)
        .clamp(1, 1000);
    let hits = files::control_search(
        &ctx.files,
        &root,
        &query,
        limit,
        !ctx.peer_is_loopback,
        files::remote_file_roots(),
    )?;
    Ok(json!({ "root": root, "query": query, "hits": hits }))
}

/// `list_dir` (server-split #23 — the Files-panel TREE over the socket): a shallow
/// directory listing (dirs first, the directory-only gitignore rule). Mirrors the
/// `list_dir` Tauri command (same `DirEntry[]` shape). A REMOTE peer is SCOPED to
/// indexed roots (`files::control_list_dir`); loopback is unrestricted. Args: `path`
/// (required), `showIgnored` (optional, default false).
fn list_dir(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let path = arg_str(args, "path").ok_or("list_dir requires a 'path' argument")?;
    let show_ignored = args
        .get("showIgnored")
        .or_else(|| args.get("show_ignored"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let entries = files::control_list_dir(
        &path,
        show_ignored,
        !ctx.peer_is_loopback,
        files::remote_file_roots(),
    )?;
    serde_json::to_value(entries).map_err(|e| e.to_string())
}

/// `read_text_file` (server-split #23 — the Files-panel READER over the socket): a
/// size-capped, binary-rejecting text read. Mirrors the `read_text_file` Tauri
/// command (same `FileContents` shape). A REMOTE peer is SCOPED to indexed roots;
/// loopback is unrestricted. WRITE stays in-process (deferred). Args: `path`.
fn read_text_file(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let path = arg_str(args, "path").ok_or("read_text_file requires a 'path' argument")?;
    let contents =
        files::control_read_text(&path, !ctx.peer_is_loopback, files::remote_file_roots())?;
    serde_json::to_value(contents).map_err(|e| e.to_string())
}

/// `list_tabs`: the snapshot-track workspace tabs. Workspace-tab persistence is a
/// later workstream (PRD §8 snapshot track / persistence workstream G), so there
/// is no live tab store to read yet. We return an explicit empty list with a note
/// rather than failing, so the tool is callable and forward-compatible.
fn list_tabs() -> Result<Value, String> {
    Ok(json!({
        "tabs": [],
        "note": "workspace-tab persistence is not yet wired (PRD §8 snapshot track); \
                 returns an empty list until the persistence workstream lands.",
    }))
}

/// `read_terminal` / `capture_pane`: return a session's recent visible output as
/// plain text so an external Claude can SEE what the session shows. Talks to tmux
/// directly (`tmux -L t-hub capture-pane -p [-S -N] -t th_<id>`), no UI round
/// trip. Args: `sessionId` (required), `historyLines` (optional, default 0 =
/// visible screen only; clamped to keep responses bounded).
fn read_terminal(args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("read_terminal requires a 'sessionId' argument")?;
    let target = tmux_target(&session_id);
    let history = args
        .get("historyLines")
        .or_else(|| args.get("history_lines"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(10_000) as u32;
    let text = tmux::capture_pane_text(&target, history)
        .map_err(|e| format!("failed to capture pane for '{session_id}': {e}"))?;
    Ok(json!({
        "sessionId": session_id,
        "target": target,
        "historyLines": history,
        "text": text,
    }))
}

// ---------------------------------------------------------------------------
// Organization-tier handlers
// ---------------------------------------------------------------------------

/// `open_file`: resolve + read a capped text file for the requested path. This is
/// the one Organization-tier action that has a real, side-effect-free backing
/// implementation today (the Files reader), so the MCP "open a file" tool returns
/// the file's contents/metadata. Args: `path` (required).
fn open_file(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let path = arg_str(args, "path").ok_or("open_file requires a 'path' argument")?;
    // Same file-read scope as the #23 reader: a REMOTE peer may only open files
    // under the operator allowlist; loopback (the local MCP) is unrestricted.
    let contents =
        files::control_read_text(&path, !ctx.peer_is_loopback, files::remote_file_roots())?;
    Ok(serde_json::to_value(contents).map_err(|e| e.to_string())?)
}

/// `create_worktree` (WS-4): create a git worktree, then open it as a new
/// workspace tab with a terminal spawned in the worktree dir. We run the git
/// command HERE (mirroring the Tauri `git_worktree_add` exec) so a git failure
/// (e.g. a branch already checked out elsewhere) is reported up front and nothing
/// is forwarded to the UI on failure. On success we forward an
/// `add_worktree_workspace` command to the frontend via the [`ApplySink`]; the
/// `controlBridge` maps it to the workspace store's atomic create→tab→spawn helper
/// (`addWorktreeWorkspace`), which is the same path the FilePanel UI uses. The git
/// worktree already exists by then, so the store SKIPS its own `gitWorktreeAdd` —
/// the forward carries `alreadyCreated: true`. Args: `repoRoot`, `worktreePath`
/// (required); `branch`, `tabName` (optional).
fn create_worktree(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let repo_root = arg_str(args, "repoRoot")
        .or_else(|| arg_str(args, "repo_root"))
        .ok_or("create_worktree requires a 'repoRoot' argument")?;
    let worktree_path = arg_str(args, "worktreePath")
        .or_else(|| arg_str(args, "worktree_path"))
        .ok_or("create_worktree requires a 'worktreePath' argument")?;
    let branch = arg_str(args, "branch");
    let tab_name = arg_str(args, "tabName").or_else(|| arg_str(args, "tab_name"));

    // #27: a REMOTE peer may create worktrees ONLY under the operator allowlist —
    // this execs `git worktree add` SERVER-SIDE at peer-controlled paths (a write/
    // exec surface). Loopback (the local frontend/MCP) is unrestricted. For a remote
    // peer we substitute the SCOPED (normalized) paths so the security check and the
    // git call can't diverge; the new worktree dir doesn't exist yet, hence
    // scoped_create_path (checks the deepest existing ancestor).
    let (repo_root, worktree_path) = if ctx.peer_is_loopback {
        (repo_root, worktree_path)
    } else {
        let roots = files::remote_file_roots();
        (
            files::scoped_create_path(&repo_root, true, roots)?
                .to_string_lossy()
                .into_owned(),
            files::scoped_create_path(&worktree_path, true, roots)?
                .to_string_lossy()
                .into_owned(),
        )
    };

    // Create the worktree on disk first (shares git_worktree_add's impl). A git
    // failure short-circuits here — no tab/terminal is spawned for a failed add.
    let git_output = git::worktree_add(&repo_root, &worktree_path, branch.as_deref())?;

    // Forward the UI orchestration (new tab + spawn a terminal in the worktree
    // dir). The git worktree already exists, so `alreadyCreated: true` tells the
    // store not to run `gitWorktreeAdd` again.
    let forward = json!({
        "worktreePath": worktree_path,
        "repoRoot": repo_root,
        "branch": branch,
        "tabName": tab_name,
        "alreadyCreated": true,
    });
    let applied = match &ctx.apply_sink {
        Some(sink) => match sink.apply("add_worktree_workspace", &forward) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("t-hub-control: failed to forward 'add_worktree_workspace' to the UI: {e}");
                false
            }
        },
        None => false,
    };
    Ok(json!({
        "accepted": "create_worktree",
        "worktreePath": worktree_path,
        "branch": branch,
        "gitOutput": git_output,
        "audited": true,
        "applied": applied,
        "note": if applied {
            "worktree created on disk; a new workspace tab + terminal in the \
             worktree dir are being opened in the UI."
        } else {
            "worktree created on disk; the UI tab/terminal forward was not \
             delivered (headless/no sink)."
        },
    }))
}

/// `remove_worktree` (WS-4): remove a git worktree WITHOUT orphaning processes.
/// We do NOT run `git worktree remove` here, because any live tiles whose cwd is
/// inside the worktree must be detached FIRST (their tmux session survives a
/// detach; killing the dir out from under a running process would orphan it). So
/// we forward a `remove_worktree_workspace` command to the frontend, which (in the
/// workspace store) detaches every tile rooted in the worktree dir AND THEN calls
/// `gitWorktreeRemove` — keeping the detach→remove ordering correct. If no apply
/// sink is wired (headless), we have no UI to detach tiles, so we refuse rather
/// than risk an orphan, telling the caller why. Args: `repoRoot`, `worktreePath`
/// (required); `force` (optional).
fn remove_worktree(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let repo_root = arg_str(args, "repoRoot")
        .or_else(|| arg_str(args, "repo_root"))
        .ok_or("remove_worktree requires a 'repoRoot' argument")?;
    let worktree_path = arg_str(args, "worktreePath")
        .or_else(|| arg_str(args, "worktree_path"))
        .ok_or("remove_worktree requires a 'worktreePath' argument")?;
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);

    // #27: a REMOTE peer may remove worktrees ONLY under the operator allowlist —
    // this forwards a `git worktree remove` of a peer-controlled path to the UI.
    // Loopback is unrestricted. (scoped_create_path also handles the existing path.)
    let (repo_root, worktree_path) = if ctx.peer_is_loopback {
        (repo_root, worktree_path)
    } else {
        let roots = files::remote_file_roots();
        (
            files::scoped_create_path(&repo_root, true, roots)?
                .to_string_lossy()
                .into_owned(),
            files::scoped_create_path(&worktree_path, true, roots)?
                .to_string_lossy()
                .into_owned(),
        )
    };

    let forward = json!({
        "worktreePath": worktree_path,
        "repoRoot": repo_root,
        "force": force,
    });
    match &ctx.apply_sink {
        Some(sink) => {
            sink.apply("remove_worktree_workspace", &forward).map_err(|e| {
                format!("remove_worktree: failed to forward removal to the UI: {e}")
            })?;
            Ok(json!({
                "accepted": "remove_worktree",
                "worktreePath": worktree_path,
                "force": force,
                "audited": true,
                // We only *forwarded* the removal request over this channel — the
                // real `git worktree remove` runs later in the frontend (after it
                // detaches live tiles) and can still fail (dirty tree without
                // force, a tile detach throwing). The control channel cannot
                // confirm that completion synchronously, so we report `requested`,
                // not `applied`, to avoid falsely telling the caller it succeeded.
                "requested": true,
                "note": "the UI was asked to detach any live tiles rooted in the \
                         worktree and then remove it (git worktree remove). \
                         Completion is NOT confirmed synchronously over this \
                         channel — the removal runs in the frontend and may still \
                         fail (e.g. a dirty tree without force).",
            }))
        }
        // No UI to detach tiles ⇒ refuse rather than orphan a process.
        None => Err(
            "remove_worktree: no UI is connected to detach the worktree's live \
             tiles first; refusing to remove it to avoid orphaning a running \
             process (the app must be running for worktree removal)"
                .to_string(),
        ),
    }
}

/// Organization-tier actions whose effect is a pure UI mutation
/// (`focus_session`, `move_tile`, `rename_tab`). We **accept and audit** them
/// (PRD §11.2: "allowed with visible audit event") AND apply them: the accepted
/// `{command, args}` is forwarded to the frontend through the [`ApplySink`]
/// (a Tauri `control://apply` event), where `controlBridge.ts` dispatches it into
/// the workspace store. `applied` reflects whether the forward happened — `true`
/// once the app has wired its sink (the normal app path), `false` in the headless
/// proof/tests that run the listener without an `AppHandle` (still audited).
fn organization_apply(ctx: &ControlContext, command: &str, args: &Value) -> Result<Value, String> {
    let applied = match &ctx.apply_sink {
        Some(sink) => match sink.apply(command, args) {
            Ok(()) => true,
            // A forward failure is non-fatal: the action is still accepted +
            // audited. Surface the reason in the note but keep the response `ok`.
            Err(e) => {
                eprintln!("t-hub-control: failed to forward '{command}' to the UI: {e}");
                false
            }
        },
        // No sink (headless proof/tests): accept + audit only.
        None => false,
    };
    Ok(json!({
        "accepted": command,
        "args": args,
        "audited": true,
        "applied": applied,
        "note": if applied {
            "organization action accepted, audited, and forwarded to the UI \
             (control://apply) for application (PRD §11.2 organization tier)."
        } else {
            "organization action accepted + audited; UI application is delivered \
             by the frontend command (PRD §11.2 organization tier)."
        },
    }))
}

/// Process-changing commands (PRD §11.2: confirmation required) are gated off on
/// the control channel for the dev-box proof — they return a clear refusal rather
/// than spawning/killing anything. The MCP tool description marks them
/// confirmation-required; enabling real execution is a deliberate later step.
fn gated_process_change(command: &str) -> Result<Value, String> {
    Err(format!(
        "control: '{command}' is a process-changing action (PRD §11.2) and is \
         gated off in this build — it requires explicit confirmation/permission \
         and is not executed over the control channel yet"
    ))
}

/// `send_text`: type literal `text` into an existing session, optionally pressing
/// Enter to submit it. Process-changing (PRD §11.2): the MCP tool description
/// marks it CONFIRMATION REQUIRED. Backend-only — drives tmux directly
/// (`send-keys -l`), no UI round trip. Args: `sessionId` + `text` (required),
/// `enter` (optional, default true). Requires the session to exist.
fn send_text(args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("send_text requires a 'sessionId' argument")?;
    let text = arg_str(args, "text").ok_or("send_text requires a 'text' argument")?;
    let enter = args.get("enter").and_then(|v| v.as_bool()).unwrap_or(true);
    let target = tmux_target(&session_id);
    if !tmux::has_session(&target) {
        return Err(format!("send_text: no such session '{session_id}' (target {target})"));
    }
    tmux::send_text(&target, &text, enter)
        .map_err(|e| format!("failed to send text to '{session_id}': {e}"))?;
    Ok(json!({
        "accepted": "send_text",
        "sessionId": session_id,
        "target": target,
        "enter": enter,
        "audited": true,
    }))
}

/// `send_keys`: send one or more named control keys (e.g. `C-c`, `Up`, `Escape`)
/// to an existing session. Process-changing (confirmation-required). Backend-only
/// (`send-keys` with key-name interpretation). Args: `sessionId` (required) +
/// `keys` (required, a non-empty array of tmux key names).
fn send_keys(args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("send_keys requires a 'sessionId' argument")?;
    let keys: Vec<String> = args
        .get("keys")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|k| k.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if keys.is_empty() {
        return Err("send_keys requires a non-empty 'keys' array of tmux key names".into());
    }
    let target = tmux_target(&session_id);
    if !tmux::has_session(&target) {
        return Err(format!("send_keys: no such session '{session_id}' (target {target})"));
    }
    let key_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
    tmux::send_keys(&target, &key_refs)
        .map_err(|e| format!("failed to send keys to '{session_id}': {e}"))?;
    Ok(json!({
        "accepted": "send_keys",
        "sessionId": session_id,
        "target": target,
        "keys": keys,
        "audited": true,
    }))
}

/// `close_terminal`: kill an existing session and its process tree. Process-
/// changing/destructive (confirmation-required). Backend-only via tmux
/// `kill-session`, which is idempotent (already-gone ⇒ success). Args:
/// `sessionId` (required).
fn close_terminal(args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("close_terminal requires a 'sessionId' argument")?;
    let target = tmux_target(&session_id);
    tmux::kill_session(&target)
        .map_err(|e| format!("failed to close terminal '{session_id}': {e}"))?;
    Ok(json!({
        "accepted": "close_terminal",
        "sessionId": session_id,
        "target": target,
        "audited": true,
    }))
}

/// Resolve a caller-supplied session id to its tmux target name on the `t-hub`
/// socket. The control listener lists terminals by stripping the `th_` prefix
/// (see [`list_terminals`]), so a bare id maps back to `th_<id>`. We also accept a
/// caller that already passed the full `th_`-prefixed name (idempotent).
fn tmux_target(session_id: &str) -> String {
    // Single shared derivation (must match commands.rs / remote_pty so client +
    // server attach to the SAME session). See tmux::target_for_id.
    tmux::target_for_id(session_id)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Pull a string field out of a JSON args object.
fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Collect a `HostMetrics`-shaped snapshot from the local system. On Linux/WSL
/// this reads `/proc`; on other platforms it returns a best-effort skeleton so
/// the tool still responds. Mirrors the agent's `host` collector shape.
fn collect_host_metrics() -> Value {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    #[cfg(target_os = "linux")]
    {
        let (mem_total, mem_avail, swap_total, swap_free) = read_meminfo();
        let load_avg = read_loadavg();
        let cpu_count = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(0);
        let process_count = count_procs();
        let distro = read_pretty_name();
        return json!({
            "memTotalKib": mem_total,
            "memAvailableKib": mem_avail,
            "swapTotalKib": swap_total,
            "swapFreeKib": swap_free,
            "cpuCount": cpu_count,
            "loadAvg": load_avg,
            "processCount": process_count,
            "distro": distro,
            "capturedAtMs": now_ms,
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let cpu_count = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(0);
        json!({
            "memTotalKib": 0u64,
            "memAvailableKib": 0u64,
            "swapTotalKib": 0u64,
            "swapFreeKib": 0u64,
            "cpuCount": cpu_count,
            "loadAvg": [0.0, 0.0, 0.0],
            "processCount": 0u32,
            "distro": serde_json::Value::Null,
            "capturedAtMs": now_ms,
        })
    }
}

#[cfg(target_os = "linux")]
fn read_meminfo() -> (u64, u64, u64, u64) {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let get = |key: &str| -> u64 {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix(key) {
                return rest
                    .trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse()
                    .unwrap_or(0);
            }
        }
        0
    };
    (
        get("MemTotal:"),
        get("MemAvailable:"),
        get("SwapTotal:"),
        get("SwapFree:"),
    )
}

#[cfg(target_os = "linux")]
fn read_loadavg() -> [f32; 3] {
    let text = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let mut it = text.split_whitespace();
    let p = |s: Option<&str>| s.and_then(|v| v.parse().ok()).unwrap_or(0.0);
    [p(it.next()), p(it.next()), p(it.next())]
}

#[cfg(target_os = "linux")]
fn count_procs() -> u32 {
    std::fs::read_dir("/proc")
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .map(|n| n.chars().all(|c| c.is_ascii_digit()))
                        .unwrap_or(false)
                })
                .count() as u32
        })
        .unwrap_or(0)
}

#[cfg(target_os = "linux")]
fn read_pretty_name() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
            return Some(rest.trim_matches('"').to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Construction from app state
// ---------------------------------------------------------------------------

impl ControlContext {
    /// Build a [`ControlContext`] from the app's shared state. `supervisor` is a
    /// closure that locks the bridge's `Supervisor` and runs a visitor — supplied
    /// by `lib.rs` so this module doesn't reach into `AgentBridge` internals.
    pub fn new(
        status: Arc<StatusBridge>,
        supervisor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync>,
        token: String,
    ) -> Self {
        Self {
            status,
            supervisor,
            files: Arc::new(files::FileIndexState::new()),
            apply_sink: None,
            fanout: Arc::new(EventFanout::new()),
            metrics: None,
            idle_timeout: CONN_READ_TIMEOUT,
            peer_is_loopback: true,
            token,
        }
    }

    /// Share the [`EventFanout`] that backend events fan out through, so a
    /// control connection that subscribes ([`SUBSCRIBE_COMMAND`]) receives the live
    /// event stream (server-split M1). `lib.rs` builds the `Arc` once and hands the
    /// same clone to the socket emitter, so emits and subscribers meet here.
    pub fn with_event_fanout(mut self, fanout: Arc<EventFanout>) -> Self {
        self.fanout = fanout;
        self
    }

    /// Attach the [`ApplySink`] that forwards Organization-tier UI mutations to
    /// the frontend (a `control://apply` Tauri event). Builder-style so `lib.rs`
    /// can wire it after constructing the context, while headless tests/proofs
    /// keep the sink-less context (they audit without applying).
    pub fn with_apply_sink(mut self, sink: Arc<dyn ApplySink>) -> Self {
        self.apply_sink = Some(sink);
        self
    }

    /// Attach the agent-bridge host-metrics RPC (server-split M3, overlay source
    /// #5). Builder-style so `lib.rs` wires it from `AgentBridge` after construction
    /// while headless tests keep the metrics-less context (they fall back to local
    /// `/proc` on Linux, or report the missing bridge elsewhere). See [`MetricsFn`].
    pub fn with_metrics(mut self, metrics: MetricsFn) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Test/proof constructor: build a context directly over a shared
    /// `Mutex<Supervisor>` (and a status bridge), wiring the visitor closure
    /// internally. Lets the end-to-end integration test seed real supervision +
    /// status state, start a real listener, and exercise the real `t-hub-mcp`
    /// binary against it — without standing up the whole Tauri app.
    #[doc(hidden)]
    pub fn with_shared_supervisor(
        status: Arc<StatusBridge>,
        supervisor: Arc<parking_lot::Mutex<Supervisor>>,
        token: String,
    ) -> Self {
        let sup = supervisor.clone();
        let visitor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync> =
            Arc::new(move |f: &mut dyn FnMut(&Supervisor)| {
                let guard = sup.lock();
                f(&guard);
            });
        Self::new(status, visitor, token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Build a ControlContext backed by a real (empty) Supervisor + StatusBridge,
    /// with a fixed token, for dispatch tests.
    fn test_ctx(token: &str) -> ControlContext {
        let supervisor = Arc::new(StdMutex::new(Supervisor::new()));
        let sup_for_closure = supervisor.clone();
        let visitor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync> =
            Arc::new(move |f: &mut dyn FnMut(&Supervisor)| {
                let guard = sup_for_closure.lock().unwrap();
                f(&guard);
            });
        ControlContext::new(Arc::new(StatusBridge::new()), visitor, token.to_string())
    }

    #[test]
    fn bad_token_is_rejected_before_dispatch() {
        let ctx = test_ctx("secret");
        let req = ControlRequest {
            token: "wrong".into(),
            command: "list_tabs".into(),
            args: Value::Null,
            v: None,
        };
        let resp = dispatch_authenticated(&ctx, req);
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("unauthorized"));
    }

    #[test]
    fn good_token_dispatches() {
        let ctx = test_ctx("secret");
        let req = ControlRequest {
            token: "secret".into(),
            command: "list_tabs".into(),
            args: Value::Null,
            v: None,
        };
        let resp = dispatch_authenticated(&ctx, req);
        assert!(resp.ok, "expected ok, got {:?}", resp.error);
        assert!(resp.result.unwrap().get("tabs").is_some());
    }

    #[test]
    fn unknown_command_is_refused() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "definitely_not_a_command", &Value::Null).unwrap_err();
        assert!(err.contains("not exposed over the control channel"));
    }

    #[test]
    fn host_metrics_prefers_the_bridge_and_serializes_snake_case() {
        // A stubbed agent-bridge metrics RPC: the handler must PREFER it over the
        // daemon's local /proc, and serialize snake_case (the frontend wire shape in
        // src/ipc/protocol.ts) — NOT the camelCase `wsl_health` shape.
        let ctx = test_ctx("t").with_metrics(Arc::new(|| {
            Ok(t_hub_protocol::HostMetrics {
                mem_total_kib: 16_000_000,
                mem_available_kib: 8_000_000,
                swap_total_kib: 2_000_000,
                swap_free_kib: 1_500_000,
                cpu_count: 12,
                load_avg: [1.0, 0.5, 0.25],
                process_count: 432,
                distro: Some("Ubuntu 24.04".into()),
                captured_at_ms: 1_700_000_000_000,
            })
        }));
        let v = dispatch(&ctx, "host_metrics", &Value::Null).unwrap();
        assert_eq!(
            v.get("mem_total_kib").and_then(|x| x.as_u64()),
            Some(16_000_000)
        );
        assert_eq!(v.get("cpu_count").and_then(|x| x.as_u64()), Some(12));
        assert_eq!(v.get("process_count").and_then(|x| x.as_u64()), Some(432));
        assert_eq!(v.get("distro").and_then(|x| x.as_str()), Some("Ubuntu 24.04"));
        assert!(
            v.get("memTotalKib").is_none(),
            "must be snake_case, not the camelCase wsl_health shape"
        );
    }

    #[test]
    fn host_metrics_falls_back_when_the_bridge_errors() {
        // Bridge says "not connected". On Linux the daemon's own /proc IS the real
        // host (native-WSL / remote-Linux daemon, or the dev box), so we serve a
        // snake_case snapshot from it. On non-Linux the local /proc would be
        // all-zeros, so we surface the error instead (preserves today's UX).
        let ctx = test_ctx("t").with_metrics(Arc::new(|| Err("not connected".into())));
        let out = dispatch(&ctx, "host_metrics", &Value::Null);
        #[cfg(target_os = "linux")]
        {
            let v = out.expect("linux falls back to local /proc");
            assert!(
                v.get("mem_total_kib").is_some(),
                "snake_case local snapshot"
            );
            assert!(v.get("captured_at_ms").is_some());
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert!(out.unwrap_err().contains("not connected"));
        }
    }

    #[test]
    fn process_changing_spawn_is_gated() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).unwrap_err();
        assert!(err.contains("process-changing"), "got: {err}");
    }

    #[test]
    fn wait_for_status_immediate_match_does_not_time_out() {
        // An empty Supervisor reports `unknown` for any unseen session, so a
        // target of "unknown" matches on the first poll and returns at once.
        let ctx = test_ctx("t");
        let v = dispatch(
            &ctx,
            "wait_for_status",
            &json!({"sessionId": "absent", "targetStatus": "unknown"}),
        )
        .unwrap();
        assert_eq!(v["finalStatus"], "unknown");
        assert_eq!(v["timedOut"], false);
    }

    #[test]
    fn wait_for_status_accepts_target_array() {
        let ctx = test_ctx("t");
        let v = dispatch(
            &ctx,
            "wait_for_status",
            &json!({"sessionId": "absent", "targetStatus": ["completed", "unknown"]}),
        )
        .unwrap();
        assert_eq!(v["finalStatus"], "unknown");
        assert_eq!(v["timedOut"], false);
    }

    #[test]
    fn wait_for_status_times_out_when_target_never_seen() {
        // A status that never occurs for an unseen session, with a 0ms timeout,
        // returns on the first iteration with timedOut:true.
        let ctx = test_ctx("t");
        let v = dispatch(
            &ctx,
            "wait_for_status",
            &json!({"sessionId": "absent", "targetStatus": "completed", "timeoutMs": 0}),
        )
        .unwrap();
        assert_eq!(v["finalStatus"], "unknown");
        assert_eq!(v["timedOut"], true);
    }

    #[test]
    fn wait_for_status_requires_session_and_target() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "wait_for_status", &json!({"targetStatus": "completed"}))
            .unwrap_err();
        assert!(err.contains("sessionId"), "got: {err}");
        let err = dispatch(&ctx, "wait_for_status", &json!({"sessionId": "x"})).unwrap_err();
        assert!(err.contains("targetStatus"), "got: {err}");
    }

    // NOTE: the former `wait_for_status_captures_transient_edge_between_polls`
    // test lived here. It drove A(working) → B(completed) → A(working) from a
    // driver thread that slept 150ms hoping to land *inside* the poller's first
    // 500ms `wait_for_status` window — a wall-clock race that slips on a loaded
    // box (the driver can run before the dispatcher even captures its `consumed`
    // watermark, or after the window it was aiming for). The semantics it tried to
    // assert ("an edge logged strictly between two polls is still observed") can't
    // be expressed at this control layer without that race: the dispatcher
    // captures `consumed = current_seq()` *internally*, so any edge that is to land
    // at `seq > consumed` must be logged by a concurrent thread after that capture,
    // and the dispatcher exposes no hook to synchronize against.
    //
    // That edge-capture logic is `Supervisor::matched_since`, which `wait_for_status`
    // calls directly — and it is already proven DETERMINISTICALLY (no threads, no
    // sleeps) by `supervision::tests::transition_log_captures_transient_edge_through_b`,
    // which drives the same A→B→A sequence and asserts `matched_since` recovers the
    // transient Completed edge from the log. That is the real coverage; this
    // duplicate was dropped rather than kept as a flaky wall-clock race.
    //
    // The deterministic dispatcher-level behaviours that DON'T need a race are still
    // covered above: immediate current-status match (`wait_for_status_immediate_
    // match_does_not_time_out`), target arrays, and the 0ms timeout path.

    #[test]
    fn read_terminal_requires_session_id() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "read_terminal", &Value::Null).unwrap_err();
        assert!(err.contains("sessionId"), "got: {err}");
    }

    #[test]
    fn send_text_requires_session_and_text() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "send_text", &json!({"text": "hi"})).unwrap_err();
        assert!(err.contains("sessionId"), "got: {err}");
        let err = dispatch(&ctx, "send_text", &json!({"sessionId": "x"})).unwrap_err();
        assert!(err.contains("text"), "got: {err}");
    }

    #[test]
    fn send_keys_requires_non_empty_keys() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "send_keys", &json!({"sessionId": "x", "keys": []})).unwrap_err();
        assert!(err.contains("keys"), "got: {err}");
    }

    #[test]
    fn close_terminal_requires_session_id() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "close_terminal", &Value::Null).unwrap_err();
        assert!(err.contains("sessionId"), "got: {err}");
    }

    #[test]
    fn send_to_missing_session_is_a_clear_error() {
        // No `th_*` session named this exists ⇒ a readable "no such session".
        let ctx = test_ctx("t");
        let err = dispatch(
            &ctx,
            "send_text",
            &json!({"sessionId": "definitely_absent_xyz", "text": "hi"}),
        )
        .unwrap_err();
        assert!(err.contains("no such session"), "got: {err}");
    }

    #[test]
    fn tmux_target_maps_id_and_is_idempotent() {
        assert_eq!(tmux_target("abc"), "th_abc");
        assert_eq!(tmux_target("th_abc"), "th_abc");
    }

    #[test]
    fn remove_worktree_requires_args() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "remove_worktree", &json!({"worktreePath": "/x"})).unwrap_err();
        assert!(err.contains("repoRoot"), "got: {err}");
        let err = dispatch(&ctx, "remove_worktree", &json!({"repoRoot": "/r"})).unwrap_err();
        assert!(err.contains("worktreePath"), "got: {err}");
    }

    #[test]
    fn remove_worktree_without_sink_refuses_to_orphan() {
        // No apply sink (headless): we have no UI to detach the worktree's tiles,
        // so removal is refused rather than risk orphaning a running process.
        let ctx = test_ctx("t");
        let err = dispatch(
            &ctx,
            "remove_worktree",
            &json!({"repoRoot": "/r", "worktreePath": "/r/wt"}),
        )
        .unwrap_err();
        assert!(err.contains("orphan"), "got: {err}");
    }

    #[test]
    fn remove_worktree_with_sink_reports_requested_not_applied() {
        // With a sink wired, the removal is only *forwarded* to the UI — the real
        // `git worktree remove` runs later in the frontend and can still fail. The
        // response must be honest about that: `requested: true`, and NO misleading
        // `applied` field claiming synchronous success.
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());
        let v = dispatch(
            &ctx,
            "remove_worktree",
            &json!({"repoRoot": "/r", "worktreePath": "/r/wt", "force": true}),
        )
        .unwrap();
        assert_eq!(v["accepted"], "remove_worktree");
        assert_eq!(v["audited"], true);
        assert_eq!(v["requested"], true);
        assert!(
            v.get("applied").is_none(),
            "remove_worktree must not claim synchronous completion via 'applied'; got {v:?}"
        );
        // The note must not falsely imply confirmed completion.
        let note = v["note"].as_str().unwrap();
        assert!(note.contains("not confirmed") || note.contains("NOT confirmed"), "got: {note}");

        // The removal was actually forwarded to the UI with the args.
        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "remove_worktree_workspace");
        assert_eq!(calls[0].1["force"], true);
    }

    #[test]
    fn create_worktree_requires_args() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "create_worktree", &json!({"worktreePath": "/x"})).unwrap_err();
        assert!(err.contains("repoRoot"), "got: {err}");
        let err = dispatch(&ctx, "create_worktree", &json!({"repoRoot": "/r"})).unwrap_err();
        assert!(err.contains("worktreePath"), "got: {err}");
    }

    #[test]
    fn remote_worktree_ops_are_gated_to_the_allowlist() {
        // A REMOTE peer (peer_is_loopback=false) with no T_HUB_REMOTE_FILE_ROOTS is
        // refused BEFORE any git runs (#27) — the scope gate fires ahead of
        // git::worktree_add and the UI forward. (test_ctx defaults to loopback, so
        // the existing create/remove tests above keep exercising the unrestricted
        // local path.)
        let mut ctx = test_ctx("t");
        ctx.peer_is_loopback = false;
        for cmd in ["create_worktree", "remove_worktree"] {
            let err = dispatch(
                &ctx,
                cmd,
                &json!({"repoRoot": "/home/x/proj", "worktreePath": "/home/x/proj-wt/feature"}),
            )
            .unwrap_err();
            assert!(
                err.contains("disabled"),
                "{cmd} should be gated for a remote peer; got: {err}"
            );
        }
        // git_info probes git at a peer-controlled cwd — same allowlist gate.
        let err = dispatch(&ctx, "git_info", &json!({"path": "/home/x/whatever"})).unwrap_err();
        assert!(err.contains("disabled"), "git_info should be gated; got: {err}");
    }

    #[test]
    fn new_tab_and_focus_tab_are_organization_apply() {
        // No sink (headless): accepted + audited, but not applied — same contract
        // as the other organization-tier actions.
        let ctx = test_ctx("t");
        for (cmd, args) in [
            ("new_tab", json!({"name": "Logs"})),
            ("focus_tab", json!({"tabId": "tab-1"})),
        ] {
            let v = dispatch(&ctx, cmd, &args).unwrap();
            assert_eq!(v["accepted"], cmd);
            assert_eq!(v["audited"], true);
            assert_eq!(v["applied"], false);
        }
    }

    /// Live round-trip through dispatch: spawn a real tmux session, type a line
    /// via `send_text`, read it back via `read_terminal`, then `close_terminal`.
    /// Needs a real tmux on PATH (WSL2 dev shell; not the Windows CI target).
    #[test]
    fn live_send_read_close_roundtrip() {
        let id = format!(
            "mcp3test{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let target = format!("th_{id}");
        let _ = tmux::kill_session(&target);
        tmux::new_session(&target, "/tmp", None).expect("spawn session");

        let ctx = test_ctx("t");
        dispatch(
            &ctx,
            "send_text",
            &json!({"sessionId": id, "text": "echo MCP3_ROUNDTRIP_OK", "enter": true}),
        )
        .expect("send_text should succeed");
        std::thread::sleep(std::time::Duration::from_millis(300));

        let v = dispatch(&ctx, "read_terminal", &json!({"sessionId": id})).unwrap();
        assert!(
            v["text"].as_str().unwrap().contains("MCP3_ROUNDTRIP_OK"),
            "read_terminal should show the echoed sentinel; got {v:?}"
        );

        let c = dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
        assert_eq!(c["accepted"], "close_terminal");
        assert!(!tmux::has_session(&target), "session should be gone after close");
    }

    #[test]
    fn idle_connection_is_closed_after_the_read_timeout() {
        use std::io::Read;
        use std::net::{TcpListener, TcpStream};

        // A listener + a context with a SHORT idle timeout. A client that connects
        // and never sends a request must be closed by the server (M2b hardening),
        // not left to park the handler thread forever.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let mut ctx = test_ctx("t");
        ctx.idle_timeout = std::time::Duration::from_millis(200);

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            // Returns Ok once the idle read times out and the request loop breaks.
            let _ = handle_conn(stream, &ctx);
        });

        // Connect, send NOTHING, then read: the server should close the socket
        // after ~200ms, so the read returns 0 (EOF). The generous 2s client-side
        // timeout only trips if the server FAILED to close us — the regression.
        let mut client = TcpStream::connect(addr).expect("connect");
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut buf = [0u8; 16];
        let n = client
            .read(&mut buf)
            .expect("read should return EOF, not time out");
        assert_eq!(n, 0, "server should have closed the idle connection (EOF)");

        server.join().unwrap();
    }

    #[test]
    fn protocol_version_gate_rejects_a_skewed_client() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let ctx = test_ctx("secret");
        // Serve one connection per assertion (each `send` opens + closes one).
        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                let (stream, _) = listener.accept().expect("accept");
                let _ = handle_conn(stream, &ctx);
            }
        });

        // Open a connection, send one frame, read one response line.
        let send = |frame: Value| -> Value {
            let mut s = TcpStream::connect(addr).expect("connect");
            let mut bytes = serde_json::to_vec(&frame).unwrap();
            bytes.push(b'\n');
            s.write_all(&bytes).unwrap();
            let mut reader = BufReader::new(s);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            serde_json::from_str::<Value>(line.trim()).unwrap()
        };

        // A valid token but a MISMATCHED version is rejected — the gate fires before
        // dispatch, with a clear, actionable message.
        let bad = send(json!({"token": "secret", "command": "list_tabs", "v": 999}));
        assert_eq!(bad["ok"], false);
        assert!(
            bad["error"]
                .as_str()
                .unwrap()
                .contains("protocol version mismatch"),
            "got: {bad}"
        );

        // The matching version passes the gate and dispatches normally.
        let good = send(json!({"token": "secret", "command": "list_tabs", "v": PROTOCOL_VERSION}));
        assert_eq!(good["ok"], true, "got: {good}");

        // No version field at all stays accepted (backward-compat: the MCP / legacy
        // clients don't advertise one).
        let legacy = send(json!({"token": "secret", "command": "list_tabs"}));
        assert_eq!(legacy["ok"], true, "got: {legacy}");

        server.join().unwrap();
    }

    #[test]
    fn loopback_file_read_bypasses_the_scope() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let ctx = test_ctx("secret");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let _ = handle_conn(stream, &ctx);
        });

        // list_dir on a NON-indexed path: over loopback the peer is trusted, so the
        // #23 scope is bypassed and the listing succeeds. This proves handle_conn
        // tags the 127.0.0.1 peer as loopback -> enforce_scope=false end-to-end (a
        // logic inversion would either over-restrict locally or — worse — fail to
        // restrict a real remote peer; the core's enforce=true path is covered by
        // the files.rs scope test).
        let mut s = TcpStream::connect(addr).expect("connect");
        let tmp = std::env::temp_dir().to_string_lossy().into_owned();
        let frame = json!({"token": "secret", "command": "list_dir", "args": {"path": tmp}});
        let mut bytes = serde_json::to_vec(&frame).unwrap();
        bytes.push(b'\n');
        s.write_all(&bytes).unwrap();
        let mut reader = BufReader::new(s);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let resp: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(resp["ok"], true, "loopback list_dir should bypass scope: {resp}");
        // Close the client so the server's next read hits EOF and handle_conn
        // returns immediately — otherwise it would park until the idle timeout.
        drop(reader);

        server.join().unwrap();
    }

    #[test]
    fn theme_commands_are_forwarded_by_name() {
        let ctx = test_ctx("t");
        // Forwarded by name; not yet wired ⇒ a clear, theme-specific error (not
        // the generic "unknown command" arm). This proves the forward path.
        for cmd in ["get_theme", "set_theme"] {
            let err = dispatch(&ctx, cmd, &Value::Null).unwrap_err();
            assert!(err.contains("theme command handler"), "got: {err}");
        }
    }

    #[test]
    fn get_status_requires_session_id() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "get_status", &Value::Null).unwrap_err();
        assert!(err.contains("sessionId"), "got: {err}");
    }

    #[test]
    fn get_status_returns_unknown_for_unseen_session() {
        let ctx = test_ctx("t");
        let v = dispatch(&ctx, "get_status", &json!({"sessionId": "nope"})).unwrap();
        assert_eq!(v["status"], "unknown");
        assert_eq!(v["sessionId"], "nope");
        assert!(v["snapshot"].is_null());
    }

    #[test]
    fn supervision_tree_unknown_session_is_null() {
        let ctx = test_ctx("t");
        let v = dispatch(&ctx, "supervision_tree", &json!({"sessionId": "nope"})).unwrap();
        assert!(v.is_null());
    }

    #[test]
    fn supervision_session_ids_returns_an_array() {
        // An empty supervisor reports no sessions — but the command returns a JSON
        // array (not null/error), matching the Tauri command's `Vec<String>`.
        let ctx = test_ctx("t");
        let v = dispatch(&ctx, "supervision_session_ids", &Value::Null).unwrap();
        assert!(v.is_array(), "expected an array, got {v:?}");
        assert_eq!(v.as_array().unwrap().len(), 0);
    }

    #[test]
    fn wsl_health_has_metrics_and_supervised_count() {
        let ctx = test_ctx("t");
        let v = dispatch(&ctx, "wsl_health", &Value::Null).unwrap();
        assert!(v.get("metrics").is_some());
        assert_eq!(v["supervisedSessions"], 0);
        // The metrics object always carries capturedAtMs + cpuCount.
        assert!(v["metrics"].get("capturedAtMs").is_some());
        assert!(v["metrics"].get("cpuCount").is_some());
    }

    #[test]
    fn organization_actions_are_accepted_and_audited() {
        // No apply sink (headless): accepted + audited, but not applied.
        let ctx = test_ctx("t");
        for cmd in ["focus_session", "move_tile", "rename_tab"] {
            let v = dispatch(&ctx, cmd, &json!({"x": 1})).unwrap();
            assert_eq!(v["accepted"], cmd);
            assert_eq!(v["audited"], true);
            assert_eq!(v["applied"], false);
        }
    }

    /// A recording sink that captures every forwarded `{command, args}` so the
    /// test can assert the dispatcher forwards Organization-tier mutations to it.
    struct RecordingSink {
        calls: StdMutex<Vec<(String, Value)>>,
    }
    impl ApplySink for RecordingSink {
        fn apply(&self, command: &str, args: &Value) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push((command.to_string(), args.clone()));
            Ok(())
        }
    }

    #[test]
    fn organization_actions_are_forwarded_and_applied_with_a_sink() {
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());

        for cmd in ["focus_session", "move_tile", "rename_tab"] {
            let v = dispatch(&ctx, cmd, &json!({"tabId": "tab-1"})).unwrap();
            assert_eq!(v["accepted"], cmd);
            assert_eq!(v["audited"], true);
            // With a sink wired, the action is forwarded to the UI and applied.
            assert_eq!(v["applied"], true, "expected applied:true for {cmd}");
        }

        // Every Organization-tier command reached the sink, in order, with args.
        let calls = sink.calls.lock().unwrap();
        let names: Vec<&str> = calls.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(names, ["focus_session", "move_tile", "rename_tab"]);
        assert_eq!(calls[0].1, json!({"tabId": "tab-1"}));
    }

    #[test]
    fn search_files_searches_a_real_tree() {
        // Build a tiny fixture and search it end-to-end through dispatch.
        let mut root = std::env::temp_dir();
        root.push(format!("t-hub-control-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("README.md"), "# hi").unwrap();

        let ctx = test_ctx("t");
        let v = dispatch(
            &ctx,
            "search_files",
            &json!({ "root": root.to_string_lossy(), "query": "main", "limit": 5 }),
        )
        .unwrap();
        let hits = v["hits"].as_array().unwrap();
        assert!(
            hits.iter().any(|h| h["relPath"] == "src/main.rs"),
            "expected src/main.rs in {hits:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_file_reads_text_contents() {
        let mut root = std::env::temp_dir();
        root.push(format!("t-hub-control-open-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("notes.md");
        std::fs::write(&file, "# Title\n\nbody").unwrap();

        let ctx = test_ctx("t");
        let v = dispatch(&ctx, "open_file", &json!({ "path": file.to_string_lossy() })).unwrap();
        assert_eq!(v["ext"], "md");
        assert!(v["text"].as_str().unwrap().contains("# Title"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn event_fanout_streams_a_frame_to_a_subscriber() {
        // server-split M1: a registered subscriber receives each backend event as a
        // newline-delimited `{event,payload}` frame; unregister drops it. Uses a
        // real loopback socket pair but is deterministic (no disconnect-timing
        // races — we assert the explicit unregister, not write-error pruning).
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let fanout = EventFanout::new();
        let id = fanout.register(server);
        assert_eq!(fanout.subscriber_count(), 1);

        fanout.emit_event(
            "session://status",
            &json!({ "sessionId": "s1", "status": "working" }),
        );

        let mut reader = BufReader::new(&client);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let frame: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(frame["event"], "session://status");
        assert_eq!(frame["payload"]["sessionId"], "s1");
        assert_eq!(frame["payload"]["status"], "working");

        fanout.unregister(id);
        assert_eq!(fanout.subscriber_count(), 0);
    }

    #[test]
    fn is_allowed_peer_admits_only_loopback_and_tailscale() {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
        // Loopback — always.
        assert!(is_allowed_peer(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(is_allowed_peer(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        // Tailscale CGNAT 100.64.0.0/10 (IPv4).
        assert!(is_allowed_peer(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
        assert!(is_allowed_peer(IpAddr::V4(Ipv4Addr::new(100, 127, 255, 254))));
        // Tailscale ULA fd7a:115c::/32 (IPv6).
        assert!(is_allowed_peer(IpAddr::V6(Ipv6Addr::new(
            0xfd7a, 0x115c, 0, 0, 0, 0, 0, 1
        ))));
        // LAN / public — rejected before auth.
        assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))));
        assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))));
        assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        // 100.x OUTSIDE the 64..=127 second octet is NOT Tailscale — rejected.
        assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(100, 0, 0, 1))));
        assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(100, 128, 0, 1))));
        // IPv4-mapped IPv6 (how IPv4 peers arrive on a dual-stack [::] bind): a
        // mapped loopback / tailnet IP is admitted, a mapped public IP rejected.
        assert!(is_allowed_peer("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_allowed_peer("::ffff:100.64.0.1".parse().unwrap()));
        assert!(!is_allowed_peer("::ffff:8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn handshake_roundtrips_through_json() {
        let h = ControlHandshake {
            addr: "127.0.0.1:5000".into(),
            token: "abc".into(),
            pid: 42,
            protocol_version: PROTOCOL_VERSION,
        };
        let s = serde_json::to_string(&h).unwrap();
        let back: ControlHandshake = serde_json::from_str(&s).unwrap();
        assert_eq!(back.addr, "127.0.0.1:5000");
        assert_eq!(back.token, "abc");
        assert_eq!(back.pid, 42);
        assert_eq!(back.protocol_version, PROTOCOL_VERSION);
    }
}
