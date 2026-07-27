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
//! On startup we bind `127.0.0.1:0` (an ephemeral port) and atomically publish the
//! address plus an ambient read credential to one stable authoritative handshake
//! file. The MCP binary reads that file to connect. A commissioned Captain then
//! proves its durable session identity to acquire a short-lived scoped control
//! lease. `T_HUB_CONTROL_ADDR` + `T_HUB_CONTROL_TOKEN` remain legacy overrides for
//! tests and harnesses. Binding to loopback keeps the channel host-local (PRD
//! §11.3: expose only what T-Hub needs).
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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::audit::{AuditLog, AuditMeta};
use crate::claude::StatusBridge;
use crate::governor::SpawnGovernor;
#[cfg(test)]
use crate::harness::{attest_launch_permissions, observe_harness_process};
use crate::harness::{Harness, HarnessPermissionAttestation, PermMode, CREW_DEFAULT_PERMISSION};
use crate::supervision::Supervisor;
use crate::{
    agent_session::{AgentCheckpoint, AgentEvent, AgentSessionRecord, RuntimeState},
    files, git, plane, pty, tmux,
};

#[doc(hidden)]
pub use crate::cortana_reconcile::{
    CortanaDurableIdentity, CortanaExecutableIdentity, CortanaManagedLaunchIntent,
    CortanaManagedLaunchPhase, CortanaManagedOwnerToken, CortanaManagedSystemTools,
    CortanaOrphanEffectIdentity, CortanaRecoveryState,
};
#[doc(hidden)]
pub use crate::identity::{IdentityStore, Role as SessionIdentityRole};

/// A single control request: a command name + free-form JSON args, authenticated
/// by an ambient tier token or identity-bound scoped lease.
#[derive(Deserialize)]
pub struct ControlRequest {
    /// Ambient read token, trusted host control token, or scoped Captain lease.
    #[serde(default)]
    pub token: String,
    /// The command/tool name to dispatch (e.g. `list_terminals`).
    pub command: String,
    /// Command arguments. Shape is per-command; absent ⇒ `null`.
    #[serde(default)]
    pub args: Value,
    /// Comms-plane Phase 3: the caller's PER-SESSION token (`T_HUB_SESSION_TOKEN`),
    /// carried ALONGSIDE the tier `token` so the app can resolve WHICH session (in what
    /// role, on what ship) is calling and enforce the enqueue/access ACLs against an
    /// unforgeable-across-sessions identity (`identity.rs` mint/bind/resolve). Absent for
    /// a request carrying the exact in-process host proof (the app's own webview,
    /// MCP, or fleet wake), which never minted a session token.
    /// A shared control token without that host proof does not substitute for an
    /// identity on Organization or ProcessChanging requests.
    /// `#[serde(default)]` so every pre-Phase-3 client keeps working unchanged.
    #[serde(default)]
    pub session: String,
    /// In-process-only proof that this request came through the local Tauri shim.
    /// Never published or injected into terminal sessions.
    #[serde(default)]
    pub host: String,
    /// Wire protocol version the client speaks (server-split M2b). Absent for the
    /// MCP / any legacy client (then unchecked, for backward compatibility); when
    /// present it must be `<=` [`PROTOCOL_VERSION`] or the server rejects the request.
    /// A LOWER version is accepted (the protocol is backward-compatible: v2 added
    /// only the opt-in binary PTY framing of T13); only a HIGHER, unknown-future
    /// version is rejected.
    #[serde(default)]
    pub v: Option<u32>,
}

impl std::fmt::Debug for ControlRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlRequest")
            .field("token", &"<redacted>")
            .field("command", &self.command)
            .field("args", &"<redacted>")
            .field("session", &"<redacted>")
            .field("host", &"<redacted>")
            .field("v", &self.v)
            .finish()
    }
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
    #[serde(rename = "errorDetails", skip_serializing_if = "Option::is_none")]
    pub error_details: Option<Value>,
    #[serde(rename = "errorKind", skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    /// True when `error` is a TRANSIENT, RETRYABLE control-plane condition rather
    /// than a definitive failure - today: a liveness probe that timed out under a
    /// degraded spawn path (the spawn-wedge de-conflation's `Unknown` arm). A fleet
    /// client / MCP consumer can auto-retry a wedge WITHOUT substring-matching the
    /// human `error` text (LOW-1 from the PR-58 review). Skipped (absent) on `ok`
    /// and on non-retryable errors, so the wire is unchanged for every existing
    /// consumer. Set structurally via the reserved [`RETRYABLE_ERROR_MARKER`], never
    /// by matching prose.
    #[serde(skip_serializing_if = "is_false")]
    pub retryable: bool,
}

/// serde `skip_serializing_if` predicate: omit a `bool` field when it is `false`.
fn is_false(b: &bool) -> bool {
    !*b
}

/// Reserved, unforgeable marker PREFIX that tags a dispatcher error string as
/// RETRYABLE. A `\u{1}` (SOH control char) can never appear in a real message, so
/// the wire layer detects the flag by an exact `strip_prefix` (NOT a fragile
/// substring search of human prose) and moves it into
/// [`ControlResponse::retryable`], leaving the human text clean. Callers never write
/// this by hand - they build retryable errors through [`retryable_error`].
const RETRYABLE_ERROR_MARKER: &str = "\u{1}retryable\u{1}";
const AGENT_FOLLOWUP_ERROR_MARKER: &str = "\u{1}agent-followup\u{1}";

fn agent_followup_error(code: &str, message: impl std::fmt::Display) -> String {
    format!("{AGENT_FOLLOWUP_ERROR_MARKER}{code}\u{1}{message}")
}

/// Build a dispatcher error tagged RETRYABLE: the human `message` prefixed with the
/// machine [`RETRYABLE_ERROR_MARKER`]. Any `ControlResponse::err` built from this
/// string (directly or via the dispatch `Result` mapping) surfaces `retryable:true`
/// with the marker stripped from the wire text.
fn retryable_error(message: impl std::fmt::Display) -> String {
    format!("{RETRYABLE_ERROR_MARKER}{message}")
}

fn is_retryable_error(message: &str) -> bool {
    message.starts_with(RETRYABLE_ERROR_MARKER)
}

fn cortana_tmux_observation_error(context: &str, error: crate::tmux::TmuxError) -> String {
    let retryable = error.is_retryable_observation();
    let message = format!("{context}: {error}");
    if retryable {
        retryable_error(message)
    } else {
        message
    }
}

fn cortana_harness_observation_error(
    context: &str,
    error: crate::harness::LaunchAttestationError,
) -> String {
    let retryable = error == crate::harness::LaunchAttestationError::UnreadableEvidence;
    let message = format!("{context}: {error}");
    if retryable {
        retryable_error(message)
    } else {
        message
    }
}

/// Keep an inconclusive Cortana observation outside the destructive quarantine
/// branch. Definitive mismatches remain available to the caller as the inner
/// error, while retryable observations return immediately with authority intact.
fn separate_retryable_cortana_observation<T>(
    result: Result<T, String>,
) -> Result<Result<T, String>, String> {
    match result {
        Err(error) if is_retryable_error(&error) => Err(error),
        result => Ok(result),
    }
}

impl ControlResponse {
    fn ok(result: Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
            error_details: None,
            error_kind: None,
            retryable: false,
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        // Structurally detect the retryable marker (never a prose match): a message
        // built via `retryable_error` carries the reserved SOH prefix, which we strip
        // and hoist into the structured `retryable` flag.
        let raw = msg.into();
        if let Some(operation) = raw
            .strip_prefix("git_required code=git_required operation=")
            .and_then(|value| value.strip_suffix(" capability=git action=initialize_git"))
        {
            return Self::git_required(operation);
        }
        if let Some(rest) = raw.strip_prefix("git_init_recovery code=git_init_recovery operation=")
        {
            let Some((operation, rest)) = rest.split_once(" phase=") else {
                return Self::plain_error(raw);
            };
            let Some((phase, message)) = rest.split_once(" message=") else {
                return Self::plain_error(raw);
            };
            return Self::git_init_recovery(operation, phase, message);
        }
        if let Some((code, message)) = raw
            .strip_prefix(AGENT_FOLLOWUP_ERROR_MARKER)
            .and_then(|rest| rest.split_once('\u{1}'))
        {
            return Self {
                ok: false,
                result: None,
                error: Some(message.to_string()),
                error_details: Some(json!({
                    "code": code,
                    "operation": "agent_followup",
                })),
                error_kind: Some(code.to_string()),
                retryable: code == "persistence_failed",
            };
        }
        match raw.strip_prefix(RETRYABLE_ERROR_MARKER) {
            Some(clean) => Self {
                ok: false,
                result: None,
                error: Some(clean.to_string()),
                error_details: None,
                error_kind: None,
                retryable: true,
            },
            None => Self::plain_error(raw),
        }
    }

    fn plain_error(message: String) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(message),
            error_details: None,
            error_kind: None,
            retryable: false,
        }
    }

    fn git_required(operation: &str) -> Self {
        let message = format!(
            "Git capability is required for {operation}; initialize Git with initialize_git"
        );
        Self {
            ok: false,
            result: None,
            error: Some(message),
            error_details: Some(json!({
                "code": "git_required",
                "operation": operation,
                "capability": "git",
                "action": "initialize_git",
            })),
            error_kind: Some("git_required".into()),
            retryable: false,
        }
    }

    fn git_init_recovery(operation: &str, phase: &str, message: &str) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(message.to_string()),
            error_details: Some(json!({
                "code": "git_init_recovery",
                "operation": operation,
                "phase": phase,
            })),
            error_kind: Some("git_init_recovery".into()),
            retryable: false,
        }
    }

    fn powder_retired(operation: &str) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(format!(
                "{operation} is retired; use the agent session operations instead"
            )),
            error_details: None,
            error_kind: Some("powder_retired".to_string()),
            retryable: false,
        }
    }
}

/// The handshake record written so the MCP binary can find + authenticate to the
/// listener. Serialized to `~/.t-hub/control.json`.
#[derive(Serialize, Deserialize)]
pub struct ControlHandshake {
    /// `127.0.0.1:<port>` the listener bound to.
    pub addr: String,
    /// Ambient read-only secret the client may present for discovery and lease
    /// renewal. The shared full-power control token is never serialized here.
    pub token: String,
    /// Per-launch **read** capability token (socket-gate Phase 2). Grants the Read
    /// tier only. Added alongside `token` so a least-privilege consumer can discover
    /// a read-only credential; `#[serde(default)]` keeps older handshake
    /// files/readers parseable.
    #[serde(default)]
    pub read_token: String,
    /// PID of the app that owns this listener (diagnostics / staleness checks).
    pub pid: u32,
    /// The control wire protocol version this server speaks ([`PROTOCOL_VERSION`]).
    /// A local client (the MCP) can read it to detect a stale binary; defaults to 0
    /// when absent so older handshake readers/files stay parseable.
    #[serde(default)]
    pub protocol_version: u32,
    /// Per-app-process listener identity. A port-only rebind keeps this value;
    /// an app restart replaces it.
    #[serde(default)]
    pub instance_id: String,
    /// Monotonic listener generation within one app process.
    #[serde(default)]
    pub listener_generation: u64,
    /// Epoch-ms publication timestamp for diagnostics and future-skew checks.
    #[serde(default)]
    pub published_at: u64,
    /// In-process-only full-power **control** token for the TRUSTED local frontend.
    /// The app's own webview drives terminals through the in-process `control_request`
    /// command, which must authenticate with full control even under Phase 3 hardening
    /// (where `token` above is only the read token). This handshake struct is returned
    /// directly to `control_client::install` over the trusted in-process channel, so
    /// the local frontend reads its full token here rather than from the published
    /// (possibly read-only) `token`. `#[serde(skip_serializing)]` guarantees it is
    /// NEVER written to `control.json` (external scrapers stay read-only under
    /// hardening); `#[serde(default)]` keeps older handshake files/readers parseable.
    #[serde(skip_serializing, default)]
    pub local_control_token: String,
    /// In-process-only origin credential for the local Tauri request shim.
    #[serde(skip_serializing, default)]
    pub local_host_token: String,
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

/// The event channel accepted Organization forwards are ALSO broadcast on (T12:
/// MCP organization continuity for socket clients). The native cockpit is a
/// socket client, not a Tauri webview, so it can never receive the
/// `control://apply` Tauri event the [`ApplySink`] emits; instead every accepted
/// forward is additionally emitted to event subscribers as
/// `{"event":"control://apply","payload":{"command":..,"args":..}}`, and the
/// native `apply/` module dispatches it into its workspace model exactly the way
/// `controlBridge.ts` dispatches the Tauri event into the webview store.
/// Additive and webview-safe: the ApplySink path is unchanged, a fanout with no
/// subscribers is a no-op, and the app's own `control://event` forwarder re-emits
/// this channel under an envelope nothing in the webview routes into applyControl
/// (verified: `controlBridge.ts` listens only to the raw Tauri event).
pub const APPLY_EVENT_CHANNEL: &str = "control://apply";

/// The command name a client sends to switch a control connection into an
/// **event-subscription stream** (server-split M1). Instead of one response, the
/// connection stays open and the server streams `{"event":<channel>,"payload":
/// <value>}` frames (newline-delimited) until the client disconnects. This is the
/// send half of the M1 event wire; the receive half is
/// `control_client::spawn_event_forwarder`.
/// The control wire protocol version (server-split M2b; T13 binary PTY framing).
/// Bump this on any additive/breaking change to the request/response/event/PTY
/// framing. The server advertises it in the handshake file + the subscribe ack so a
/// client can DISCOVER the server's capabilities (e.g. that it can speak binary PTY
/// frames — T13).
///
/// **v2 (T13):** the server can speak length-prefixed BINARY PTY frames on an
/// attach connection when the client opts in (`attach_pty` arg `"binary": true`).
/// This is ADDITIVE and NEGOTIATED per-attach: a client that doesn't opt in — the
/// webview, any v1 peer — still gets the v1 base64-NDJSON framing unchanged. So the
/// request-version gate ([`ControlRequest::v`]) accepts every version *at or below*
/// this one and rejects only a HIGHER (unknown-future) version; a v1 client talking
/// to this v2 server keeps working.
pub const PROTOCOL_VERSION: u32 = 2;

pub const SUBSCRIBE_COMMAND: &str = "__subscribe_events";

/// The command name that switches a control connection into a **PTY stream**
/// (server-split M2a): the connection becomes a full-duplex terminal channel —
/// the server captures scrollback, spawns the PTY-runs-`tmux attach`, streams
/// output frames down, and reads write/resize frames back up, until the client
/// disconnects (then it detaches — the tmux session survives).
///
/// Args: `sessionId` (required), `cols`, `rows`, and (T13) `binary` (optional bool).
///
/// **Framing (T13, negotiated here):** with `binary` absent/false the connection
/// speaks **v1** — newline-delimited JSON, base64 payloads: opening
/// `{"scrollback":"<b64>"}`, then `{"out":"<b64>"}` / `{"exit":code}` (plus an
/// ignorable idle `{"keepalive":"..."}`) down and `{"write":"<b64>"}` /
/// `{"resize":{cols,rows}}` up. With `"binary": true` it speaks **v2** —
/// length-prefixed binary frames ([`pty::binframe`]): a SCROLLBACK frame opens,
/// then OUT / EXIT / KEEPALIVE down and WRITE / RESIZE up, with no base64 and no
/// JSON envelope on the firehose. The webview (v1) is unaffected; only a client
/// that asks for `binary` gets v2.
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
///
/// The socket is wrapped in its OWN `Arc<Mutex<..>>` so [`emit_event`](EventFanout::emit_event)
/// can hold the tiny registry lock only long enough to CLONE these handles, then
/// do every blocking socket write with the registry lock RELEASED. The
/// per-subscriber mutex still serializes writes to the SAME socket (frames never
/// interleave) without letting one stuck subscriber's write block emits to any
/// OTHER subscriber - or the registry lock that register/unregister need.
struct Subscriber {
    id: u64,
    writer: Arc<Mutex<TcpStream>>,
}

impl EventFanout {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a subscriber's socket; returns an id for [`unregister`](Self::unregister).
    ///
    /// We set a WRITE TIMEOUT on the subscriber's socket: [`emit_event`](Self::emit_event)
    /// still does a blocking `write_all` per frame, so without a bound a single
    /// stuck/slow client (its kernel send buffer full) would block THAT subscriber's
    /// write - and any emit thread queued on its per-socket mutex - indefinitely. On
    /// loopback the local forwarder drains promptly so this never fires; it matters
    /// the moment M2 binds this wire to a remote/Tailscale host. On timeout the write
    /// errors and `emit_event` prunes the subscriber, so one wedged client self-heals.
    /// (The registry lock is no longer held across these writes - see `emit_event` -
    /// so a stuck client can no longer stall other subscribers or registration.)
    fn register(&self, writer: TcpStream) -> u64 {
        let _ = writer.set_write_timeout(Some(std::time::Duration::from_secs(5)));
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut subs) = self.subs.lock() {
            subs.push(Subscriber {
                id,
                writer: Arc::new(Mutex::new(writer)),
            });
        }
        id
    }

    #[cfg(test)]
    pub(crate) fn register_test_subscriber(&self, writer: TcpStream) -> u64 {
        self.register(writer)
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
    /// never affects another or the emitting (journal-consumption) path.
    ///
    /// SERVE-PATH WEDGE FIX: the registry lock is held only long enough to CLONE the
    /// per-subscriber socket handles, then RELEASED before any blocking write. The
    /// previous version held `subs` across every `write_all`/`flush`, each bounded by
    /// a 5s `SO_SNDTIMEO`; a single stuck/slow subscriber (a webview that stopped
    /// draining) parked the registry lock for up to 5s PER stuck subscriber. That
    /// serialized EVERY emit, every Organization-tier apply-broadcast, and every
    /// `register`/`unregister`/`subscriber_count` behind the slowest peer - the exact
    /// "one stuck peer stalls everyone" shape this channel must never have. Now each
    /// write takes only that subscriber's OWN mutex (frames to the same socket still
    /// never interleave), so a stuck subscriber can delay only its own delivery, and
    /// the registry lock a new subscriber needs is never held across a socket write.
    ///
    /// Returns how many subscribers the frame was delivered to (T12: the apply
    /// broadcast reports delivery when no [`ApplySink`] is wired). Existing
    /// callers ignore it.
    pub fn emit_event(&self, channel: &str, payload: &Value) -> usize {
        let mut frame = match serde_json::to_vec(&json!({ "event": channel, "payload": payload })) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("t-hub-control: failed to serialize event {channel}: {e}");
                return 0;
            }
        };
        frame.push(b'\n');
        // Snapshot the subscriber handles under the registry lock, then drop it
        // BEFORE any blocking write (see the wedge note above). Cloning an
        // `Arc<Mutex<TcpStream>>` is O(1) and never touches the socket.
        let targets: Vec<(u64, Arc<Mutex<TcpStream>>)> = {
            let Ok(subs) = self.subs.lock() else {
                return 0;
            };
            subs.iter().map(|s| (s.id, Arc::clone(&s.writer))).collect()
        };
        // Write each frame with the registry lock released. The per-subscriber
        // mutex serializes concurrent emits to the SAME socket (no interleaving)
        // but never blocks writes to a different subscriber. A poisoned per-socket
        // mutex (a panicked prior writer) is treated as a failed delivery and pruned.
        let mut failed: Vec<u64> = Vec::new();
        let mut delivered = 0usize;
        for (id, writer) in &targets {
            let ok = match writer.lock() {
                Ok(mut w) => w.write_all(&frame).and_then(|()| w.flush()).is_ok(),
                Err(_) => false,
            };
            if ok {
                delivered += 1;
            } else {
                failed.push(*id);
            }
        }
        // Prune the subscribers whose write failed, under a brief re-lock. A
        // subscriber registered (or already pruned) since the snapshot is
        // unaffected - we only drop ids we actually saw fail.
        if !failed.is_empty() {
            if let Ok(mut subs) = self.subs.lock() {
                subs.retain(|s| !failed.contains(&s.id));
            }
        }
        delivered
    }

    /// Number of live subscribers (diagnostics / tests).
    pub fn subscriber_count(&self) -> usize {
        self.subs.lock().map(|s| s.len()).unwrap_or(0)
    }
}

/// One workspace tab as the control channel sees it: a stable id, a display name,
/// and the ids of the tiles it holds (TASK C / #22).
///
/// Serialized camelCase (`{id, name, tileIds}`) in BOTH directions: the frontend
/// reports its tabs up as this shape, and `list_tabs` returns it verbatim.
pub const CAPTAIN_WORKSPACE_ID: &str = "captains-reserved";
pub const CAPTAIN_WORKSPACE_NAME: &str = "Captain Workspace";
const WORKSPACE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceKind {
    Work,
    Captain,
}

#[derive(Debug, Clone)]
pub struct TabRecord {
    pub id: String,
    pub name: String,
    /// Tile ids in this Workspace, in order.
    pub tile_ids: Vec<String>,
}

impl TabRecord {
    pub fn kind(&self) -> WorkspaceKind {
        if self.id == CAPTAIN_WORKSPACE_ID {
            WorkspaceKind::Captain
        } else {
            WorkspaceKind::Work
        }
    }

    fn canonicalize(mut self) -> Result<Self, String> {
        if self.id.trim().is_empty() {
            return Err("workspace id must not be empty".into());
        }
        if self.kind() == WorkspaceKind::Captain {
            self.name = CAPTAIN_WORKSPACE_NAME.to_string();
        } else if self.name.trim().is_empty() {
            return Err(format!("work Workspace '{}' has an empty name", self.id));
        }
        Ok(self)
    }
}

impl Serialize for TabRecord {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            schema_version: u32,
            id: &'a str,
            name: &'a str,
            kind: WorkspaceKind,
            tile_ids: &'a [String],
        }
        let canonical_name = if self.kind() == WorkspaceKind::Captain {
            CAPTAIN_WORKSPACE_NAME
        } else {
            &self.name
        };
        Wire {
            schema_version: WORKSPACE_SCHEMA_VERSION,
            id: &self.id,
            name: canonical_name,
            kind: self.kind(),
            tile_ids: &self.tile_ids,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TabRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            #[serde(default)]
            schema_version: u32,
            id: String,
            name: String,
            #[serde(default)]
            kind: Option<WorkspaceKind>,
            #[serde(default, alias = "order")]
            tile_ids: Vec<String>,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.schema_version > WORKSPACE_SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "unsupported Workspace schemaVersion {}",
                wire.schema_version
            )));
        }
        let derived = if wire.id == CAPTAIN_WORKSPACE_ID {
            WorkspaceKind::Captain
        } else {
            WorkspaceKind::Work
        };
        if wire.kind.is_some_and(|kind| kind != derived) {
            return Err(serde::de::Error::custom(
                "Workspace kind conflicts with its canonical id",
            ));
        }
        TabRecord {
            id: wire.id,
            name: wire.name,
            tile_ids: wire.tile_ids,
        }
        .canonicalize()
        .map_err(serde::de::Error::custom)
    }
}

/// A full, versioned copy of the registry: what `list_tabs` returns and what every
/// organization forward carries down to the UI (the UI renders FROM this).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrySnapshot {
    pub seq: u64,
    pub active_tab_id: Option<String>,
    pub tabs: Vec<TabRecord>,
}

/// Outcome of a UI up-sync report (see [`TabRegistry::report`]).
pub enum ReportOutcome {
    /// The report was based on the current revision and replaced the registry.
    /// `removed_tab_ids` are the tabs that existed before this report but are
    /// absent from it (the primary UI tab-close path): the caller prunes them
    /// from the captains registry's `workspaceTabIds` so a normally-closed tab
    /// never lingers as a phantom controlled-workspace.
    Accepted {
        seq: u64,
        removed_tab_ids: Vec<String>,
    },
    /// The report predates a server-side mutation the reporter has not applied
    /// yet; the registry is unchanged and the caller gets the authoritative
    /// snapshot to converge on.
    Stale(RegistrySnapshot),
}

#[derive(Clone, Default)]
struct RegistryInner {
    tabs: Vec<TabRecord>,
    /// Process-local terminal death tombstones. A cleanup commit records the
    /// tombstone under the same identity transaction that removes the tile, so
    /// a queued move/report cannot resurrect a killed or recovery-retired id.
    retired_tile_ids: std::collections::HashSet<String>,
    /// The UI's active (visible) tab, mirrored from its reports and from
    /// `focus_tab`. Used as the default placement target for un-named spawns and
    /// exposed via `list_tabs` so a socket caller can prove focus did NOT move.
    active_tab_id: Option<String>,
    /// Monotonic revision. Bumped on every accepted mutation, server- or
    /// UI-originated. A UI report carrying a stale `baseSeq` is rejected, which is
    /// what makes server-side mutations durable against the old lost-update race
    /// (UI report clobbering a headless `move_tile`).
    seq: u64,
}

/// The CORE's authoritative workspace-tab registry.
///
/// Ownership model (headless-org): the SERVER owns the tab/tile organization -
/// every organization-tier command applies to this registry first (and errors on
/// invalid targets), then the authoritative [`RegistrySnapshot`] is forwarded to
/// the UI, which renders from it. The frontend up-syncs USER-originated layout
/// changes via `report_workspace_tabs`, but a report based on a stale revision
/// (`baseSeq < seq`) is rejected and answered with the current snapshot, so a
/// hidden tab or a minimized/suspended webview can never silently undo a headless
/// mutation. This replaces the earlier mirror model where the frontend was the
/// source of truth and `move_tile` could be accepted-then-lost.
///
/// Deliberately NOT the PRD §8 persistence layer - in-memory, per app run; the
/// frontend still persists layout for restarts and seeds this via its first report.
pub struct TabRegistry {
    inner: Mutex<RegistryInner>,
    identity_transaction: Mutex<()>,
    startup_authoritative: AtomicBool,
}

impl Default for TabRegistry {
    fn default() -> Self {
        Self {
            inner: Mutex::new(RegistryInner::default()),
            identity_transaction: Mutex::new(()),
            startup_authoritative: AtomicBool::new(true),
        }
    }
}

impl TabRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct the production startup projection before terminal liveness has
    /// been observed. Reads and full-layout reports remain retryable until
    /// [`Self::publish_startup`] publishes the reconciled projection.
    pub fn new_pending_startup() -> Self {
        Self {
            inner: Mutex::new(RegistryInner::default()),
            identity_transaction: Mutex::new(()),
            startup_authoritative: AtomicBool::new(false),
        }
    }

    pub fn startup_is_authoritative(&self) -> bool {
        self.startup_authoritative.load(Ordering::Acquire)
    }

    pub fn require_authoritative_startup(&self) -> Result<(), String> {
        if self.startup_is_authoritative() {
            Ok(())
        } else {
            Err("workspace startup reconciliation is still pending".into())
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RegistryInner> {
        // A poisoned registry lock means a panic mid-mutation; the data is a plain
        // Vec so continuing with it is safe (same policy as recovering the guard).
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn identity_transaction(&self) -> std::sync::MutexGuard<'_, ()> {
        self.identity_transaction
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Replace the whole registry (legacy up-sync; no staleness check). Kept for
    /// reporters that predate `baseSeq` (native cockpit) and for tests.
    fn normalize_tabs(tabs: Vec<TabRecord>) -> Result<Vec<TabRecord>, String> {
        let mut normalized = Vec::with_capacity(tabs.len().saturating_add(1));
        let mut ids = std::collections::HashSet::new();
        let mut captain_tiles = Vec::new();
        for tab in tabs {
            let tab = tab.canonicalize()?;
            if tab.kind() == WorkspaceKind::Captain {
                for tile in tab.tile_ids {
                    if !captain_tiles.contains(&tile) {
                        captain_tiles.push(tile);
                    }
                }
                continue;
            }
            if !ids.insert(tab.id.clone()) {
                return Err(format!("duplicate work Workspace id '{}'", tab.id));
            }
            normalized.push(tab);
        }
        normalized.push(TabRecord {
            id: CAPTAIN_WORKSPACE_ID.to_string(),
            name: CAPTAIN_WORKSPACE_NAME.to_string(),
            tile_ids: captain_tiles,
        });
        Ok(normalized)
    }

    fn replace_inner(&self, tabs: Vec<TabRecord>, publish_startup: bool) {
        let mut g = self.lock();
        g.tabs = Self::normalize_tabs(tabs)
            .expect("internal tab fixtures must contain valid Workspace records");
        if !g
            .active_tab_id
            .as_ref()
            .is_some_and(|active| g.tabs.iter().any(|tab| &tab.id == active))
        {
            g.active_tab_id = g.tabs.first().map(|tab| tab.id.clone());
        }
        g.seq += 1;
        if publish_startup {
            self.startup_authoritative.store(true, Ordering::Release);
        }
    }

    pub fn replace(&self, tabs: Vec<TabRecord>) {
        self.replace_inner(tabs, false);
    }

    pub fn publish_startup(&self, tabs: Vec<TabRecord>) {
        self.replace_inner(tabs, true);
    }

    /// A UI up-sync with optimistic-concurrency: accepted (and revision bumped)
    /// only when `base_seq` matches the current revision; `None` means a legacy
    /// reporter and is accepted unconditionally.
    pub fn report(
        &self,
        tabs: Vec<TabRecord>,
        active_tab_id: Option<String>,
        base_seq: Option<u64>,
    ) -> Result<ReportOutcome, String> {
        self.require_authoritative_startup()?;
        let tabs = Self::normalize_tabs(tabs)?;
        let mut g = self.lock();
        if let Some(base) = base_seq {
            if base != g.seq {
                return Ok(ReportOutcome::Stale(RegistrySnapshot {
                    seq: g.seq,
                    active_tab_id: g.active_tab_id.clone(),
                    tabs: g.tabs.clone(),
                }));
            }
        }
        // Which tabs is this report dropping? Computed atomically under the lock
        // (old ids not present in the new set) so captains-registry pruning can
        // never race a concurrent tab mutation.
        let removed_tab_ids: Vec<String> = g
            .tabs
            .iter()
            .filter(|old| !tabs.iter().any(|t| t.id == old.id))
            .map(|t| t.id.clone())
            .collect();
        g.tabs = tabs;
        // Adopt the reported active tab only if it names a tab in the SAME report
        // (defensive: a torn report must not leave the pointer dangling), and
        // heal a pointer the new tab set invalidated either way.
        if let Some(active) = active_tab_id.filter(|id| g.tabs.iter().any(|t| &t.id == id)) {
            g.active_tab_id = Some(active);
        } else if !g
            .active_tab_id
            .as_ref()
            .is_some_and(|id| g.tabs.iter().any(|t| &t.id == id))
        {
            g.active_tab_id = g.tabs.first().map(|t| t.id.clone());
        }
        g.seq += 1;
        Ok(ReportOutcome::Accepted {
            seq: g.seq,
            removed_tab_ids,
        })
    }

    /// A clone of the current tab list (for tests / callers that only need tabs).
    pub fn snapshot(&self) -> Vec<TabRecord> {
        self.lock().tabs.clone()
    }

    /// The full versioned snapshot (`list_tabs` + every organization forward).
    pub fn snapshot_full(&self) -> RegistrySnapshot {
        let g = self.lock();
        RegistrySnapshot {
            seq: g.seq,
            active_tab_id: g.active_tab_id.clone(),
            tabs: g.tabs.clone(),
        }
    }

    pub fn prune_gone_tiles_if_seq(
        &self,
        expected_seq: u64,
        live_tile_ids: &std::collections::HashSet<String>,
    ) -> Option<RegistrySnapshot> {
        let mut g = self.lock();
        if g.seq != expected_seq {
            return None;
        }
        let gone = g
            .tabs
            .iter()
            .flat_map(|tab| tab.tile_ids.iter())
            .filter(|tile_id| !live_tile_ids.contains(*tile_id))
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        if gone.is_empty() {
            return None;
        }
        g.retired_tile_ids.extend(gone.iter().cloned());
        for tab in &mut g.tabs {
            tab.tile_ids.retain(|tile_id| !gone.contains(tile_id));
        }
        g.seq = g.seq.saturating_add(1);
        Some(RegistrySnapshot {
            seq: g.seq,
            active_tab_id: g.active_tab_id.clone(),
            tabs: g.tabs.clone(),
        })
    }

    /// The id of the tab whose name matches exactly, if any (named-placement reuse).
    fn id_for_name(&self, name: &str) -> Option<String> {
        self.lock()
            .tabs
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.id.clone())
    }

    /// True if a tab with this id exists.
    fn has_tab(&self, id: &str) -> bool {
        self.lock().tabs.iter().any(|t| t.id == id)
    }

    fn kind_for_id(&self, id: &str) -> Option<WorkspaceKind> {
        self.lock()
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .map(TabRecord::kind)
    }

    fn work_workspace(&self, id: &str) -> Option<TabRecord> {
        self.lock()
            .tabs
            .iter()
            .find(|tab| tab.id == id && tab.kind() == WorkspaceKind::Work)
            .cloned()
    }

    fn workspace_for_tile(&self, tile_id: &str) -> Option<String> {
        self.lock()
            .tabs
            .iter()
            .find(|tab| tab.tile_ids.iter().any(|tile| tile == tile_id))
            .map(|tab| tab.id.clone())
    }

    fn restore_tile_placement_locked(
        &self,
        tile_id: &str,
        workspace_id: Option<&str>,
    ) -> Result<(), String> {
        match workspace_id {
            Some(workspace_id) => self.move_tile(tile_id, workspace_id),
            None => {
                self.remove_tile_locked(tile_id);
                Ok(())
            }
        }
    }

    /// Record a new (empty) tab so its id is addressable immediately. No-op (no
    /// revision bump) if a tab with this id already exists.
    fn insert_tab(&self, id: &str, name: &str) {
        let mut g = self.lock();
        if !g.tabs.iter().any(|t| t.id == id) {
            g.tabs.push(TabRecord {
                id: id.to_string(),
                name: if id == CAPTAIN_WORKSPACE_ID {
                    CAPTAIN_WORKSPACE_NAME.to_string()
                } else {
                    name.to_string()
                },
                tile_ids: Vec::new(),
            });
            g.seq += 1;
        }
    }

    /// Remove an empty tab created by an uncommitted transaction. A reused tab
    /// is never passed here. If another actor placed a tile meanwhile, preserve
    /// the tab and report the ownership conflict instead of deleting shared
    /// state. Unlike the user-facing close policy, an owned last empty tab may
    /// be removed because it did not exist before the failed transaction.
    fn rollback_owned_empty_tab(&self, id: &str) -> Result<(), String> {
        let mut g = self.lock();
        let Some(index) = g.tabs.iter().position(|tab| tab.id == id) else {
            return Ok(());
        };
        if !g.tabs[index].tile_ids.is_empty() {
            return Err(format!(
                "owned tab rollback refused because tab '{id}' gained a tile"
            ));
        }
        g.tabs.remove(index);
        if g.active_tab_id.as_deref() == Some(id) {
            g.active_tab_id = g.tabs.first().map(|tab| tab.id.clone());
        }
        g.seq += 1;
        Ok(())
    }

    /// Move a tile into `tab_id`: drop it from every tab, then append. Errors when
    /// the target tab is unknown (the old silent no-op is exactly how a headless
    /// `move_tile` got accepted-then-lost). A tile id not currently placed anywhere
    /// is still placed (it may be a live session the UI has not adopted yet).
    fn move_tile(&self, tile_id: &str, tab_id: &str) -> Result<(), String> {
        let mut g = self.lock();
        if g.retired_tile_ids.contains(tile_id) {
            return Err(format!(
                "move_tile: terminal '{tile_id}' was retired and cannot be reinserted"
            ));
        }
        if !g.tabs.iter().any(|t| t.id == tab_id) {
            return Err(format!(
                "move_tile: unknown tabId '{tab_id}' (list_tabs shows valid ids; new_tab creates one)"
            ));
        }
        for t in g.tabs.iter_mut() {
            t.tile_ids.retain(|x| x != tile_id);
        }
        if let Some(t) = g.tabs.iter_mut().find(|t| t.id == tab_id) {
            t.tile_ids.push(tile_id.to_string());
        }
        g.seq += 1;
        Ok(())
    }

    /// Place a freshly-spawned tile, resolving the target ATOMICALLY under the
    /// registry lock: `tab_id` if it still exists, else the active tab, else the
    /// first tab. A spawned session must ALWAYS land in the registry - the target
    /// tab may have been closed in the race window between spawn and placement,
    /// and leaving the tile unplaced would orphan it outside every tab. Returns
    /// the tab id actually used; `None` only when the registry holds no tabs at
    /// all (headless boot - the UI adopts the tile into its active tab and
    /// reports back).
    fn place_tile_with_fallback(&self, tile_id: &str, tab_id: Option<&str>) -> Option<String> {
        let mut g = self.lock();
        if g.retired_tile_ids.contains(tile_id) {
            return None;
        }
        let target = tab_id
            .filter(|id| {
                g.tabs
                    .iter()
                    .any(|tab| &tab.id == id && tab.kind() == WorkspaceKind::Work)
            })
            .map(str::to_string)
            .or_else(|| {
                g.active_tab_id.clone().filter(|id| {
                    g.tabs
                        .iter()
                        .any(|tab| &tab.id == id && tab.kind() == WorkspaceKind::Work)
                })
            })
            .or_else(|| {
                g.tabs
                    .iter()
                    .find(|tab| tab.kind() == WorkspaceKind::Work)
                    .map(|tab| tab.id.clone())
            })?;
        for t in g.tabs.iter_mut() {
            t.tile_ids.retain(|x| x != tile_id);
        }
        if let Some(t) = g.tabs.iter_mut().find(|t| t.id == target) {
            t.tile_ids.push(tile_id.to_string());
        }
        g.seq += 1;
        Some(target)
    }

    /// Place a new tile only if the exact target still exists.
    /// This is used by durable identity transactions where fallback would silently
    /// change ownership after authority was validated.
    fn place_tile_exact(&self, tile_id: &str, tab_id: &str) -> Result<String, String> {
        let mut g = self.lock();
        if g.retired_tile_ids.contains(tile_id) {
            return Err(format!(
                "terminal '{tile_id}' was retired and cannot be placed"
            ));
        }
        if !g.tabs.iter().any(|tab| tab.id == tab_id) {
            return Err(format!(
                "Workspace '{tab_id}' closed before exact terminal placement"
            ));
        }
        for tab in &mut g.tabs {
            tab.tile_ids.retain(|candidate| candidate != tile_id);
        }
        let target = g
            .tabs
            .iter_mut()
            .find(|tab| tab.id == tab_id)
            .expect("target existence checked under the same lock");
        target.tile_ids.push(tile_id.to_string());
        g.seq = g.seq.saturating_add(1);
        Ok(tab_id.to_string())
    }

    /// Drop a tile from every tab (a terminal was closed). Returns true (and bumps
    /// the revision) only if the tile was actually placed somewhere.
    fn remove_tile_locked(&self, tile_id: &str) -> bool {
        let mut g = self.lock();
        let mut removed = false;
        for t in g.tabs.iter_mut() {
            let before = t.tile_ids.len();
            t.tile_ids.retain(|x| x != tile_id);
            removed |= t.tile_ids.len() != before;
        }
        if removed {
            g.seq += 1;
        }
        removed
    }

    fn retire_tile_locked(&self, tile_id: &str) -> bool {
        let mut g = self.lock();
        let newly_retired = g.retired_tile_ids.insert(tile_id.to_string());
        let mut removed = false;
        for tab in &mut g.tabs {
            let before = tab.tile_ids.len();
            tab.tile_ids.retain(|candidate| candidate != tile_id);
            removed |= tab.tile_ids.len() != before;
        }
        let changed = newly_retired || removed;
        if changed {
            g.seq = g.seq.saturating_add(1);
        }
        changed
    }

    /// Rename a tab. Errors when the tab is unknown.
    fn rename_tab(&self, tab_id: &str, name: &str) -> Result<(), String> {
        if tab_id == CAPTAIN_WORKSPACE_ID {
            return Err("rename_tab: Captain Workspace cannot be renamed".into());
        }
        let mut g = self.lock();
        match g.tabs.iter_mut().find(|t| t.id == tab_id) {
            Some(t) => {
                t.name = name.to_string();
                g.seq += 1;
                Ok(())
            }
            None => Err(format!("rename_tab: unknown tabId '{tab_id}'")),
        }
    }

    #[cfg(test)]
    fn remove_tab(&self, tab_id: &str, force: bool) -> Result<Vec<String>, String> {
        self.validate_remove_tab(tab_id, force)?;
        let mut g = self.lock();
        let idx = g
            .tabs
            .iter()
            .position(|tab| tab.id == tab_id)
            .ok_or_else(|| format!("close_tab: unknown tabId '{tab_id}'"))?;
        let removed = g.tabs.remove(idx);
        let active_valid = g
            .active_tab_id
            .as_ref()
            .is_some_and(|id| g.tabs.iter().any(|tab| &tab.id == id));
        if !active_valid {
            g.active_tab_id = g.tabs.first().map(|tab| tab.id.clone());
        }
        g.seq += 1;
        Ok(removed.tile_ids)
    }

    #[cfg(test)]
    fn validate_remove_tab(&self, tab_id: &str, force: bool) -> Result<Vec<String>, String> {
        if tab_id == CAPTAIN_WORKSPACE_ID {
            return Err("close_tab: Captain Workspace cannot be closed".into());
        }
        let g = self.lock();
        let Some(idx) = g.tabs.iter().position(|t| t.id == tab_id) else {
            return Err(format!("close_tab: unknown tabId '{tab_id}'"));
        };
        if g.tabs
            .iter()
            .filter(|tab| tab.kind() == WorkspaceKind::Work)
            .count()
            <= 1
        {
            return Err(
                "close_tab: refusing to close the last tab (the final Work Workspace)".to_string(),
            );
        }
        if !g.tabs[idx].tile_ids.is_empty() && !force {
            return Err(format!(
                "close_tab: tab '{tab_id}' still holds {} tile(s); close its terminals first \
                 (close_terminal) or pass force: true",
                g.tabs[idx].tile_ids.len()
            ));
        }
        Ok(g.tabs[idx].tile_ids.clone())
    }

    /// Mirror the UI's active tab (from `focus_tab` - the one organization command
    /// that intentionally moves the user's view). Validate-and-set ATOMICALLY:
    /// returns false (pointer untouched) when the tab no longer exists, so a
    /// focus_tab racing a close_tab cannot point the registry at a deleted tab.
    fn set_active_tab(&self, tab_id: &str) -> bool {
        let mut g = self.lock();
        if !g.tabs.iter().any(|t| t.id == tab_id) {
            return false;
        }
        g.active_tab_id = Some(tab_id.to_string());
        true
    }

    /// Auto-name a new tab "Workspace N" at the lowest free index — the same scheme
    /// the frontend's `addTab` uses, so core- and UI-created tabs share one naming.
    fn auto_name(&self) -> String {
        let used: std::collections::HashSet<u32> = self
            .lock()
            .tabs
            .iter()
            .filter_map(|t| {
                t.name
                    .strip_prefix("Workspace ")
                    .and_then(|n| n.trim().parse().ok())
            })
            .collect();
        let mut n = 1u32;
        while used.contains(&n) {
            n += 1;
        }
        format!("Workspace {n}")
    }
}

// ---------------------------------------------------------------------------
// Captains registry (captain-chat phase 2: ship-registry unification)
// ---------------------------------------------------------------------------

/// Epoch-ms now (registry lifecycle timestamps: `Orphaned{since}` etc.). 0 on the
/// impossible pre-1970 clock, matching the other epoch-ms sites in this file.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The reserved ship slug the apex singleton occupies. A legacy `ship_slug ==
/// "cortana"` captain claim (the pre-item-2 slug hack, R-H2) migrates to a
/// first-class `role: Cortana` on this slug (item-2 §2.4/D2, MED-6).
pub const CORTANA_SLUG: &str = "cortana";

/// Bounded retry budget for the claim compare-and-swap (item-2 §2.2/MED-3). The
/// window between the lock-free liveness probe and the re-validated mutate is tiny;
/// a few retries absorb a concurrent mutation. Exhausting it (pathological churn)
/// surfaces as a contended error rather than looping forever.
const CLAIM_CAS_ATTEMPTS: usize = 8;

/// The current on-disk schema version for `captains.json`.
/// v0 used terminal-keyed captains and string crew; v1 introduced durable ship
/// identities; v2 adds registered projects plus reset-safe Captain, Crew, and
/// Powder bindings; v3 adds durable per-project Powder event cursors; v4 makes
/// provider identity strict; v5 adds durable Powder completion recovery state;
/// v6 adds exact run-bound mutation intent recovery; v7 binds those intents to
/// Powder's canonical request digest while preserving legacy v1 intents in a
/// fail-closed state; v8 records bounded definitive terminal rejections so an
/// exact replay stays stable without wedging replacement work or cleanup.
/// v10 adds a durable pre-release recovery state machine for exact post-bind
/// dispatch rollback, preventing an older registry shape from dropping a frozen
/// Powder scope after an ambiguous release. v11 pins the canonical protected
/// endpoint as well, preventing a remapped profile name from selecting a new
/// Powder instance during recovery. v12 replaces that persisted endpoint with
/// an endpoint digest. v13 replaces the unsalted URL-derived digest with a
/// standard HMAC-SHA-256 identity keyed by the protected client credential, so
/// a protected profile URL is never copied into the registry or a captain sync
/// payload and the durable value is not a URL equality oracle.
/// v17 adds the Powder-independent durable agent-session collection.
/// v21 adds the exact Cortana orphan-replacement transaction. An older binary
/// must not discard that write-ahead authorization after an external tmux
/// effect, so snapshots containing it are protected by the schema boundary.
/// v22 adds one-use provenance for an exact healthy schema-v18 Cortana binding.
/// It permits retirement of that exact legacy runtime after its identity has
/// disappeared without weakening the stable-discovery contract for adoption.
/// v23 binds orphan retirement to one exact tmux and Linux process generation.
/// Snapshots older than a recovery shape load and upgrade only when they carry
/// no such recovery state.  A recovery record requires its exact schema and
/// fails closed rather than letting an older binary discard it.
/// v25 replaces process enumeration with a versioned user-systemd/cgroup-v2
/// owner token and converts all pre-owner Cortana generations into durable,
/// authority-revoked quarantine records that are never automatically signaled.
/// It also records the generated unit and nonce before any launch effect.
/// v26 binds every newly observed managed launch to one credential-safe,
/// provider-native Harness process identity before singleton publication.
/// v27 binds the independently resolved provider entry point in Prepared before
/// any managed effect and requires the first live observation to match it.
/// v31 replaces the singular Cortana quarantine record with a bounded ledger so
/// every still-live no-signal exclusion retains its exact identity and process
/// generation while a later managed generation is replaced.
pub const CAPTAINS_SCHEMA_VERSION: u32 = 31;
const MAX_CORTANA_QUARANTINE_RECORDS: usize = 64;
const STRICT_RUNTIME_IDENTITY_SCHEMA_VERSION: u32 = 4;
const MAX_CAPTAIN_DISPLAY_NAME_BYTES: usize = 120;
const MAX_PENDING_FLEET_OPERATIONS: usize = 128;
const MAX_RETIRED_FLEET_TILES: usize = 4096;

#[cfg(test)]
thread_local! {
    static PROJECT_PROBE_COUNTS: std::cell::RefCell<[usize; 6]> =
        const { std::cell::RefCell::new([0; 6]) };
}

#[cfg(test)]
fn record_project_probe(kind: usize) {
    PROJECT_PROBE_COUNTS.with(|counts| counts.borrow_mut()[kind] += 1);
}

#[cfg(not(test))]
fn record_project_probe(_kind: usize) {}

#[cfg(test)]
fn reset_project_probe_counts() {
    PROJECT_PROBE_COUNTS.with(|counts| *counts.borrow_mut() = [0; 6]);
}

#[cfg(test)]
fn project_probe_counts() -> [usize; 6] {
    PROJECT_PROBE_COUNTS.with(|counts| *counts.borrow())
}

fn assignment_id_for(project_id: Option<&str>, ship_slug: &str) -> String {
    format!(
        "assignment:{}:{}",
        project_id.unwrap_or("unbound"),
        ship_slug
    )
}

fn normalize_captain_display_name(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("Captain displayName must not be empty".into());
    }
    if value.len() > MAX_CAPTAIN_DISPLAY_NAME_BYTES {
        return Err(format!(
            "Captain displayName must be at most {MAX_CAPTAIN_DISPLAY_NAME_BYTES} bytes"
        ));
    }
    Ok(value.to_string())
}

/// A Crew member's durable pointer into Powder's work ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowderWorkBinding {
    pub card_id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_expires_at: Option<i64>,
    /// Exact in-flight mutation identity persisted before a Powder write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_intent: Option<PowderMutationIntent>,
    /// A post-bind dispatch rollback has durably pinned the original Powder
    /// scope in a matching [`PendingDispatchRelease`].
    ///
    /// This marker makes the two records a validated pair, so restart cleanup
    /// cannot rediscover a replacement Project binding.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dispatch_release_recovery: bool,
    #[serde(default)]
    pub state: PowderWorkState,
}

/// A claim POST whose remote outcome is not trusted enough to bind to a Crew.
///
/// This is deliberately separate from [`PowderWorkBinding`]: no untrusted run,
/// receipt card, or receipt agent is allowed into a durable Crew binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingDispatchClaim {
    pub project_id: String,
    pub connection_profile: String,
    pub repository: String,
    pub card_id: String,
    pub configured_agent: String,
    pub operation_id: String,
    pub created_at: u64,
}

/// A trusted post-bind claim whose exact release response was ambiguous.
///
/// Unlike [`PendingDispatchClaim`], this record is not an unresolved initial
/// claim attempt.  It is a transaction-owned release recovery record that pins
/// the original protected Powder scope across Captain or Project replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingDispatchRelease {
    pub crew_session_id: String,
    pub project_id: String,
    pub connection_profile: String,
    /// HMAC-SHA-256 identity of the normalized protected profile base URL.
    ///
    /// The URL itself can carry gateway credentials in its path, query, or
    /// fragment, so it must never be durable registry or sync state.
    pub connection_endpoint_identity: String,
    pub repository: String,
    pub card_id: String,
    pub run_id: String,
    pub agent: String,
    pub operation_id: String,
    pub created_at: u64,
    pub state: PendingDispatchReleaseState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingDispatchReleaseState {
    /// Local cleanup is durable, but no release POST has been sent.
    Prepared,
    /// The exact release POST may have reached Powder but no trusted receipt exists.
    InFlight,
    /// A returned release error was recorded after an in-flight attempt.
    Ambiguous,
}

/// Recovery records are loaded before any remote Powder call.
/// Keep their identifiers as strict as the protected request identities they
/// will later authorize, so malformed persistence cannot select a scope.
fn is_canonical_dispatch_recovery_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn is_canonical_dispatch_recovery_endpoint_identity(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("hmac-sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// Detect release-record fields an older or foreign writer could have added
/// before serde deserializes the snapshot.  A raw endpoint must be treated as
/// incompatible state, not as recoverable corruption, because it can identify
/// an exact outstanding remote claim.
fn releases_contain_unknown_fields(snapshot: &Value) -> bool {
    const FIELDS: &[&str] = &[
        "crewSessionId",
        "projectId",
        "connectionProfile",
        "connectionEndpointIdentity",
        "repository",
        "cardId",
        "runId",
        "agent",
        "operationId",
        "createdAt",
        "state",
    ];
    snapshot
        .get("pendingDispatchReleases")
        .and_then(Value::as_array)
        .is_some_and(|releases| {
            releases.iter().any(|release| {
                release.as_object().is_none_or(|fields| {
                    fields.keys().any(|field| !FIELDS.contains(&field.as_str()))
                })
            })
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowderMutationKind {
    WorkLogAppend,
    CriterionReview,
    Completion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowderMutationTerminalState {
    Conflict,
    Expired,
    Rejected,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowderMutationTerminalRejection {
    pub state: PowderMutationTerminalState,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowderMutationIntent {
    pub schema_version: u32,
    pub operation_id: String,
    pub payload_digest: String,
    /// Powder's exact canonical request digest for operation-status recovery.
    ///
    /// Schema-v1 intents remain readable for honest historical retention, but
    /// cannot be mutated or reconciled without an exact caller replay upgrade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub powder_request_digest: Option<String>,
    pub repository: String,
    pub card_id: String,
    pub expected_run_id: String,
    pub mutation_kind: PowderMutationKind,
    pub requested_by: String,
    pub created_at: u64,
    /// A definitive authoritative rejection recorded atomically with release of
    /// the active mutation slot. Exact replay remains local and stable, while a
    /// different operation may replace this bounded tombstone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_rejection: Option<PowderMutationTerminalRejection>,
}

const POWDER_MUTATION_INTENT_SCHEMA_VERSION: u32 = 3;

fn valid_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_powder_request_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(valid_lower_hex_digest)
}

/// Byte ceiling on a persisted Powder operation id. Inlined from the retired
/// `powder` module so the kept on-disk registry validation below still compiles
/// after the Powder runtime was removed (Phase A retirement).
const MAX_OPERATION_ID_BYTES: usize = 128;

/// Validate a persisted Powder operation id (registry deserialization guard).
/// Inlined from the retired `powder::validate_operation_id`: an id must be a
/// non-empty, trimmed, bounded token of ASCII letters, digits, '-', '_', '.', ':'.
/// The kept `validate_powder_mutation_intent` only consults `.is_err()`, so this
/// returns a `Result<(), ()>` shape via `String` to preserve that call site.
fn validate_powder_operation_id(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("operation id must not be empty".to_string());
    }
    if value.len() > MAX_OPERATION_ID_BYTES {
        return Err(format!(
            "operation id exceeds the {MAX_OPERATION_ID_BYTES}-byte limit"
        ));
    }
    let valid = value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'));
    if !valid {
        return Err(
            "operation id must use only ASCII letters, digits, '-', '_', '.', or ':'".to_string(),
        );
    }
    Ok(())
}

fn validate_powder_mutation_intent(
    crew_session_id: &str,
    work: &PowderWorkBinding,
    intent: &PowderMutationIntent,
) -> Result<(), String> {
    let request_digest_is_valid = match intent.schema_version {
        1 => intent.powder_request_digest.is_none() && intent.terminal_rejection.is_none(),
        2 => {
            intent
                .powder_request_digest
                .as_deref()
                .is_some_and(valid_powder_request_digest)
                && intent.terminal_rejection.is_none()
        }
        POWDER_MUTATION_INTENT_SCHEMA_VERSION => intent
            .powder_request_digest
            .as_deref()
            .is_some_and(valid_powder_request_digest),
        _ => false,
    };
    let terminal_rejection_is_valid = intent.terminal_rejection.as_ref().is_none_or(|rejection| {
        rejection.recorded_at > 0 && rejection.recorded_at >= intent.created_at
    });
    if !request_digest_is_valid
        || !terminal_rejection_is_valid
        || validate_powder_operation_id(&intent.operation_id).is_err()
        || intent.repository.trim().is_empty()
        || intent.card_id != work.card_id
        || intent.expected_run_id != work.run_id
        || intent.requested_by.trim().is_empty()
        || intent.created_at == 0
        || !valid_lower_hex_digest(&intent.payload_digest)
    {
        return Err(format!(
            "Crew '{crew_session_id}' has an invalid Powder mutation intent"
        ));
    }
    Ok(())
}

/// Durable T-Hub-side state for a Crew member's exact Powder run.
///
/// Powder remains authoritative for the card and run. The local pending marker
/// prevents an ambiguous completion response from being retried blindly. Only a
/// digest of the proof request is persisted, so recovery requires the Captain to
/// present the same bounded proof again before T-Hub re-reads Powder.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PowderWorkState {
    #[default]
    Active,
    CompletionPending {
        #[serde(rename = "requestDigest", alias = "request_digest")]
        request_digest: String,
        since: u64,
    },
    Completed {
        #[serde(rename = "requestDigest", alias = "request_digest")]
        request_digest: String,
        #[serde(rename = "completedAt", alias = "completed_at")]
        completed_at: u64,
    },
}

fn validate_completion_marker(
    crew_session_id: &str,
    request_digest: &str,
    timestamp: u64,
    state: &str,
) -> Result<(), String> {
    let digest_is_valid = valid_lower_hex_digest(request_digest);
    if !digest_is_valid || timestamp == 0 {
        return Err(format!(
            "Crew '{crew_session_id}' has an invalid Powder {state} completion marker"
        ));
    }
    Ok(())
}

/// Project-level Powder mapping. Credentials never belong in this registry;
/// `connection_profile` names separately protected endpoint configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowderProjectBinding {
    #[serde(default = "default_powder_profile")]
    pub connection_profile: String,
    pub repository: String,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub event_cursor: i64,
}

fn default_powder_profile() -> String {
    "default".to_string()
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

fn migrate_project_identities(snapshot: &mut CaptainsSnapshot) -> Result<(), String> {
    for project in &mut snapshot.projects {
        let source = project.root_path.as_deref().unwrap_or(&project.repo_root);
        let root = canonical_project_identity(source)?;
        project.repo_root = root.clone();
        project.root_path = Some(root.clone());
        if project.vcs_capability.is_none() {
            project.vcs_capability = Some(if snapshot.schema_version < CAPTAINS_SCHEMA_VERSION {
                "git".into()
            } else {
                "none".into()
            });
        }
        if project.vcs_capability.as_deref() == Some("git") && project.git_main_root.is_none() {
            project.git_main_root = Some(root);
        } else if let Some(main_root) = project.git_main_root.as_ref() {
            project.git_main_root = Some(canonical_project_identity(main_root)?);
        }
    }
    Ok(())
}

/// Parse the public Project-root identity before authorization or any filesystem,
/// Git, or registry probe.
/// `rootPath` is authoritative; `repoRoot` and `repo_root` are deprecated aliases.
fn requested_project_root(args: &Value, command: &str) -> Result<String, String> {
    let mut identities = Vec::new();
    for field in ["rootPath", "repoRoot", "repo_root"] {
        if let Some(value) = arg_str(args, field) {
            identities.push((field, canonical_project_identity(&value)?));
        }
    }
    if let Some((_, first)) = identities.first() {
        if identities.iter().any(|(_, identity)| identity != first) {
            return Err(format!(
                "{command} received conflicting rootPath and repoRoot values"
            ));
        }
        return Ok(first.clone());
    }
    Err(format!("{command} requires a 'rootPath' argument"))
}

/// Deserialize `crew` from BOTH schema versions (item-2 §3.2/D2): the legacy
/// `Vec<String>` of bare tile ids AND the modern `Vec<CrewRef>`. A bare string
/// upgrades through [`CrewRef::new`] so an on-disk v0 file loads without a manual
/// `Value` walk and every v2 field receives its safe default.
fn deserialize_crew<'de, D>(d: D) -> Result<Vec<CrewRef>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum CrewWire {
        Legacy(String),
        Modern(Box<CrewRef>),
    }
    let raw = Vec::<CrewWire>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .map(|c| match c {
            CrewWire::Legacy(tile) => CrewRef::new(&tile),
            CrewWire::Modern(r) => *r,
        })
        .collect())
}

fn validate_harness_name(value: &str, field: &str) -> Result<(), String> {
    if matches!(value, "codex" | "claude") {
        Ok(())
    } else {
        Err(format!("{field} must be 'codex' or 'claude'"))
    }
}

fn validate_runtime_identity(
    scope: &str,
    harness: Option<&str>,
    provider: Option<&str>,
    provider_session_id: Option<&str>,
    claude_uuid: Option<&str>,
    strict: bool,
) -> Result<(), String> {
    if let Some(harness) = harness {
        validate_harness_name(harness, &format!("{scope} harness"))?;
    }
    if let Some(provider) = provider {
        validate_harness_name(provider, &format!("{scope} provider"))?;
    }
    if let (Some(harness), Some(provider)) = (harness, provider) {
        if harness != provider {
            return Err(format!("{scope} provider must match its harness"));
        }
    }
    if strict && provider_session_id.is_some() && provider.is_none() {
        return Err(format!("{scope} providerSessionId requires a provider"));
    }
    match provider {
        Some("claude")
            if (strict || provider_session_id.is_some() && claude_uuid.is_some())
                && provider_session_id != claude_uuid =>
        {
            return Err(format!(
                "{scope} Claude providerSessionId and claudeUuid must match"
            ));
        }
        Some("codex") if claude_uuid.is_some() => {
            return Err(format!("{scope} Codex identity must not carry claudeUuid"));
        }
        None if strict && claude_uuid.is_some() => {
            return Err(format!("{scope} claudeUuid requires the Claude provider"));
        }
        _ => {}
    }
    Ok(())
}

fn reconcile_legacy_runtime_identity(
    harness: &mut Option<String>,
    provider: &mut Option<String>,
    provider_session_id: &mut Option<String>,
    claude_uuid: &mut Option<String>,
) {
    if provider.is_none() {
        *provider = harness
            .clone()
            .or_else(|| claude_uuid.as_ref().map(|_| "claude".to_string()));
    }
    if harness.is_none() {
        *harness = provider.clone();
    }
    if provider.as_deref() == Some("claude") {
        if provider_session_id.is_none() {
            *provider_session_id = claude_uuid.clone();
        }
        if claude_uuid.is_none() {
            *claude_uuid = provider_session_id.clone();
        }
    }
}

fn validate_workspace_occupant(
    captains: &CaptainsRegistry,
    terminal_id: &str,
    workspace_id: &str,
    kind: WorkspaceKind,
) -> Result<(), String> {
    let snapshot = captains.snapshot();
    validate_workspace_occupant_records(&snapshot.captains, terminal_id, workspace_id, kind)
}

fn validate_workspace_occupant_records(
    captains: &[CaptainRecord],
    terminal_id: &str,
    workspace_id: &str,
    kind: WorkspaceKind,
) -> Result<(), String> {
    if captains.iter().any(|captain| {
        captain.crew.iter().any(|crew| {
            crew.terminal_id == terminal_id && matches!(crew.state, CrewState::Removed { .. })
        })
    }) {
        return Err(format!(
            "Workspace placement denied: removed Crew terminal '{terminal_id}' cannot be reinserted"
        ));
    }
    let membership = captains.iter().find_map(|captain| {
        if captain.terminal_id.as_deref() == Some(terminal_id)
            && captain.state == ClaimState::Active
        {
            Some(ShipMembership::Supervisor {
                ship_slug: captain.ship_slug.clone(),
                role: captain.role,
            })
        } else if captain.crew.iter().any(|crew| {
            crew.terminal_id == terminal_id && !matches!(crew.state, CrewState::Removed { .. })
        }) {
            Some(ShipMembership::Crew {
                ship_slug: captain.ship_slug.clone(),
            })
        } else {
            None
        }
    });
    match (kind, membership) {
        (WorkspaceKind::Captain, Some(ShipMembership::Supervisor { .. })) => Ok(()),
        (WorkspaceKind::Captain, _) => Err(format!(
            "Workspace placement denied: terminal '{terminal_id}' is not a durable Cortana or Captain identity"
        )),
        (WorkspaceKind::Work, Some(ShipMembership::Supervisor { .. })) => Err(format!(
            "Workspace placement denied: Captain terminal '{terminal_id}' belongs to Captain Workspace"
        )),
        (WorkspaceKind::Work, Some(ShipMembership::Crew { ship_slug })) => {
            if captains.iter().any(|captain| {
                captain.ship_slug == ship_slug
                    && captain
                        .workspace_tab_ids
                        .iter()
                        .any(|owned| owned == workspace_id)
            }) {
                Ok(())
            } else {
                Err(format!(
                    "Workspace placement denied: Work Workspace '{workspace_id}' is not owned by Crew terminal '{terminal_id}' Captain '{ship_slug}'"
                ))
            }
        }
        (WorkspaceKind::Work, None) => Ok(()),
    }
}

pub fn validate_workspace_report(
    tabs: &[TabRecord],
    captains: &CaptainsRegistry,
) -> Result<(), String> {
    let snapshot = captains.snapshot();
    validate_workspace_report_records(tabs, &snapshot.captains)
}

fn validate_workspace_report_records(
    tabs: &[TabRecord],
    captains: &[CaptainRecord],
) -> Result<(), String> {
    let mut placed = std::collections::HashSet::new();
    for tab in tabs {
        for terminal_id in &tab.tile_ids {
            if !placed.insert(terminal_id.as_str()) {
                return Err(format!(
                    "Workspace report assigns terminal '{terminal_id}' more than once"
                ));
            }
            validate_workspace_occupant_records(captains, terminal_id, &tab.id, tab.kind())?;
        }
    }
    Ok(())
}

fn validate_unique_workspace_placements(tabs: &[TabRecord]) -> Result<(), String> {
    let mut placed = std::collections::HashSet::new();
    for tab in tabs {
        for terminal_id in &tab.tile_ids {
            if !placed.insert(terminal_id.as_str()) {
                return Err(format!(
                    "Workspace report assigns terminal '{terminal_id}' more than once"
                ));
            }
        }
    }
    Ok(())
}

fn validate_work_workspace_present(tabs: &[TabRecord]) -> Result<(), String> {
    if tabs.iter().any(|tab| tab.kind() == WorkspaceKind::Work) {
        return Ok(());
    }
    Err("Workspace report must retain at least one Work Workspace".into())
}

fn reconcile_supervisor_workspace_candidates(
    captains: &[CaptainRecord],
    tabs: &mut [TabRecord],
) -> Result<bool, String> {
    let supervisors = captains
        .iter()
        .filter(|captain| captain.state == ClaimState::Active)
        .filter_map(|captain| captain.terminal_id.as_ref())
        .cloned()
        .collect::<Vec<_>>();
    let mut changed = false;
    for terminal_id in supervisors {
        let exact = tabs.iter().all(|tab| {
            let occurrences = tab
                .tile_ids
                .iter()
                .filter(|tile| *tile == &terminal_id)
                .count();
            if tab.id == CAPTAIN_WORKSPACE_ID {
                occurrences == 1
            } else {
                occurrences == 0
            }
        });
        if exact {
            continue;
        }
        for tab in tabs.iter_mut() {
            tab.tile_ids.retain(|tile| tile != &terminal_id);
        }
        tabs.iter_mut()
            .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
            .ok_or("Captain Workspace disappeared during startup reconciliation")?
            .tile_ids
            .push(terminal_id);
        changed = true;
    }
    Ok(changed)
}

fn startup_supervisor_reconciliation_required(
    captains: &[CaptainRecord],
    current_tabs: &[TabRecord],
    reported_tabs: &[TabRecord],
) -> Result<bool, String> {
    let placements = |tabs: &[TabRecord], terminal_id: &str| {
        tabs.iter()
            .filter(|tab| tab.tile_ids.iter().any(|tile| tile == terminal_id))
            .map(|tab| tab.id.clone())
            .collect::<Vec<_>>()
    };
    let mut required = false;
    let backend_is_unseeded = current_tabs.is_empty();
    for terminal_id in captains
        .iter()
        .filter(|captain| captain.state == ClaimState::Active)
        .filter_map(|captain| captain.terminal_id.as_deref())
    {
        let reported = placements(reported_tabs, terminal_id);
        if reported == [CAPTAIN_WORKSPACE_ID.to_string()] {
            continue;
        }
        // Production deliberately starts every app run with an empty in-memory
        // TabRegistry and lets the frontend's first complete report seed it.
        // Durable Active supervisor identity is sufficient to canonicalize that
        // known terminal into Captain Workspace during this one unseeded state.
        // Duplicate placements were already rejected, and the post-reconcile
        // occupant validation still rejects every unknown/foreign tile in the
        // reserved Workspace.
        if backend_is_unseeded {
            required = true;
            continue;
        }
        let current = placements(current_tabs, terminal_id);
        if reported != current {
            validate_workspace_report_records(reported_tabs, captains)?;
            return Err(format!(
                "Workspace report attempted to redesignate Captain terminal '{terminal_id}' outside startup reconciliation"
            ));
        }
        required = true;
    }
    Ok(required)
}

fn reconcile_crew_workspace_candidates(
    captains: &mut [CaptainRecord],
    tabs: &mut [TabRecord],
) -> Result<bool, String> {
    let work_ids: std::collections::HashSet<String> = tabs
        .iter()
        .filter(|tab| tab.kind() == WorkspaceKind::Work)
        .map(|tab| tab.id.clone())
        .collect();
    let mut changed = false;
    for captain in captains {
        let owned: Vec<String> = captain
            .workspace_tab_ids
            .iter()
            .filter(|id| work_ids.contains(*id))
            .cloned()
            .collect();
        for crew in &mut captain.crew {
            if !matches!(
                crew.state,
                CrewState::Active | CrewState::NeedsAssignment { .. }
            ) {
                continue;
            }
            let placements: Vec<String> = tabs
                .iter()
                .filter(|tab| tab.tile_ids.iter().any(|id| id == &crew.terminal_id))
                .map(|tab| tab.id.clone())
                .collect();
            let durable_is_exact = crew.workspace_tab_id.as_ref().is_some_and(|id| {
                owned.iter().any(|owned_id| owned_id == id)
                    && placements.len() == 1
                    && placements[0] == *id
            });
            if durable_is_exact {
                if matches!(crew.state, CrewState::NeedsAssignment { .. }) {
                    crew.state = CrewState::Active;
                    changed = true;
                }
                continue;
            }
            let legacy_exact = crew.workspace_tab_id.is_none()
                && placements.len() == 1
                && owned.iter().any(|id| id == &placements[0]);
            let destination = if legacy_exact {
                Some(placements[0].clone())
            } else if crew.workspace_tab_id.is_none() && owned.len() == 1 {
                Some(owned[0].clone())
            } else {
                None
            };
            if let Some(destination) = destination {
                for tab in tabs.iter_mut() {
                    tab.tile_ids.retain(|id| id != &crew.terminal_id);
                }
                let target = tabs
                    .iter_mut()
                    .find(|tab| tab.id == destination)
                    .ok_or_else(|| {
                        format!("owned Workspace '{destination}' disappeared during reconciliation")
                    })?;
                target.tile_ids.push(crew.terminal_id.clone());
                crew.workspace_tab_id = Some(destination);
                crew.state = CrewState::Active;
                changed = true;
            } else {
                for tab in tabs.iter_mut() {
                    tab.tile_ids.retain(|id| id != &crew.terminal_id);
                }
                crew.workspace_tab_id = None;
                if !matches!(crew.state, CrewState::NeedsAssignment { .. }) {
                    crew.state = CrewState::NeedsAssignment { since: now_ms() };
                    changed = true;
                }
            }
        }
    }
    Ok(changed)
}

fn durable_workspaces_from_report(
    captains: &[CaptainRecord],
    tabs: &[TabRecord],
) -> Result<Vec<FleetWorkspaceRecord>, String> {
    let mut durable = Vec::with_capacity(tabs.len());
    for tab in tabs {
        if tab.kind() == WorkspaceKind::Captain {
            durable.push(FleetWorkspaceRecord {
                id: CAPTAIN_WORKSPACE_ID.to_string(),
                name: CAPTAIN_WORKSPACE_NAME.to_string(),
                kind: WorkspaceKind::Captain,
                owner: None,
                tile_ids: tab.tile_ids.clone(),
            });
            continue;
        }
        let owners = captains
            .iter()
            .filter(|captain| captain.workspace_tab_ids.contains(&tab.id))
            .filter_map(|captain| {
                captain
                    .project_id
                    .as_ref()
                    .map(|project_id| FleetWorkspaceOwner {
                        project_id: project_id.clone(),
                        assignment_id: captain.assignment_id.clone(),
                        ship_slug: captain.ship_slug.clone(),
                    })
            })
            .collect::<Vec<_>>();
        if owners.len() > 1 {
            return Err(format!(
                "Workspace report found ambiguous durable Project/Assignment ownership for Work Workspace '{}'",
                tab.id
            ));
        }
        durable.push(FleetWorkspaceRecord {
            id: tab.id.clone(),
            name: tab.name.clone(),
            kind: WorkspaceKind::Work,
            owner: owners.into_iter().next(),
            tile_ids: tab.tile_ids.clone(),
        });
    }
    Ok(durable)
}

pub fn apply_workspace_report(
    tabs_registry: &TabRegistry,
    captains_registry: &CaptainsRegistry,
    tabs: Vec<TabRecord>,
    active_tab_id: Option<String>,
    base_seq: Option<u64>,
) -> Result<(ReportOutcome, bool, bool), String> {
    let _identity_transaction = tabs_registry.identity_transaction();
    let mut tabs = TabRegistry::normalize_tabs(tabs)?;
    validate_work_workspace_present(&tabs)?;
    validate_unique_workspace_placements(&tabs)?;
    let _mutation = captains_registry
        .mutation
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut current_tabs = tabs_registry.lock();
    if base_seq.is_some_and(|base| base != current_tabs.seq) {
        return Ok((
            ReportOutcome::Stale(RegistrySnapshot {
                seq: current_tabs.seq,
                active_tab_id: current_tabs.active_tab_id.clone(),
                tabs: current_tabs.tabs.clone(),
            }),
            false,
            false,
        ));
    }
    if let Some(retired) = tabs
        .iter()
        .flat_map(|tab| &tab.tile_ids)
        .find(|tile| current_tabs.retired_tile_ids.contains(tile.as_str()))
    {
        return Err(format!(
            "Workspace report attempted to reinsert retired terminal '{retired}'"
        ));
    }

    let current_captains = captains_registry.lock();
    if let Some(retired) = tabs
        .iter()
        .flat_map(|tab| &tab.tile_ids)
        .find(|tile| current_captains.retired_fleet_tile_ids.contains(tile))
    {
        return Err(format!(
            "Workspace report attempted to reinsert retired terminal '{retired}'"
        ));
    }
    let previous_captains = current_captains.clone();
    let mut candidate_captains = current_captains.clone();
    let supervisor_reconciled =
        startup_supervisor_reconciliation_required(
            &candidate_captains.captains,
            &current_tabs.tabs,
            &tabs,
        )? && reconcile_supervisor_workspace_candidates(&candidate_captains.captains, &mut tabs)?;
    validate_workspace_report_records(&tabs, &current_captains.captains)?;
    let crew_reconciled =
        reconcile_crew_workspace_candidates(&mut candidate_captains.captains, &mut tabs)?;
    let reconciled = supervisor_reconciled || crew_reconciled;
    validate_workspace_report_records(&tabs, &candidate_captains.captains)?;
    let removed_tab_ids = current_tabs
        .tabs
        .iter()
        .filter(|old| !tabs.iter().any(|tab| tab.id == old.id))
        .map(|tab| tab.id.clone())
        .collect::<Vec<_>>();
    let mut pruned = false;
    for captain in &mut candidate_captains.captains {
        let before = captain.workspace_tab_ids.len();
        captain
            .workspace_tab_ids
            .retain(|id| !removed_tab_ids.contains(id));
        pruned |= captain.workspace_tab_ids.len() != before;
    }
    candidate_captains.workspaces =
        durable_workspaces_from_report(&candidate_captains.captains, &tabs)?;
    let workspaces_changed = candidate_captains.workspaces != previous_captains.workspaces;
    let captains_changed = crew_reconciled || pruned || workspaces_changed;
    if captains_changed {
        candidate_captains.seq = candidate_captains.seq.saturating_add(1);
        let changes = AuthorityGenerationChanges::between(&previous_captains, &candidate_captains);
        candidate_captains.authority_generations.advance(changes)?;
        let snapshot = CaptainsRegistry::snapshot_for_persist(&candidate_captains);
        CaptainsRegistry::validate_snapshot(&snapshot)?;
        drop(current_captains);
        captains_registry.persist(snapshot)?;
    } else {
        drop(current_captains);
    }

    let mut candidate_tabs = current_tabs.clone();
    candidate_tabs.tabs = tabs;
    if let Some(active) =
        active_tab_id.filter(|id| candidate_tabs.tabs.iter().any(|tab| &tab.id == id))
    {
        candidate_tabs.active_tab_id = Some(active);
    } else if !candidate_tabs
        .active_tab_id
        .as_ref()
        .is_some_and(|id| candidate_tabs.tabs.iter().any(|tab| &tab.id == id))
    {
        candidate_tabs.active_tab_id = candidate_tabs.tabs.first().map(|tab| tab.id.clone());
    }
    candidate_tabs.seq = candidate_tabs.seq.saturating_add(1);
    let seq = candidate_tabs.seq;
    if captains_changed {
        *captains_registry.lock() = candidate_captains;
    }
    *current_tabs = candidate_tabs;
    Ok((
        ReportOutcome::Accepted {
            seq,
            removed_tab_ids,
        },
        captains_changed,
        reconciled,
    ))
}

mod captains_registry;
// The CaptainsRegistry data model + internals live in the submodule alongside its
// impl. `use *` pulls the `pub(super)` internal types (authority machinery, inner
// state) into this module for the free fns/tests that still reference them; the
// `pub use` re-exports the public ones so external paths keep resolving -
// `control::FleetRole` (used by `acl.rs`) and `control::CaptainsRegistry` (used by
// `commands.rs` / `lib.rs`), plus the bare names in this module + sibling submodules.
use captains_registry::*;
pub use captains_registry::{
    CaptainsRegistry, ClaimState, CrewRef, CrewState, FleetRole, ProjectRecord,
};

mod idempotency;
use idempotency::*;

/// Resolve the captains persistence file: `$T_HUB_CAPTAINS_FILE` if set, else
/// `~/.t-hub/captains.json`. Mirrors [`handshake_path`] so dev-isolation can
/// point it elsewhere via the env var.
pub fn captains_path() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_CAPTAINS_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("captains.json")
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

/// Validated provider-capacity policy and its provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderCapacityEvidence {
    session_capacity: usize,
    status: crate::governor::ProviderCapacityStatus,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackagedProviderCapacity {
    schema_version: u32,
    source: String,
    session_capacity: usize,
}

/// Resolve the provider's authoritative concurrent-session ceiling.
///
/// A validated environment override is authoritative when present. A normal
/// installed build otherwise uses the signed-in-binary conservative policy in
/// `provider-capacity.json`. The packaged source is reported as degraded because
/// it is a safety ceiling, not live account quota telemetry.
type ProviderCapacityFn = Arc<dyn Fn() -> Result<ProviderCapacityEvidence, String> + Send + Sync>;

/// Count sessions that actually host a provider Harness.
///
/// Generic tmux terminals still consume machine capacity but do not consume a
/// provider-concurrency slot. Production attests each live T-Hub session;
/// deterministic tests replace this seam with exact evidence.
type ProviderLiveSessionsFn =
    Arc<dyn Fn(&CaptainsSnapshot, &[String]) -> Result<usize, String> + Send + Sync>;

const PROVIDER_SESSION_ENV: &str = "T_HUB_PROVIDER_SESSION";

/// Enumerate the authoritative tmux session registry for admission.
///
/// The indirection is a failure-injection seam. A failed enumeration is an
/// unavailable capacity observation, never an observation of zero sessions.
type LiveSessionsFn = Arc<dyn Fn() -> Result<Vec<String>, String> + Send + Sync>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRootAuthority {
    /// Stable registered POSIX identity used by profiles and fingerprints.
    pub posix_identity: String,
    /// Host-native path used only to open the registered Project capability.
    pub host_open_path: PathBuf,
}

type PreviewControlFn =
    Arc<dyn Fn(&str, &Value, &PreviewRootAuthority) -> Result<Value, String> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum SpawnPurpose {
    Ordinary,
    Cortana,
    FleetAdmin,
    ShipAdmin { ship_slug: String },
    Recovery,
}

/// An admitted spawn holds the shared capacity lock until its tmux create (and
/// any durable state transition surrounding it) has completed. This makes the
/// live-count, reservation, rate-token, and process creation one serialized
/// operation instead of a check-then-spawn race.
#[derive(Debug)]
pub(crate) struct SpawnAdmissionGuard<'a> {
    _lock: std::sync::MutexGuard<'a, ()>,
    _capacity: crate::governor::CapacityReport,
}

enum GovernorAdmission<'a> {
    None,
    Spawn {
        _guard: Box<SpawnAdmissionGuard<'a>>,
        governor: &'a crate::governor::SpawnGovernor,
    },
    Destructive {
        governor: &'a crate::governor::SpawnGovernor,
    },
}

impl GovernorAdmission<'_> {
    fn rollback(self) {
        match self {
            Self::None => {}
            Self::Spawn { governor, .. } => governor.refund_spawn(),
            Self::Destructive { governor } => governor.refund_destructive(),
        }
    }
}

#[derive(Clone)]
pub struct ControlContext {
    status: Arc<StatusBridge>,
    /// Provider-neutral conversation catalog. One cache is shared across every
    /// control connection so History scans never become per-request WSL churn.
    history: Arc<crate::history::HistoryService>,
    /// Shared provider-neutral Preview operation seam.
    /// Control dispatch forwards exact command arguments and owns no Preview
    /// discovery, lifecycle, endpoint, or process-ownership policy.
    preview_control: PreviewControlFn,
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
    /// Authoritative provider capacity evidence. Missing or invalid evidence
    /// refuses admission instead of defaulting to the governor's machine cap.
    provider_capacity: ProviderCapacityFn,
    /// Authoritative count of live provider-consuming Harness sessions.
    provider_live_sessions: ProviderLiveSessionsFn,
    /// Authoritative tmux enumeration, injectable only for deterministic tests.
    live_sessions: LiveSessionsFn,
    /// The CORE's addressable tab registry (TASK C / #22). Read by `list_tabs`,
    /// updated optimistically by `new_tab` / `move_tile` / named placement, and
    /// replaced wholesale by the frontend's `report_workspace_tabs` up-sync. Shared
    /// (`Arc`) with the Tauri command that receives those reports; own empty one in
    /// headless tests.
    tabs: Arc<TabRegistry>,
    /// The CORE's authoritative captains registry (captain-chat phase 2). Read by
    /// `list_captains`, mutated by `claim_captain`/`release_captain` and the
    /// `spawnedBy` crew plumbing; persistent across restarts (unlike `tabs`).
    /// Own empty in-memory one in headless tests.
    captains: Arc<CaptainsRegistry>,
    /// Serializes modern Crew admission from its authoritative snapshot through
    /// exact-baseline verification, dependency ancestry, governor preflight, and
    /// durable insertion. This closes the check-then-insert race where two
    /// concurrent starts could otherwise observe the same free capacity and
    /// mutable resources before either record became visible.
    /// Global dual-lock order: acquire this admission lock before
    /// `CaptainsRegistry::provision` whenever an operation needs both. Cortana
    /// first inspects under provision alone and retries in this order only when a
    /// replacement spawn is necessary.
    dispatch_admission: Arc<Mutex<()>>,
    /// The orchestrator-wake watch registry. Armed by `watch_fleet` / cleared by
    /// `unwatch_fleet`; read by the [`crate::fleet::FleetNotifier`] wired in
    /// `setup()`, which shares the same `Arc`. In-memory only (a watch is
    /// meaningful only while its orchestrator session is live). Own empty one in
    /// headless tests.
    fleet_watches: Arc<crate::fleet::FleetWatchRegistry>,
    /// Idle read timeout for a connection's request phase ([`CONN_READ_TIMEOUT`] by
    /// default). A field (not the bare const) so tests can drive a short timeout
    /// against a real listener; could later carry an operator override.
    idle_timeout: std::time::Duration,
    /// Write timeout for a PTY attach connection's socket
    /// ([`ATTACH_WRITE_TIMEOUT`] by default; a field so tests can drive a short
    /// one). Bounds the scrollback seed AND the streaming sink - see
    /// [`serve_pty_attach`] for why an unbounded write is the churn wedge.
    attach_write_timeout: std::time::Duration,
    /// Cap on concurrently live PTY attach forwarders
    /// ([`MAX_ATTACH_FORWARDERS`] by default; a field so tests can drive a tiny
    /// one). Defense in depth under client churn - see [`AttachForwarderGuard`].
    max_attach_forwarders: usize,
    /// How often an idle forwarder writes a keepalive so a gone/stalled client is
    /// reaped instead of leaking the slot ([`ATTACH_KEEPALIVE_INTERVAL`] by default;
    /// a field so tests can drive a short one). See [`serve_pty_attach`].
    attach_keepalive_interval: std::time::Duration,
    /// Whether the connection being served is from the LOCAL loopback (same machine,
    /// fully trusted) vs a REMOTE tailnet peer. Set per-connection in `handle_conn`;
    /// `true` by default (tests + the loopback case). Gates the file-read scope (#23):
    /// remote peers are restricted to indexed roots, loopback is unrestricted.
    peer_is_loopback: bool,
    /// The per-launch full-power **control** auth token. Authorizes every tier
    /// (Read + Organization + ProcessChanging). Published to `control.json` as
    /// `token` (backward-compatible) unless the Phase 3 harden flag flips it.
    token: String,
    /// The per-launch **read** capability token (socket-gate Phase 2). Authorizes
    /// the Read tier ONLY; a holder cannot spawn, type into, or kill sessions.
    /// Empty when unconfigured (headless tests) — an empty read token authorizes
    /// nothing (guarded in [`resolve_capability`]).
    read_token: String,
    /// Secret known only to the in-process Tauri transport. It distinguishes the
    /// trusted UI from a terminal that merely possesses the shared control token.
    host_token: String,
    /// The loopback address the listener bound to (`127.0.0.1:<port>`), set in
    /// [`start`] after bind. Its presence gates injection of the stable discovery
    /// path into spawned sessions. The rotating value itself is not injected.
    /// Empty in headless tests.
    addr: String,
    /// Stable identifier and monotonic generation for discovery publication.
    listener_instance_id: String,
    listener_generation: Arc<AtomicU64>,
    /// Immutable identity allocated for this exact serve loop before it starts.
    /// Unlike `listener_generation`, this never observes a later overlapping rebind.
    bound_listener_generation: u64,
    /// Fleet spawn budget + rate limits (socket-gate Phase 1). Shared `Arc` so one
    /// fleet-wide budget is enforced across every connection handler thread.
    /// Consulted from [`dispatch_authenticated`] for the ProcessChanging tier only.
    governor: Arc<SpawnGovernor>,
    /// Tamper-evident audit sink for Organization/ProcessChanging commands and
    /// governor refusals (socket-gate Phase 1). Shared `Arc`; cheap to hold (no I/O
    /// until the first record).
    audit: Arc<AuditLog>,
    /// Completed-request outcome cache for spawn-class idempotency (ask #1). A
    /// spawn-class command carrying a client `requestId` applies exactly once per
    /// id; a retry of the same id replays the stored outcome instead of
    /// double-applying, and `get_request_status` resolves an ambiguous response
    /// leg. Shared `Arc` so every connection handler thread dedups against one
    /// cache. Per-launch, in-memory (a fresh launch's ids never collide).
    requests: Arc<RequestCache>,
    /// Coordinates control-listener rebinds for the relay-wedge self-heal (cause 2).
    /// Shared `Arc` so the `rebind_control` handler (on any connection thread) drives
    /// the same rate-limit + retires the same live listener. See [`RebindController`].
    rebind: Arc<RebindController>,
    /// Comms-plane Phase 2: the per-session identity store (mint/bind/resolve). Shared
    /// `Arc` so the spawn path mints+binds and the enqueue/ack path resolves against
    /// one store. Persistent across restarts (`identities.json`); an ephemeral
    /// in-memory one in headless tests.
    identity: Arc<crate::identity::IdentityStore>,
    /// Ephemeral, identity-bound Captain control leases. A listener rebind keeps
    /// this store; an app restart intentionally drops it so the durable identity
    /// must prove itself again against current registry and liveness state.
    control_leases: Arc<CaptainControlLeases>,
    /// Comms-plane Phase 2: the durable inbox (per-recipient segmented store + seq +
    /// receipt state machine). Shared `Arc` so the fleet notifier (first client)
    /// enqueues/drains and the `inbox_ack`/`inbox_status` handlers reach the same
    /// queues. Persistent (`~/.t-hub/inbox/`); an ephemeral in-memory one in headless
    /// tests.
    inbox: Arc<crate::inbox::Inbox>,
    /// Comms-plane Phase 3: the delegation-gate carrier store (durable general-
    /// authorization artifacts). Shared `Arc` so the `authorize` record path and the
    /// `check_authorization` resolve-and-verify gate (a captain's money/publish consult)
    /// reach one store. Persistent (`authorizations.json`); ephemeral in headless tests.
    authz: Arc<crate::authz::AuthzStore>,
    /// Durable Ship Admin and Fleet Admin appointments.
    /// Control capability admission remains a separate outer gate.
    delegated_admin: Arc<crate::delegated_admin::DelegatedAdminStore>,
    /// Durable Cargo-cleanup reservations.
    /// Every worktree creation, terminal spawn, and agent start consults this
    /// coordinator before it creates runtime or filesystem state.
    worktrees: Arc<crate::worktree_coordinator::WorktreeCoordinator>,
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

    /// Admit a local webview terminal spawn through the same held lock, evidence,
    /// provider, reservation, and rate gates as a control-socket spawn.
    pub(crate) fn admit_ui_spawn(
        &self,
        provider_lanes: usize,
    ) -> Result<SpawnAdmissionGuard<'_>, crate::governor::Refusal> {
        admit_spawn(self, SpawnPurpose::Ordinary, provider_lanes, None)
    }

    pub(crate) fn authorize_ui_spawn(
        &self,
        provider_lanes: usize,
        args: &Value,
    ) -> Result<SpawnAdmissionGuard<'_>, String> {
        let admission = self
            .admit_ui_spawn(provider_lanes)
            .map_err(|refusal| refusal.message)?;
        if let Err(audit_error) = self.audit.try_record(
            "spawn_terminal",
            CommandTier::ProcessChanging.label(),
            "allowed",
            args,
            AuditMeta {
                peer: "loopback",
                token_tier: "control",
                session: None,
                spawned_by: None,
                error: None,
            },
        ) {
            self.governor.refund_spawn();
            eprintln!(
                "t-hub-audit: refusing Tauri UI spawn because the audit sink is unavailable: {audit_error}"
            );
            let message = "refused: audit sink unavailable; 'spawn_terminal' was not executed";
            self.fanout.emit_event(
                "control://governor",
                &json!({
                    "command": "spawn_terminal",
                    "decision": "refused-audit",
                    "error": message,
                }),
            );
            return Err(message.into());
        }
        Ok(admission)
    }

    pub(crate) fn ensure_worktree_available(
        &self,
        path: &str,
        operation: &str,
    ) -> Result<(), String> {
        self.worktrees.ensure_available(path, operation)
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

/// Convert the Windows-hosted authoritative discovery path into the spelling a
/// WSL terminal can read. POSIX paths (development and pure-WSL runs) are kept
/// unchanged. This is a stable path only; it never contains a listener address
/// or credential value.
fn wsl_discovery_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let bytes = raw.as_bytes();
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        return format!("/mnt/{drive}/{}", raw[3..].trim_start_matches('/'));
    }
    raw
}

pub(crate) fn discovery_file_for_spawn() -> String {
    wsl_discovery_path(&handshake_path())
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

/// Max age (seconds) before a persistent key is rotated at startup (item-3 Pillar B
/// rotation-on-restart, general-decision #6). `T_HUB_KEY_MAX_AGE_SECS` overrides
/// (`0` => rotate on EVERY restart). Default 7 days: long enough that a remote
/// pairing survives normal restarts, short enough to bound a leaked key's lifetime.
/// The cadence is a knob (N4), not the mechanism.
fn key_max_age_secs() -> u64 {
    std::env::var("T_HUB_KEY_MAX_AGE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7 * 24 * 60 * 60)
}

/// Whether a rotation is forced this startup regardless of age - the
/// suspected-leak / operator-triggered path (`T_HUB_ROTATE_KEYS=1`).
fn force_key_rotation() -> bool {
    std::env::var("T_HUB_ROTATE_KEYS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Whether the key file at `path` (by its mtime) is at/past the rotation age. A
/// missing file or an unreadable mtime is NOT "past age" (the caller handles missing
/// via mint); a future mtime (clock skew) is treated as fresh, never rotated.
fn key_is_past_max_age(path: &Path, max_age_secs: u64) -> bool {
    let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
        return false;
    };
    matches!(modified.elapsed(), Ok(age) if age.as_secs() >= max_age_secs)
}

/// Write a secret key to `path`, SEALED for at-rest (DPAPI on Windows, plaintext +
/// `0600` fallback elsewhere - see [`crate::secret_seal`]), mint-and-replace
/// (truncating overwrite). Best-effort: a write failure still leaves the in-memory
/// key usable.
fn write_key_file(path: &Path, key: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let sealed = crate::secret_seal::seal_str(key);
    if std::fs::write(path, sealed.as_bytes()).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

/// Load a persistent secret key from `path`, ROTATING it (mint a fresh UUID + OVERWRITE
/// the file) when it is missing/unreadable, past `max_age_secs`, or `force`d; otherwise
/// KEEP the existing value, upgrading a legacy plaintext file to the sealed at-rest form
/// in place. Rotation is mint-and-replace - it NEVER re-reads the same key (the
/// reviewer's non-gating note: `persistent_key` used to reuse the file, so "rotate at
/// startup" must actively overwrite it). The returned key is always the LIVE value and
/// is usable in-memory even if the disk write fails. Policy is passed in so this core
/// is pure/testable; [`load_or_rotate_key`] resolves it from the environment.
///
/// Whether AGE-based rotation may fire, given whether the on-disk value is already in
/// item-3's sealed form and whether sealing is active on this host (MED-1). On a
/// sealing host (Windows/DPAPI) a pre-item-3 key is UNsealed, so age-rotation is held
/// off until the first restart has ADOPTED (sealed) it - never stranding pre-existing
/// fleet on the very first item-3 restart. Where sealing is inactive there is no sealed
/// form to key on, so age-rotation is always eligible (prior behavior). A FORCED
/// rotation ignores this gate entirely.
fn age_rotation_eligible(existing_is_sealed: bool, sealing_active: bool) -> bool {
    !sealing_active || existing_is_sealed
}

fn load_or_rotate_key_with(path: &Path, force: bool, max_age_secs: u64) -> String {
    let raw = std::fs::read_to_string(path).ok();
    let existing = raw
        .as_deref()
        .and_then(crate::secret_seal::unseal_str)
        .filter(|k| !k.is_empty());
    let raw_is_sealed = raw
        .as_deref()
        .map(crate::secret_seal::is_sealed)
        .unwrap_or(false);
    // MED-1 mitigation: age-based rotation only fires once the key is ALREADY in item-3's
    // sealed form. On the Windows host a PRE-ITEM-3 key is unsealed, so the FIRST item-3
    // restart ADOPTS it (keeps the value + seals it, resetting the rotation clock) rather
    // than rotating and stranding pre-existing in-tmux fleet sessions / remote pairings.
    // The clock then measures from adoption. A forced rotation (T_HUB_ROTATE_KEYS) still
    // fires regardless. On a non-sealing host (pure-WSL/ext4 + dev/CI) there is no sealed
    // form to gate on, so age-rotation keeps its prior behavior.
    let age_eligible = age_rotation_eligible(raw_is_sealed, crate::secret_seal::sealing_active());
    let rotate =
        force || existing.is_none() || (age_eligible && key_is_past_max_age(path, max_age_secs));
    if !rotate {
        if let Some(k) = existing {
            // Keep the credential; upgrade a legacy/plaintext file to the sealed form
            // (this is also the MED-1 first-restart ADOPTION that resets the clock).
            if crate::secret_seal::sealing_active() && !raw_is_sealed {
                write_key_file(path, &k);
            }
            return k;
        }
    }
    // Mint fresh and OVERWRITE (mint-and-replace).
    let key = uuid::Uuid::new_v4().to_string();
    write_key_file(path, &key);
    key
}

/// Environment-resolved wrapper over [`load_or_rotate_key_with`] (rotation forced by
/// `T_HUB_ROTATE_KEYS`, age from `T_HUB_KEY_MAX_AGE_SECS`).
fn load_or_rotate_key(path: &Path) -> String {
    load_or_rotate_key_with(path, force_key_rotation(), key_max_age_secs())
}

/// The PERSISTENT control auth key (server-split M2b): the server's stable identity
/// across restarts, so a remote client paired once need not re-pair each launch -
/// bounded by item-3's rotation-on-restart age ([`load_or_rotate_key`]). Sealed at
/// rest. On any read/write failure we still return a usable (in-memory) key so the
/// channel always comes up.
pub fn persistent_key() -> String {
    load_or_rotate_key(&key_path())
}

fn write_key_file_durable_with(
    path: &Path,
    key: &str,
    before_publish: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| "control key path has no parent directory".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "could not create control key directory '{}': {error}",
            parent.display()
        )
    })?;
    let temporary = path.with_extension(format!(
        "key.tmp.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("could not create rotated control key: {error}"))?;
        file.write_all(crate::secret_seal::seal_str(key).as_bytes())
            .map_err(|error| format!("could not write rotated control key: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|error| {
                    format!("could not restrict rotated control key permissions: {error}")
                })?;
        }
        file.sync_all()
            .map_err(|error| format!("could not sync rotated control key: {error}"))?;
        before_publish()?;
        std::fs::rename(&temporary, path)
            .map_err(|error| format!("could not publish rotated control key: {error}"))?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("could not sync control key directory: {error}"))?;
        let stored = std::fs::read_to_string(path)
            .map_err(|error| format!("could not verify rotated control key: {error}"))?;
        if crate::secret_seal::unseal_str(&stored).as_deref() != Some(key) {
            return Err("rotated control key verification failed".into());
        }
        Ok(())
    })();
    if result.is_err() {
        std::fs::remove_file(&temporary).ok();
    }
    result
}

fn write_key_file_durable(path: &Path, key: &str) -> Result<(), String> {
    write_key_file_durable_with(path, key, || Ok(()))
}

fn persistent_key_for_start_with(
    path: &Path,
    force: bool,
    max_age_secs: u64,
    legacy_orphan_bearer_may_be_live: bool,
) -> Result<String, String> {
    if legacy_orphan_bearer_may_be_live {
        let key = uuid::Uuid::new_v4().to_string();
        write_key_file_durable(path, &key)?;
        return Ok(key);
    }
    Ok(load_or_rotate_key_with(path, force, max_age_secs))
}

/// Resolve the full control key before listener publication.
///
/// A validated legacy Cortana orphan can still hold the persistent full-control
/// bearer from the previous listener generation. Rotate before publishing the new
/// listener so reconciliation can prove that the retained endpoint is stale and
/// quarantine its exact generation without leaving the old process authorized.
pub fn persistent_key_for_start(legacy_orphan_bearer_may_be_live: bool) -> Result<String, String> {
    persistent_key_for_start_with(
        &key_path(),
        force_key_rotation(),
        key_max_age_secs(),
        legacy_orphan_bearer_may_be_live,
    )
}

/// Resolve the persistent **read**-key file: `$T_HUB_SERVER_READ_KEY_FILE` if set,
/// else `~/.t-hub/server-read-key`. Mirrors [`key_path`] so dev-isolation can point
/// it elsewhere; kept separate from the control key so the two secrets never share
/// a file.
fn read_key_path() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_SERVER_READ_KEY_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("server-read-key")
}

/// The PERSISTENT **read** capability key (socket-gate Phase 2): a distinct,
/// stable-across-restarts secret from [`persistent_key`] (the control key), so a
/// read-only consumer paired once keeps working - bounded by the same rotation-on-
/// restart age and sealed at rest ([`load_or_rotate_key`]). Always returns a usable
/// in-memory key on any I/O failure.
pub fn persistent_read_key() -> String {
    load_or_rotate_key(&read_key_path())
}

/// Phase 3 hardening flag (socket-gate). When ON, [`start`] stops publishing the
/// control token to `control.json` and publishes only the read token there, so a
/// process that merely scrapes the discovery file gets read-only; elevated sessions
/// then rely on the control token injected down the spawn tree (Phase 2b). item-3
/// flip #2 (ratified 2026-07-10): DEFAULT ON - `T_HUB_CONTROL_HARDEN=0` (or `false`)
/// is the instant, rebuild-free rollback to the Phase-2 disk behavior.
///
/// HISTORY (2026-07-07 incident): an earlier ON default (0.3.47) was reverted the
/// same day because the app's OWN frontend authenticated to the control socket with
/// the token published in `control.json`; hardening downgraded that to the read token
/// and the webview lost control ("session detached - reconnecting", PR #29). The cure
/// is now structurally in the tree and independently re-verified (item-3 §1.2): the
/// webview reads the FULL token from the in-process, never-serialized
/// `local_control_token` (see [`ControlHandshake::local_control_token`] and
/// `control_client::resolve_endpoint`), so the disk token can be read-only without
/// touching the webview's credential. The §3.1 five-check verification gate (see the
/// `hardened_*` tests) pins every one of those webview token paths, including
/// reconnect-after-rebind, so the flip cannot silently re-break attach. See
/// `docs/SOCKET-AUTH-DESIGN.md`.
#[cfg(test)]
fn phase3_harden_enabled() -> bool {
    std::env::var("T_HUB_CONTROL_HARDEN")
        // Ratified default-ON: only an explicit `0`/`false` disables hardening.
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true)
}

/// Pick the token published in the `token` field of `control.json`. With hardening
/// ON (the Phase 3 default) this is the read token (ambient discovery becomes
/// read-only); with hardening OFF (`T_HUB_CONTROL_HARDEN=0`) it is the full-power
/// control token (Phase 2 backward-compatible behavior). An empty read token falls
/// back to the control token even when hardening is ON, so a context that never
/// minted a read token (e.g. a bare probe server) is never locked out. Pure so it
/// is directly unit-testable.
#[cfg(test)]
fn select_published_token<'a>(
    control_token: &'a str,
    read_token: &'a str,
    harden: bool,
) -> &'a str {
    if harden && !read_token.is_empty() {
        read_token
    } else {
        control_token
    }
}

/// Write the handshake file (best-effort `0600` on unix) so the MCP binary can
/// discover the live listener.
///
/// ATOMIC (temp + rename): the relay-wedge self-heal rewrites this file while live
/// clients are re-reading it (post-#38 they re-read on every transport failure), so
/// a reader must never observe a torn/half-written file. We write a sibling temp
/// file, `0600` it, then `rename` it over the target - `rename` within a directory
/// is atomic on both unix and Windows, so a concurrent reader sees either the whole
/// old file or the whole new one, never a mix.
fn write_handshake(handshake: &ControlHandshake) -> std::io::Result<()> {
    let path = handshake_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(handshake)?;
    // Temp sibling in the SAME directory (so `rename` stays on one filesystem and is
    // truly atomic). Suffix with the pid so two processes never collide on the temp.
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, &body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    // Atomic publish. On failure clean up the temp so we never leak it.
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Minimum spacing between control-listener rebinds (relay-wedge self-heal). A
/// misbehaving or flapping client must not be able to churn the listener port; a
/// rebind requested sooner is refused with the remaining cooldown. Generous: a real
/// relay wedge lasts many minutes and self-heal needs to fire at most once per
/// episode, so 45s comfortably rate-limits abuse without blocking a legitimate heal.
const REBIND_MIN_INTERVAL: Duration = Duration::from_secs(45);

/// Coordinates control-listener rebinds for the relay-wedge self-heal (cause 2 of
/// the control-socket wedge; see PR #49 for the two-cause analysis).
///
/// The WSL2 mirrored-loopback relay can wedge the flow for the app's specific port
/// for minutes while the app is perfectly healthy - every WSL-side request times out
/// but Windows-side requests to the same port are instant. A wedged WSL client
/// triggers [`rebind_control`] over the Windows-side powershell bridge (the one path
/// that works mid-wedge); the app then binds a FRESH port, atomically rewrites
/// `control.json`, and stops the old listener. Post-#38 clients re-read `control.json`
/// on transport failure and resume on the new port with NO app restart - which is
/// exactly what a manual restart achieved (a fresh port ⇒ fresh relay flow state),
/// minus the restart.
struct RebindController {
    inner: Mutex<RebindInner>,
    /// Rate-limit window between successful rebinds.
    min_interval: Duration,
}

#[derive(Default)]
struct RebindInner {
    /// When the last rebind completed - the rate-limit anchor. `None` until the first
    /// rebind, so the very first heal after launch is never rate-limited.
    last_rebind: Option<Instant>,
    /// Stop flag for the CURRENTLY-serving loopback listener. Setting it (and waking
    /// the blocked `accept` with a self-connect) retires the old listener when a
    /// rebind supersedes it. `None` in headless contexts that never called [`start`].
    current_stop: Option<Arc<AtomicBool>>,
}

impl RebindController {
    fn new(min_interval: Duration) -> Self {
        Self {
            inner: Mutex::new(RebindInner::default()),
            min_interval,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RebindInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Record the initial listener's stop flag (called once from [`start`]). Does NOT
    /// set `last_rebind`, so an immediate wedge right after launch can still heal.
    fn set_initial_stop(&self, stop: Arc<AtomicBool>) {
        self.lock().current_stop = Some(stop);
    }
}

/// Best-effort wake of a listener blocked in `accept`: a throwaway local connection
/// makes `accept` return so the serve loop observes its stop flag and exits promptly.
/// App-local loopback is NOT affected by the WSL relay wedge (only WSL->Windows is),
/// so this reaches the old listener even mid-wedge. Bounded so a refused/gone port
/// never parks the caller.
fn wake_accept(addr: &str) {
    if let Ok(sock) = addr.parse::<SocketAddr>() {
        if let Ok(stream) = TcpStream::connect_timeout(&sock, Duration::from_secs(1)) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Start the control listener on a background thread.
///
/// Binds `127.0.0.1:0`, writes the handshake file, and serves NDJSON control
/// requests until the process exits. Returns the bound address + token so the
/// caller (and tests) know where it landed. A bind failure is returned to the
/// caller; the app logs it and continues (the control channel is optional, like
/// the agent bridge).
pub fn start(mut ctx: ControlContext) -> std::io::Result<ControlHandshake> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    if ctx.read_token.is_empty() {
        ctx.read_token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
    }
    // Record that discovery is live before spawned sessions receive its stable path.
    ctx.addr = addr.to_string();
    let listener_generation = ctx.listener_generation.fetch_add(1, Ordering::AcqRel) + 1;
    ctx.bound_listener_generation = listener_generation;
    // The stable discovery record is always ambient read-only. The trusted
    // in-process frontend receives its credential through skipped fields below,
    // while durable Captains prove identity to acquire a scoped lease.
    let handshake = ControlHandshake {
        addr: addr.to_string(),
        // Discovery is always ambient read-only. Durable Captains reacquire an
        // identity-bound scoped lease; the shared global control credential is
        // never published through this stable file.
        token: ctx.read_token.clone(),
        read_token: ctx.read_token.clone(),
        pid: std::process::id(),
        protocol_version: PROTOCOL_VERSION,
        instance_id: ctx.listener_instance_id.clone(),
        listener_generation,
        published_at: now_ms(),
        // The full-power control token, carried ONLY in this returned struct (never
        // serialized - see the field's `#[serde(skip_serializing)]`). Under Phase 3
        // hardening `token` above is the read token, so the trusted local frontend
        // takes its credential from here to keep terminal attach working while
        // `control.json` still withholds full power from external scrapers.
        local_control_token: ctx.token.clone(),
        local_host_token: ctx.host_token.clone(),
    };
    write_handshake(&handshake)?;

    let integrity = ctx.audit.startup_integrity_check();
    if !integrity.ok() {
        ctx.fanout.emit_event(
            "control://audit",
            &json!({
                "event": "integrity-check-failed",
                "breaks": integrity.breaks.len(),
                "records": integrity.records,
                "files": integrity.files,
            }),
        );
    }

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
                let mut ctx_remote = ctx.clone();
                ctx_remote.addr = remote_addr;
                ctx_remote.bound_listener_generation = ctx_remote
                    .listener_generation
                    .fetch_add(1, Ordering::AcqRel)
                    + 1;
                // The remote listener is not part of the loopback relay-wedge path
                // and is never rebound, so it gets a stop flag that is never set.
                let remote_stop = Arc::new(AtomicBool::new(false));
                std::thread::Builder::new()
                    .name("t-hub-control-remote".into())
                    .spawn(move || serve(remote_listener, ctx_remote, remote_stop))
                    .ok();
            }
            Err(e) => eprintln!("t-hub: remote control bind '{bind}' failed: {e}"),
        }
    }

    // Register the primary loopback listener's stop flag so a later `rebind_control`
    // can retire it (relay-wedge self-heal). Not counted as a rebind, so the first
    // heal after launch is never rate-limited.
    let stop = Arc::new(AtomicBool::new(false));
    ctx.rebind.set_initial_stop(stop.clone());
    std::thread::Builder::new()
        .name("t-hub-control".into())
        .spawn(move || serve(listener, ctx, stop))
        .ok();

    Ok(handshake)
}

pub fn recover_pending_fleet_operations_after_audit_check(ctx: &ControlContext) -> bool {
    if !ctx.audit.startup_integrity_check().ok() {
        eprintln!("t-hub-audit: startup recovery skipped because audit integrity is unavailable");
        return false;
    }
    recover_pending_fleet_operations(ctx);
    true
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

/// A socket timeout surfaces as `WouldBlock` (SO_RCVTIMEO/SO_SNDTIMEO on unix) or
/// `TimedOut` (windows). On the READ path both mean "idle — close this connection
/// cleanly"; on the WRITE path both mean "send buffer full — retry the remainder"
/// (see [`write_response`]). Named for the condition, since both paths use it.
fn is_would_block_or_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// Write timeout for a PTY attach connection's socket (s27 churn-proofing).
/// SO_SNDTIMEO is a property of the underlying socket, shared by every
/// `try_clone`, so one setting bounds the scrollback seed written by the
/// connection thread AND the output firehose written by the forwarder thread.
/// Without it, a client that stops draining (suspended, wedged, or dead with no
/// RST) leaves `write_all` blocked FOREVER: a received FIN does not unblock a
/// blocked write, so the socket sits in CLOSE_WAIT while the handler thread
/// pins an [`ACTIVE_CONNS`] slot - accumulate enough and `serve` rejects every
/// new connection, which is exactly the incident that wedged the live server
/// (fresh `attach_pty` failing for all clients while existing attaches stream).
/// Generous: a healthy loopback/tailnet client drains a 30s backlog trivially;
/// one that can't is gone, and tearing it down lets it reattach cleanly.
const ATTACH_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Write timeout for a normal request/response connection's socket. Same wedge as
/// [`ATTACH_WRITE_TIMEOUT`], different phase: the response leg. `write_response`
/// runs a single blocking `write_all` AFTER a command's side effects are already
/// committed; with no SO_SNDTIMEO a client that stopped draining (suspended,
/// wedged, dead-with-no-RST) parks the handler thread FOREVER in that write,
/// pinning an [`ACTIVE_CONNS`] slot. Enough stuck responses and `serve` rejects
/// every new connection - the whole control channel goes dark even though the app
/// is alive (Incident D: bare TCP connects still complete via the kernel backlog
/// while no request is ever answered). Bounding the write lets the thread give up,
/// free its slot, and keep the accept loop healthy. Generous: a healthy loopback
/// peer drains a one-line response instantly. See [`write_response`] for the
/// per-attempt WouldBlock retry that rides on top of this bound.
const RESPONSE_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Defensive cap on concurrently live PTY attach forwarders (s27). Each
/// forwarder costs a PTY pair, a `tmux attach` client, a reader thread, and a
/// socket, and every one also holds an [`ACTIVE_CONNS`] slot - so a churn storm
/// of attaches must never be able to starve the request/event paths (cap is
/// well under [`MAX_CONNS`]). Generous: a full cockpit is ~14 attaches
/// (T10-measured), satellites included, so 64 fits 4+ complete clients.
const MAX_ATTACH_FORWARDERS: usize = 64;
static ACTIVE_ATTACH_FORWARDERS: AtomicUsize = AtomicUsize::new(0);

/// How often an idle PTY attach forwarder writes a keepalive frame to its client
/// (s27 idle-leak fix). The forwarder used to notice a dead client ONLY when it
/// had real output to write; an IDLE terminal produces none, so a client that
/// stopped draining or vanished holding the socket (no FIN the input read could
/// see) was never noticed - the forwarder parked forever on the silent PTY read
/// and leaked, wedging the table at [`MAX_ATTACH_FORWARDERS`]. A periodic keepalive
/// forces a write on the otherwise-silent stream, so a gone/stalled client surfaces
/// as a write error or a full-buffer [`ATTACH_WRITE_TIMEOUT`] and reaps like any
/// other. A healthy client drains it as a no-op. A field on [`ControlContext`] so
/// tests can drive a short one.
const ATTACH_KEEPALIVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Number of live PTY attach forwarders (diagnostics / the churn regression
/// test's return-to-baseline assertion).
pub fn attach_forwarder_count() -> usize {
    ACTIVE_ATTACH_FORWARDERS.load(Ordering::Relaxed)
}

/// RAII slot in the attach forwarder table: acquired for the lifetime of one
/// `serve_pty_attach` streaming phase, released on every exit path (including
/// panics) via `Drop`. Acquisition is a CAS loop so the cap is exact under
/// concurrent attach storms (no over-admit window).
struct AttachForwarderGuard;
impl AttachForwarderGuard {
    fn try_acquire(limit: usize) -> Option<Self> {
        let mut cur = ACTIVE_ATTACH_FORWARDERS.load(Ordering::Relaxed);
        loop {
            if cur >= limit {
                return None;
            }
            match ACTIVE_ATTACH_FORWARDERS.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Self),
                Err(now) => cur = now,
            }
        }
    }
}
impl Drop for AttachForwarderGuard {
    fn drop(&mut self) {
        ACTIVE_ATTACH_FORWARDERS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Enable TCP keepalive on an accepted control connection (s27 churn-proofing).
/// The long-lived modes (event subscribe, PTY attach) deliberately clear the
/// idle read timeout - an untouched terminal legitimately sends nothing for
/// hours - so a peer that vanishes SILENTLY (no FIN, no RST: a powered-off
/// tailnet box, a killed WSLg/msrdc window, a dropped VPN) would otherwise park
/// the handler read forever and leak the forwarder behind it. Keepalive probes
/// make that read fail within minutes; the kernel answers them even when the
/// peer app is idle, so a healthy quiet client is never torn down. Best-effort:
/// a platform refusing the option costs resilience, not correctness.
fn enable_tcp_keepalive(stream: &TcpStream) {
    let params = socket2::TcpKeepalive::new()
        .with_time(std::time::Duration::from_secs(60))
        .with_interval(std::time::Duration::from_secs(15));
    if let Err(e) = socket2::SockRef::from(stream).set_tcp_keepalive(&params) {
        eprintln!("t-hub-control: failed to enable TCP keepalive: {e}");
    }
}

/// Decrements the live-connection counter when a connection handler thread exits.
struct ConnGuard;
impl Drop for ConnGuard {
    fn drop(&mut self) {
        ACTIVE_CONNS.fetch_sub(1, Ordering::Relaxed);
    }
}

fn serve(listener: TcpListener, ctx: ControlContext, stop: Arc<AtomicBool>) {
    for stream in listener.incoming() {
        // Relay-wedge self-heal: a rebind that superseded this listener sets `stop`
        // and wakes this blocked `accept` with a throwaway self-connect (see
        // `wake_accept`). Observe it BEFORE handling the woken stream so the old port
        // stops accepting and the listener is dropped (freeing the port). A live
        // client that raced onto the old port here is dropped and re-reads
        // `control.json` (post-#38) onto the fresh port on its next attempt.
        if stop.load(Ordering::Acquire) {
            break;
        }
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
                // Builder::spawn (not thread::spawn) so a failed spawn under
                // resource exhaustion returns an error instead of PANICKING the
                // accept loop - the listener must survive exactly the conditions
                // (fd/thread pressure from leaked forwarders) it exists to serve.
                let spawned = std::thread::Builder::new()
                    .name("t-hub-control-conn".into())
                    .spawn(move || {
                        let _guard = ConnGuard; // decrements ACTIVE_CONNS on exit
                        if let Err(e) = handle_conn(stream, &ctx) {
                            eprintln!("t-hub-control: connection error: {e}");
                        }
                    });
                if let Err(e) = spawned {
                    // The closure never ran, so its ConnGuard never will: undo the
                    // count here (the moved stream was dropped/closed with it).
                    ACTIVE_CONNS.fetch_sub(1, Ordering::Relaxed);
                    eprintln!("t-hub-control: failed to spawn connection handler: {e}");
                }
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
    // Keepalive on every admitted connection, BEFORE any mode can clear the idle
    // read timeout: silent peer death (no FIN/RST) must never park a handler -
    // or the attach forwarder behind it - forever. See enable_tcp_keepalive.
    enable_tcp_keepalive(&stream);
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
    // Bound the RESPONSE leg too (Incident D): a client that stops draining must
    // not park this handler thread forever in `write_response`'s `write_all` and
    // pin an ACTIVE_CONNS slot until `serve` starts rejecting every new
    // connection. SO_SNDTIMEO is a socket property shared by every `try_clone`, so
    // this one call bounds the dispatch response here AND the fanout's frames on a
    // subscribed connection. The long-lived PTY attach re-sets its own
    // ([`ATTACH_WRITE_TIMEOUT`]) when it takes over the stream below.
    writer.set_write_timeout(Some(RESPONSE_WRITE_TIMEOUT)).ok();
    // Read lines manually (not `reader.lines()`) so a connection mode that takes
    // over the rest of the stream (the PTY attach) can be handed `&mut reader`.
    let mut reader = BufReader::new(stream);
    // Bound the request phase with an idle read timeout (M2b hardening): a client
    // that connects but never sends — or stalls mid-line — closes itself rather
    // than parking this thread forever. CLEARED below when the connection becomes
    // a long-lived event/PTY stream (those block on reads for minutes by design).
    reader
        .get_ref()
        .set_read_timeout(Some(ctx.idle_timeout))
        .ok();
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
            Err(e) if is_would_block_or_timeout(&e) => break,
            Err(e) => return Err(e),
        }
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ControlRequest>(&line) {
            Ok(req) => {
                // Protocol-version gate (M2b hardening; T13 relaxed to a ceiling).
                // The protocol is backward-compatible — v2 only ADDED the opt-in
                // binary PTY framing negotiated per-attach — so a client advertising
                // an EQUAL-OR-LOWER version is served (the v1 webview keeps working
                // against this v2 server). Only a HIGHER, unknown-future version is
                // rejected, with a CLEAR message, rather than letting a client that
                // expects framing we don't yet speak fail cryptically downstream. A
                // client that sends NO version (the MCP, any legacy peer) is allowed.
                // The peer is already IP-gated (is_allowed_peer), so echoing our
                // version here leaks nothing the handshake file doesn't already record.
                if let Some(v) = req.v {
                    if v > PROTOCOL_VERSION {
                        write_response(
                            &mut writer,
                            &ControlResponse::err(format!(
                                "protocol version too new: server speaks up to v{PROTOCOL_VERSION}, \
                                 client asked for v{v}; upgrade T-Hub on this end"
                            )),
                        )?;
                        continue;
                    }
                }
                // Event-subscription handshake: switch this connection into a one-way
                // event stream. After the ack we send no per-line responses — the
                // fanout owns the socket and the read loop just parks until disconnect.
                if req.command == SUBSCRIBE_COMMAND {
                    // Read-tier stream: the read token may subscribe too (a
                    // least-privilege monitor legitimately needs the event feed).
                    // PTY attach below stays control-token-only (it can type).
                    if !token_is_valid(ctx, &req.token) {
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
                    // untouched terminal isn't force-detached after 120s. (Half-open
                    // peer death is covered by keepalive, set at accept.)
                    reader.get_ref().set_read_timeout(None).ok();
                    serve_pty_attach(ctx, &mut writer, &mut reader, &req.args)?;
                    break;
                } else {
                    let response = if is_retired_powder_command(&req.command)
                        && resolve_capability(ctx, &req.token).is_some()
                    {
                        ControlResponse::powder_retired(&req.command)
                    } else {
                        dispatch_authenticated(ctx, req)
                    };
                    write_response(&mut writer, &response)?;
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

/// Serve a PTY stream (M2a) on this connection: send an empty compatibility seed,
/// spawn the PTY-runs-`tmux attach` streaming output frames down (via a clone of the
/// writer), then read write/resize frames from the client until
/// it disconnects, and detach (the tmux session survives).
///
/// Framing is negotiated from `args.binary` (T13): `true` ⇒ v2 length-prefixed
/// BINARY frames, else v1 base64-NDJSON. The choice governs BOTH directions — the
/// scrollback/out/exit/error/keepalive frames written down AND the write/resize
/// frames read up — so a v1 client is byte-for-byte unchanged and a v2 client never
/// sees base64.
///
/// Churn-proofing (s27) - every leak path a dying client can take is bounded:
///   - a slot in the forwarder table is acquired first (refused with a clear
///     error at the cap) and released on every exit path via `Drop`;
///   - the socket gets a write timeout before the seed, so a client that dies
///     or stalls DURING the scrollback seed (or while streaming) fails the
///     write instead of parking this thread forever;
///   - when the stream ends first (sink death or PTY exit), the forwarder
///     thread shuts the socket down (`on_stream_end`), unblocking the input
///     read below so teardown never waits on a dead client to close;
///   - teardown itself shuts the socket down BEFORE joining the forwarder, so
///     the join can never wait behind a blocked write.
fn serve_pty_attach(
    ctx: &ControlContext,
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    args: &Value,
) -> std::io::Result<()> {
    let framing = if args
        .get("binary")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        pty::PtyFraming::V2Binary
    } else {
        pty::PtyFraming::V1Json
    };

    let session_id = match arg_str(args, "sessionId").or_else(|| arg_str(args, "session_id")) {
        Some(s) => s,
        None => {
            return send_attach_error(
                writer,
                framing,
                "attach_pty requires a 'sessionId' argument",
            );
        }
    };
    let tmux_session = tmux_target(&session_id);
    let cols = args.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16;
    let rows = args.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16;

    // Defensive bound on the forwarder table (s27): refuse - with an actionable
    // error, not a silent close - rather than let runaway churn pile forwarders
    // onto the PTY/thread/fd budget. Held until this function returns, i.e. for
    // the whole streaming phase.
    let Some(_forwarder_slot) = AttachForwarderGuard::try_acquire(ctx.max_attach_forwarders) else {
        return send_attach_error(
            writer,
            framing,
            format!(
                "attach_pty: forwarder table is full ({} live attach forwarders); \
                 refusing a new attach - detach stale clients or investigate leaked \
                 forwarders",
                attach_forwarder_count()
            ),
        );
    };

    // Bound every write on this connection (seed, output firehose, exit frame):
    // SO_SNDTIMEO lives on the underlying socket, shared by every clone, so this
    // one call covers the sink the forwarder thread writes too.
    writer
        .set_write_timeout(Some(ctx.attach_write_timeout))
        .ok();

    // De-conflation (spawn-wedge): only a DEFINITIVE `Gone` is "no longer exists";
    // an `Unknown` probe (timed out / failed to spawn) is the degraded-control-plane
    // signal, and reporting it as "no longer exists" is exactly the false negative
    // that made the webview drop live tiles. Surface a retryable timeout instead so
    // the frontend's auto-reattach keeps trying rather than tearing the tile down.
    match tmux::session_liveness(&tmux_session) {
        tmux::SessionLiveness::Alive => {}
        tmux::SessionLiveness::Gone => {
            return send_attach_error(
                writer,
                framing,
                format!(
                    "attach_pty: tmux session {tmux_session} for terminal {session_id} no longer exists"
                ),
            );
        }
        tmux::SessionLiveness::Unknown => {
            return send_attach_error(
                writer,
                framing,
                format!(
                    "attach_pty: liveness probe for tmux session {tmux_session} (terminal \
                     {session_id}) timed out; NOT confirmed gone — retry"
                ),
            );
        }
    }

    // Belt-and-braces (tile-attach fix): reassert `window-size latest` on every
    // attach, not just at session creation. If this session was ever flipped to
    // `window-size manual` out of band (the retired captain 220x50 workaround),
    // that override otherwise persists for the life of the tmux session and
    // clips content off the right edge. Reasserting here guarantees the window
    // tracks the newly-attached client's real width. Best-effort: the session is
    // Alive and about to stream, so never fail the attach over this.
    tmux::reassert_window_size_latest(&tmux_session);

    // Opening compatibility frame, sent BEFORE the stream starts so the client can
    // complete its handshake before output arrives. The attached tmux client is the
    // single authoritative renderer for the current screen. Replaying capture-pane
    // here and then streaming tmux's initial redraw rendered the same inline TUI
    // frame twice, including an apparently duplicated Codex composer draft.
    let scrollback: &[u8] = &[];
    match framing {
        pty::PtyFraming::V1Json => write_json_line(
            writer,
            &json!({ "scrollback": STANDARD.encode(scrollback) }),
        )?,
        pty::PtyFraming::V2Binary => {
            pty::write_bin_frame(writer, pty::binframe::SCROLLBACK, scrollback)?
        }
    }

    // Spawn the PTY streaming output to a clone of this connection, in the same
    // framing. `on_stream_end` shuts the SOCKET down when the stream is over, so
    // the input loop below unblocks promptly whether the stream died because the
    // client vanished (sink error) or because the tmux session exited under a
    // still-connected client - without it, teardown waited on the client.
    let outbound = Arc::new(Mutex::new(writer.try_clone()?));
    let sink = SharedPtyWriter {
        outbound: outbound.clone(),
        buffer: Vec::new(),
    };
    let conn_for_stream_end = writer.try_clone()?;
    let on_stream_end: Box<dyn FnOnce() + Send> = Box::new(move || {
        let _ = conn_for_stream_end.shutdown(std::net::Shutdown::Both);
    });
    let cwd = std::env::var("HOME").unwrap_or_default();
    let mut handle = match pty::stream_attach_to_sink(
        &tmux_session,
        &cwd,
        cols,
        rows,
        Box::new(sink),
        framing,
        ctx.attach_keepalive_interval,
        Some(on_stream_end),
    ) {
        Ok(h) => h,
        Err(e) => {
            return send_attach_error(writer, framing, format!("attach_pty: {e}"));
        }
    };

    // Drive write/resize frames from the client until it disconnects (EOF), in the
    // negotiated framing. Capture the result instead of `?` so teardown runs on
    // the error paths too (an abrupt RST mid-stream must still reap everything).
    let input_result = match framing {
        pty::PtyFraming::V1Json => read_pty_input_v1(reader, &mut handle, cols, rows, &outbound),
        pty::PtyFraming::V2Binary => read_pty_input_v2(reader, &mut handle),
    };
    // Deterministic teardown, same order on every path: shut the socket down
    // FIRST so the forwarder thread can never sit blocked in a write while
    // detach() joins it, then kill the attach client + join. The tmux session
    // survives, like close_terminal.
    let _ = writer.shutdown(std::net::Shutdown::Both);
    handle.detach();
    input_result
}

/// Serialize PTY output frames and probe acknowledgements onto one TCP byte
/// stream. The output forwarder and input handler run concurrently, so sharing
/// the writer prevents an acknowledgement from splitting an output frame.
struct SharedPtyWriter {
    outbound: Arc<Mutex<TcpStream>>,
    buffer: Vec<u8>,
}

impl Write for SharedPtyWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut outbound = self
            .outbound
            .lock()
            .map_err(|_| std::io::Error::other("PTY output writer lock poisoned"))?;
        outbound.write_all(&self.buffer)?;
        outbound.flush()?;
        self.buffer.clear();
        Ok(())
    }
}

/// Emit an attach-time error in the negotiated framing: a v1 `{"ok":false,error}`
/// control response, or a v2 binary ERROR frame. Used for the pre-stream failures
/// (missing session, dead tmux session, spawn failure) so a v2 client's binary
/// reader never has to parse a stray JSON line.
fn send_attach_error(
    writer: &mut TcpStream,
    framing: pty::PtyFraming,
    msg: impl Into<String>,
) -> std::io::Result<()> {
    let msg = msg.into();
    match framing {
        pty::PtyFraming::V1Json => write_response(writer, &ControlResponse::err(msg)),
        pty::PtyFraming::V2Binary => {
            pty::write_bin_frame(writer, pty::binframe::ERROR, msg.as_bytes())
        }
    }
}

/// Read v1 base64-NDJSON `{"write"}`/`{"resize"}` frames from the client until EOF,
/// applying each to the PTY handle. A malformed line is skipped, not fatal.
fn read_pty_input_v1(
    reader: &mut BufReader<TcpStream>,
    handle: &mut pty::PtyStreamHandle,
    cols: u16,
    rows: u16,
    outbound: &Arc<Mutex<TcpStream>>,
) -> std::io::Result<()> {
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
        if let Some(probe) = frame.get("probe").and_then(|v| v.as_u64()) {
            let mut writer = outbound
                .lock()
                .map_err(|_| std::io::Error::other("PTY output writer lock poisoned"))?;
            write_json_line(&mut writer, &json!({ "probeAck": probe }))?;
        } else if let Some(b64) = frame.get("write").and_then(|v| v.as_str()) {
            if let Ok(bytes) = STANDARD.decode(b64) {
                let _ = handle.write(&bytes);
            }
        } else if let Some(rz) = frame.get("resize") {
            let c = rz
                .get("cols")
                .and_then(|v| v.as_u64())
                .unwrap_or(cols as u64) as u16;
            let r = rz
                .get("rows")
                .and_then(|v| v.as_u64())
                .unwrap_or(rows as u64) as u16;
            let _ = handle.resize(c, r);
        }
    }
    Ok(())
}

/// Read v2 length-prefixed binary WRITE/RESIZE frames from the client until EOF,
/// applying each to the PTY handle. Frame layout: `[u8 type][u32 BE len][payload]`.
/// EOF at a frame boundary is a clean disconnect; a truncated frame ends the stream;
/// an over-long declared length ([`pty::BIN_MAX_FRAME`]) tears it down (corrupt/
/// hostile peer); an unknown type tag is skipped (forward-compat).
fn read_pty_input_v2(
    reader: &mut BufReader<TcpStream>,
    handle: &mut pty::PtyStreamHandle,
) -> std::io::Result<()> {
    let mut header = [0u8; pty::BIN_HEADER_LEN];
    loop {
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            // EOF at a frame boundary (or a truncated header): the client is gone.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        let ty = header[0];
        let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
        if len > pty::BIN_MAX_FRAME {
            eprintln!("t-hub-control: attach_pty v2 frame len {len} exceeds cap; tearing down");
            break;
        }
        let mut payload = vec![0u8; len];
        if len > 0 {
            match reader.read_exact(&mut payload) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }
        match ty {
            pty::binframe::WRITE => {
                let _ = handle.write(&payload);
            }
            pty::binframe::RESIZE if payload.len() == 4 => {
                let c = u16::from_be_bytes([payload[0], payload[1]]);
                let r = u16::from_be_bytes([payload[2], payload[3]]);
                let _ = handle.resize(c, r);
            }
            _ => {} // unknown/ malformed upstream frame: skip, don't tear down
        }
    }
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
///
/// EAGAIN-robust (Incident D / ask #2, server side): the command's side effects
/// are ALREADY committed by the time we get here, so a transient full send buffer
/// - `WouldBlock`/`TimedOut` from the [`RESPONSE_WRITE_TIMEOUT`] SO_SNDTIMEO -
/// must NOT drop the connection and leave the caller unable to tell whether the
/// command took effect. Instead we retry the unwritten remainder until an overall
/// deadline, giving a briefly-backpressured but live peer time to drain. Only a
/// peer that stays unwritable for the whole deadline is abandoned (its handler
/// thread then exits and frees its ACTIVE_CONNS slot rather than parking forever).
fn write_response(writer: &mut TcpStream, resp: &ControlResponse) -> std::io::Result<()> {
    let mut body = serde_json::to_vec(resp)
        .unwrap_or_else(|_| br#"{"ok":false,"error":"failed to serialize response"}"#.to_vec());
    body.push(b'\n');
    write_all_eagain_robust(writer, &body)?;
    // A flush can itself hit WouldBlock on a backpressured socket; treat that as
    // best-effort (the bytes are already handed to the kernel by write_all).
    match writer.flush() {
        Ok(()) => Ok(()),
        Err(e) if is_would_block_or_timeout(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

/// `write_all`, but a `WouldBlock`/`TimedOut` (a full send buffer under the
/// socket's write timeout) retries the UNWRITTEN remainder until
/// [`RESPONSE_WRITE_TIMEOUT`] * a small factor elapses, rather than failing after
/// side effects are committed. Bytes already accepted by the kernel are never
/// resent (we advance past them), so the framing stays intact. Returns the last
/// error if the peer never drains within the deadline.
fn write_all_eagain_robust(writer: &mut TcpStream, body: &[u8]) -> std::io::Result<()> {
    // The per-write SO_SNDTIMEO already bounds each syscall; cap the total so a
    // permanently stuck peer is abandoned (thread freed) instead of looping.
    let deadline = std::time::Instant::now() + RESPONSE_WRITE_TIMEOUT.saturating_mul(2);
    let mut written = 0usize;
    loop {
        match writer.write(&body[written..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "control response write returned 0 (peer closed)",
                ));
            }
            Ok(n) => {
                written += n;
                if written >= body.len() {
                    return Ok(());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if is_would_block_or_timeout(&e) => {
                if std::time::Instant::now() >= deadline {
                    return Err(e);
                }
                // Loop and retry the remainder; the peer is backpressured, not gone.
            }
            Err(e) => return Err(e),
        }
    }
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

/// The authorization/audit tier of a control command (socket-gate Phase 1). The
/// SINGLE server-side source of truth for command classification, derived from the
/// same grouping the [`dispatch`] match uses. Phase 1 uses it to decide which
/// commands the governor gates (ProcessChanging) and which the audit log records
/// (Organization + ProcessChanging); Phase 2 reuses it for the capability gate, so
/// the annotation-vs-enforcement drift that motivated this work cannot recur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandTier {
    Read,
    Organization,
    ProcessChanging,
}

impl CommandTier {
    fn label(self) -> &'static str {
        match self {
            CommandTier::Read => "read",
            CommandTier::Organization => "organization",
            CommandTier::ProcessChanging => "process-changing",
        }
    }
}

/// The **single table-driven source of truth** mapping a command name to the tier
/// it requires (socket-gate Phase 2, §3). Mirrors the tier blocks in [`dispatch`]
/// and the MCP `Tier` enum (`crates/t-hub-mcp/src/tools.rs`) so the
/// annotation-vs-enforcement drift that motivated this whole effort cannot recur.
///
/// Filesystem-mutating "Organization-destructive" commands (`create_worktree`,
/// `remove_worktree`, `archive_recent_project`) are Organization tier: since the
/// read token authorizes the Read tier ONLY, Organization already requires the
/// control token (§3's "control-tier" treatment), and keeping them out of
/// ProcessChanging leaves them un-throttled by the spawn governor (they are not raw
/// process spawns).
fn required_tier(command: &str) -> CommandTier {
    match command {
        "spawn_terminal" | "history_resume" | "preview_start" | "preview_stop"
        | "preview_restart" | "start_agent" | "reconcile_cortana"
        | "commission_captain" | "attach_captain"
        | "send_text" | "send_keys" | "close_terminal"
        // comms-plane Phase 3: `abort_session` interrupts a running process (like
        // send_keys/close) and `plane_admin` purges durable queues - both are
        // process/state-changing and control-gated + audited.
        | "abort_session" | "plane_admin" | "cleanup_worktree_artifacts" => {
            CommandTier::ProcessChanging
        }
        "focus_session" | "history_focus" | "history_list" | "preview_select"
        | "preview_refresh" | "preview_open" | "move_tile" | "rename_tab" | "new_tab" | "close_tab" | "remove_tab"
        | "focus_tab" | "open_file" | "create_worktree" | "remove_worktree"
        | "archive_recent_project" | "register_project" | "initialize_git"
        | "claim_captain" | "release_captain" | "rename_captain" | "captain_checkpoint" | "agent_checkpoint" | "report_workspace_tabs" | "watch_fleet"
        | "unwatch_fleet"
        | "rebind_control"
        // Comms-plane Phase 2 (review H1): `inbox_ack` MUTATES durable receipt state
        // (Delivered -> Processed) and force-compacts records, so it must NOT fall
        // through to the read tier - it needs the control capability AND the audit a
        // mutating, non-spawn command gets (like `create_worktree`).
        //
        // item-3 §2.4.1: `inbox_ack`'s BASE tier STAYS Organization - the host/relay
        // ack-on-behalf path needs the control capability, and a non-self-ack still does.
        // comms-plane Phase 3 (§2.4.1) then RETIRES the interim price: the session-token-
        // on-request substrate now lands the caller's identity on the wire, so a SELF-ack
        // (the caller's own session token resolves to the recipient tile) is admitted at
        // READ via a proven-self-ack bypass in `dispatch_authenticated`, and `can_ack`
        // re-checks ownership in the handler. The cross-session spoof the old
        // Organization gate feared is closed by per-session identity, not re-opened - the
        // crew ack loop no longer needs a control-capable relay. Ledger row 17: LAW-now.
        //
        // `inbox_status`'s BASE tier is Read (genuinely counts-only), but item-3 §2.4
        // refines it by SCOPE in [`effective_tier`]: an unscoped fleet-wide
        // enumeration is Organization.
        | "inbox_ack"
        // comms-plane Phase 3: `authorize` records a durable governance artifact
        // (mutating, audited); only the general originates (enforced by the handler ACL).
        | "authorize" | "appoint_admin" | "approve_admin_action" | "execute_admin_operation" | "revoke_admin" | "record_agent_delivery" | "agent_followup" => {
            CommandTier::Organization
        }
        "preview_discover" | "preview_status" => CommandTier::Read,
        // comms-plane Phase 3: `plane_send` is Read base tier so an identified CREW
        // (least-privilege read token) can send up to its captain; the handler REQUIRES a
        // resolved session identity (or a Full host) and the `can_message` ACL is the real
        // wall. `check_authorization` is a read-only resolve-and-verify consult.
        // (Every other command's tier is its default Read.)
        _ => CommandTier::Read,
    }
}

/// The tier a request must satisfy, refined by the request ARGS where the scope
/// changes the privilege. Base tiers come from [`required_tier`]; item-3 §2.4 adds
/// one refinement (ledger #15, closing the PR-56 L3 enumeration leak):
///
/// - `inbox_status` SCOPED to a single recipient (`sessionId` present) stays Read -
///   it returns just that recipient's counts/cursors, never content.
/// - `inbox_status` UNSCOPED (`depth_all`: every recipient's counts/cursors/oldest
///   age) is Organization tier, so a bare read token cannot enumerate the whole
///   fleet's inbox health.
///
/// Every other command's effective tier is exactly its [`required_tier`].
fn effective_tier(command: &str, args: &Value) -> CommandTier {
    if command == "inbox_status" {
        let scoped = arg_str(args, "sessionId")
            .or_else(|| arg_str(args, "session_id"))
            .is_some();
        if !scoped {
            return CommandTier::Organization;
        }
    }
    required_tier(command)
}

/// A resolved caller capability (socket-gate Phase 2). The read token resolves to
/// [`ReadOnly`](Capability::ReadOnly) (Read tier only); the control token to
/// [`Full`](Capability::Full) (every tier).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capability {
    ReadOnly,
    /// A short-lived lease bound to one exact, currently active Captain
    /// identity. It admits the same command tiers as the legacy global control
    /// token, while downstream identity ACLs keep its authority ship-scoped.
    ScopedControl,
    Full,
}

impl Capability {
    /// Whether this capability may run a command of the given required tier. The
    /// read token is strictly Read-only (the general's chosen default); everything
    /// else requires the full control token.
    fn allows(self, tier: CommandTier) -> bool {
        match self {
            Capability::Full => true,
            Capability::ScopedControl => true,
            Capability::ReadOnly => tier == CommandTier::Read,
        }
    }

    /// The audit `tokenTier` label for this capability.
    fn tier_label(self) -> &'static str {
        match self {
            Capability::Full => "control",
            Capability::ScopedControl => "control",
            Capability::ReadOnly => "read",
        }
    }
}

/// Resolve the presented token to a [`Capability`], or `None` if it matches no
/// known token (⇒ `unauthorized: bad control token`, byte-identical to before).
///
/// The presented token is compared against BOTH known tokens in constant time with
/// **no early return**, so timing never reveals which (if any) matched. The control
/// token wins if both somehow match. An empty configured read token authorizes
/// nothing (guards the headless-default case where no read token is set).
///
/// Belt-and-suspenders (open Q4): a REMOTE (non-loopback) peer is capped to
/// `ReadOnly` even with the control token, so a token leaked over the opt-in
/// network bind cannot spawn/type/kill via the command channel. (The separate
/// PTY-attach path keeps its own control-token check, preserving the remote
/// cockpit.)
fn resolve_capability(ctx: &ControlContext, presented: &str) -> Option<Capability> {
    let is_control = ct_token_eq(presented, &ctx.token);
    let is_read = !ctx.read_token.is_empty() && ct_token_eq(presented, &ctx.read_token);
    let cap = if is_control {
        Some(Capability::Full)
    } else if is_read {
        Some(Capability::ReadOnly)
    } else {
        None
    };
    match cap {
        Some(Capability::Full) if !ctx.peer_is_loopback => Some(Capability::ReadOnly),
        other => other,
    }
}

/// Whether a presented token is valid at all (either capability). Used by the
/// read-tier event-subscribe handshake, which a read-only monitor legitimately
/// needs. (PTY attach stays control-token-only - it can type.)
fn token_is_valid(ctx: &ControlContext, presented: &str) -> bool {
    resolve_capability(ctx, presented).is_some()
}

/// A per-session identity resolved to its DURABLE ship/role (item-2 §2.6 RESOLVE, the
/// widened resolver). This is the KEY the comms-plane enqueue-ACL, delegation-gate,
/// and cross-ship ownership ACL (item-1 2.6/H3) consume - item 2 provides the key +
/// resolver; the ACL WIRING stays item-1 Phase 3 (§2.8, Phase D).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIdentity {
    /// The minted per-session id (the non-secret attribution handle).
    pub session_id: String,
    /// The coarse mint-time role (`Captain`/`Crew`/...).
    pub mint_role: crate::identity::Role,
    /// The tile this session is bound to (a mutable pointer), if bound yet.
    pub tile: Option<String>,
    /// `ship_of(session)` - the DURABLE ship, registry-authoritative when the tile
    /// resolves in the captains registry, else the mint-time ship copy.
    pub ship_slug: Option<String>,
    /// `role_of(session)` at the registry's granularity: the first-class fleet role
    /// when the tile is a SUPERVISOR terminal, else `None` (a crew is not a fleet role).
    pub fleet_role: Option<FleetRole>,
    /// `uuid_of(session)` - the Claude continuity anchor, when the StatusBridge has
    /// resolved it (async, HIGH-1).
    pub claude_uuid: Option<String>,
}

/// RESOLVE (item-2 §2.6): map a presented per-session token to its widened
/// [`ResolvedIdentity`] - `ship_of` / `role_of` / `uuid_of` in one lookup. Kept
/// BESIDE the unchanged [`resolve_capability`] (LOW-9: identity resolution is a
/// bounded add, not a return-type widening of the tier resolver, so tier-check
/// callers keep their signature). Returns `None` for an empty/unknown token. This is
/// IDENTIFICATION, never authorization (the ACL is item-1 Phase 3).
pub fn resolve_identity(ctx: &ControlContext, presented: &str) -> Option<ResolvedIdentity> {
    let ident = ctx.identity.resolve(presented)?;
    let tile = ident.session_tile.clone();
    // Registry-authoritative ship/role, falling back to the mint-time ship copy when
    // the tile is not (yet) a registry member.
    let (ship_slug, mut fleet_role) = match tile.as_deref().and_then(|t| ctx.captains.ship_of(t)) {
        Some(ShipMembership::Supervisor { ship_slug, role }) => (Some(ship_slug), Some(role)),
        Some(ShipMembership::Crew { ship_slug }) => (Some(ship_slug), None),
        None => (ident.ship_slug.clone(), None),
    };
    let authoritative_cortana = authoritative_cortana_identity(ctx, &ident);
    if fleet_role == Some(FleetRole::Cortana) && !authoritative_cortana {
        fleet_role = None;
    }
    let mint_role = if ident.role == crate::identity::Role::Cortana && !authoritative_cortana {
        crate::identity::Role::Unknown
    } else {
        ident.role
    };
    let claude_uuid = tile
        .as_deref()
        .and_then(|t| ctx.status.session_for_terminal(t));
    Some(ResolvedIdentity {
        session_id: ident.id,
        mint_role,
        tile,
        ship_slug,
        fleet_role,
        claude_uuid,
    })
}

#[derive(Clone)]
struct ControlLeaseAuthority {
    identity_id: String,
    terminal_id: String,
    authority: LeaseAuthority,
}

fn exact_live_identity_terminal(
    ctx: &ControlContext,
    caller: &ResolvedIdentity,
) -> Result<String, String> {
    let terminal_id = caller
        .tile
        .as_deref()
        .ok_or("control_reauthentication_required: identity is not terminal-bound")?;
    if ctx.identity.count_for_tile(terminal_id) != 1 {
        return Err(
            "control_reauthentication_required: terminal identity binding is missing or ambiguous"
                .into(),
        );
    }
    let live = (ctx.live_sessions)().map_err(|error| {
        format!("control_reauthentication_required: terminal liveness is unavailable: {error}")
    })?;
    let target = tmux_target(terminal_id);
    if !live
        .iter()
        .any(|session| session == terminal_id || session == &target)
    {
        return Err(
            "control_reauthentication_required: terminal is not alive; durable identity was preserved"
                .into(),
        );
    }
    Ok(terminal_id.to_string())
}

/// Derive renewable mutation authority only from current durable state.
///
/// Possession of a read token, an old global control token, or a historical
/// mint-time role is insufficient. The exact identity, terminal, live runtime,
/// current fleet binding, and current scoped grant must agree.
fn control_lease_authority(
    ctx: &ControlContext,
    caller: &ResolvedIdentity,
) -> Result<ControlLeaseAuthority, String> {
    let terminal_id = exact_live_identity_terminal(ctx, caller)?;

    if caller.fleet_role == Some(FleetRole::Cortana)
        && caller.mint_role == crate::identity::Role::Cortana
    {
        let identity = ctx
            .identity
            .get(&caller.session_id)
            .ok_or("control_reauthentication_required: durable Cortana identity is unavailable")?;
        if !authoritative_cortana_identity(ctx, &identity) {
            return Err(
                "control_reauthentication_required: Cortana identity is not authoritative".into(),
            );
        }
        return Ok(ControlLeaseAuthority {
            identity_id: caller.session_id.clone(),
            terminal_id,
            authority: LeaseAuthority::Cortana {
                generation: ctx.captains.cortana_identity().generation,
            },
        });
    }

    let active_grants = ctx
        .delegated_admin
        .grants_for_actor(&caller.session_id)
        .into_iter()
        .filter(|grant| grant.state.is_active())
        .collect::<Vec<_>>();
    if let [grant] = active_grants.as_slice() {
        let supervisor = current_delegating_supervisor(ctx, grant);
        let actor = current_admin_actor(ctx, grant);
        ctx.delegated_admin
            .validate_effective_grant(grant, &actor, &supervisor)
            .map_err(|error| format!("control_reauthentication_required: {error}"))?;
        if actor.identity_id != caller.session_id
            || actor.session_tile.as_deref() != Some(terminal_id.as_str())
        {
            return Err(
                "control_reauthentication_required: delegated administrator identity changed"
                    .into(),
            );
        }
        return Ok(ControlLeaseAuthority {
            identity_id: caller.session_id.clone(),
            terminal_id,
            authority: LeaseAuthority::DelegatedAdmin {
                grant_id: grant.grant_id.clone(),
                grant_generation: grant.grant_generation,
                role: grant.role,
                scope: grant.scope.clone(),
            },
        });
    }

    if caller.mint_role != crate::identity::Role::Captain
        || caller.fleet_role != Some(FleetRole::Captain)
    {
        return Err(
            "control_reauthentication_required: identity has no active scoped mutation authority"
                .into(),
        );
    }

    let (snapshot, generations, registry_epoch) =
        ctx.captains.snapshot_with_authority_generations();
    let matches = snapshot
        .captains
        .iter()
        .filter(|captain| {
            captain.role == FleetRole::Captain
                && captain.state == ClaimState::Active
                && captain.terminal_id.as_deref() == Some(terminal_id.as_str())
                && caller
                    .ship_slug
                    .as_deref()
                    .is_some_and(|ship| captain.ship_slug == ship)
        })
        .collect::<Vec<_>>();
    let captain = match matches.as_slice() {
        [captain] => *captain,
        _ => {
            return Err(
                "control_reauthentication_required: Captain registry binding is missing or ambiguous"
                    .into(),
            );
        }
    };
    let project_id = captain
        .project_id
        .as_deref()
        .ok_or("control_reauthentication_required: Captain has no active Project binding")?;
    if snapshot
        .projects
        .iter()
        .filter(|project| project.project_id == project_id)
        .count()
        != 1
    {
        return Err(
            "control_reauthentication_required: Captain Project binding is missing or ambiguous"
                .into(),
        );
    }

    Ok(ControlLeaseAuthority {
        identity_id: caller.session_id.clone(),
        terminal_id: terminal_id.clone(),
        authority: LeaseAuthority::Captain {
            ship_slug: captain.ship_slug.clone(),
            project_id: project_id.to_string(),
            generation: generations.scoped(
                registry_epoch,
                &captain.ship_slug,
                &terminal_id,
                project_id,
            ),
        },
    })
}

fn renew_captain_control_lease(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    if !ctx.peer_is_loopback {
        return Err(
            "control_reauthentication_required: Captain lease renewal is loopback-only".into(),
        );
    }
    if trusted_internal {
        return Err(
            "control_reauthentication_required: trusted host transport does not use Captain leases"
                .into(),
        );
    }
    let caller = caller.ok_or(
        "control_reauthentication_required: durable session identity could not be verified",
    )?;
    let authority = control_lease_authority(ctx, caller)?;
    let lease_ttl = captain_control_lease_ttl();
    let expires_at = Instant::now() + lease_ttl;
    let response_scope = match &authority.authority {
        LeaseAuthority::Captain {
            ship_slug,
            project_id,
            ..
        } => json!({
            "kind": "captain",
            "shipSlug": ship_slug,
            "projectId": project_id,
        }),
        LeaseAuthority::Cortana { .. } => json!({ "kind": "cortana" }),
        LeaseAuthority::DelegatedAdmin { role, scope, .. } => json!({
            "kind": "delegatedAdmin",
            "role": role,
            "scope": scope,
        }),
    };
    let expires_at_epoch_ms = now_ms().saturating_add(lease_ttl.as_millis() as u64);
    let (secret, expires_at_epoch_ms) = ctx.control_leases.issue(CaptainControlLease {
        identity_id: authority.identity_id,
        terminal_id: authority.terminal_id.clone(),
        authority: authority.authority,
        expires_at,
        expires_at_epoch_ms,
    });
    Ok(json!({
        "lease": secret,
        "expiresAt": expires_at_epoch_ms,
        "terminalId": authority.terminal_id,
        "scope": response_scope,
        "capability": "control",
    }))
}

fn resolve_captain_control_lease(
    ctx: &ControlContext,
    presented: &str,
    caller: Option<&ResolvedIdentity>,
) -> Option<Capability> {
    let caller = caller?;
    let lease = ctx.control_leases.get(presented)?;
    if caller.session_id != lease.identity_id
        || caller.tile.as_deref() != Some(lease.terminal_id.as_str())
    {
        return None;
    }
    let authority = control_lease_authority(ctx, caller).ok()?;
    (authority.identity_id == lease.identity_id
        && authority.terminal_id == lease.terminal_id
        && authority.authority == lease.authority)
        .then_some(Capability::ScopedControl)
}

/// A Cortana bearer is apex only while all durable singleton facts agree on the
/// exact identity, terminal, generation, healthy recovery state, and one active
/// Fleet claim. Mint-time `Role::Cortana` is historical attribution, not authority.
fn authoritative_cortana_identity(
    ctx: &ControlContext,
    identity: &crate::identity::SessionIdentity,
) -> bool {
    if identity.role != crate::identity::Role::Cortana {
        return false;
    }
    let Some(tile) = identity.session_tile.as_deref() else {
        return false;
    };
    // Read all durable singleton facts from one versioned snapshot.  Mixing a
    // standalone Cortana read with a later registry read admits torn authority.
    let snapshot = ctx.captains.snapshot();
    let durable = snapshot.cortana;
    if durable.generation == 0
        || durable.identity_id.as_deref() != Some(identity.id.as_str())
        || durable.terminal_id.as_deref() != Some(tile)
        || !matches!(
            durable.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
        )
    {
        return false;
    }
    let active_cortana_claims = snapshot
        .captains
        .into_iter()
        .filter(|captain| captain.role == FleetRole::Cortana && captain.state == ClaimState::Active)
        .collect::<Vec<_>>();
    let exact_claim = matches!(
        active_cortana_claims.as_slice(),
        [captain] if captain.terminal_id.as_deref() == Some(tile)
    );
    exact_claim && revalidate_active_cortana_authority(ctx, &durable).is_ok()
}

/// Map a [`ResolvedIdentity`] into the [`crate::acl::AclActor`] the Phase-3 ACL keys on.
/// The effective role prefers the registry-authoritative FLEET role (a supervisor
/// terminal is a Captain/Cortana) and falls back to the mint-time role (Crew/General).
fn acl_actor(id: &ResolvedIdentity) -> crate::acl::AclActor {
    use crate::acl::AclRole;
    let role = match id.fleet_role {
        Some(FleetRole::Cortana) => AclRole::Cortana,
        Some(FleetRole::Captain) => AclRole::Captain,
        None => match id.mint_role {
            crate::identity::Role::General => AclRole::General,
            // Cortana authority is registry-derived above. A bare mint-time role
            // must never resurrect a released or superseded singleton bearer.
            crate::identity::Role::Cortana => AclRole::Unknown,
            crate::identity::Role::Captain => AclRole::Captain,
            crate::identity::Role::Crew => AclRole::Crew,
            crate::identity::Role::Unknown => AclRole::Unknown,
        },
    };
    crate::acl::AclActor {
        role,
        ship: id.ship_slug.clone(),
        tile: id.tile.clone(),
        session_id: id.session_id.clone(),
    }
}

/// Resolve a TARGET tile's [`crate::acl::ShipRef`] from the captains registry (the
/// access/abort target's ship membership). An unregistered tile is `Unowned` - nothing
/// to isolate.
fn target_ship_ref(ctx: &ControlContext, tile: &str) -> crate::acl::ShipRef {
    match ctx.captains.ship_of(tile) {
        Some(ShipMembership::Supervisor { ship_slug, .. }) => {
            crate::acl::ShipRef::Supervisor { ship: ship_slug }
        }
        Some(ShipMembership::Crew { ship_slug }) => crate::acl::ShipRef::Crew { ship: ship_slug },
        None => crate::acl::ShipRef::Unowned,
    }
}

fn target_ship_slug(ctx: &ControlContext, tile: &str) -> Option<String> {
    match ctx.captains.ship_of(tile) {
        Some(ShipMembership::Supervisor { ship_slug, .. })
        | Some(ShipMembership::Crew { ship_slug }) => Some(ship_slug),
        None => None,
    }
}

/// The recipient's [`crate::acl::MessageTarget`] (role + ship) for the send ACL, from
/// the captains registry. An unregistered recipient tile is `Unknown`/no-ship.
fn message_target(ctx: &ControlContext, tile: &str) -> crate::acl::MessageTarget {
    use crate::acl::AclRole;
    match ctx.captains.ship_of(tile) {
        Some(ShipMembership::Supervisor { ship_slug, role }) => crate::acl::MessageTarget {
            role: match role {
                FleetRole::Cortana => AclRole::Cortana,
                FleetRole::Captain => AclRole::Captain,
            },
            ship: Some(ship_slug),
        },
        Some(ShipMembership::Crew { ship_slug }) => crate::acl::MessageTarget {
            role: AclRole::Crew,
            ship: Some(ship_slug),
        },
        None => crate::acl::MessageTarget {
            role: AclRole::Unknown,
            ship: None,
        },
    }
}

/// The cross-ship isolation gate on a READ/WRITE session handler (§2.6 H3, the one
/// mechanization add). A caller without a session identity is admitted only with
/// the in-process host proof. For an IDENTIFIED session, enforce
/// [`crate::acl::can_access_session`] against the target tile's ship, refusing +
/// attributing a cross-ship reach.
fn enforce_session_access(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    target_tile: &str,
) -> Result<(), String> {
    require_socket_identity(caller, trusted_internal, "session access")?;
    let Some(id) = caller else { return Ok(()) };
    let actor = acl_actor(id);
    let target = target_ship_ref(ctx, target_tile);
    crate::acl::can_access_session(&actor, &target).map_err(|d| {
        // Attribute the denial in the audit/governor stream so a refused cross-ship
        // reach is never a silent drop.
        ctx.fanout.emit_event(
            "control://acl",
            &json!({
                "cell": "cross-ship-isolation",
                "decision": "refused",
                "session": actor.session_id.as_str(),
                "role": actor.role.label(),
                "target": target_tile,
                "reason": d.reason.as_str(),
            }),
        );
        format!("acl: {}", d.reason)
    })
}

fn caller_is_apex(caller: Option<&ResolvedIdentity>, trusted_internal: bool) -> bool {
    if trusted_internal {
        return true;
    }
    let Some(caller) = caller else { return false };
    caller.fleet_role == Some(FleetRole::Cortana)
        || caller.mint_role == crate::identity::Role::General
}

fn agent_session_has_privileged_admin_intent(agent: &AgentSessionRecord) -> bool {
    matches!(
        agent.admission_purpose,
        crate::governor::AdmissionPurpose::FleetAdmin
            | crate::governor::AdmissionPurpose::ShipAdmin
            | crate::governor::AdmissionPurpose::Recovery
    )
}

/// Administrative intent is permanent authority-boundary history.
///
/// The durable AgentSession record establishes this history while it is still in
/// Starting, before a role is appointed, and keeps it after runtime and work-stage
/// transitions.
/// Grant records contribute the same history whether active, revoked, or invalidated.
fn has_delegated_admin_history(ctx: &ControlContext, identity_id: &str) -> bool {
    if !ctx.delegated_admin.grants_for_actor(identity_id).is_empty() {
        return true;
    }
    let Some(tile) = ctx
        .identity
        .get(identity_id)
        .and_then(|identity| identity.session_tile)
    else {
        return false;
    };
    ctx.captains.snapshot().agent_sessions.iter().any(|agent| {
        agent.agent_session_id == tile && agent_session_has_privileged_admin_intent(agent)
    })
}

fn target_has_delegated_admin_history(ctx: &ControlContext, terminal_id: &str) -> bool {
    let privileged_intent = ctx.captains.snapshot().agent_sessions.iter().any(|agent| {
        agent.agent_session_id == terminal_id && agent_session_has_privileged_admin_intent(agent)
    });
    privileged_intent
        || ctx
            .identity
            .for_tile(terminal_id)
            .is_some_and(|identity| has_delegated_admin_history(ctx, &identity.id))
}

fn enforce_ship_authority(
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    ship_slug: &str,
) -> Result<(), String> {
    if caller_is_apex(caller, trusted_internal) {
        return Ok(());
    }
    let caller = caller.expect("non-apex caller must be identified");
    if caller.fleet_role == Some(FleetRole::Captain)
        && caller.ship_slug.as_deref() == Some(ship_slug)
    {
        return Ok(());
    }
    Err(format!(
        "acl: Captain lifecycle access to ship '{ship_slug}' requires General/Cortana authority or the same ship"
    ))
}

fn enforce_attach_authority(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    target_terminal: &str,
    role: FleetRole,
) -> Result<(), String> {
    if target_has_delegated_admin_history(ctx, target_terminal) {
        return Err(
            "acl: an administrative Crew identity cannot acquire Captain or Cortana authority"
                .into(),
        );
    }
    if caller_is_apex(caller, trusted_internal) {
        return Ok(());
    }
    if role == FleetRole::Cortana {
        return Err("acl: only General/Cortana may assign the Cortana role or slug".into());
    }
    let caller = caller.expect("non-apex caller must be identified");
    if has_delegated_admin_history(ctx, &caller.session_id) {
        return Err(
            "acl: delegated administrators cannot acquire Captain or Cortana authority".into(),
        );
    }
    if caller.tile.as_deref() != Some(target_terminal) {
        return Err("acl: only General/Cortana may attach a different terminal as Captain".into());
    }
    // Self-attachment is the controlled promotion path for a newly elevated session.
    // The command itself still requires the Full control token, so a normal read-only
    // Crew session cannot use this path. Once attached, registry resolution promotes
    // the caller to Captain and subsequent lifecycle operations require its same ship.
    Ok(())
}

fn require_socket_identity(
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    command: &str,
) -> Result<(), String> {
    if trusted_internal || caller.is_some() {
        Ok(())
    } else {
        Err(format!(
            "acl: '{command}' requires a valid T_HUB_SESSION_TOKEN over the control socket"
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetLifecycleAuthority {
    terminal_id: String,
    captain_terminal_id: String,
    ship_slug: String,
    project_id: Option<String>,
    generation: ScopedAuthorityGeneration,
}

fn enforce_target_lifecycle_authority(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    target_terminal: &str,
) -> Result<Option<TargetLifecycleAuthority>, String> {
    if caller_is_apex(caller, trusted_internal) {
        return Ok(None);
    }
    let caller = caller.ok_or("acl: lifecycle mutation requires a session identity")?;
    let target_terminal = target_terminal
        .strip_prefix("th_")
        .unwrap_or(target_terminal);
    if caller.tile.as_deref() == Some(target_terminal) {
        return Ok(None);
    }
    let caller_terminal_id = caller
        .tile
        .as_deref()
        .ok_or("acl: authenticated Captain has no terminal identity")?;
    let caller_ship = caller
        .ship_slug
        .as_deref()
        .ok_or("acl: authenticated Captain has no ship identity")?;
    let (snapshot, generations, registry_epoch) =
        ctx.captains.snapshot_with_authority_generations();
    let owners = snapshot
        .captains
        .iter()
        .filter(|captain| {
            captain
                .crew
                .iter()
                .any(|crew| crew.terminal_id == target_terminal)
        })
        .collect::<Vec<_>>();
    let owner = match owners.as_slice() {
        [owner] => *owner,
        _ => {
            return Err(
                "acl: only General/Cortana, the target session, or its Captain may mutate this lifecycle"
                    .into(),
            );
        }
    };
    let project_id = owner.project_id.clone();
    if caller.fleet_role != Some(FleetRole::Captain)
        || owner.role != FleetRole::Captain
        || owner.state != ClaimState::Active
        || owner.terminal_id.as_deref() != Some(caller_terminal_id)
        || owner.ship_slug != caller_ship
    {
        return Err(
            "acl: only General/Cortana, the target session, or its current Captain may mutate this lifecycle"
                .into(),
        );
    }
    Ok(Some(TargetLifecycleAuthority {
        terminal_id: target_terminal.to_string(),
        captain_terminal_id: caller_terminal_id.to_string(),
        ship_slug: owner.ship_slug.clone(),
        project_id: project_id.clone(),
        generation: generations.scoped(
            registry_epoch,
            &owner.ship_slug,
            target_terminal,
            project_id.as_deref().unwrap_or(""),
        ),
    }))
}

fn revalidate_target_lifecycle_authority(
    ctx: &ControlContext,
    expected: &TargetLifecycleAuthority,
) -> Result<(), String> {
    let (snapshot, generations, registry_epoch) =
        ctx.captains.snapshot_with_authority_generations();
    let current_generation = generations.scoped(
        registry_epoch,
        &expected.ship_slug,
        &expected.terminal_id,
        expected.project_id.as_deref().unwrap_or(""),
    );
    let owner = snapshot.captains.iter().find(|captain| {
        captain
            .crew
            .iter()
            .any(|crew| crew.terminal_id == expected.terminal_id)
    });
    if current_generation != expected.generation
        || owner.is_none_or(|owner| {
            owner.role != FleetRole::Captain
                || owner.state != ClaimState::Active
                || owner.terminal_id.as_deref() != Some(expected.captain_terminal_id.as_str())
                || owner.ship_slug != expected.ship_slug
                || owner.project_id != expected.project_id
        })
    {
        return Err(format!(
            "acl: Crew session '{}' lifecycle authority changed while the operation waited; retry from the current owning Captain",
            expected.terminal_id
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
struct AuthenticatedCaptainAuthority {
    terminal_id: String,
    ship_slug: String,
    project_id: String,
    generation: ScopedAuthorityGeneration,
}

#[cfg(test)]
#[allow(dead_code)]
fn enforce_removed_crew_powder_cleanup_authority(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    target_terminal: &str,
) -> Result<AuthenticatedCaptainAuthority, String> {
    let caller = caller.ok_or("acl: Powder cleanup requires a session identity")?;
    let caller_terminal = caller
        .tile
        .as_deref()
        .ok_or("acl: authenticated Captain has no terminal identity")?;
    let caller_ship = caller
        .ship_slug
        .as_deref()
        .ok_or("acl: authenticated Captain has no ship identity")?;
    if caller.fleet_role != Some(FleetRole::Captain) {
        return Err(
            "acl: only the owning Captain may reconcile this removed Crew Powder binding".into(),
        );
    }

    let (snapshot, generations, registry_epoch) =
        ctx.captains.snapshot_with_authority_generations();
    let owners = snapshot
        .captains
        .iter()
        .filter(|captain| {
            captain.crew.iter().any(|crew| {
                crew.terminal_id == target_terminal
                    && matches!(crew.state, CrewState::Removed { .. })
                    && crew.powder_work.is_some()
            })
        })
        .collect::<Vec<_>>();
    let owner = match owners.as_slice() {
        [] => {
            return Err(
                "acl: target has no removed Crew Powder binding eligible for historical cleanup"
                    .into(),
            );
        }
        [owner] => *owner,
        _ => {
            return Err(format!(
                "Crew session '{target_terminal}' has ambiguous historical Powder ownership"
            ));
        }
    };
    let project_id = owner
        .project_id
        .as_deref()
        .ok_or_else(|| format!("Crew session '{target_terminal}' ship has no Project binding"))?;
    let project_matches = snapshot
        .projects
        .iter()
        .filter(|project| project.project_id == project_id)
        .count();
    if owner.role != FleetRole::Captain
        || owner.state != ClaimState::Active
        || owner.terminal_id.as_deref() != Some(caller_terminal)
        || owner.ship_slug != caller_ship
        || project_matches != 1
    {
        return Err(
            "acl: only the owning Captain may reconcile this removed Crew Powder binding".into(),
        );
    }
    Ok(AuthenticatedCaptainAuthority {
        terminal_id: caller_terminal.to_string(),
        ship_slug: caller_ship.to_string(),
        project_id: project_id.to_string(),
        generation: generations.scoped(
            registry_epoch,
            &owner.ship_slug,
            target_terminal,
            project_id,
        ),
    })
}

fn enforce_project_authority(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    project_id: Option<&str>,
) -> Result<(), String> {
    if caller_is_apex(caller, trusted_internal) {
        return Ok(());
    }
    let caller = caller.ok_or("acl: project mutation requires a session identity")?;
    let Some(project_id) = project_id else {
        return Err("acl: only General/Cortana may register a new project".into());
    };
    let owner = ctx
        .captains
        .snapshot()
        .captains
        .into_iter()
        .find(|captain| captain.project_id.as_deref() == Some(project_id));
    if caller.fleet_role == Some(FleetRole::Captain)
        && owner
            .as_ref()
            .is_some_and(|owner| caller.ship_slug.as_deref() == Some(owner.ship_slug.as_str()))
    {
        Ok(())
    } else {
        Err("acl: project mutation requires General/Cortana or the owning Captain".into())
    }
}

fn enforce_project_path_authority(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    path: &str,
    command: &str,
) -> Result<(), String> {
    if caller_is_apex(caller, trusted_internal) {
        return Ok(());
    }
    let caller = caller.ok_or_else(|| format!("acl: '{command}' requires a Fleet identity"))?;
    let terminal_id = caller
        .tile
        .as_deref()
        .ok_or_else(|| format!("acl: '{command}' caller has no terminal binding"))?;
    let snapshot = ctx.captains.snapshot();
    let captain = snapshot
        .captains
        .iter()
        .find(|captain| {
            captain.role == FleetRole::Captain
                && captain.state == ClaimState::Active
                && captain.terminal_id.as_deref() == Some(terminal_id)
                && caller.ship_slug.as_deref() == Some(captain.ship_slug.as_str())
        })
        .ok_or_else(|| format!("acl: '{command}' requires an active Captain"))?;
    let project_id = captain
        .project_id
        .as_deref()
        .ok_or_else(|| format!("acl: '{command}' Captain has no Project"))?;
    let project = snapshot
        .projects
        .iter()
        .find(|project| project.project_id == project_id)
        .ok_or_else(|| format!("acl: '{command}' Captain Project is unavailable"))?;
    let requested = files::posix_form(path).trim_end_matches('/').to_string();
    if requested.split('/').any(|segment| segment == "..") {
        return Err(format!("acl: '{command}' path traversal is not permitted"));
    }
    let root = files::posix_form(&project.repo_root)
        .trim_end_matches('/')
        .to_string();
    if requested == root || requested.starts_with(&format!("{root}/")) {
        Ok(())
    } else {
        Err(format!(
            "acl: '{command}' path is outside caller Project '{}'",
            project.project_id
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WorkspaceMutationAuthority {
    Apex,
    Assignment(FleetWorkspaceOwner),
}

fn workspace_mutation_authority(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    command: &str,
) -> Result<WorkspaceMutationAuthority, String> {
    if trusted_internal {
        return Ok(WorkspaceMutationAuthority::Apex);
    }
    let caller = caller
        .ok_or_else(|| format!("acl: '{command}' requires a terminal-bound Fleet identity"))?;
    if caller.fleet_role == Some(FleetRole::Cortana)
        || caller.mint_role == crate::identity::Role::General
    {
        return Ok(WorkspaceMutationAuthority::Apex);
    }
    if caller.fleet_role != Some(FleetRole::Captain) {
        return Err(format!(
            "acl: '{command}' requires General/Cortana or a durable Captain Assignment"
        ));
    }
    let terminal_id = caller
        .tile
        .as_deref()
        .ok_or_else(|| format!("acl: '{command}' Captain caller has no terminal binding"))?;
    let snapshot = ctx.captains.snapshot();
    let matches = snapshot
        .captains
        .iter()
        .filter(|captain| {
            captain.state == ClaimState::Active
                && captain.terminal_id.as_deref() == Some(terminal_id)
                && caller.ship_slug.as_deref() == Some(captain.ship_slug.as_str())
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format!(
            "acl: '{command}' caller does not resolve to exactly one active Captain"
        ));
    }
    let captain = matches[0];
    let project_id = captain
        .project_id
        .clone()
        .ok_or_else(|| format!("acl: '{command}' Captain has no durable Project binding"))?;
    Ok(WorkspaceMutationAuthority::Assignment(
        FleetWorkspaceOwner {
            project_id,
            assignment_id: captain.assignment_id.clone(),
            ship_slug: captain.ship_slug.clone(),
        },
    ))
}

fn enforce_workspace_owner(
    ctx: &ControlContext,
    authority: &WorkspaceMutationAuthority,
    workspace_id: &str,
    command: &str,
) -> Result<(), String> {
    if matches!(authority, WorkspaceMutationAuthority::Apex) {
        return Ok(());
    }
    let WorkspaceMutationAuthority::Assignment(caller_owner) = authority else {
        unreachable!();
    };
    let snapshot = ctx.captains.snapshot();
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| format!("{command}: unknown durable Workspace '{workspace_id}'"))?;
    if workspace.kind == WorkspaceKind::Captain {
        return Err(format!(
            "acl: '{command}' Captain Workspace mutation requires General/Cortana"
        ));
    }
    if workspace.owner.as_ref() == Some(caller_owner) {
        Ok(())
    } else {
        Err(format!(
            "acl: '{command}' cannot mutate Workspace '{workspace_id}' outside caller Project/Assignment"
        ))
    }
}

/// Validate the public spawn capability contract.
///
/// Generic spawns always mint a Crew identity and therefore cannot safely request
/// orchestration authority. Captain, Cortana, and delegated administrator authority
/// is acquired only after the corresponding durable server-side binding exists.
fn require_read_only_spawn(args: &Value, command: &str) -> Result<(), String> {
    let Some(declared) = arg_str(args, "capability") else {
        return Ok(());
    };
    if declared.eq_ignore_ascii_case("read") {
        return Ok(());
    }
    if declared.eq_ignore_ascii_case("control") {
        return Err(format!(
            "{command}: capability 'control' is unsupported for generic Crew spawns; use the durable Captain, Cortana, or delegated-administrator workflow"
        ));
    }
    Err(format!(
        "{command}: capability must be 'read' when supplied"
    ))
}

/// Environment passed to a spawned session for durable control discovery.
///
/// Only the stable authoritative file path is populated. Rotating endpoint and
/// tier credential variables are explicitly blanked so inherited process state
/// cannot override discovery or leak the shared global control capability.
fn elevation_env(ctx: &ControlContext, _args: &Value) -> Vec<(String, String)> {
    if ctx.addr.is_empty() {
        return Vec::new();
    }
    vec![
        ("T_HUB_CONTROL_FILE".to_string(), discovery_file_for_spawn()),
        ("T_HUB_CONTROL_ADDR".to_string(), String::new()),
        ("T_HUB_CONTROL_TOKEN".to_string(), String::new()),
    ]
}

/// The directory `GH_CONFIG_DIR` points crew at: an EMPTY t-hub-owned config dir
/// with no `hosts.yml`, so `gh` finds no ambient credential there.
///
/// The value is injected into the crew's WSL shell via `tmux -e`, so it MUST be a
/// POSIX path. The old form built it with `PathBuf::join` and a `USERPROFILE`
/// fallback: on a Windows binary that yields BACKSLASH separators and/or a `C:`
/// drive path (`C:\Users\...\.t-hub\crew-gh-empty`) that is meaningless in WSL -
/// so `gh` got a mangled dir and the credential-withholding wall silently broke
/// (audit HIGH, hit 3 crews). Fix: resolve ONLY from a POSIX-absolute `$HOME` and
/// build the string with explicit forward slashes; when `$HOME` is absent or a
/// Windows-style value (a native-Windows launch), fall back to a fixed POSIX path.
/// Either way the crew gets a valid POSIX dir with no `hosts.yml` - never a
/// backslash/drive path.
fn crew_empty_gh_config_dir() -> String {
    let dir = crew_gh_config_dir_from_home(std::env::var("HOME").ok().as_deref());
    // The security property is the ABSENCE of a `hosts.yml` at this path, not that
    // the app created it, so a best-effort create is enough even if the app's FS
    // view differs from the crew's WSL view.
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Pure core of [`crew_empty_gh_config_dir`] (env-free, so it is unit-testable
/// without racy env mutation). Given the raw `$HOME` value, produce the POSIX
/// `GH_CONFIG_DIR` string for the crew's WSL shell.
///
/// A POSIX-absolute `$HOME` (the WSL-launched app, the common case) is used
/// directly; anything else - `None` or a Windows `USERPROFILE`-style value
/// (`C:\Users\...`, no leading `/`) - falls back to a fixed `/tmp` path. The
/// string is built with explicit forward slashes (NOT `PathBuf::join`, which
/// emits `\` on a Windows binary), so the result is ALWAYS a backslash-free POSIX
/// path.
#[cfg(feature = "devbuild")]
const CREW_GH_CONFIG_SUBDIR: &str = ".t-hub-dev/crew-gh-empty";
#[cfg(not(feature = "devbuild"))]
const CREW_GH_CONFIG_SUBDIR: &str = ".t-hub/crew-gh-empty";

fn crew_gh_config_dir_from_home(home: Option<&str>) -> String {
    let base = home
        .filter(|h| h.starts_with('/'))
        .unwrap_or("/tmp")
        .trim_end_matches('/');
    format!("{base}/{CREW_GH_CONFIG_SUBDIR}")
}

/// item-3 §2.3.5 (MED-5): the credential-WITHHOLDING env for a Crew spawn - the
/// second, independent wall behind the PreToolUse gate ("hook OR missing
/// credential"). Points `gh` at an empty config dir (so it finds no `hosts.yml`
/// credential) AND blanks the common ambient token env vars at the SESSION level (a
/// tmux `-e KEY=` overrides any inherited value), so a crew that evades the gate still
/// fails at the remote for lack of a credential.
/// Capability and organizational role are independent: an administrative Crew may
/// later acquire an identity-bound scoped lease while still receiving no ambient
/// publishing credentials.
pub(crate) fn crew_credential_withholding_env() -> Vec<(String, String)> {
    let mut env = vec![("GH_CONFIG_DIR".to_string(), crew_empty_gh_config_dir())];
    // Blank the ambient publish/registry/spend tokens a crew must not wield. Setting
    // them empty at the session level scrubs any value inherited from the app env.
    for key in [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "NPM_TOKEN",
        "NODE_AUTH_TOKEN",
        "CARGO_REGISTRY_TOKEN",
    ] {
        env.push((key.to_string(), String::new()));
    }
    env
}

/// Emit a keyed audit record for a control-capability spawn (item-3 §2.1.1
/// piece 4: "a control-spawn is never silent"). A distinct `control-spawn` decision
/// with `tokenTier: control` so a log review enumerates exactly who was elevated and
/// by whom (the `spawnedBy` meta). Read-tier spawns (the least-privilege default) are
/// already covered by the command's own allowed-path audit, so only the elevation is
/// recorded here.
fn audit_control_spawn(ctx: &ControlContext, command: &str, args: &Value) {
    let session = args
        .get("sessionId")
        .or_else(|| args.get("session_id"))
        .and_then(|v| v.as_str());
    let spawned_by = args
        .get("spawnedBy")
        .or_else(|| args.get("spawned_by"))
        .and_then(|v| v.as_str());
    ctx.audit.record(
        command,
        required_tier(command).label(),
        "control-spawn",
        args,
        AuditMeta {
            peer: if ctx.peer_is_loopback {
                "loopback"
            } else {
                "remote"
            },
            token_tier: Capability::Full.tier_label(),
            session,
            spawned_by,
            error: None,
        },
    );
}

/// Comms-plane Phase 2 (§2.3, D9): build the spawn env AND mint the session's
/// per-session identity, injecting the per-session token (`T_HUB_SESSION_TOKEN`)
/// ALONGSIDE the tier token that [`elevation_env`] already sets. Returns the env plus
/// the minted identity. A known requested session id is bound durably before the
/// child starts; a generic terminal id is bound once `spawn_tmux_terminal` returns.
/// When no capability env is injected (headless / addr unknown) no identity is minted
/// and the session behaves exactly as before - the identity slice is additive.
///
/// Role at mint is best-effort `Crew`: `spawn_terminal` / `create_worktree` are the
/// crew-spawn paths (a captain is created via `claim_captain`, not here).
///
/// Item-2 §2.6/D5 (the widened binding): the mint now ALSO carries the crew's durable
/// SHIP, resolved from the SPAWNER's identity - a crew inherits its spawner captain's
/// ship (`ship_of(spawnedBy)`). This is the same seam item 1 stood up, widened from
/// `{claude_uuid}` to `{claude_uuid, ship_slug, role}`; the durable ship key still
/// lives authoritatively in the captains registry, this is the fast-path attribution
/// copy. `None` when the spawner has no claim yet (the ship is unresolved).
fn spawn_env_with_identity(
    ctx: &ControlContext,
    args: &Value,
    command: &str,
    requested_session_id: Option<&str>,
) -> Result<
    (
        Vec<(String, String)>,
        Option<crate::identity::SessionIdentity>,
    ),
    String,
> {
    require_read_only_spawn(args, command)?;
    let mut env = elevation_env(ctx, args);
    if env.is_empty() {
        // No addr => headless; do not mint (there is no channel for the session to
        // present its token over anyway).
        return Ok((env, None));
    }
    // Every identity minted by this helper is Crew. A later durable appointment can
    // let that exact identity acquire a scoped administrative lease.
    env.extend(crew_credential_withholding_env());
    // Resolve the spawner's ship so the crew's binding carries it (item-2 §2.3/§2.6).
    let ship = arg_str(args, "spawnedBy")
        .or_else(|| arg_str(args, "spawned_by"))
        .and_then(|spawner| ctx.captains.ship_of(&spawner))
        .map(|m| m.ship_slug().to_string());
    let identity = match requested_session_id {
        Some(session_id) => ctx
            .identity
            .mint_and_bind(crate::identity::Role::Crew, ship, session_id)
            .map_err(|error| {
                format!("spawn_terminal: identity pre-binding persistence failed: {error}")
            })?,
        None => ctx.identity.mint_for(crate::identity::Role::Crew, ship)?,
    };
    env.push((
        crate::identity::SESSION_TOKEN_ENV.to_string(),
        identity.secret.clone(),
    ));
    Ok((env, Some(identity)))
}

/// Read the authoritative tmux registry and include durable Crew records that are
/// still in `Starting` before their tmux session exists.
///
/// A failed tmux enumeration is propagated. Returning zero here would erase both
/// the concurrent ceiling and reserved-slot policy during the exact outage in
/// which the process source of truth cannot be checked.
#[derive(Debug)]
struct LiveSessionEvidence {
    tmux_sessions: Vec<String>,
    total_live_sessions: usize,
    pending_provider_sessions: usize,
}

fn agent_has_durable_provider_intent(agent: &AgentSessionRecord) -> bool {
    if agent.work_stage == crate::agent_session::WorkStage::Stopped {
        return false;
    }
    !(agent.work_stage == crate::agent_session::WorkStage::Complete
        && agent
            .delivery
            .as_ref()
            .is_some_and(|delivery| delivery.states().integrated))
}

fn live_session_evidence(
    ctx: &ControlContext,
    snapshot: &CaptainsSnapshot,
    excluded_history_terminal_id: Option<&str>,
) -> Result<LiveSessionEvidence, String> {
    let sessions = (ctx.live_sessions)()
        .map_err(|error| format!("tmux session evidence unavailable: {error}"))?;
    let live = sessions
        .iter()
        .filter(|session| session.starts_with("th_"))
        .cloned()
        .collect::<BTreeSet<_>>();
    let durable_pending_sessions = snapshot
        .agent_sessions
        .iter()
        .filter(|agent| agent.runtime_state == RuntimeState::Starting)
        .filter(|agent| !live.contains(&tmux_target(&agent.agent_session_id)))
        .count();
    let pending_agent_providers = snapshot
        .agent_sessions
        .iter()
        .filter(|agent| agent_has_durable_provider_intent(agent))
        .filter(|agent| !live.contains(&tmux_target(&agent.agent_session_id)))
        .count();
    let pending_history = ctx
        .history
        .pending_resumes()
        .map_err(|error| format!("durable History provider intent is unavailable: {error}"))?
        .into_iter()
        .filter(|pending| {
            excluded_history_terminal_id != Some(pending.terminal_id.as_str())
                && !live.contains(&tmux_target(&pending.terminal_id))
        })
        .count();
    Ok(LiveSessionEvidence {
        tmux_sessions: live.iter().cloned().collect(),
        total_live_sessions: live
            .len()
            .saturating_add(durable_pending_sessions)
            .saturating_add(pending_history),
        pending_provider_sessions: pending_agent_providers.saturating_add(pending_history),
    })
}

#[cfg(test)]
fn live_session_count(ctx: &ControlContext, snapshot: &CaptainsSnapshot) -> Result<usize, String> {
    Ok(live_session_evidence(ctx, snapshot, None)?.total_live_sessions)
}

/// Whether a `send_keys` payload carries a process-signal / kill-style key. The
/// destructive throttle applies to these (interrupt / quit / EOF / suspend), not
/// to benign navigation keys, so typing `Up`/`Enter` is never rate-limited.
fn keys_are_kill_style(args: &Value) -> bool {
    args.get("keys")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|k| k.as_str()).any(is_kill_key))
        .unwrap_or(false)
}

fn is_kill_key(k: &str) -> bool {
    matches!(
        k.trim().to_ascii_uppercase().as_str(),
        "C-C" | "C-\\" | "C-D" | "C-Z"
    )
}

/// The fleet gate (socket-gate Phase 1 §4): consult the governor for the
/// process-changing command about to run. `spawn_terminal` is bounded by the
/// concurrent-session cap + spawn rate; `close_terminal` and kill-style `send_keys`
/// by the destructive throttle; `send_text` and benign `send_keys` are not
/// throttled (only audited).
fn governor_gate<'a>(
    ctx: &'a ControlContext,
    command: &str,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<GovernorAdmission<'a>, crate::governor::Refusal> {
    let now = std::time::Instant::now();
    match command {
        // start_agent owns the same admission guard inside its implementation so
        // direct test/internal calls cannot bypass the atomic check. Cortana only
        // reserves when reconciliation actually needs a replacement runtime.
        "start_agent" | "reconcile_cortana" => Ok(GovernorAdmission::None),
        // A durable delegated administrator reaches only the maintenance-only
        // create_worktree handler, which cannot create a tab, identity, terminal,
        // capability, or Crew record. Do not consume or require spawn evidence for
        // a filesystem-only operation. The handler still requires one active exact
        // Ship Admin grant, so revoked historical identities fail there.
        "create_worktree"
            if caller
                .is_some_and(|identity| has_delegated_admin_history(ctx, &identity.session_id)) =>
        {
            Ok(GovernorAdmission::None)
        }
        "spawn_terminal" | "create_worktree" | "add_worktree_workspace" => {
            let purpose = requested_spawn_purpose(command, args, caller, trusted_internal)?;
            let requested_provider_lanes = usize::from(
                arg_str(args, "_providerHarness").is_some()
                    || arg_str(args, "providerIntent").is_some(),
            );
            admit_spawn(ctx, purpose, requested_provider_lanes, None).map(|guard| {
                GovernorAdmission::Spawn {
                    _guard: Box::new(guard),
                    governor: &ctx.governor,
                }
            })
        }
        "close_terminal" | "cleanup_worktree_artifacts" => {
            ctx.governor
                .check_destructive(now)
                .map(|()| GovernorAdmission::Destructive {
                    governor: &ctx.governor,
                })
        }
        "send_keys" if keys_are_kill_style(args) => {
            ctx.governor
                .check_destructive(now)
                .map(|()| GovernorAdmission::Destructive {
                    governor: &ctx.governor,
                })
        }
        _ => Ok(GovernorAdmission::None),
    }
}

/// Keep the public terminal and worktree primitives from becoming a second,
/// incomplete Crew dispatcher.
///
/// A trusted in-process host still uses these primitives for UI terminals and
/// internal launch transactions.
/// An identified supervisor may also open a plain shell or a plain worktree.
/// Once a request links the terminal to a Captain, launches a command, or asks
/// for reserved Crew capacity, however, it is an agent assignment and must flow
/// through `start_agent` so the exact baseline, lane claims, collision preflight,
/// durable Starting record, and dependency evidence are one transaction.
fn enforce_public_spawn_contract(
    command: &str,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<(), String> {
    if trusted_internal {
        return Ok(());
    }
    let Some(caller) = caller else {
        return Ok(());
    };
    let is_supervisor = caller.fleet_role.is_some()
        || matches!(
            caller.mint_role,
            crate::identity::Role::General
                | crate::identity::Role::Cortana
                | crate::identity::Role::Captain
        );
    if !is_supervisor {
        return Ok(());
    }
    let spawned_by = arg_str(args, "spawnedBy")
        .or_else(|| arg_str(args, "spawned_by"))
        .is_some_and(|value| !value.trim().is_empty());
    let startup_command = arg_str(args, "startupCommand")
        .or_else(|| arg_str(args, "startup_command"))
        .is_some_and(|value| !value.trim().is_empty());
    let shell_command = command == "spawn_terminal"
        && arg_str(args, "shell").is_some_and(|value| !value.trim().is_empty());
    let reserved_capacity = arg_str(args, "admissionPurpose")
        .or_else(|| arg_str(args, "admission_purpose"))
        .is_some_and(|value| !value.trim().is_empty() && !value.eq_ignore_ascii_case("ordinary"));
    let control_capability = arg_str(args, "capability")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("control"));
    if spawned_by || startup_command || shell_command || reserved_capacity || control_capability {
        return Err(format!(
            "{command}: supervisor Crew/provider assignments must use start_agent with an exact sourceCommit, durable lane and dependency claims, collision preflight, and a Starting record; no terminal or worktree was created"
        ));
    }
    Ok(())
}

fn requested_spawn_purpose(
    command: &str,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<SpawnPurpose, crate::governor::Refusal> {
    let requested = arg_str(args, "admissionPurpose")
        .or_else(|| arg_str(args, "admission_purpose"))
        .unwrap_or_else(|| "ordinary".into());
    let deny = |message: &str| crate::governor::Refusal {
        code: "refused-role",
        message: format!("spawn refused: {message}"),
    };
    match requested.trim().to_ascii_lowercase().as_str() {
        "ordinary" => Ok(SpawnPurpose::Ordinary),
        "fleet-admin" => {
            if !matches!(
                command,
                "spawn_terminal" | "create_worktree" | "start_agent"
            ) {
                return Err(deny(
                    "fleet-admin admission is only valid for a Crew terminal spawn",
                ));
            }
            if caller_is_apex(caller, trusted_internal) {
                Ok(SpawnPurpose::FleetAdmin)
            } else {
                Err(deny(
                    "fleet-admin reserved capacity requires General or Cortana authority",
                ))
            }
        }
        "ship-admin" => {
            if !matches!(
                command,
                "spawn_terminal" | "create_worktree" | "start_agent"
            ) {
                return Err(deny(
                    "ship-admin admission is only valid for a Crew terminal spawn",
                ));
            }
            let Some(caller) = caller else {
                return Err(deny(
                    "ship-admin reserved capacity requires an identified owning Captain",
                ));
            };
            let spawned_by = if command == "start_agent" {
                arg_str(args, "captainSessionId")
            } else {
                arg_str(args, "spawnedBy").or_else(|| arg_str(args, "spawned_by"))
            }
            .filter(|value| !value.trim().is_empty());
            if caller.fleet_role == Some(FleetRole::Captain)
                && caller.tile.as_deref() == spawned_by.as_deref()
                && caller.ship_slug.is_some()
            {
                Ok(SpawnPurpose::ShipAdmin {
                    ship_slug: caller.ship_slug.clone().expect("checked above"),
                })
            } else {
                Err(deny(
                    "ship-admin reserved capacity requires the same owning Captain in spawnedBy",
                ))
            }
        }
        "recovery" => {
            if caller_is_apex(caller, trusted_internal) {
                Ok(SpawnPurpose::Recovery)
            } else {
                Err(deny(
                    "recovery reserved capacity requires General or Cortana authority",
                ))
            }
        }
        "cortana" => Err(deny(
            "Cortana reserved capacity is available only to singleton reconciliation",
        )),
        other => Err(deny(&format!("unknown admissionPurpose '{other}'"))),
    }
}

fn durable_admission_purpose(purpose: &SpawnPurpose) -> crate::governor::AdmissionPurpose {
    match purpose {
        SpawnPurpose::Ordinary => crate::governor::AdmissionPurpose::Ordinary,
        SpawnPurpose::Cortana => crate::governor::AdmissionPurpose::Cortana,
        SpawnPurpose::FleetAdmin => crate::governor::AdmissionPurpose::FleetAdmin,
        SpawnPurpose::ShipAdmin { .. } => crate::governor::AdmissionPurpose::ShipAdmin,
        SpawnPurpose::Recovery => crate::governor::AdmissionPurpose::Recovery,
    }
}

fn ship_admin_scope(purpose: &SpawnPurpose) -> Option<String> {
    match purpose {
        SpawnPurpose::ShipAdmin { ship_slug } => Some(ship_slug.clone()),
        _ => None,
    }
}

/// Write one audit record for an Organization/ProcessChanging command (or a
/// governor refusal). `decision` is the gate outcome (`allowed` / `refused-*`);
/// `error` carries a downstream dispatch failure for an allowed command.
fn audit_command(
    ctx: &ControlContext,
    req: &ControlRequest,
    tier: CommandTier,
    cap: Capability,
    decision: &str,
    error: Option<&str>,
) {
    if decision.starts_with("refused-")
        && !ctx.governor.admit_refusal_audit(std::time::Instant::now())
    {
        return;
    }
    if let Err(audit_error) = try_audit_command(ctx, req, tier, cap, decision, error) {
        eprintln!(
            "t-hub-audit: failed to write audit record for '{}': {audit_error}",
            req.command
        );
    }
}

fn try_audit_command(
    ctx: &ControlContext,
    req: &ControlRequest,
    tier: CommandTier,
    cap: Capability,
    decision: &str,
    error: Option<&str>,
) -> std::io::Result<()> {
    let session = req
        .args
        .get("sessionId")
        .or_else(|| req.args.get("session_id"))
        .and_then(|v| v.as_str());
    let spawned_by = req
        .args
        .get("spawnedBy")
        .or_else(|| req.args.get("spawned_by"))
        .and_then(|v| v.as_str());
    ctx.audit.try_record(
        &req.command,
        tier.label(),
        decision,
        &req.args,
        AuditMeta {
            peer: if ctx.peer_is_loopback {
                "loopback"
            } else {
                "remote"
            },
            // Phase 2: the capability the presented token resolved to.
            token_tier: cap.tier_label(),
            session,
            spawned_by,
            error,
        },
    )
}

/// Resolve capability, gate + audit, then dispatch. A bad token is rejected before
/// any command runs (byte-identical message, no leak of which commands exist).
///
/// Order (§3): (1) resolve the presented token to a [`Capability`]; (2) the
/// command's [`required_tier`] must be covered by that capability, else refuse
/// `refused-authz` and audit it; (3) for ProcessChanging the fleet governor runs
/// (Phase 1, refuse-past-ceiling); (4) dispatch. Every Organization/ProcessChanging
/// command is offered to the audit log with its `tokenTier`, and a refusal is
/// mirrored live onto the event fanout.
/// ProcessChanging authorization is durably recorded before dispatch; other
/// records remain best-effort.
fn dispatch_authenticated(ctx: &ControlContext, req: ControlRequest) -> ControlResponse {
    // Comms-plane Phase 3: resolve the caller's PER-SESSION identity from the session
    // token carried on the request (`req.session`), if any. IDENTIFICATION only
    // (`resolve_identity` is not authorization); the per-command ACL wiring consumes it.
    // A control-token HOST (no session token) resolves to `None`.
    let caller = resolve_identity(ctx, &req.session);
    let trusted_internal =
        ctx.peer_is_loopback && !req.host.is_empty() && ct_token_eq(&req.host, &ctx.host_token);
    let Some(cap) = resolve_capability(ctx, &req.token)
        .or_else(|| resolve_captain_control_lease(ctx, &req.token, caller.as_ref()))
    else {
        return ControlResponse::err("unauthorized: bad control token");
    };

    // item-3 §2.4: the effective tier is args-refined - an UNSCOPED `inbox_status`
    // (fleet-wide enumeration) is Organization even though its base tier is Read.
    let tier = effective_tier(&req.command, &req.args);

    // Comms-plane Phase 3 (§2.4.1): the inbox-ack SELF-SCOPE upgrade retires the interim
    // "ack stays Organization; a control-capable relay carries it" price (PR-56/#59) now
    // that the per-session token lands the caller's identity on the wire. A session that
    // presents a valid token resolving to the recipient's OWN tile may self-ack even with
    // a bare READ token (the crew ack loop, no relay needed). The tier refusal below is
    // bypassed ONLY for this proven self-ack; a cross-session ack still needs the control
    // capability and is re-checked by `can_ack` in the handler. The Full capability
    // never substitutes for the separate in-process host proof.
    let inbox_self_ack = req.command == "inbox_ack"
        && caller
            .as_ref()
            .zip(arg_str(&req.args, "sessionId").or_else(|| arg_str(&req.args, "session_id")))
            .map(|(id, recipient)| id.tile.as_deref() == Some(recipient.as_str()))
            .unwrap_or(false);
    let crew_self_work_log = req.command == "append_crew_powder_work_log"
        && caller
            .as_ref()
            .map(|identity| {
                identity.mint_role == crate::identity::Role::Crew
                    && identity.fleet_role.is_none()
                    && identity.tile.is_some()
            })
            .unwrap_or(false);
    let agent_self_checkpoint = req.command == "agent_checkpoint"
        && caller
            .as_ref()
            .zip(arg_str(&req.args, "agentSessionId"))
            .is_some_and(|(identity, agent)| {
                identity.mint_role == crate::identity::Role::Crew
                    && identity.fleet_role.is_none()
                    && identity.tile.as_deref() == Some(agent.as_str())
            });
    let agent_self_delivery = req.command == "record_agent_delivery"
        && matches!(
            arg_str(&req.args, "state").as_deref(),
            Some("implemented" | "tested")
        )
        && caller
            .as_ref()
            .zip(arg_str(&req.args, "agentSessionId"))
            .is_some_and(|(identity, agent)| {
                identity.mint_role == crate::identity::Role::Crew
                    && identity.fleet_role.is_none()
                    && identity.tile.as_deref() == Some(agent.as_str())
            });

    // A shared Full capability permits use of the mutation surface, but it does not
    // identify who is exercising it.
    // Every untrusted Organization or ProcessChanging request must also present a
    // currently valid per-session identity.
    // Only the exact in-process host provenance may omit that identity.
    if matches!(cap, Capability::Full | Capability::ScopedControl)
        && tier != CommandTier::Read
        && !trusted_internal
        && caller
            .as_ref()
            .is_none_or(|identity| identity.tile.is_none())
    {
        let message = format!(
            "unauthorized: '{}' requires a valid T_HUB_SESSION_TOKEN with the control capability",
            req.command
        );
        audit_command(ctx, &req, tier, cap, "refused-identity", Some(&message));
        ctx.fanout.emit_event(
            "control://governor",
            &json!({
                "command": req.command.as_str(),
                "decision": "refused-identity",
                "error": message.as_str(),
            }),
        );
        return ControlResponse::err(message);
    }

    // Phase 2 capability gate: the presented token's capability must cover the
    // command's required tier. The read token authorizes Read only; Organization
    // and ProcessChanging require the control token.
    if !cap.allows(tier)
        && !inbox_self_ack
        && !crew_self_work_log
        && !agent_self_checkpoint
        && !agent_self_delivery
    {
        let message = format!(
            "unauthorized: '{}' requires the control capability (this token is read-only)",
            req.command
        );
        audit_command(ctx, &req, tier, cap, "refused-authz", None);
        ctx.fanout.emit_event(
            "control://governor",
            &json!({
                "command": req.command.as_str(),
                "decision": "refused-authz",
                "error": message.as_str(),
            }),
        );
        return ControlResponse::err(message);
    }

    // Renewal is deliberately a Read-tier authentication operation. It accepts
    // only the ambient read credential plus the durable session secret, then
    // derives all control authority from live server state. No global control
    // token crosses this response boundary.
    if req.command == "renew_captain_control_lease" {
        return match renew_captain_control_lease(ctx, caller.as_ref(), trusted_internal) {
            Ok(result) => ControlResponse::ok(result),
            Err(error) => ControlResponse::err(error),
        };
    }
    // Internal MCP discovery proof. The ambient read token authenticates the
    // handshake, while the echoed nonce prevents replay and the live instance /
    // generation binds the response to this listener. The caller never needs to
    // present a durable Captain session secret.
    if req.command == "control_discovery_proof" {
        let Some(nonce) =
            arg_str(&req.args, "nonce").filter(|value| !value.is_empty() && value.len() <= 256)
        else {
            return ControlResponse::err(
                "control_discovery_proof requires a bounded non-empty nonce",
            );
        };
        return ControlResponse::ok(json!({
            "nonce": nonce,
            "protocolVersion": PROTOCOL_VERSION,
            "instanceId": ctx.listener_instance_id,
            "listenerGeneration": ctx.bound_listener_generation,
            "listenerAddr": ctx.addr,
        }));
    }
    let delegated_control_access = if tier == CommandTier::Read
        || inbox_self_ack
        || crew_self_work_log
        || agent_self_checkpoint
        || agent_self_delivery
    {
        crate::delegated_admin::ControlAccess::Read
    } else {
        crate::delegated_admin::ControlAccess::Mutation
    };
    let delegated_capability = match cap {
        Capability::ReadOnly => crate::delegated_admin::ControlCapability::ReadOnly,
        Capability::ScopedControl | Capability::Full => {
            crate::delegated_admin::ControlCapability::Full
        }
    };
    if let Err(error) = crate::delegated_admin::require_control_capability(
        delegated_capability,
        delegated_control_access,
    ) {
        return ControlResponse::err(format!(
            "unauthorized: delegated control admission failed: {error}"
        ));
    }

    // item-3 Pillar C: `my_capability` echoes the CALLER's resolved capability so the
    // out-of-process PreToolUse gate can resolve its own class (control vs read) from
    // the unspoofable token it presents. Read-tier (any valid token passes the gate
    // above), no side effect, so it is answered here from `cap` directly.
    if req.command == "my_capability" {
        return ControlResponse::ok(json!({ "capability": cap.tier_label() }));
    }

    if req.command == "audit_verify" {
        return ControlResponse::ok(ctx.audit.verify_self_cached().to_json());
    }

    if matches!(req.command.as_str(), "spawn_terminal" | "create_worktree") {
        if let Err(error) = enforce_public_spawn_contract(
            &req.command,
            &req.args,
            caller.as_ref(),
            trusted_internal,
        ) {
            audit_command(ctx, &req, tier, cap, "refused-contract", Some(&error));
            ctx.fanout.emit_event(
                "control://governor",
                &json!({
                    "command": req.command.as_str(),
                    "decision": "refused-contract",
                    "error": error.as_str(),
                }),
            );
            return ControlResponse::err(error);
        }
    }

    // A create-worktree request must prove its project/path authority and its
    // registered Git capability before it reserves an idempotency slot.  This
    // keeps both fresh and stale retries free of admission state changes when a
    // non-Git Project or an unauthorized remote path is rejected.
    if req.command == "create_worktree" {
        match authorize_reprobe_create_worktree(ctx, &req.args, caller.as_ref(), trusted_internal) {
            Ok(repo_root) => {
                if let Err(error) =
                    require_registered_git_capability(ctx, "create_worktree", &repo_root)
                {
                    return ControlResponse::err(error);
                }
            }
            Err(error) => return ControlResponse::err(error),
        }
    }

    // Authorization must precede idempotency-cache lookup. Otherwise a caller
    // outside the owning ship could replay an owner's cached success or reserve
    // the owner's requestId with a cached refusal.
    if req.command == "agent_followup" {
        let followup = match parse_agent_followup(&req.args) {
            Ok(followup) => followup,
            Err(error) => {
                return ControlResponse::err(agent_followup_error("invalid_request", error));
            }
        };
        if let Err(error) =
            authorize_agent_followup(ctx, &followup, caller.as_ref(), trusted_internal)
        {
            return ControlResponse::err(error);
        }
    }

    // Spawn-class idempotency (ask #1): a client-supplied `requestId` on a
    // spawn-class command makes it safely retryable across an ambiguous response
    // leg. We consult the outcome cache BEFORE the governor charges budget so a
    // retry neither double-applies the side effect (the Incident A/B
    // duplicate-maker) nor double-charges the fleet budget. A command without a
    // requestId is unaffected - it dispatches exactly as before.
    let request_id = if is_idempotent_command(&req.command) {
        arg_str(&req.args, "requestId").or_else(|| arg_str(&req.args, "request_id"))
    } else {
        None
    };
    let mut request_reservation = None;
    let mut request_signature_value = None;
    if let Some(id) = &request_id {
        let signature = request_signature_for_caller(&req.command, &req.args, caller.as_ref());
        let (begin, reservation) = ctx.requests.begin_bound_with_reservation(id, &signature);
        request_reservation = reservation;
        request_signature_value = Some(signature);
        match begin {
            // This exact request already completed: replay its stored outcome. Do
            // NOT re-run, re-charge, or re-audit - the side effect is already done.
            BeginOutcome::Duplicate(outcome) => {
                let outcome = if req.command == "agent_followup" {
                    outcome.map_err(|error| {
                        if error.starts_with("request_conflict:") {
                            agent_followup_error("request_conflict", error)
                        } else {
                            error
                        }
                    })
                } else {
                    outcome
                };
                return replay_response(outcome);
            }
            // A prior identical request is still running (a retry that raced the
            // original, Incident B): refuse to spawn a second one. The caller polls
            // get_request_status (or retries) until it resolves.
            BeginOutcome::InFlight => {
                return ControlResponse::err(format!(
                    "request '{id}' is already in flight (a prior identical '{}' has \
                     not finished); it will NOT be double-applied - poll \
                     get_request_status or retry to get its outcome",
                    req.command
                ));
            }
            BeginOutcome::Fresh => {}
            // M1 full fix: the prior reservation for this id was reaped (presumed
            // dead after the reap window). Before re-applying, re-probe reality: if
            // the artifact the original request was creating already exists, the
            // original DID land (or is still landing) - re-applying would DUPLICATE
            // it (the Incident A/B duplicate-maker the reap window only mitigated).
            // Record that reality as this id's outcome so the retry - and every
            // future one - resolves against it. Only when reality shows NOTHING was
            // created do we fall through and apply fresh (the original truly died).
            BeginOutcome::FreshAfterReap => {
                let pre_probe = if req.command == "create_worktree" {
                    authorize_reprobe_create_worktree(
                        ctx,
                        &req.args,
                        caller.as_ref(),
                        trusted_internal,
                    )
                    .map(|_| ())
                } else {
                    Ok(())
                };
                let outcome = match pre_probe {
                    Ok(_) => reprobe_reaped_request(ctx, &req.command, &req.args),
                    Err(error) => Some(Err(error)),
                };
                if let Some(outcome) = outcome {
                    let outcome = ctx.requests.finish_reserved(
                        id,
                        request_reservation.expect("fresh reservation"),
                        request_signature_value
                            .as_deref()
                            .expect("reserved request has a signature"),
                        outcome,
                    );
                    return replay_response(outcome);
                }
            }
        }
    }

    // Phase 1 fleet gate: budget + rate limits for process-changing commands only.
    // Read/Organization tiers never touch the governor.
    let spawn_producing_organization_command = matches!(
        req.command.as_str(),
        "create_worktree" | "add_worktree_workspace"
    );
    let governor_admission =
        if tier == CommandTier::ProcessChanging || spawn_producing_organization_command {
            match governor_gate(
                ctx,
                &req.command,
                &req.args,
                caller.as_ref(),
                trusted_internal,
            ) {
                Ok(admission) => admission,
                Err(refusal) => {
                    // A pre-side-effect gate refusal is not an applied outcome: release the
                    // reservation so a retry after the budget frees can still succeed
                    // (rather than being permanently stuck replaying the refusal).
                    if let Some(id) = &request_id {
                        ctx.requests.cancel_reserved(
                            id,
                            request_reservation.expect("reserved id reaches governor"),
                        );
                    }
                    audit_command(ctx, &req, tier, cap, refusal.code, None);
                    ctx.fanout.emit_event(
                        "control://governor",
                        &json!({
                            "command": req.command.as_str(),
                            "decision": refusal.code,
                            "error": refusal.message.as_str(),
                        }),
                    );
                    return ControlResponse::err(refusal.message);
                }
            }
        } else {
            GovernorAdmission::None
        };

    // A process-changing command must have a durable authorization record before
    // its side effect begins.
    // If the keyed log, authenticated manifest/checkpoint, or integrity state is
    // unavailable, release any idempotency reservation and refuse the command.
    if tier == CommandTier::ProcessChanging {
        if let Err(audit_error) = try_audit_command(ctx, &req, tier, cap, "allowed", None) {
            governor_admission.rollback();
            if let Some(id) = &request_id {
                ctx.requests.cancel_reserved(
                    id,
                    request_reservation.expect("reserved id reaches audit gate"),
                );
            }
            eprintln!(
                "t-hub-audit: refusing process-changing command '{}' because the audit sink is unavailable: {audit_error}",
                req.command
            );
            let message = format!(
                "refused: audit sink unavailable; '{}' was not executed",
                req.command
            );
            ctx.fanout.emit_event(
                "control://governor",
                &json!({
                    "command": req.command.as_str(),
                    "decision": "refused-audit",
                    "error": message.as_str(),
                }),
            );
            return ControlResponse::err(message);
        }
    }

    // Dispatch, then record the outcome under the requestId (if any) so a later
    // retry replays exactly this result. `finish` returns the outcome back. The caller
    // identity resolved above is threaded in so the per-command ACL wiring can enforce
    // the enqueue/access/ack cells against an unforgeable-across-sessions identity.
    let outcome = dispatch_with_caller(
        ctx,
        &req.command,
        &req.args,
        caller.as_ref(),
        trusted_internal,
    );
    let outcome = match &request_id {
        Some(id)
            if outcome.as_ref().is_err_and(|error| {
                error.starts_with(RETRYABLE_ERROR_MARKER)
                    || error.starts_with(&format!(
                        "{AGENT_FOLLOWUP_ERROR_MARKER}persistence_failed\u{1}"
                    ))
            }) =>
        {
            ctx.requests.cancel_reserved(
                id,
                request_reservation.expect("reserved id reaches retryable dispatch outcome"),
            );
            outcome
        }
        Some(id) => ctx.requests.finish_reserved(
            id,
            request_reservation.expect("reserved id reaches dispatch"),
            request_signature_value
                .as_deref()
                .expect("reserved request has a signature"),
            outcome,
        ),
        None => outcome,
    };
    let response = match outcome {
        Ok(value) => ControlResponse::ok(value),
        Err(e) => ControlResponse::err(e),
    };

    // Organization commands remain best-effort and are recorded after dispatch so
    // their downstream outcome is available.
    // Process-changing authorization was durably recorded before dispatch above.
    if tier == CommandTier::Organization {
        let err = if response.ok {
            None
        } else {
            response.error.as_deref()
        };
        audit_command(ctx, &req, tier, cap, "allowed", err);
    }

    response
}

fn is_retired_powder_command(command: &str) -> bool {
    matches!(
        command,
        "dispatch_crew"
            | "list_powder_boards"
            | "bind_project_powder"
            | "project_board_snapshot"
            | "powder_status"
            | "heartbeat_crew_powder"
            | "append_crew_powder_work_log"
            | "read_crew_powder_evidence"
            | "review_crew_powder_criterion"
            | "complete_crew_powder"
    )
}

/// Commands whose side effects require a client `requestId`. This includes the
/// spawn-class commands from the field incidents and typed durable follow-up
/// delivery, where a transport retry must not enqueue a second instruction.
const IDEMPOTENT_COMMANDS: &[&str] = &[
    "spawn_terminal",
    "create_worktree",
    "history_resume",
    "reconcile_cortana",
    "commission_captain",
    "dispatch_crew",
    "start_agent",
    "agent_followup",
];

fn is_idempotent_command(command: &str) -> bool {
    IDEMPOTENT_COMMANDS.contains(&command)
}

fn request_signature(command: &str, args: &Value) -> String {
    let normalized = json!({ "command": command, "args": args });
    format!("{:x}", sha2::Sha256::digest(normalized.to_string()))
}

fn request_signature_for_caller(
    command: &str,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
) -> String {
    if command != "agent_followup" {
        return request_signature(command, args);
    }
    let normalized = json!({
        "command": command,
        "args": args,
        "caller": caller.map(|identity| json!({
            "identityId": identity.session_id,
            "tile": identity.tile,
            "shipSlug": identity.ship_slug,
        })),
    });
    format!("{:x}", sha2::Sha256::digest(normalized.to_string()))
}

/// M1 full fix: when an InFlight reservation was REAPED (presumed dead after the
/// reap window) and the same `requestId` is retried, probe REALITY for the artifact
/// the original command was creating BEFORE allowing a re-apply. Returns:
///   - `Some(outcome)` — the artifact already exists, so the original DID land; the
///     caller records this as the id's outcome and replays it instead of re-applying
///     (which would duplicate). The outcome is a success payload tagged
///     `reprobedAfterReap: true` so an observer sees the retry resolved against
///     reality, not a fresh apply.
///   - `None` — reality shows nothing was created (the original truly died before it
///     applied), OR this command has no probe-able artifact, so the caller proceeds
///     to apply fresh (the prior, mitigation-only behavior).
///
/// Probe-ability is per command:
///   - `create_worktree` — the target `worktreePath` is CALLER-supplied and
///     deterministic, so `git worktree list` for `repoRoot` is an exact reality
///     check. This is the M1 incident (a slow `git worktree add` on the
///     OneDrive-backed store reaped mid-flight, then re-applied → duplicate).
///   - `spawn_terminal` — the tmux session name is SERVER-minted (a fresh uuid per
///     apply), so a retry carries no identifier to probe by; there is nothing to
///     resolve against and we return `None`. The reap window (default 600s, well
///     above any real spawn) remains its guard - a spawn that hung that long is
///     genuinely dead, so applying fresh is correct.
fn reprobe_reaped_request(
    ctx: &ControlContext,
    command: &str,
    args: &Value,
) -> Option<Result<Value, String>> {
    match command {
        "create_worktree" => {
            let repo_root = arg_str(args, "repoRoot").or_else(|| arg_str(args, "repo_root"))?;
            let worktree_path =
                arg_str(args, "worktreePath").or_else(|| arg_str(args, "worktree_path"))?;
            // Loopback vs remote path scoping mirrors `create_worktree`: for a remote
            // peer the git call there ran against the SCOPED path, so probe the same
            // one (an out-of-scope path can't have been created, so scoping-failure =
            // not created = None, which correctly proceeds to a fresh, re-checked apply).
            let (repo_root, worktree_path) = if ctx.peer_is_loopback {
                (repo_root, worktree_path)
            } else {
                let roots = files::remote_file_roots();
                (
                    files::scoped_create_path(&repo_root, true, roots)
                        .ok()?
                        .to_string_lossy()
                        .into_owned(),
                    files::scoped_create_path(&worktree_path, true, roots)
                        .ok()?
                        .to_string_lossy()
                        .into_owned(),
                )
            };
            // Does the worktree already exist for this repo? Compare canonicalized
            // paths so a trailing slash / `.`-segment / symlinked ancestor can't make
            // an existing worktree read as absent (which would wrongly re-apply and
            // duplicate). A git failure (repo unreadable) yields an empty list ⇒ None
            // ⇒ proceed to a fresh apply, which re-runs the real git check anyway.
            if let Err(error) =
                require_registered_git_capability(ctx, "create_worktree", &repo_root)
            {
                return Some(Err(error));
            }
            let want = std::fs::canonicalize(&worktree_path)
                .unwrap_or_else(|_| std::path::PathBuf::from(&worktree_path));
            let exists = git::worktree_list(&repo_root)
                .unwrap_or_default()
                .into_iter()
                .any(|wt| {
                    std::fs::canonicalize(&wt.path)
                        .unwrap_or_else(|_| std::path::PathBuf::from(&wt.path))
                        == want
                });
            if exists {
                Some(Ok(json!({
                    "accepted": "create_worktree",
                    "worktreePath": worktree_path,
                    "alreadyCreated": true,
                    "reprobedAfterReap": true,
                    "note": "the original create_worktree for this requestId was reaped as \
                             stale, but the worktree already exists on disk - resolved \
                             against reality instead of re-creating it (which would \
                             duplicate). Refresh the terminal list to adopt its tile.",
                })))
            } else {
                None
            }
        }
        "commission_captain" => {
            let project_id = arg_str(args, "projectId").or_else(|| arg_str(args, "project_id"))?;
            let project = ctx
                .captains
                .projects()
                .into_iter()
                .find(|project| project.project_id == project_id)?;
            let ship_slug = arg_str(args, "shipSlug")
                .or_else(|| arg_str(args, "ship_slug"))
                .map(|value| slugify_ship(&value))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| slugify_ship(&project.name));
            match existing_project_captain(ctx, &project_id, &ship_slug) {
                Ok(Some(captain)) => Some(Ok(commissioned_response(captain, project, true))),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            }
        }
        // Server-minted artifact id (see doc comment): nothing in args to probe by.
        _ => None,
    }
}

fn authorize_reprobe_create_worktree(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<String, String> {
    let repo_root = arg_str(args, "repoRoot")
        .or_else(|| arg_str(args, "repo_root"))
        .ok_or("create_worktree requires a 'repoRoot' argument")?;
    let worktree_path = arg_str(args, "worktreePath")
        .or_else(|| arg_str(args, "worktree_path"))
        .ok_or("create_worktree requires a 'worktreePath' argument")?;
    if !ctx.peer_is_loopback {
        let roots = files::remote_file_roots();
        files::scoped_create_path(&repo_root, true, roots)?;
        files::scoped_create_path(&worktree_path, true, roots)?;
    }
    let startup_command =
        arg_str(args, "startupCommand").or_else(|| arg_str(args, "startup_command"));
    let spawned_by = arg_str(args, "spawnedBy").or_else(|| arg_str(args, "spawned_by"));
    authorize_worktree_maintenance(
        ctx,
        caller,
        trusted_internal,
        args,
        &repo_root,
        &worktree_path,
        startup_command.as_deref(),
        spawned_by.as_deref(),
    )
    .map(|_| repo_root)
}

/// Build the response for a replayed (idempotent-duplicate) request. The stored
/// outcome is returned verbatim so a retrying caller transparently receives the
/// original result; when it is a JSON object we tag it `idempotentReplay: true` so
/// observers can see the retry resolved to the prior apply rather than a new one.
fn replay_response(outcome: Result<Value, String>) -> ControlResponse {
    match outcome {
        Ok(mut value) => {
            if let Value::Object(map) = &mut value {
                map.insert("idempotentReplay".to_string(), Value::Bool(true));
            }
            ControlResponse::ok(value)
        }
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
/// `rebind_control` handler (relay-wedge self-heal, cause 2 of the control-socket
/// wedge; see PR #49 for the two-cause analysis). Binds a FRESH loopback port,
/// atomically rewrites `control.json`, spawns a serve loop on the new port, then
/// retires the old listener. Rate-limited to one rebind per [`REBIND_MIN_INTERVAL`].
///
/// TOKENS KEPT (not rotated): a rebind is a transport recovery, not a security
/// event. Rotating would force every in-flight client - and the app's OWN webview,
/// which authenticates to this socket with the published token - to re-read before
/// its next call, WIDENING the outage the heal exists to close. The addr is the only
/// thing that must change to escape the wedged relay flow.
///
/// EXISTING CONNECTIONS survive: retiring the old listener only stops it ACCEPTING;
/// already-accepted handler threads (including this one, still writing its response)
/// own independent sockets and run to completion. The app's own event subscribers
/// reconnect through the post-#49 forwarder (exponential backoff) and re-subscribe on
/// the fresh port after they re-read `control.json`.
///
/// The `rebind` lock is intentionally held across the bind + spawn + file write: it
/// serializes concurrent rebinds (two racing heals must not both bind a port) and is
/// contended ONLY by other `rebind_control` calls, never by the hot request path, so
/// it cannot re-introduce the #49 serve-path stall.
fn rebind_control(ctx: &ControlContext) -> Result<Value, String> {
    let mut inner = ctx.rebind.lock();

    // Rate limit: refuse a too-soon rebind with the remaining cooldown so a flapping
    // client cannot churn the port.
    if let Some(last) = inner.last_rebind {
        let elapsed = last.elapsed();
        if elapsed < ctx.rebind.min_interval {
            let remaining = (ctx.rebind.min_interval - elapsed).as_secs() + 1;
            return Err(format!(
                "rebind_control refused: rate-limited, retry in ~{remaining}s (min interval \
                 {}s between rebinds)",
                ctx.rebind.min_interval.as_secs()
            ));
        }
    }

    // Bind a fresh port FIRST - on failure nothing has changed.
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("rebind_control: failed to bind a fresh port: {e}"))?;
    let new_addr = listener
        .local_addr()
        .map_err(|e| format!("rebind_control: bound but could not read fresh addr: {e}"))?
        .to_string();
    let old_addr = ctx.addr.clone();
    // Reserve this listener's immutable generation before its serve loop starts.
    // Failed publication intentionally leaves a gap; no overlapping listener can
    // ever claim another listener's generation.
    let listener_generation = ctx.listener_generation.fetch_add(1, Ordering::AcqRel) + 1;

    // New serve loop context: the SAME shared state (fanout, registries, governor,
    // ...), only `addr` changes so spawns injected AFTER the rebind carry it.
    let mut new_ctx = ctx.clone();
    new_ctx.addr = new_addr.clone();
    new_ctx.bound_listener_generation = listener_generation;
    let new_stop = Arc::new(AtomicBool::new(false));
    let serve_stop = new_stop.clone();

    // Spawn the new serve loop BEFORE publishing the addr, so `control.json` never
    // names a port nobody is accepting on.
    std::thread::Builder::new()
        .name("t-hub-control".into())
        .spawn(move || serve(listener, new_ctx, serve_stop))
        .map_err(|e| format!("rebind_control: failed to spawn serve loop: {e}"))?;

    // Publish the fresh addr atomically (temp+rename), KEEPING tokens.
    let handshake = ControlHandshake {
        addr: new_addr.clone(),
        token: ctx.read_token.clone(),
        read_token: ctx.read_token.clone(),
        pid: std::process::id(),
        protocol_version: PROTOCOL_VERSION,
        instance_id: ctx.listener_instance_id.clone(),
        listener_generation,
        published_at: now_ms(),
        local_control_token: ctx.token.clone(),
        local_host_token: ctx.host_token.clone(),
    };
    if let Err(e) = write_handshake(&handshake) {
        // Roll back to a fully-consistent old state: retire the just-spawned listener
        // (so we never leak it) and leave the old listener + old control.json intact.
        new_stop.store(true, Ordering::Release);
        wake_accept(&new_addr);
        return Err(format!(
            "rebind_control: bound fresh port {new_addr} but failed to publish control.json \
             (old listener kept live): {e}"
        ));
    }

    // Retire the old listener: flag it, then wake its blocked `accept` so it exits and
    // frees the old port promptly.
    if let Some(old_stop) = inner.current_stop.replace(new_stop) {
        old_stop.store(true, Ordering::Release);
        wake_accept(&old_addr);
    }
    inner.last_rebind = Some(Instant::now());

    eprintln!(
        "t-hub-control: rebind_control moved the listener {old_addr} -> {new_addr} \
         (relay-wedge self-heal)"
    );
    Ok(json!({
        "rebound": true,
        "addr": new_addr,
        "previousAddr": old_addr,
        "tokensRotated": false,
        "note": "control.json rewritten with the fresh addr (tokens kept); re-read it and \
                 resume on the new port",
    }))
}

/// The 3-arg dispatcher used by in-file unit tests as the exact trusted in-process
/// host (`None` identity plus trusted host provenance). The authenticated production path calls
/// [`dispatch_with_caller`] directly with the resolved caller. Kept so the ~90 existing
/// dispatch tests read unchanged; the Phase-3 ACL tests call `dispatch_with_caller`.
#[cfg(test)]
fn dispatch(ctx: &ControlContext, command: &str, args: &Value) -> Result<Value, String> {
    dispatch_with_caller(ctx, command, args, None, true)
}

fn dispatch_with_caller(
    ctx: &ControlContext,
    command: &str,
    args: &Value,
    // Comms-plane Phase 3: the resolved per-session caller and exact trusted-host
    // provenance are separate inputs. The ACL wiring consumes both.
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    enforce_delegated_admin_command(ctx, caller, command)?;
    if matches!(
        command,
        "list_tabs"
            | "list_captains"
            | "new_tab"
            | "close_tab"
            | "rename_tab"
            | "focus_tab"
            | "focus_session"
            | "move_tile"
            | "report_workspace_tabs"
            | "spawn_terminal"
            | "history_resume"
            | "start_agent"
            | "reconcile_cortana"
            | "commission_captain"
            | "attach_captain"
            | "create_worktree"
    ) {
        ctx.tabs
            .require_authoritative_startup()
            .map_err(retryable_error)?;
    }
    match command {
        // ---- Read tier (PRD §11.2: allowed) --------------------------------
        "list_terminals" => list_terminals(),
        "get_status" => get_status(ctx, args),
        // Idempotency (ask #1): "what happened to request X?" - resolves an
        // ambiguous spawn-class response leg without guessing (Read tier).
        "get_request_status" => get_request_status(ctx, args, caller, trusted_internal),
        "wait_for_status" => wait_for_status(ctx, args),
        "supervision_tree" => supervision_tree(ctx, args),
        "supervision_session_ids" => supervision_session_ids(ctx),
        "wsl_health" => wsl_health(ctx),
        "recent_sessions" => recent_sessions(),
        "invalidate_recent_cache" => invalidate_recent_cache(),
        "history_list" => history_list(ctx, args, caller, trusted_internal),
        "preview_discover" | "preview_status" => {
            preview_control(ctx, command, args, caller, trusted_internal)
        }
        "invalidate_history_cache" => invalidate_history_cache(ctx),
        // "Is the general dictating?" - reads the Scribe voice-gate status file
        // (fails open to listening=false when it can't tell). Lets agents defer
        // a spoken cue / a barge-in while the user is talking.
        "scribe_status" => scribe_status(),
        "claude_usage" => claude_usage(),
        "codex_usage" => codex_usage(),
        "host_metrics" => host_metrics(ctx),
        "git_info" => git_info(ctx, args),
        "index_project" => index_project(ctx, args),
        "search_files" => search_files(ctx, args),
        "list_dir" => list_dir(ctx, args),
        "read_text_file" => read_text_file(ctx, args),
        "list_tabs" => list_tabs(ctx),
        "list_captains" => list_captains(ctx),
        "list_projects" => list_projects(ctx),
        "list_agents" => list_agents(ctx, args, caller, trusted_internal),
        "dispatch_preflight" => dispatch_preflight(ctx, args, caller, trusted_internal),
        "get_agent" => get_agent(ctx, args, caller, trusted_internal),
        "agent_events" => agent_events(ctx, args, caller, trusted_internal),
        "captain_bootstrap" => captain_bootstrap(ctx, args),
        "cortana_bootstrap" => cortana_bootstrap(ctx, args, caller),
        "list_fleet_watches" => list_fleet_watches(ctx),
        "read_terminal" | "capture_pane" => read_terminal(ctx, args, caller, trusted_internal),
        // Comms-plane Phase 2: the durable inbox's read-tier surface. `inbox_ack` is
        // the recipient's `delivered -> processed` intake confirmation (the receipt
        // state machine's ack channel, §2.4 M2); `inbox_status` is the per-recipient
        // observability snapshot (§2.8). Read-tier: an ack only retires the
        // recipient's own already-delivered message (idempotent, never a re-write),
        // and status is counts-only. Phase 3 adds the ownership ACL that gates a
        // cross-session ack; Phase 2 does not authorize (no ACLs yet).
        "inbox_ack" => inbox_ack(ctx, args, caller, trusted_internal),
        "inbox_status" => inbox_status(ctx, args),
        // Comms-plane Phase 3: the agent-to-agent plane SEND, gated by the settled
        // matrix message rows (`can_message`) + the EMERGENCY-flag authority
        // (`can_flag_emergency`). Read base tier so an identified CREW (least-privilege
        // read token) can send up to its captain; the handler REQUIRES a resolved
        // session identity (or proven in-process host) - a token with no session cannot
        // enqueue.
        "plane_send" => plane_send(ctx, args, caller, trusted_internal),
        // The resolve-and-verify GATE a captain's money/publish gate consults
        // (`general_authorization_present`): read-only, Read tier.
        "check_authorization" => check_authorization(ctx, args),
        "list_admin_grants" => list_admin_grants(ctx, args, caller, trusted_internal),

        // ---- Organization tier (PRD §11.2: allowed, audited) ---------------
        // These are surfaced by the MCP server and accepted here, but the
        // process-changing subset (spawn) is gated behind the confirmation flag
        // in the MCP tool description AND refused here unless explicitly enabled,
        // so the dev-box proof never spawns/kills anything by accident.
        "focus_session" => {
            let session_id = arg_str(args, "sessionId")
                .or_else(|| arg_str(args, "session_id"))
                .ok_or("focus_session requires a 'sessionId' argument")?;
            enforce_session_access(ctx, caller, trusted_internal, &session_id)?;
            organization_apply(ctx, "focus_session", args)
        }
        "history_focus" => history_focus(ctx, args, caller, trusted_internal),
        "preview_select" | "preview_refresh" | "preview_open" => {
            preview_control(ctx, command, args, caller, trusted_internal)
        }
        // Headless-org: the organization mutations below apply to the SERVER tab
        // registry first (authoritative; hard error on an invalid target) and then
        // forward the registry snapshot for the UI to render from.
        "move_tile" => move_tile(ctx, args, caller, trusted_internal),
        "rename_tab" => rename_tab(ctx, args, caller, trusted_internal),
        // new_tab mints the tab id CORE-side so it can return it (TASK C:
        // addressable tabs) and forwards that id for the frontend to adopt.
        "new_tab" => new_tab(ctx, args, caller, trusted_internal),
        "close_tab" | "remove_tab" => close_tab(ctx, args, caller, trusted_internal),
        "focus_tab" => focus_tab(ctx, args, caller, trusted_internal),
        "open_file" => open_file(ctx, args, caller, trusted_internal),
        // WS-4 git worktrees: create runs git here then forwards the tab+spawn to
        // the UI; remove forwards to the UI so it detaches live tiles BEFORE git
        // tears the dir down (no orphaned processes). list (T-B) is the read-only
        // socket twin of the `git_worktree_list` Tauri command, for a socket UI's
        // worktree list/re-open/remove flows.
        "create_worktree" => create_worktree(ctx, args, caller, trusted_internal),
        "remove_worktree" => remove_worktree(ctx, args, caller, trusted_internal),
        "list_worktrees" | "git_worktree_list" => list_worktrees(ctx, args),
        // Recent list × made durable: move a project's transcripts out of the
        // scanned catalog into projects-archive (reversible). App-initiated from
        // the sidebar; filesystem-mutating like the worktree ops above.
        "archive_recent_project" => archive_recent_project(ctx, args, caller, trusted_internal),
        "register_project" => register_project(ctx, args, caller, trusted_internal),
        "initialize_git" => initialize_git(ctx, args, caller, trusted_internal),
        // Captain-chat phase 2: captaincy is a SERVER mutation (audited) - the
        // UI's pin action and an MCP captain's self-registration both land here,
        // and every mutation forwards the authoritative captains snapshot.
        "claim_captain" => claim_captain(ctx, args, caller, trusted_internal),
        "report_workspace_tabs" => report_workspace_tabs(ctx, args, caller, trusted_internal),
        "release_captain" => release_captain(ctx, args, caller, trusted_internal),
        "rename_captain" => rename_captain(ctx, args, caller, trusted_internal),
        "captain_checkpoint" => captain_checkpoint(ctx, args, caller, trusted_internal),
        "agent_checkpoint" => agent_checkpoint(ctx, args, caller, trusted_internal),
        "agent_followup" => agent_followup(ctx, args, caller, trusted_internal),
        "record_agent_delivery" => record_agent_delivery(ctx, args, caller, trusted_internal),
        "appoint_admin" => appoint_admin(ctx, args, caller, trusted_internal),
        "approve_admin_action" => approve_admin_action(ctx, args, caller, trusted_internal),
        "execute_admin_operation" => execute_admin_operation(ctx, args, caller, trusted_internal),
        "revoke_admin" => revoke_admin(ctx, args, caller, trusted_internal),
        // Orchestrator wake: arm/disarm a server-side push that re-invokes the
        // orchestrator's loop when a watched session goes idle / needs-input /
        // completes. Organization tier (audited); the wake itself injects via the
        // same backend send_text path the ProcessChanging tier gates.
        "watch_fleet" => watch_fleet(ctx, args, caller, trusted_internal),
        "unwatch_fleet" => unwatch_fleet(ctx, args, caller, trusted_internal),
        // Relay-wedge self-heal (cause 2): move the listener to a fresh port +
        // rewrite control.json so a WSL client stuck behind the mirrored-loopback
        // relay wedge recovers without an app restart. WRITE-token gated
        // (Organization tier - a read-only token cannot churn the port) and
        // rate-limited. Triggered by a wedged WSL client over the Windows-side
        // powershell bridge, the one path that reaches the app mid-wedge.
        "rebind_control" => rebind_control(ctx),

        // ---- Process-changing tier (PRD §11.2: confirmation required) ------
        // `spawn_terminal` is confirmation-gated (its MCP description carries the
        // CONFIRMATION REQUIRED contract), but functional: it routes through the
        // SAME ApplySink adoption path create_worktree uses, so the frontend spawns
        // a real tile + live session it OWNS (no untracked tmux session). Refused
        // only when no UI is connected to adopt the tile. The session-targeted
        // process actions — typing into / interrupting / closing an *existing*
        // session — execute directly against tmux (they only act on a `th_*`
        // session the app already owns).
        "spawn_terminal" => spawn_terminal(ctx, args, caller, trusted_internal),
        "history_resume" => history_resume(ctx, args, caller, trusted_internal),
        "preview_start" | "preview_stop" | "preview_restart" => {
            preview_control(ctx, command, args, caller, trusted_internal)
        }
        "start_agent" => start_agent(ctx, args, caller, trusted_internal),
        "reconcile_cortana" => reconcile_cortana(ctx, args, trusted_internal),
        "commission_captain" => commission_captain(ctx, args, caller, trusted_internal),
        "attach_captain" => attach_captain(ctx, args, caller, trusted_internal),
        // comms-plane Phase 1: `send_text`/`send_keys` are DEMOTED to audited
        // break-glass. They still execute (H2: demote, not deny) but every use is
        // marked loudly, because the primary automation path is now the plane
        // (`plane::deliver_tmux` for the wake, `deliver_agent_input` for in-app
        // automation), not these direct writers. `th send` reaches `send_text`, so
        // it inherits the same marker.
        "send_text" => {
            // Phase 3 (§2.6 H3): break-glass STILL rides the cross-ship isolation ACL -
            // an identified session may only write a pane on its OWN ship. The host
            // with valid in-process host provenance is admitted.
            if let Some(tile) = arg_str(args, "sessionId").or_else(|| arg_str(args, "session_id")) {
                enforce_session_access(ctx, caller, trusted_internal, &tile)?;
            }
            mark_break_glass(ctx, "send_text", args);
            send_text(args)
        }
        "send_keys" => {
            if let Some(tile) = arg_str(args, "sessionId").or_else(|| arg_str(args, "session_id")) {
                enforce_session_access(ctx, caller, trusted_internal, &tile)?;
            }
            mark_break_glass(ctx, "send_keys", args);
            send_keys(args)
        }
        "close_terminal" => close_terminal_authorized(ctx, args, caller, trusted_internal),
        "cleanup_worktree_artifacts" => {
            cleanup_worktree_artifacts(ctx, args, caller, trusted_internal)
        }
        // Comms-plane Phase 3: the ABORT/interrupt-subordinate primitive (§2.7 R-H3). A
        // preempt CONTROL signal (an Escape interrupt), NOT a queued input message, so it
        // cannot be typed over or corrupt a draft. Gated by `can_abort` (Cortana->captain,
        // captain->own crew, general->anyone; cross-ship/sibling DENIED; crew never).
        "abort_session" => abort_session(ctx, args, caller, trusted_internal),
        // Comms-plane Phase 3: record a durable general-authorization artifact (the
        // delegation-gate carrier, M1). Gated by `can_originate_authorization` (only the
        // general ORIGINATES; Cortana may relay by reference, never originate).
        "authorize" => authorize(ctx, args, caller),
        // Comms-plane Phase 3: operate-fleet-infra (§2.7 R-L2) - the plane's own
        // administrative ops (queue purge/flush), gated to the apex fleet-infra owner
        // (`can_operate_fleet_infra`).
        "plane_admin" => plane_admin(ctx, args, caller, trusted_internal),

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

fn preview_control(
    ctx: &ControlContext,
    command: &str,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let top_scope = args
        .get("scope")
        .map(|scope| serde_json::from_value::<crate::preview::model::PreviewScope>(scope.clone()))
        .transpose()
        .map_err(|error| format!("{command} has an invalid scope: {error}"))?;
    let target_scope = args
        .pointer("/target/scope")
        .map(|scope| serde_json::from_value::<crate::preview::model::PreviewScope>(scope.clone()))
        .transpose()
        .map_err(|error| format!("{command} has an invalid target scope: {error}"))?;
    if top_scope
        .as_ref()
        .zip(target_scope.as_ref())
        .is_some_and(|(top, target)| top != target)
    {
        return Err(format!(
            "{command} top-level and target scopes must match exactly"
        ));
    }
    let requested_scope = top_scope.as_ref().or(target_scope.as_ref());
    let requested_root = ["rootPath", "repoRoot", "repo_root"]
        .into_iter()
        .filter_map(|field| arg_str(args, field).map(|value| (field, value)))
        .collect::<Vec<_>>();
    let requested_project_id = requested_scope.map(|scope| scope.project_id.as_str());
    let projects = ctx.captains.projects();
    let project = if let Some(project_id) = requested_project_id {
        let matches = projects
            .iter()
            .filter(|project| project.project_id == project_id)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [project] => *project,
            [] => {
                return Err(format!(
                    "Preview scope names unknown projectId '{project_id}'"
                ))
            }
            _ => return Err(format!("Preview projectId '{project_id}' is ambiguous")),
        }
    } else if command == "preview_discover" {
        let root = requested_project_root(args, command)?;
        let matches = projects
            .iter()
            .filter(|project| project_identity_matches(project, &root))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [project] => *project,
            [] => return Err("Preview discovery requires a registered Project root".into()),
            _ => return Err("Preview discovery Project root is ambiguous".into()),
        }
    } else {
        return Err(format!("{command} requires a typed scope with a projectId"));
    };
    let root_authority = preview_root_authority(project)?;
    let authoritative_root = root_authority.posix_identity.as_str();
    for (field, supplied) in &requested_root {
        let supplied = canonical_project_identity(supplied)?;
        if supplied != authoritative_root {
            return Err(format!(
                "{command} {field} does not match registered Project '{}'",
                project.project_id
            ));
        }
    }
    let caller_authority = enforce_preview_project_authority(
        ctx,
        caller,
        trusted_internal,
        &project.project_id,
        command,
    )?;
    if let Some(workspace_id) = requested_scope.and_then(|scope| scope.workspace_id.as_deref()) {
        enforce_preview_workspace_authority(
            ctx,
            caller,
            trusted_internal,
            &project.project_id,
            workspace_id,
            command,
            caller_authority.as_ref(),
        )?;
    }

    let mut authorized = args
        .as_object()
        .cloned()
        .ok_or_else(|| format!("{command} arguments must be an object"))?;
    authorized.remove("repoRoot");
    authorized.remove("repo_root");
    if !requested_root.is_empty() {
        authorized.insert(
            "rootPath".into(),
            Value::String(authoritative_root.to_string()),
        );
    }
    (ctx.preview_control)(command, &Value::Object(authorized), &root_authority)
}

fn preview_root_authority(project: &ProjectRecord) -> Result<PreviewRootAuthority, String> {
    preview_root_authority_with(project, files::to_host_path)
}

fn preview_root_authority_with(
    project: &ProjectRecord,
    to_host_path: impl FnOnce(&str) -> PathBuf,
) -> Result<PreviewRootAuthority, String> {
    let registered = project
        .root_path
        .as_deref()
        .unwrap_or(project.repo_root.as_str());
    let posix_identity = canonical_project_identity(registered)?;
    Ok(PreviewRootAuthority {
        host_open_path: to_host_path(&posix_identity),
        posix_identity,
    })
}

fn enforce_preview_project_authority(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    project_id: &str,
    command: &str,
) -> Result<Option<FleetWorkspaceOwner>, String> {
    if caller_is_apex(caller, trusted_internal) {
        return Ok(None);
    }
    let caller = caller.ok_or_else(|| format!("acl: '{command}' requires a Fleet identity"))?;
    let terminal_id = caller
        .tile
        .as_deref()
        .ok_or_else(|| format!("acl: '{command}' caller has no terminal binding"))?;
    let matches = ctx
        .captains
        .snapshot()
        .captains
        .into_iter()
        .filter(|captain| {
            captain.role == FleetRole::Captain
                && captain.state == ClaimState::Active
                && captain.project_id.as_deref() == Some(project_id)
                && captain.terminal_id.as_deref() == Some(terminal_id)
                && caller.ship_slug.as_deref() == Some(captain.ship_slug.as_str())
        })
        .collect::<Vec<_>>();
    if caller.fleet_role != Some(FleetRole::Captain) || matches.len() != 1 {
        return Err(format!(
            "acl: '{command}' requires General/Cortana or the owning Project Captain"
        ));
    }
    let captain = &matches[0];
    Ok(Some(FleetWorkspaceOwner {
        project_id: project_id.to_string(),
        assignment_id: captain.assignment_id.clone(),
        ship_slug: captain.ship_slug.clone(),
    }))
}

fn enforce_preview_workspace_authority(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    project_id: &str,
    workspace_id: &str,
    command: &str,
    caller_authority: Option<&FleetWorkspaceOwner>,
) -> Result<(), String> {
    let matches = ctx
        .captains
        .snapshot()
        .workspaces
        .into_iter()
        .filter(|workspace| workspace.id == workspace_id)
        .collect::<Vec<_>>();
    let workspace = match matches.as_slice() {
        [workspace] => workspace,
        [] => {
            return Err(format!(
                "{command} names unknown durable workspaceId '{workspace_id}'"
            ))
        }
        _ => {
            return Err(format!(
                "{command} durable workspaceId '{workspace_id}' is ambiguous"
            ))
        }
    };
    let owner = workspace.owner.as_ref().ok_or_else(|| {
        format!("{command} workspaceId '{workspace_id}' has no durable Project owner")
    })?;
    if owner.project_id != project_id {
        return Err(format!(
            "{command} workspaceId '{workspace_id}' belongs to another Project"
        ));
    }
    if caller_is_apex(caller, trusted_internal) && caller_authority.is_none() {
        return Ok(());
    }
    if caller_authority == Some(owner) {
        Ok(())
    } else {
        Err(format!(
            "acl: '{command}' workspace belongs to another Captain Assignment"
        ))
    }
}

fn enforce_delegated_admin_command(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
    command: &str,
) -> Result<(), String> {
    let Some(caller) = caller else {
        return Ok(());
    };
    if !has_delegated_admin_history(ctx, &caller.session_id) {
        return Ok(());
    }
    if matches!(
        command,
        "list_agents"
            | "read_terminal"
            | "capture_pane"
            | "create_worktree"
            | "close_terminal"
            | "cleanup_worktree_artifacts"
            | "execute_admin_operation"
            | "list_admin_grants"
    ) {
        return Ok(());
    }
    Err(format!(
        "acl: delegated administrators cannot call '{command}' because it is outside their exact administrative operation grants"
    ))
}

// ---------------------------------------------------------------------------
// Read-tier handlers
// ---------------------------------------------------------------------------

/// `list_terminals`: reconstruct the terminal list from the tmux source of truth
/// on the isolated `t-hub` socket. Mirrors `commands::list_terminals`, minus
/// the in-memory Live/Detached refinement (the control channel does not own the
/// UI's PTY map; everything tmux reports is a live tmux session).
fn list_terminals() -> Result<Value, String> {
    let sessions =
        tmux::list_sessions().map_err(|e| format!("failed to list tmux sessions: {e}"))?;
    // Correlate each session with its pane's live cwd (the same `pane_info`
    // source `commands::list_terminals` uses) so socket clients can map
    // sessions to filesystem paths - `th worktree ls/prune` lease detection
    // depends on it. Best-effort: a pane_info failure just leaves cwd empty.
    let pane_map: std::collections::HashMap<String, (String, String)> = tmux::pane_info()
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.session, (p.command, p.cwd)))
        .collect();
    let terminals: Vec<Value> = sessions
        .iter()
        .filter(|s| s.starts_with("th_"))
        .map(|tmux_session| {
            let id = tmux_session
                .strip_prefix("th_")
                .unwrap_or(tmux_session)
                .to_string();
            let cwd = pane_map
                .get(tmux_session)
                .map(|(_, cwd)| cwd.clone())
                .unwrap_or_default();
            json!({
                "id": id,
                "tmuxSession": tmux_session,
                "title": tmux_session,
                "cwd": cwd,
                // Source-of-truth listing: present as live tmux-backed sessions.
                "state": "live",
            })
        })
        .collect();
    Ok(json!({ "terminals": terminals, "count": terminals.len() }))
}

fn agent_page_limit(args: &Value, command: &str) -> Result<usize, String> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20);
    let limit = usize::try_from(limit).map_err(|_| format!("{command} limit is too large"))?;
    if !(1..=100).contains(&limit) {
        return Err(format!("{command} limit must be between 1 and 100"));
    }
    Ok(limit)
}

fn agent_page_cursor(args: &Value, command: &str) -> Result<usize, String> {
    let cursor = args.get("cursor").and_then(Value::as_str).unwrap_or("0");
    cursor
        .parse::<usize>()
        .map_err(|_| format!("{command} cursor must be a non-negative integer"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentAuthority {
    Apex,
    Captain,
    Agent,
}

fn authorize_agent(
    ctx: &ControlContext,
    agent: &AgentSessionRecord,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    command: &str,
) -> Result<AgentAuthority, String> {
    if caller_is_apex(caller, trusted_internal) {
        return Ok(AgentAuthority::Apex);
    }
    let caller = caller.ok_or_else(|| {
        format!("acl: '{command}' requires the owning Captain or exact agent session")
    })?;
    let snapshot = ctx.captains.snapshot();
    let captain = snapshot
        .captains
        .iter()
        .find(|captain| captain.terminal_id.as_deref() == Some(agent.captain_session_id.as_str()))
        .ok_or_else(|| format!("acl: '{command}' agent ownership is unavailable"))?;
    let same_ship = caller.ship_slug.as_deref() == Some(captain.ship_slug.as_str());
    let owning_captain = same_ship
        && caller.fleet_role == Some(FleetRole::Captain)
        && caller.tile.as_deref() == captain.terminal_id.as_deref()
        && captain.role == FleetRole::Captain
        && captain.state == ClaimState::Active;
    if owning_captain {
        return Ok(AgentAuthority::Captain);
    }
    let exact_agent = same_ship
        && caller.mint_role == crate::identity::Role::Crew
        && caller.fleet_role.is_none()
        && caller.tile.as_deref() == Some(agent.agent_session_id.as_str());
    if exact_agent {
        return Ok(AgentAuthority::Agent);
    }
    Err(format!(
        "acl: '{command}' requires the owning Captain or exact agent session"
    ))
}

fn authorize_agent_filter(
    ctx: &ControlContext,
    captain_session_id: Option<&str>,
    project_id: Option<&str>,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
    command: &str,
    allow_delegated_status: bool,
) -> Result<AgentFilterAuthorization, String> {
    let active_admin_grant = caller.and_then(|caller| {
        ctx.delegated_admin
            .grants_for_actor(&caller.session_id)
            .into_iter()
            .find(|grant| grant.state.is_active())
    });
    if active_admin_grant.is_none() && caller_is_apex(caller, trusted_internal) {
        return Ok(AgentFilterAuthorization::default());
    }
    let caller = caller.ok_or_else(|| {
        format!("acl: '{command}' requires the owning Captain or a fleet supervisor")
    })?;
    if !allow_delegated_status
        && (active_admin_grant.is_some() || caller.fleet_role != Some(FleetRole::Captain))
    {
        return Err(format!(
            "acl: '{command}' requires the owning Captain or a fleet supervisor"
        ));
    }
    let snapshot = ctx.captains.snapshot();
    let owned = snapshot.captains.iter().find(|captain| {
        captain.role == FleetRole::Captain
            && captain.state == ClaimState::Active
            && caller.tile.as_deref() == captain.terminal_id.as_deref()
            && caller.ship_slug.as_deref() == Some(captain.ship_slug.as_str())
            && captain_session_id.is_none_or(|id| captain.terminal_id.as_deref() == Some(id))
            && project_id.is_none_or(|id| captain.project_id.as_deref() == Some(id))
    });
    if active_admin_grant.is_none() {
        if let Some(captain) = owned {
            return Ok(AgentFilterAuthorization {
                caller_ship: Some(captain.ship_slug.clone()),
                delegated_audit: None,
            });
        }
    }
    if !allow_delegated_status {
        return Err(format!(
            "acl: '{command}' requires the owning Captain or a fleet supervisor"
        ));
    }

    let grant = active_admin_grant.ok_or_else(|| {
        format!("acl: '{command}' requires the owning Captain or a delegated administrator")
    })?;
    match grant.role {
        crate::delegated_admin::DelegatedAdminRole::FleetAdmin => {
            let audit = authorize_delegated_admin(
                ctx,
                caller,
                crate::delegated_admin::AdminOperation::BuildCrossCaptainReport,
                crate::delegated_admin::AdminTarget::Fleet,
                crate::delegated_admin::AdminSafeguards::default(),
            )?;
            Ok(AgentFilterAuthorization {
                caller_ship: None,
                delegated_audit: Some(audit),
            })
        }
        crate::delegated_admin::DelegatedAdminRole::ShipAdmin => {
            let matching = snapshot
                .captains
                .iter()
                .filter(|captain| {
                    captain.role == FleetRole::Captain
                        && captain.state == ClaimState::Active
                        && captain_session_id
                            .is_none_or(|id| captain.terminal_id.as_deref() == Some(id))
                        && project_id.is_none_or(|id| captain.project_id.as_deref() == Some(id))
                })
                .collect::<Vec<_>>();
            let captain = match matching.as_slice() {
                [captain] => *captain,
                _ => {
                    return Err(format!(
                        "acl: '{command}' Ship Admin status reads require one exact Captain target"
                    ));
                }
            };
            let captain_identity_id = captain
                .terminal_id
                .as_deref()
                .and_then(|terminal_id| ctx.identity.for_tile(terminal_id))
                .map(|identity| identity.id)
                .unwrap_or_else(|| captain.assignment_id.clone());
            let audit = authorize_delegated_admin(
                ctx,
                caller,
                crate::delegated_admin::AdminOperation::InspectStatus,
                crate::delegated_admin::AdminTarget::Captain {
                    ship_slug: captain.ship_slug.clone(),
                    captain_identity_id,
                },
                crate::delegated_admin::AdminSafeguards::default(),
            )?;
            Ok(AgentFilterAuthorization {
                caller_ship: Some(captain.ship_slug.clone()),
                delegated_audit: Some(audit),
            })
        }
    }
}

#[derive(Debug, Default)]
struct AgentFilterAuthorization {
    caller_ship: Option<String>,
    delegated_audit: Option<crate::delegated_admin::AdminAuditContext>,
}

/// Refresh one durable agent from terminal and provider evidence.
/// Unknown probes are deliberately non-mutating so a transient WSL or tmux
/// failure cannot turn a live agent into an exited one.
fn reconcile_agent_runtime(ctx: &ControlContext, agent_session_id: &str) {
    let snapshot = ctx.captains.snapshot();
    let Some(agent) = snapshot
        .agent_sessions
        .iter()
        .find(|agent| agent.agent_session_id == agent_session_id)
        .cloned()
    else {
        return;
    };
    let runtime_state = match tmux::session_liveness(&tmux_target(&agent.agent_session_id)) {
        tmux::SessionLiveness::Gone => RuntimeState::Exited,
        tmux::SessionLiveness::Unknown => return,
        tmux::SessionLiveness::Alive => {
            match tmux::harness_liveness(&tmux_target(&agent.agent_session_id), &agent.provider) {
                tmux::SessionLiveness::Alive => RuntimeState::Running,
                tmux::SessionLiveness::Gone => RuntimeState::Idle,
                tmux::SessionLiveness::Unknown => return,
            }
        }
    };
    let provider_conversation_id = if matches!(runtime_state, RuntimeState::Running) {
        trusted_provider_session_id(ctx, &agent.agent_session_id, &agent.provider, None)
            .ok()
            .flatten()
    } else {
        None
    };
    let _ = ctx.captains.reconcile_agent_runtime(
        &agent.agent_session_id,
        runtime_state,
        provider_conversation_id,
    );
}

mod handlers_agents;
use handlers_agents::*;

mod handlers_status;
use handlers_status::*;

// History handlers live in the `handlers_history` submodule.
mod handlers_history;
use handlers_history::*;

/// `archive_recent_project`: the Recent list's × made durable. Moves the project
/// at `args.cwd` out of `~/.claude/projects` into `projects-archive` (reversible)
/// so the dismissed project stops appearing in Recent and stops costing scan time.
/// Returns `true` on success.
fn archive_recent_project(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let cwd = args.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    if cwd.is_empty() {
        return Err("archive_recent_project requires a 'cwd'".into());
    }
    enforce_project_path_authority(ctx, caller, trusted_internal, cwd, "archive_recent_project")?;
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

// File/git read handlers live in the `handlers_files` submodule (below the
// dispatch match that routes to them).
mod handlers_files;
use handlers_files::*;

/// `list_captains`: the claimed captains from the CORE captains registry
/// (captain-chat phase 2), each `{shipSlug, captainSessionId, workspaceTabIds,
/// crew}` plus the registry revision - the same versioned-snapshot contract as
/// `list_tabs`. This is the ONE source of truth the UI's sidebar/overlay and an
/// MCP captain both read; ship files remain the captain-side roster only.
fn list_captains(ctx: &ControlContext) -> Result<Value, String> {
    ctx.tabs
        .require_authoritative_startup()
        .map_err(retryable_error)?;
    let snap = ctx.captains.snapshot();
    let admin_grants = ctx.delegated_admin.active_grants();
    let mut captains = serde_json::to_value(&snap.captains).map_err(|e| e.to_string())?;
    if let Some(items) = captains.as_array_mut() {
        for captain in items {
            if let Some(crew) = captain.get_mut("crew").and_then(Value::as_array_mut) {
                for member in crew {
                    if let Some(object) = member.as_object_mut() {
                        object.remove("powderWork");
                        let delegated_grant = object
                            .get("terminalId")
                            .and_then(Value::as_str)
                            .and_then(|terminal_id| ctx.identity.for_tile(terminal_id))
                            .and_then(|identity| {
                                admin_grants
                                    .iter()
                                    .find(|grant| grant.actor_identity_id == identity.id)
                            });
                        if let Some(grant) = delegated_grant {
                            object.insert("delegatedRole".into(), json!(grant.role.label()));
                            object.insert(
                                "delegatedGrantGeneration".into(),
                                json!(grant.grant_generation),
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(json!({
        "captains": captains,
        "count": snap.captains.len(),
        "seq": snap.seq,
    }))
}

/// Return the durable registered-project catalog. This is separate from Recent,
/// which is activity-derived and may include unregistered scratch directories.
fn list_projects(ctx: &ControlContext) -> Result<Value, String> {
    let snap = ctx.captains.snapshot();
    let mut projects = serde_json::to_value(&snap.projects).map_err(|e| e.to_string())?;
    if let Some(items) = projects.as_array_mut() {
        for project in items {
            if let Some(object) = project.as_object_mut() {
                object.remove("powder");
            }
        }
    }
    let (wsl_home, wsl_home_error) = match files::user_home_path() {
        Ok(home) => (Some(home), None),
        Err(error) => (None, Some(error)),
    };
    Ok(json!({
        "projects": projects,
        "count": snap.projects.len(),
        "seq": snap.seq,
        "wslHome": wsl_home,
        "wslHomeError": wsl_home_error,
    }))
}

const GIT_INIT_INTENT_VERSION: u32 = 1;
const GIT_INIT_MARKER_FILE: &str = "t-hub-git-init-marker.json";

#[cfg(test)]
thread_local! {
    static GIT_INIT_FAULT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn set_git_init_fault(boundary: &str) {
    GIT_INIT_FAULT.with(|fault| *fault.borrow_mut() = Some(boundary.to_string()));
}

#[cfg(test)]
fn clear_git_init_fault() {
    GIT_INIT_FAULT.with(|fault| *fault.borrow_mut() = None);
}

fn git_init_fault(boundary: &str) -> Result<(), String> {
    #[cfg(test)]
    {
        let matched = GIT_INIT_FAULT.with(|fault| {
            fault
                .borrow()
                .as_deref()
                .is_some_and(|configured| configured == boundary)
        });
        if matched {
            return Err(format!("injected Git initialization fault at {boundary}"));
        }
    }
    let _ = boundary;
    Ok(())
}

fn git_init_recovery_error(intent: &GitInitIntent, message: impl std::fmt::Display) -> String {
    format!(
        "git_init_recovery code=git_init_recovery operation={} phase={} message={}",
        intent.operation_id, intent.phase, message
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GitInitMarker {
    version: u32,
    operation_id: String,
    root_path: String,
    marker_nonce: String,
    repository_fingerprint: String,
}

fn git_init_marker_path(root: &str) -> std::path::PathBuf {
    files::to_host_path(root)
        .join(".git")
        .join(GIT_INIT_MARKER_FILE)
}

fn git_init_repository_fingerprint(root: &str) -> Result<String, String> {
    fn walk(
        root: &std::path::Path,
        current: &std::path::Path,
        entries: &mut Vec<(String, Vec<u8>)>,
    ) -> Result<(), String> {
        let mut children = std::fs::read_dir(current)
            .map_err(|error| format!("could not inspect initialized Git state: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("could not inspect initialized Git state: {error}"))?;
        children.sort_by_key(|entry| entry.file_name());
        for entry in children {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|error| format!("could not fingerprint initialized Git state: {error}"))?
                .to_string_lossy()
                .replace('\\', "/");
            if relative == GIT_INIT_MARKER_FILE {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("could not inspect initialized Git state: {error}"))?;
            if metadata.file_type().is_symlink() {
                return Err("Git initialization recovery refused a symlink in .git".into());
            }
            if metadata.is_dir() {
                entries.push((format!("dir:{relative}"), Vec::new()));
                walk(root, &path, entries)?;
            } else if metadata.is_file() {
                let bytes = std::fs::read(&path)
                    .map_err(|error| format!("could not read initialized Git state: {error}"))?;
                entries.push((format!("file:{relative}"), bytes));
            } else {
                return Err("Git initialization recovery refused an unusual .git entry".into());
            }
        }
        Ok(())
    }

    let git_dir = files::to_host_path(root).join(".git");
    let mut entries = Vec::new();
    walk(&git_dir, &git_dir, &mut entries)?;
    let mut digest = Sha256::new();
    for (path, bytes) in entries {
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(bytes);
        digest.update([0]);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn write_git_init_marker(root: &str, marker: &GitInitMarker) -> Result<(), String> {
    let path = git_init_marker_path(root);
    let temp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let body = serde_json::to_vec_pretty(marker)
        .map_err(|error| format!("could not serialize Git initialization marker: {error}"))?;
    std::fs::write(&temp, body)
        .map_err(|error| format!("could not write Git initialization marker: {error}"))?;
    if let Err(error) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!(
            "could not publish Git initialization marker: {error}"
        ));
    }
    Ok(())
}

fn read_git_init_marker(root: &str) -> Result<GitInitMarker, String> {
    let path = git_init_marker_path(root);
    let body = std::fs::read_to_string(&path)
        .map_err(|error| format!("Git initialization ownership marker is unavailable: {error}"))?;
    serde_json::from_str(&body)
        .map_err(|error| format!("Git initialization ownership marker is invalid: {error}"))
}

fn remove_git_init_marker(root: &str) -> Result<(), String> {
    let path = git_init_marker_path(root);
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not remove Git initialization marker: {error}"
        )),
    }
}

fn validate_git_init_ownership(root: &str, intent: &GitInitIntent) -> Result<(), String> {
    let canonical = canonical_project_root(root, false)?;
    if canonical != intent.root_path {
        return Err("Git initialization recovery refused a swapped or symlinked root".into());
    }
    let marker = read_git_init_marker(root)?;
    if marker.version != GIT_INIT_INTENT_VERSION
        || marker.operation_id != intent.operation_id
        || marker.root_path != intent.root_path
        || marker.marker_nonce != intent.marker_nonce
    {
        return Err("Git initialization ownership marker does not match its durable intent".into());
    }
    let fingerprint = git_init_repository_fingerprint(root)?;
    if fingerprint != marker.repository_fingerprint {
        return Err(
            "Git initialization recovery refused changed Git state or foreign repository data"
                .into(),
        );
    }
    if !git::git_info_cached(root).is_repo {
        return Err("Git initialization recovery found an invalid Git repository".into());
    }
    Ok(())
}

fn recover_git_initialization(
    registry: &CaptainsRegistry,
    intent: &GitInitIntent,
) -> Result<(), String> {
    if matches!(intent.phase.as_str(), "recovery_blocked" | "foreign_git") {
        return Err(intent
            .recovery_error
            .clone()
            .unwrap_or_else(|| "Git initialization recovery remains blocked".into()));
    }
    let git_dir = files::to_host_path(&intent.root_path).join(".git");
    let git_exists = git_dir
        .try_exists()
        .map_err(|error| format!("could not inspect Git initialization recovery state: {error}"))?;
    if intent.phase == "intent_written" {
        if !git_exists {
            return registry.clear_git_initialization(&intent.operation_id);
        }
        validate_git_init_ownership(&intent.root_path, intent)?;
    } else if !git_exists {
        return Err("Git initialization recovery found that its owned .git is missing".into());
    } else if intent.phase != "cleanup_pending"
        || git_init_marker_path(&intent.root_path)
            .try_exists()
            .unwrap_or(false)
    {
        validate_git_init_ownership(&intent.root_path, intent)?;
    }
    let existing = registry
        .projects()
        .into_iter()
        .find(|project| project_identity_matches(project, &intent.root_path));
    if let Some(project) = &existing {
        if project.vcs_capability.as_deref() != Some("git")
            || project.project_id != intent.project_id
        {
            return Err("Git initialization recovery found a conflicting durable Project".into());
        }
    } else {
        let info = git::git_info_cached(&intent.root_path);
        let main_root = info
            .worktree_root
            .as_deref()
            .map(files::posix_form)
            .unwrap_or_else(|| intent.root_path.clone());
        registry.upsert_project(ProjectRecord {
            project_id: intent.project_id.clone(),
            name: intent.name.clone(),
            repo_root: intent.root_path.clone(),
            root_path: Some(intent.root_path.clone()),
            vcs_capability: Some("git".into()),
            git_main_root: Some(main_root),
            remote_url: info.remote_url,
            default_branch: info.default_branch.or_else(|| Some("main".into())),
            powder: None,
            created_at: intent.created_at,
            updated_at: 0,
        })?;
    }
    if intent.phase != "cleanup_pending" {
        registry.update_git_initialization(&intent.operation_id, "cleanup_pending", None)?;
    }
    let marker_exists = git_init_marker_path(&intent.root_path)
        .try_exists()
        .unwrap_or(false);
    if marker_exists {
        validate_git_init_ownership(&intent.root_path, intent)?;
        remove_git_init_marker(&intent.root_path)?;
    }
    registry.clear_git_initialization(&intent.operation_id)
}

/// Register an existing Git repository using its canonical main-worktree root.
/// Re-registering the same root updates metadata while preserving its project id.
fn initialize_git(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_exact_args(
        args,
        "initialize_git",
        &[
            "rootPath",
            "repoRoot",
            "repo_root",
            "projectId",
            "project_id",
            "name",
        ],
    )?;
    let requested_identity = requested_project_root(args, "initialize_git")?;
    require_socket_identity(caller, trusted_internal, "initialize_git")?;
    let existing = ctx
        .captains
        .projects()
        .into_iter()
        .find(|project| project_identity_matches(project, &requested_identity));
    enforce_project_authority(
        ctx,
        caller,
        trusted_internal,
        existing.as_ref().map(|project| project.project_id.as_str()),
    )?;
    let root = canonical_project_root(&requested_identity, false)?;
    let existing = existing.or_else(|| {
        ctx.captains
            .projects()
            .into_iter()
            .find(|project| project_identity_matches(project, &root))
    });
    enforce_project_authority(
        ctx,
        caller,
        trusted_internal,
        existing.as_ref().map(|project| project.project_id.as_str()),
    )?;
    let name = arg_str(args, "name")
        .filter(|value| !value.trim().is_empty())
        .ok_or("initialize_git requires a non-empty 'name'")?;
    if let Some(project) = existing.as_ref() {
        if project.vcs_capability.as_deref() == Some("git") {
            return serde_json::to_value(project).map_err(|error| error.to_string());
        }
    }

    let _git_initialization = ctx.captains.git_initialization_guard();
    let existing = ctx
        .captains
        .projects()
        .into_iter()
        .find(|project| project_identity_matches(project, &root));
    if let Some(project) = existing.as_ref() {
        if project.name != name && project.vcs_capability.as_deref() == Some("git") {
            return Err(
                "initialize_git has a conflicting durable Project name for this root".into(),
            );
        }
        if project.vcs_capability.as_deref() == Some("git") {
            return serde_json::to_value(project).map_err(|error| error.to_string());
        }
    }
    let owner_identity = caller
        .map(|identity| identity.session_id.clone())
        .unwrap_or_else(|| "trusted-internal".into());
    let intent = GitInitIntent {
        version: GIT_INIT_INTENT_VERSION,
        operation_id: format!("git-init-{}", uuid::Uuid::new_v4()),
        root_path: root.clone(),
        name: name.to_string(),
        project_id: existing
            .as_ref()
            .map(|project| project.project_id.clone())
            .unwrap_or_else(|| format!("project-{}", uuid::Uuid::new_v4())),
        owner_identity,
        phase: "intent_written".into(),
        marker_nonce: uuid::Uuid::new_v4().to_string(),
        created_at: now_ms(),
        recovery_error: None,
    };
    git_init_fault("before_intent_write")?;
    let requested_operation_id = intent.operation_id.clone();
    let mut intent = ctx.captains.begin_git_initialization(intent)?;
    let resumed = intent.operation_id != requested_operation_id;

    let git_dir = files::to_host_path(&root).join(".git");
    let git_exists = git_dir
        .try_exists()
        .map_err(|error| git_init_recovery_error(&intent, error))?;
    if git_exists {
        if resumed {
            recover_git_initialization(&ctx.captains, &intent)
                .map_err(|error| git_init_recovery_error(&intent, error))?;
            let project = ctx
                .captains
                .projects()
                .into_iter()
                .find(|project| project.project_id == intent.project_id)
                .ok_or_else(|| {
                    git_init_recovery_error(&intent, "recovery completed without a durable Project")
                })?;
            return serde_json::to_value(project)
                .map_err(|error| git_init_recovery_error(&intent, error));
        }
        ctx.captains
            .update_git_initialization(&intent.operation_id, "foreign_git", None)
            .map_err(|error| git_init_recovery_error(&intent, error))?;
        intent.phase = "foreign_git".into();
        ctx.captains
            .clear_git_initialization(&intent.operation_id)
            .map_err(|error| git_init_recovery_error(&intent, error))?;
        return Err(git_init_recovery_error(
            &intent,
            "refused a pre-existing .git entry; T-Hub will not claim foreign Git state",
        ));
    }

    git_init_fault("after_intent_before_git_init")
        .map_err(|error| git_init_recovery_error(&intent, error))?;

    if let Err(error) = git::initialize_repository(&root) {
        let git_exists_after = git_dir
            .try_exists()
            .map_err(|probe| git_init_recovery_error(&intent, probe))?;
        if !git_exists_after {
            ctx.captains
                .clear_git_initialization(&intent.operation_id)
                .map_err(|cleanup| git_init_recovery_error(&intent, cleanup))?;
        }
        return Err(git_init_recovery_error(
            &intent,
            format!("Git initialization failed before durable Project creation: {error}"),
        ));
    }
    git_init_fault("after_git_init_before_marker")
        .map_err(|error| git_init_recovery_error(&intent, error))?;
    let repository_fingerprint = git_init_repository_fingerprint(&root)
        .map_err(|error| git_init_recovery_error(&intent, error))?;
    let marker = GitInitMarker {
        version: GIT_INIT_INTENT_VERSION,
        operation_id: intent.operation_id.clone(),
        root_path: root.clone(),
        marker_nonce: intent.marker_nonce.clone(),
        repository_fingerprint,
    };
    write_git_init_marker(&root, &marker)
        .map_err(|error| git_init_recovery_error(&intent, error))?;
    ctx.captains
        .update_git_initialization(&intent.operation_id, "git_initialized", None)
        .map_err(|error| git_init_recovery_error(&intent, error))?;
    intent.phase = "git_initialized".into();
    git_init_fault("after_marker_before_project")
        .map_err(|error| git_init_recovery_error(&intent, error))?;

    let info = git::git_info_cached(&root);
    let main_root = info
        .worktree_root
        .as_deref()
        .map(files::posix_form)
        .unwrap_or_else(|| root.clone());
    let main_branch = git::worktree_list(&main_root).ok().and_then(|worktrees| {
        worktrees
            .into_iter()
            .find(|worktree| !worktree.is_linked)
            .and_then(|worktree| worktree.branch)
    });
    let project = existing.unwrap_or(ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: intent.project_id.clone(),
        name: name.to_string(),
        repo_root: root.clone(),
        remote_url: None,
        default_branch: None,
        powder: None,
        created_at: 0,
        updated_at: 0,
    });
    let updated = ctx
        .captains
        .upsert_project(ProjectRecord {
            project_id: project.project_id,
            name: name.to_string(),
            repo_root: root.clone(),
            root_path: Some(root.clone()),
            vcs_capability: Some("git".into()),
            git_main_root: Some(main_root),
            remote_url: project.remote_url.or(info.remote_url),
            default_branch: project
                .default_branch
                .or(info.default_branch)
                .or(main_branch)
                .or_else(|| Some("main".into())),
            powder: project.powder,
            created_at: project.created_at,
            updated_at: 0,
        })
        .map_err(|error| git_init_recovery_error(&intent, error))?;
    git_init_fault("after_project_before_clear")
        .map_err(|error| git_init_recovery_error(&intent, error))?;
    ctx.captains
        .update_git_initialization(&intent.operation_id, "cleanup_pending", None)
        .map_err(|error| git_init_recovery_error(&intent, error))?;
    intent.phase = "cleanup_pending".into();
    git_init_fault("during_cleanup").map_err(|error| git_init_recovery_error(&intent, error))?;
    remove_git_init_marker(&root).map_err(|error| git_init_recovery_error(&intent, error))?;
    ctx.captains
        .clear_git_initialization(&intent.operation_id)
        .map_err(|error| git_init_recovery_error(&intent, error))?;
    serde_json::to_value(updated).map_err(|error| git_init_recovery_error(&intent, error))
}

fn register_project(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    require_exact_args(
        args,
        "register_project",
        &[
            "rootPath",
            "repoRoot",
            "repo_root",
            "createDirectory",
            "create_directory",
            "name",
            "remoteUrl",
            "remote_url",
        ],
    )?;
    let requested_root = requested_project_root(args, "register_project")?;
    require_socket_identity(caller, trusted_internal, "register_project")?;
    let explicit_name = arg_str(args, "name")
        .filter(|value| !value.trim().is_empty())
        .ok_or("register_project requires a non-empty 'name'")?;
    let create_directory = args
        .get("createDirectory")
        .or_else(|| args.get("create_directory"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let requested_identity = requested_root.clone();
    let requested_existing = ctx
        .captains
        .projects()
        .into_iter()
        .find(|project| project_identity_matches(project, &requested_identity));
    enforce_project_authority(
        ctx,
        caller,
        trusted_internal,
        requested_existing
            .as_ref()
            .map(|project| project.project_id.as_str()),
    )?;
    let canonical_root = canonical_project_root(&requested_root, create_directory)?;
    let existing_before_probe = requested_existing.or_else(|| {
        ctx.captains
            .projects()
            .into_iter()
            .find(|project| project_identity_matches(project, &canonical_root))
    });
    enforce_project_authority(
        ctx,
        caller,
        trusted_internal,
        existing_before_probe
            .as_ref()
            .map(|project| project.project_id.as_str()),
    )?;
    if create_directory && !ctx.peer_is_loopback {
        files::scoped_create_path(&canonical_root, true, files::remote_file_roots())?;
    }
    let created_directory = if create_directory {
        create_new_project_directory(&canonical_root)?;
        true
    } else {
        false
    };
    let canonical_root = if created_directory {
        files::canonical_posix_path(&canonical_root).map_err(|error| {
            rollback_project_creation_error(
                &canonical_root,
                false,
                true,
                format!("register_project: created root could not be canonicalized: {error}"),
            )
        })?
    } else {
        canonical_root
    };
    let (worktrees, initialized_git) = if created_directory {
        (Vec::new(), false)
    } else {
        record_project_probe(2);
        let git_info = git::git_info_cached(&canonical_root);
        let worktrees = if git_info.is_repo {
            record_project_probe(3);
            git::worktree_list(&canonical_root)
                .map_err(|e| format!("register_project: repository validation failed: {e}"))?
        } else {
            Vec::new()
        };
        (worktrees, false)
    };

    let result = (|| {
        let main = worktrees.iter().find(|worktree| !worktree.is_linked);
        let selected_root = canonical_root.clone();
        record_project_probe(2);
        let git_info = git::git_info_cached(&selected_root);
        let git_main_root = main
            .and_then(|worktree| {
                record_project_probe(2);
                git::git_info_cached(&worktree.path).worktree_root
            })
            .map(|root| files::posix_form(&root));
        let name = explicit_name;
        let existing = ctx
            .captains
            .projects()
            .into_iter()
            .find(|project| project_identity_matches(project, &selected_root));
        enforce_project_authority(
            ctx,
            caller,
            trusted_internal,
            existing.as_ref().map(|project| project.project_id.as_str()),
        )?;
        let project = ctx.captains.upsert_project(ProjectRecord {
            project_id: existing
                .as_ref()
                .map(|project| project.project_id.clone())
                .unwrap_or_else(|| format!("project-{}", uuid::Uuid::new_v4())),
            name,
            repo_root: selected_root.clone(),
            root_path: Some(selected_root),
            vcs_capability: Some(if git_info.is_repo { "git" } else { "none" }.into()),
            git_main_root: git_info.is_repo.then_some(git_main_root).flatten(),
            remote_url: arg_str(args, "remoteUrl")
                .or_else(|| arg_str(args, "remote_url"))
                .or(git_info.remote_url)
                .or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|project| project.remote_url.clone())
                }),
            default_branch: git_info
                .default_branch
                .or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|project| project.default_branch.clone())
                })
                .or_else(|| main.and_then(|worktree| worktree.branch.clone())),
            powder: existing.as_ref().and_then(|project| project.powder.clone()),
            created_at: existing.as_ref().map_or(0, |project| project.created_at),
            updated_at: 0,
        })?;
        serde_json::to_value(project).map_err(|e| e.to_string())
    })();

    if let Err(error) = result {
        if initialized_git || created_directory {
            return Err(rollback_project_creation_error(
                &canonical_root,
                initialized_git,
                created_directory,
                error,
            ));
        }
        return Err(error);
    }
    result
}

fn canonical_project_identity(requested: &str) -> Result<String, String> {
    let raw = requested.trim();
    files::validate_configured_wsl_path(raw)?;
    if raw.split(['/', '\\']).any(|part| part == "..") {
        return Err("path traversal is not allowed".into());
    }
    let root = files::posix_form(raw);
    if root.is_empty() || !root.starts_with('/') || root.starts_with("//") || root.contains('\0') {
        return Err("rootPath must be an absolute WSL path".into());
    }
    Ok(root)
}

fn project_identity_matches(project: &ProjectRecord, identity: &str) -> bool {
    project
        .root_path
        .as_deref()
        .or(Some(project.repo_root.as_str()))
        .is_some_and(|root| files::posix_form(root) == identity)
}

/// Project identity is always the canonical POSIX root seen by WSL.
/// Host-side `Path::is_absolute` and `canonicalize` are intentionally absent:
/// they reject or rewrite valid WSL roots when the daemon runs on Windows.
fn canonical_project_root(requested: &str, allow_missing: bool) -> Result<String, String> {
    let raw = requested.trim();
    files::validate_configured_wsl_path(raw)
        .map_err(|error| format!("register_project: {error}"))?;
    if raw.split(['/', '\\']).any(|part| part == "..") {
        return Err("register_project: path traversal is not allowed".into());
    }
    let root = files::posix_form(raw);
    if root.is_empty() || !root.starts_with('/') || root.starts_with("//") || root.contains('\0') {
        return Err("register_project: rootPath must be an absolute WSL path".into());
    }
    record_project_probe(0);
    let metadata = match std::fs::metadata(files::to_host_path(&root)) {
        Ok(metadata) => metadata,
        Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(root)
        }
        Err(error) => {
            return Err(format!(
                "register_project: could not inspect root '{root}': {error}"
            ));
        }
    };
    if !metadata.is_dir() {
        return Err("register_project: rootPath must refer to a directory".into());
    }
    record_project_probe(1);
    files::canonical_posix_path(&root)
        .map_err(|error| format!("register_project: could not canonicalize root '{root}': {error}"))
}

fn create_new_project_directory(repo_root: &str) -> Result<(), String> {
    record_project_probe(4);
    let path = repo_root.trim();
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.ends_with('/')
        || path.contains('\\')
    {
        return Err(
            "register_project: new codebase destination must be an absolute WSL path".into(),
        );
    }
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty()
        || segments
            .iter()
            .any(|segment| matches!(*segment, "." | "..") || segment.chars().any(char::is_control))
    {
        return Err(
            "register_project: new codebase destination contains an invalid path segment".into(),
        );
    }
    let host_path = files::to_host_path(path);
    match std::fs::symlink_metadata(&host_path) {
        Ok(_) => {
            return Err(format!(
                "register_project: new codebase destination '{path}' already exists"
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "register_project: could not inspect new codebase destination: {error}"
            ));
        }
    }
    let parent = host_path
        .parent()
        .ok_or("register_project: new codebase destination has no parent directory")?;
    let parent_metadata = std::fs::metadata(parent).map_err(|error| {
        format!("register_project: could not inspect parent directory: {error}")
    })?;
    if !parent_metadata.is_dir() {
        return Err("register_project: new codebase parent is not a directory".into());
    }
    std::fs::create_dir(&host_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            format!("register_project: new codebase destination '{path}' already exists")
        } else {
            format!("register_project: could not create new codebase directory: {error}")
        }
    })
}

fn rollback_project_creation_error(
    repo_root: &str,
    initialized_git: bool,
    created_directory: bool,
    error: String,
) -> String {
    if !created_directory {
        return if initialized_git {
            rollback_initialized_git_error(repo_root, error)
        } else {
            error
        };
    }
    let mut rollback_errors = Vec::new();
    if initialized_git {
        if let Err(rollback_error) = git::rollback_initialized_repository(repo_root) {
            rollback_errors.push(format!("Git rollback failed: {rollback_error}"));
        }
    }
    if let Err(rollback_error) = std::fs::remove_dir(files::to_host_path(repo_root)) {
        rollback_errors.push(format!("directory rollback failed: {rollback_error}"));
    }
    if rollback_errors.is_empty() {
        format!("{error}. T-Hub rolled back the new directory and Git repository it created")
    } else {
        format!(
            "{error}. T-Hub could not completely roll back the new codebase: {}. No recursive directory deletion was attempted",
            rollback_errors.join("; ")
        )
    }
}

fn rollback_initialized_git_error(repo_root: &str, error: String) -> String {
    match git::rollback_initialized_repository(repo_root) {
        Ok(()) => format!(
            "{error}. T-Hub rolled back the Git repository it initialized; the existing folder and files were preserved"
        ),
        Err(rollback_error) => format!(
            "{error}. T-Hub could not roll back the Git repository it initialized: {rollback_error}. The existing folder and files were preserved"
        ),
    }
}

fn resolve_bootstrap_context(
    ctx: &ControlContext,
    args: &Value,
) -> Result<(CaptainRecord, ProjectRecord), String> {
    let ship_slug = arg_str(args, "shipSlug").or_else(|| arg_str(args, "ship_slug"));
    let session_id = arg_str(args, "captainSessionId")
        .or_else(|| arg_str(args, "captain_session_id"))
        .or_else(|| arg_str(args, "sessionId"))
        .or_else(|| arg_str(args, "session_id"));
    if ship_slug.is_none() && session_id.is_none() {
        return Err(
            "captain_bootstrap requires a 'shipSlug' or 'captainSessionId' argument".into(),
        );
    }
    let snapshot = ctx.captains.snapshot();
    let captain = snapshot
        .captains
        .into_iter()
        .find(|captain| {
            ship_slug
                .as_deref()
                .is_some_and(|slug| captain.ship_slug == slugify_ship(slug))
                || session_id
                    .as_deref()
                    .is_some_and(|id| captain.terminal_id.as_deref() == Some(id))
        })
        .ok_or("captain_bootstrap: no matching Captain is registered")?;
    let project_id = captain
        .project_id
        .as_deref()
        .ok_or("captain_bootstrap: Captain is not bound to a registered project")?;
    let project = snapshot
        .projects
        .into_iter()
        .find(|project| project.project_id == project_id)
        .ok_or_else(|| {
            format!("captain_bootstrap: Captain references unknown projectId '{project_id}'")
        })?;
    Ok((captain, project))
}

fn bootstrap_instructions(captain: &CaptainRecord, project: &ProjectRecord) -> String {
    let assignment = captain.assignment.as_deref().unwrap_or("Unassigned");
    let invocation = match captain.harness.as_deref() {
        Some(provider) => Harness::from_provider(provider).captain_invocation(),
        // Historical Captain records did not store a harness. Preserve the
        // existing Codex bootstrap behavior until the record is commissioned.
        None => Harness::Codex.captain_invocation(),
    };
    let runtime_root = files::posix_form(&project.repo_root);
    format!(
        "Use {invocation}. Recover ship '{}' for project '{}' at '{}'. Assignment: {} Read the durable Captain and agent-session records before acting, then keep checkpoints and the registry resume point current.",
        captain.ship_slug, project.name, runtime_root, assignment
    )
}

/// Return the complete durable recovery packet for a Captain conversation.
/// This command is deliberately independent of cwd and harness recall state.
fn captain_bootstrap(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let (captain, project) = resolve_bootstrap_context(ctx, args)?;
    let instructions = bootstrap_instructions(&captain, &project);
    let snapshot = ctx.captains.snapshot();
    let mut agents: Vec<Value> = snapshot
        .agent_sessions
        .iter()
        .filter(|agent| {
            agent.captain_session_id == captain.terminal_id.as_deref().unwrap_or_default()
                && agent.project_id == project.project_id
        })
        .map(|agent| {
            json!({
                "agentSessionId": agent.agent_session_id,
                "captainSessionId": agent.captain_session_id,
                "projectId": agent.project_id,
                "directory": agent.directory,
                "worktreePath": agent.worktree_path,
                "branch": agent.branch,
                "workspaceTabId": agent.workspace_tab_id,
                "harness": agent.harness,
                "provider": agent.provider,
                "runtimeState": agent.runtime_state,
                "workStage": agent.work_stage,
                "updatedAt": agent.updated_at,
            })
        })
        .collect();
    agents.sort_by(|left, right| {
        left["agentSessionId"]
            .as_str()
            .cmp(&right["agentSessionId"].as_str())
    });
    let agent_digest = crate::agent_session::snapshot_digest(&agents)?;
    let event_cursor = snapshot
        .agent_events
        .iter()
        .filter(|event| {
            agents.iter().any(|agent| {
                agent["agentSessionId"].as_str() == Some(event.agent_session_id.as_str())
            })
        })
        .map(|event| event.cursor)
        .max()
        .unwrap_or(0);
    Ok(json!({
        "captain": captain,
        "project": project,
        "agents": agents,
        "agentCount": agents.len(),
        "agentDigest": agent_digest,
        "agentEventCursor": event_cursor.to_string(),
        "instructions": instructions,
        "recoverySource": "captains-registry",
    }))
}

const CORTANA_BOOTSTRAP_MAX_SHIPS: usize = 16;
const CORTANA_BOOTSTRAP_MAX_TEXT_BYTES: usize = 1_024;
const CORTANA_BOOTSTRAP_MAX_RESPONSE_BYTES: usize = 32_768;

fn bounded_bootstrap_text(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    if value.len() <= CORTANA_BOOTSTRAP_MAX_TEXT_BYTES {
        return Some(value.to_string());
    }
    let mut end = CORTANA_BOOTSTRAP_MAX_TEXT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    Some(value[..end].to_string())
}

fn exact_cortana_bootstrap_live_terminal(
    ctx: &ControlContext,
    caller: &ResolvedIdentity,
) -> Result<String, String> {
    let terminal_id = exact_live_identity_terminal(ctx, caller)
        .map_err(|error| format!("cortana_bootstrap: {error}"))?;
    let live = (ctx.live_sessions)()
        .map_err(|error| format!("cortana_bootstrap: terminal liveness is unavailable: {error}"))?;
    let target = tmux_target(&terminal_id);
    if live
        .iter()
        .filter(|session| **session == terminal_id || **session == target)
        .count()
        != 1
    {
        return Err("cortana_bootstrap: Cortana terminal liveness is missing or ambiguous".into());
    }
    Ok(terminal_id)
}

fn authorize_inflight_cortana_bootstrap(
    ctx: &ControlContext,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    identity: &crate::identity::SessionIdentity,
) -> Result<(), String> {
    let launch = durable
        .managed_launch
        .as_ref()
        .filter(|launch| {
            matches!(
                launch.phase,
                crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
                    | crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
                    | crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed
            )
        })
        .ok_or("cortana_bootstrap: Cortana is not healthy or in an admitted launch phase")?;
    let owner = durable
        .owner
        .as_ref()
        .ok_or("cortana_bootstrap: in-flight Cortana has no exact managed owner")?;
    if launch.generation == 0
        || launch.identity_id != identity.id
        || identity.session_tile.as_deref() != Some(launch.terminal_id.as_str())
        || launch.tmux_target != tmux_target(&launch.terminal_id)
        || owner.unit_name != launch.unit_name
        || owner.launch_nonce != launch.launch_nonce
        || owner.tools != launch.tools
    {
        return Err(
            "cortana_bootstrap: in-flight identity, generation, terminal, Harness, launch, and owner do not agree"
                .into(),
        );
    }
    let snapshot = ctx.captains.snapshot();
    let claims = snapshot
        .captains
        .iter()
        .filter(|claim| claim.role == FleetRole::Cortana && claim.state == ClaimState::Active)
        .collect::<Vec<_>>();
    let claim_is_exact = matches!(
        claims.as_slice(),
        [claim]
            if claim.terminal_id.as_deref() == Some(launch.terminal_id.as_str())
                && claim.harness.as_deref() == Some(launch.harness.as_str())
    );
    if (launch.phase == crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed
        && !claim_is_exact)
        || (launch.phase != crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed
            && !claims.is_empty())
    {
        return Err("cortana_bootstrap: in-flight Fleet claim is missing or ambiguous".into());
    }
    tmux::revalidate_managed_runtime_owner(&launch.tmux_target, &tmux_cortana_owner(owner))
        .map_err(|error| {
            cortana_tmux_observation_error(
                "cortana_bootstrap: managed owner revalidation failed",
                error,
            )
        })?;
    let expected = launch
        .expected_harness_launch_provenance
        .as_ref()
        .filter(|expected| crate::harness::valid_expected_harness_launch_provenance(expected))
        .ok_or("cortana_bootstrap: in-flight Harness provenance is unavailable")?;
    let harness = match launch.harness.as_str() {
        "codex" => Harness::Codex,
        "claude" => Harness::Claude,
        _ => return Err("cortana_bootstrap: in-flight Harness is unsupported".into()),
    };
    let observed = crate::harness::observe_scoped_harness_process(
        &launch.tmux_target,
        harness,
        expected,
        &identity.id,
        &identity.secret,
        &owner.cgroup_path,
        owner.tmux.pane_start_ticks,
        Instant::now() + Duration::from_secs(5),
    )
    .map_err(|error| {
        cortana_harness_observation_error("cortana_bootstrap: Harness revalidation failed", error)
    })?;
    if launch
        .harness_process
        .as_ref()
        .is_some_and(|process| process != &observed)
    {
        return Err("cortana_bootstrap: in-flight Harness process changed".into());
    }
    Ok(())
}

fn authorize_cortana_bootstrap(
    ctx: &ControlContext,
    caller: Option<&ResolvedIdentity>,
) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
    let caller = caller
        .ok_or("cortana_bootstrap: an authenticated terminal-bound Cortana bearer is required")?;
    let identity = ctx
        .identity
        .get(&caller.session_id)
        .filter(|identity| identity.role == crate::identity::Role::Cortana)
        .ok_or("cortana_bootstrap: authenticated bearer does not resolve to Cortana")?;
    let terminal_id = exact_cortana_bootstrap_live_terminal(ctx, caller)?;
    if identity.session_tile.as_deref() != Some(terminal_id.as_str()) {
        return Err("cortana_bootstrap: Cortana bearer terminal binding changed".into());
    }
    let before = ctx.captains.cortana_identity();
    if matches!(
        before.recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
    ) {
        if before.identity_id.as_deref() != Some(identity.id.as_str())
            || before.terminal_id.as_deref() != Some(terminal_id.as_str())
            || before.generation == 0
            || before.harness.as_deref().is_none()
        {
            return Err(
                "cortana_bootstrap: authenticated bearer is not the exact durable Cortana generation"
                    .into(),
            );
        }
        if !authoritative_cortana_identity(ctx, &identity) {
            return Err("cortana_bootstrap: healthy Cortana authority did not revalidate".into());
        }
    } else {
        authorize_inflight_cortana_bootstrap(ctx, &before, &identity)?;
        if before
            .managed_launch
            .as_ref()
            .is_none_or(|launch| launch.terminal_id != terminal_id)
        {
            return Err(
                "cortana_bootstrap: live terminal does not match the durable in-flight launch"
                    .into(),
            );
        }
    }
    if ctx.captains.cortana_identity() != before {
        return Err("cortana_bootstrap: Cortana durable state changed during revalidation".into());
    }
    Ok(before)
}

fn cortana_bootstrap(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
) -> Result<Value, String> {
    if args.as_object().is_none_or(|object| !object.is_empty()) {
        return Err("cortana_bootstrap accepts no arguments".into());
    }
    let durable = authorize_cortana_bootstrap(ctx, caller)?;
    let snapshot = ctx.captains.snapshot();
    let mut ships = snapshot
        .captains
        .iter()
        .filter(|ship| ship.role == FleetRole::Captain && ship.state == ClaimState::Active)
        .map(|ship| {
            json!({
                "shipSlug": ship.ship_slug,
                "terminalId": ship.terminal_id,
                "projectId": ship.project_id,
                "harness": ship.harness,
                "providerSessionId": ship.provider_session_id,
                "conversationId": ship.conversation_id,
                "resumePoint": bounded_bootstrap_text(ship.resume_point.as_deref()),
            })
        })
        .collect::<Vec<_>>();
    ships.sort_by(|left, right| {
        left["shipSlug"]
            .as_str()
            .cmp(&right["shipSlug"].as_str())
            .then_with(|| {
                left["terminalId"]
                    .as_str()
                    .cmp(&right["terminalId"].as_str())
            })
    });
    let active_count = ships.len();
    ships.truncate(CORTANA_BOOTSTRAP_MAX_SHIPS);
    let returned_count = ships.len();
    let healthy = matches!(
        durable.recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
    );
    let (generation, terminal_id, harness) = if healthy {
        (
            durable.generation,
            durable.terminal_id.as_deref(),
            durable.harness.as_deref(),
        )
    } else {
        let launch = durable
            .managed_launch
            .as_ref()
            .ok_or("cortana_bootstrap: authorized in-flight launch disappeared")?;
        (
            launch.generation,
            Some(launch.terminal_id.as_str()),
            Some(launch.harness.as_str()),
        )
    };
    let response = json!({
        "cortana": {
            "generation": generation,
            "terminalId": terminal_id,
            "harness": harness,
            "checkpoint": bounded_bootstrap_text(durable.checkpoint.as_deref()),
            "state": if healthy {
                "healthy"
            } else {
                "inFlight"
            },
        },
        "ships": ships,
        "activeCount": active_count,
        "returnedCount": returned_count,
        "omittedCount": active_count.saturating_sub(returned_count),
        "truncated": active_count > CORTANA_BOOTSTRAP_MAX_SHIPS,
        "recoverySource": "captains-registry",
    });
    if serde_json::to_vec(&response)
        .map_err(|error| format!("cortana_bootstrap: response encoding failed: {error}"))?
        .len()
        > CORTANA_BOOTSTRAP_MAX_RESPONSE_BYTES
    {
        return Err("cortana_bootstrap: bounded response ceiling exceeded".into());
    }
    #[cfg(test)]
    if !ctx
        .captains
        .pause_dispatch("cortana-bootstrap-response-built")
    {
        return Err("cortana_bootstrap: response revalidation was interrupted".into());
    }
    let confirmed = authorize_cortana_bootstrap(ctx, caller)?;
    if confirmed != durable {
        return Err("cortana_bootstrap: Cortana basis changed while building the response".into());
    }
    Ok(response)
}

fn existing_project_captain(
    ctx: &ControlContext,
    project_id: &str,
    ship_slug: &str,
) -> Result<Option<CaptainRecord>, String> {
    let existing = ctx
        .captains
        .snapshot()
        .captains
        .into_iter()
        .find(|captain| {
            captain.project_id.as_deref() == Some(project_id) && captain.ship_slug == ship_slug
        });
    let Some(captain) = existing else {
        return Ok(None);
    };
    let Some(terminal_id) = captain.terminal_id.as_deref() else {
        return Ok(None);
    };
    let harness = captain.harness.as_deref().ok_or_else(|| {
        retryable_error(format!(
            "commission_captain: existing Captain '{}' has no recorded harness",
            captain.ship_slug
        ))
    })?;
    match tmux::harness_liveness(&tmux_target(terminal_id), harness) {
        tmux::SessionLiveness::Alive => Ok(Some(captain)),
        tmux::SessionLiveness::Gone => Ok(None),
        tmux::SessionLiveness::Unknown => Err(retryable_error(format!(
            "commission_captain: existing Captain '{}' could not be verified alive or gone; retry when terminal liveness recovers",
            captain.ship_slug
        ))),
    }
}

fn wait_for_harness_started(session_id: &str, harness: &str) -> Result<(), String> {
    let target = tmux_target(session_id);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match tmux::harness_liveness(&target, harness) {
            tmux::SessionLiveness::Alive => return Ok(()),
            tmux::SessionLiveness::Gone if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            tmux::SessionLiveness::Gone => {
                return Err(format!(
                    "{harness} did not remain active in terminal '{session_id}'"
                ));
            }
            tmux::SessionLiveness::Unknown if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            tmux::SessionLiveness::Unknown => {
                return Err(retryable_error(format!(
                    "{harness} liveness is unavailable for terminal '{session_id}'"
                )));
            }
        }
    }
}

fn commissioned_response(
    captain: CaptainRecord,
    project: ProjectRecord,
    already_commissioned: bool,
) -> Value {
    let instructions = bootstrap_instructions(&captain, &project);
    json!({
        "accepted": "commission_captain",
        "audited": true,
        "alreadyCommissioned": already_commissioned,
        "captain": captain,
        "project": project,
        "instructions": instructions,
    })
}

fn inspect_commission_contract(
    ctx: &ControlContext,
    project: &ProjectRecord,
    ship_slug: &str,
    assignment: &str,
    harness: Harness,
) -> Result<Option<Value>, String> {
    let Some(captain) = existing_project_captain(ctx, &project.project_id, ship_slug)? else {
        return Ok(None);
    };
    let same_contract = captain.assignment.as_deref() == Some(assignment)
        && captain.harness.as_deref() == Some(harness.as_provider())
        && captain.ship_slug == ship_slug;
    if !same_contract {
        return Err(format!(
            "commission_captain: project '{}' already has live Captain '{}' with a different assignment, harness, or shipSlug; release or update that Captain explicitly",
            project.name, captain.ship_slug
        ));
    }
    Ok(Some(commissioned_response(captain, project.clone(), true)))
}

fn detected_harness(terminal_id: &str) -> Option<String> {
    for provider in ["codex", "claude"] {
        if tmux::harness_liveness(&tmux_target(terminal_id), provider)
            == tmux::SessionLiveness::Alive
        {
            return Some(provider.to_string());
        }
    }
    None
}

const CORTANA_GENERATION_ENV: &str = "T_HUB_CORTANA_GENERATION";

#[cfg(feature = "devbuild")]
const CORTANA_HOME_DEFAULT: &str = ".t-hub-dev/orchestrator";
#[cfg(not(feature = "devbuild"))]
const CORTANA_HOME_DEFAULT: &str = ".t-hub/orchestrator";

fn resolve_orchestrator_home(user_home: &str, configured: Option<&str>) -> Result<String, String> {
    let requested = configured
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(CORTANA_HOME_DEFAULT)
        .trim_end_matches('/');
    if requested.is_empty()
        || requested.contains('\\')
        || requested
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    {
        return Err("T_HUB_CORTANA_HOME must be a safe POSIX directory path".to_string());
    }
    if requested.starts_with('/') {
        return Ok(requested.to_string());
    }
    Ok(format!("{}/{}", user_home.trim_end_matches('/'), requested))
}

fn orchestrator_home(args: &Value) -> Result<String, String> {
    #[cfg(test)]
    if let Some(path) = arg_str(args, "testOrchestratorHome") {
        return Ok(path.trim_end_matches('/').to_string());
    }
    let _ = args;
    let user_home = files::user_home_path()?;
    resolve_orchestrator_home(
        &user_home,
        std::env::var("T_HUB_CORTANA_HOME").ok().as_deref(),
    )
}

fn runtime_evidence(value: tmux::SessionLiveness) -> crate::cortana_reconcile::RuntimeEvidence {
    match value {
        tmux::SessionLiveness::Alive => crate::cortana_reconcile::RuntimeEvidence::Alive,
        tmux::SessionLiveness::Gone => crate::cortana_reconcile::RuntimeEvidence::Gone,
        tmux::SessionLiveness::Unknown => crate::cortana_reconcile::RuntimeEvidence::Unknown,
    }
}

fn valid_cortana_effect_identity(
    identity: &crate::cortana_reconcile::CortanaOrphanEffectIdentity,
) -> bool {
    identity.tmux_session_created > 0
        && identity.pane_pid > 0
        && identity.pane_start_ticks > 0
        && identity.pane_process_group_id > 0
        && identity.pane_process_session_id > 0
        && identity.foreground_pid > 0
        && identity.foreground_start_ticks > 0
        && identity.foreground_process_group_id == identity.foreground_pid
        && identity.foreground_process_session_id == identity.pane_process_session_id
}

fn cortana_quarantine_ledger_sha256(
    ledger: &[crate::cortana_reconcile::CortanaLegacyQuarantine],
) -> String {
    let canonical = serde_json::to_vec(ledger)
        .expect("Cortana quarantine evidence is always JSON serializable");
    format!("{:x}", Sha256::digest(canonical))
}

fn managed_cortana_quarantine_basis_matches(
    current: &CaptainsInner,
    basis: &crate::cortana_reconcile::CortanaManagedQuarantineBasis,
    terminal_id: &str,
    identity_id: &str,
    generation: u64,
    harness: &str,
    effect_identity: &crate::cortana_reconcile::CortanaOrphanEffectIdentity,
) -> bool {
    if basis.version != crate::cortana_reconcile::MANAGED_QUARANTINE_BASIS_VERSION
        || basis.claim_terminal_id != terminal_id
        || basis.claim_harness != harness
        || !same_cortana_tmux_generation(&basis.owner.tmux, effect_identity)
        || basis.replacement_generation != generation.saturating_add(1)
        || current.cortana.identity_id.as_deref() != Some(identity_id)
        || current.cortana.generation != generation
        || current.cortana.terminal_id.as_deref() != Some(terminal_id)
        || current.cortana.harness.as_deref() != Some(harness)
        || current.cortana.owner.as_ref() != Some(&basis.owner)
        || current.cortana.active_harness_attestation != basis.active_harness_attestation
        || current
            .cortana
            .active_harness_attestation_recovery
            .is_some()
        || basis.prior_ledger_count != current.cortana.quarantine_ledger.len()
        || basis.prior_ledger_sha256
            != cortana_quarantine_ledger_sha256(&current.cortana.quarantine_ledger)
    {
        return false;
    }
    let claims = current
        .captains
        .iter()
        .filter(|captain| captain.role == FleetRole::Cortana && captain.state == ClaimState::Active)
        .collect::<Vec<_>>();
    if claims.len() != 1 {
        return false;
    }
    let claim = claims[0];
    if claim.ship_slug != basis.claim_ship_slug
        || claim.assignment_id != basis.claim_assignment_id
        || claim.terminal_id.as_deref() != Some(basis.claim_terminal_id.as_str())
        || claim.harness.as_deref() != Some(basis.claim_harness.as_str())
    {
        return false;
    }
    basis.workspace_ids
        == current
            .workspaces
            .iter()
            .filter(|workspace| workspace.tile_ids.iter().any(|tile| tile == terminal_id))
            .map(|workspace| workspace.id.clone())
            .collect::<Vec<_>>()
}

fn valid_cortana_python_tool(tool: &crate::cortana_reconcile::CortanaExecutableIdentity) -> bool {
    (tool.path == "/usr/bin/python3"
        || tool
            .path
            .strip_prefix("/usr/bin/python3.")
            .is_some_and(|version| {
                !version.is_empty()
                    && version
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || byte == b'.')
            }))
        && tool.device > 0
        && tool.inode > 0
}

fn valid_cortana_managed_owner(owner: &crate::cortana_reconcile::CortanaManagedOwnerToken) -> bool {
    let lowercase_hex_32 = |value: &str| {
        value.len() == 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    let valid_tool = |tool: &crate::cortana_reconcile::CortanaExecutableIdentity,
                      basename: &str| {
        matches!(
            tool.path.as_str(),
            "/usr/bin/systemctl" | "/usr/bin/systemd-run"
        ) && tool.path.ends_with(basename)
            && tool.device > 0
            && tool.inode > 0
    };
    let expected_suffix = format!("/app.slice/{}", owner.unit_name);
    owner.version == crate::cortana_reconcile::MANAGED_OWNER_TOKEN_VERSION
        && owner.unit_name.starts_with("t-hub-")
        && owner.unit_name.ends_with(".scope")
        && lowercase_hex_32(
            owner
                .unit_name
                .strip_prefix("t-hub-")
                .and_then(|value| value.strip_suffix(".scope"))
                .unwrap_or_default(),
        )
        && lowercase_hex_32(&owner.invocation_id)
        && lowercase_hex_32(&owner.launch_nonce)
        && owner.cgroup_path.starts_with("/user.slice/user-")
        && owner.cgroup_path.ends_with(&expected_suffix)
        && owner.cgroup_path.rsplit('/').next() == Some(owner.unit_name.as_str())
        && !owner.cgroup_path.split('/').any(|part| part == "..")
        && owner.cgroup_inode > 0
        && owner.launcher_pid > 0
        && owner.launcher_start_ticks > 0
        && owner.launcher_pid == owner.tmux.pane_pid
        && owner.launcher_start_ticks == owner.tmux.pane_start_ticks
        && valid_cortana_python_tool(&owner.tools.python)
        && valid_tool(&owner.tools.systemctl, "/systemctl")
        && valid_tool(&owner.tools.systemd_run, "/systemd-run")
        && valid_cortana_effect_identity(&owner.tmux)
}

fn valid_cortana_managed_launch(
    launch: &crate::cortana_reconcile::CortanaManagedLaunchIntent,
) -> bool {
    let nonce_valid = launch.launch_nonce.len() == 32
        && launch
            .launch_nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    let valid_tool = |tool: &crate::cortana_reconcile::CortanaExecutableIdentity, path: &str| {
        tool.path == path && tool.device > 0 && tool.inode > 0
    };
    let phase_valid = match launch.version {
        1 => {
            launch.expected_harness_launch_provenance.is_none()
                && launch.harness_process.is_none()
                && matches!(
                    launch.phase,
                    crate::cortana_reconcile::CortanaManagedLaunchPhase::Prepared
                        | crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
                )
        }
        2 => {
            launch.expected_harness_launch_provenance.is_none()
                && match launch.phase {
                    crate::cortana_reconcile::CortanaManagedLaunchPhase::Prepared
                    | crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved => {
                        launch.harness_process.is_none()
                    }
                    crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed => launch
                        .harness_process
                        .as_ref()
                        .is_some_and(crate::harness::valid_harness_process_identity),
                    crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed => false,
                }
        }
        3 => {
            launch
                .expected_harness_launch_provenance
                .as_ref()
                .is_some_and(|expected| {
                    expected.provider == launch.harness
                        && expected.version == 1
                        && crate::harness::valid_expected_harness_launch_provenance(expected)
                })
                && match launch.phase {
                    crate::cortana_reconcile::CortanaManagedLaunchPhase::Prepared
                    | crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved => {
                        launch.harness_process.is_none()
                    }
                    crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed => launch
                        .harness_process
                        .as_ref()
                        .is_some_and(crate::harness::valid_harness_process_identity),
                    crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed => false,
                }
        }
        4 => {
            launch
                .expected_harness_launch_provenance
                .as_ref()
                .is_some_and(|expected| {
                    expected.provider == launch.harness
                        && matches!(
                            expected.version,
                            2 | crate::harness::EXPECTED_HARNESS_LAUNCH_PROVENANCE_VERSION
                        )
                        && crate::harness::valid_expected_harness_launch_provenance(expected)
                })
                && match launch.phase {
                    crate::cortana_reconcile::CortanaManagedLaunchPhase::Prepared
                    | crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved => {
                        launch.harness_process.is_none()
                    }
                    crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
                    | crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed => launch
                        .harness_process
                        .as_ref()
                        .is_some_and(crate::harness::valid_harness_process_identity),
                }
        }
        _ => false,
    };
    phase_valid
        && !launch.operation_id.trim().is_empty()
        && exact_cortana_tmux_target(&launch.terminal_id)
            .is_ok_and(|target| target == launch.tmux_target)
        && !launch.identity_id.trim().is_empty()
        && launch.generation > 0
        && matches!(launch.harness.as_str(), "codex" | "claude")
        && nonce_valid
        && launch.unit_name == format!("t-hub-{}.scope", launch.launch_nonce)
        && valid_cortana_python_tool(&launch.tools.python)
        && valid_tool(&launch.tools.systemctl, "/usr/bin/systemctl")
        && valid_tool(&launch.tools.systemd_run, "/usr/bin/systemd-run")
}

fn valid_cortana_active_harness_attestation(
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    attestation: &crate::cortana_reconcile::CortanaActiveHarnessAttestation,
) -> bool {
    let Some(owner) = durable.owner.as_ref() else {
        return false;
    };
    let Some(harness) = durable.harness.as_deref() else {
        return false;
    };
    let expected = &attestation.expected_launch_provenance;
    let process = &attestation.process;
    let expected_process_executable = expected
        .trusted_child_executable
        .as_ref()
        .unwrap_or(&expected.executable);
    attestation.version == crate::cortana_reconcile::ACTIVE_HARNESS_ATTESTATION_VERSION
        && crate::harness::valid_expected_harness_launch_provenance(expected)
        && crate::harness::valid_harness_process_identity(process)
        && durable.identity_id.is_some()
        && durable.terminal_id.is_some()
        && expected.provider == harness
        && process.provider == harness
        && &process.executable == expected_process_executable
        && process.tmux_session_id == owner.tmux.tmux_session_id
        && process.tmux_session_created == owner.tmux.tmux_session_created
        && process.tmux_window_id == owner.tmux.tmux_window_id
        && process.tmux_pane_id == owner.tmux.tmux_pane_id
        && process.pane_pid == owner.tmux.pane_pid
        && process.pane_start_ticks == owner.tmux.pane_start_ticks
        && process.cgroup_path == owner.cgroup_path
}

fn valid_cortana_active_harness_attestation_recovery(
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    recovery: &crate::cortana_reconcile::CortanaActiveHarnessAttestationRecovery,
) -> bool {
    let Some(owner) = durable.owner.as_ref() else {
        return false;
    };
    let expected_process_executable = recovery
        .expected_launch_provenance
        .trusted_child_executable
        .as_ref()
        .unwrap_or(&recovery.expected_launch_provenance.executable);
    recovery.version == crate::cortana_reconcile::ACTIVE_HARNESS_ATTESTATION_RECOVERY_VERSION
        && !recovery.operation_id.trim().is_empty()
        && durable.identity_id.as_deref() == Some(recovery.identity_id.as_str())
        && durable.generation == recovery.generation
        && durable.terminal_id.as_deref() == Some(recovery.terminal_id.as_str())
        && durable.harness.as_deref() == Some(recovery.harness.as_str())
        && durable.active_harness_attestation.is_none()
        && durable.managed_launch.is_none()
        && matches!(
            &durable.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::Recovering {
                operation_id,
                ..
            } if operation_id == &recovery.operation_id
        )
        && crate::harness::valid_expected_harness_launch_provenance(
            &recovery.expected_launch_provenance,
        )
        && recovery.expected_launch_provenance.provider == recovery.harness
        && crate::harness::valid_harness_process_identity(&recovery.process)
        && recovery.process.provider == recovery.harness
        && &recovery.process.executable == expected_process_executable
        && recovery.process.tmux_session_id == owner.tmux.tmux_session_id
        && recovery.process.tmux_session_created == owner.tmux.tmux_session_created
        && recovery.process.tmux_window_id == owner.tmux.tmux_window_id
        && recovery.process.tmux_pane_id == owner.tmux.tmux_pane_id
        && recovery.process.pane_pid == owner.tmux.pane_pid
        && recovery.process.pane_start_ticks == owner.tmux.pane_start_ticks
        && recovery.process.cgroup_path == owner.cgroup_path
}

#[cfg(test)]
fn synthetic_cortana_managed_owner() -> crate::cortana_reconcile::CortanaManagedOwnerToken {
    let tmux = crate::cortana_reconcile::CortanaOrphanEffectIdentity {
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
    };
    crate::cortana_reconcile::CortanaManagedOwnerToken {
        version: crate::cortana_reconcile::MANAGED_OWNER_TOKEN_VERSION,
        unit_name: format!("t-hub-{}.scope", "a".repeat(32)),
        invocation_id: "b".repeat(32),
        cgroup_path: format!(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/t-hub-{}.scope",
            "a".repeat(32)
        ),
        cgroup_inode: 1,
        launcher_pid: tmux.pane_pid,
        launcher_start_ticks: tmux.pane_start_ticks,
        launch_nonce: "a".repeat(32),
        tools: crate::cortana_reconcile::CortanaManagedSystemTools {
            python: crate::cortana_reconcile::CortanaExecutableIdentity {
                path: "/usr/bin/python3.12".into(),
                device: 1,
                inode: 3,
            },
            systemctl: crate::cortana_reconcile::CortanaExecutableIdentity {
                path: "/usr/bin/systemctl".into(),
                device: 1,
                inode: 1,
            },
            systemd_run: crate::cortana_reconcile::CortanaExecutableIdentity {
                path: "/usr/bin/systemd-run".into(),
                device: 1,
                inode: 2,
            },
        },
        tmux,
    }
}

#[cfg(test)]
fn synthetic_cortana_harness_process(
    owner: &crate::cortana_reconcile::CortanaManagedOwnerToken,
    harness: &str,
) -> crate::harness::HarnessProcessIdentity {
    crate::harness::HarnessProcessIdentity {
        version: crate::harness::HARNESS_PROCESS_IDENTITY_VERSION,
        provider: harness.into(),
        pid: owner.tmux.pane_pid,
        start_ticks: owner.tmux.pane_start_ticks,
        executable: crate::harness::HarnessExecutableIdentity {
            path: "/bin/sleep".into(),
            device: 1,
            inode: 1,
        },
        argv_sha256: format!("sha256:{}", "a".repeat(64)),
        process_group_id: owner.tmux.pane_process_group_id,
        process_session_id: owner.tmux.pane_process_session_id,
        tmux_session_id: owner.tmux.tmux_session_id,
        tmux_session_created: owner.tmux.tmux_session_created,
        tmux_window_id: owner.tmux.tmux_window_id,
        tmux_pane_id: owner.tmux.tmux_pane_id,
        pane_pid: owner.tmux.pane_pid,
        pane_start_ticks: owner.tmux.pane_start_ticks,
        ancestry: vec![crate::harness::HarnessProcessAncestor {
            pid: owner.tmux.pane_pid,
            start_ticks: owner.tmux.pane_start_ticks,
        }],
        cgroup_path: owner.cgroup_path.clone(),
        session_token_sha256: format!("sha256:{}", "b".repeat(64)),
    }
}

#[cfg(test)]
fn synthetic_cortana_expected_harness_launch(
    harness: &str,
) -> crate::harness::ExpectedHarnessLaunchProvenance {
    let codex_arguments = vec![
        "--sandbox".into(),
        "read-only".into(),
        "-c".into(),
        crate::harness::CORTANA_CODEX_TOOL_APPROVAL_OVERRIDE.into(),
        "restore".into(),
    ];
    crate::harness::ExpectedHarnessLaunchProvenance {
        version: crate::harness::EXPECTED_HARNESS_LAUNCH_PROVENANCE_VERSION,
        provider: harness.into(),
        kind: "direct".into(),
        executable: crate::harness::HarnessExecutableIdentity {
            path: "/bin/sleep".into(),
            device: 1,
            inode: 1,
        },
        entry_script: None,
        trusted_child_executable: None,
        argv_layout_sha256: None,
        launch_policy_sha256: (harness == "codex")
            .then(crate::harness::cortana_codex_launch_policy_sha256),
        semantic_argv_sha256: (harness == "codex")
            .then(|| crate::harness::cortana_codex_semantic_argv_sha256(&codex_arguments)),
    }
}

fn durable_cortana_effect_identity(
    identity: tmux::SessionEffectIdentity,
) -> crate::cortana_reconcile::CortanaOrphanEffectIdentity {
    crate::cortana_reconcile::CortanaOrphanEffectIdentity {
        tmux_session_id: identity.tmux_session_id,
        tmux_session_created: identity.tmux_session_created,
        tmux_window_id: identity.tmux_window_id,
        tmux_pane_id: identity.tmux_pane_id,
        pane_pid: identity.pane_pid,
        pane_start_ticks: identity.pane_start_ticks,
        pane_process_group_id: identity.pane_process_group_id,
        pane_process_session_id: identity.pane_process_session_id,
        foreground_pid: identity.foreground_pid,
        foreground_start_ticks: identity.foreground_start_ticks,
        foreground_process_group_id: identity.foreground_process_group_id,
        foreground_process_session_id: identity.foreground_process_session_id,
    }
}

fn tmux_cortana_effect_identity(
    identity: &crate::cortana_reconcile::CortanaOrphanEffectIdentity,
) -> tmux::SessionEffectIdentity {
    tmux::SessionEffectIdentity {
        tmux_session_id: identity.tmux_session_id,
        tmux_session_created: identity.tmux_session_created,
        tmux_window_id: identity.tmux_window_id,
        tmux_pane_id: identity.tmux_pane_id,
        pane_pid: identity.pane_pid,
        pane_start_ticks: identity.pane_start_ticks,
        pane_process_group_id: identity.pane_process_group_id,
        pane_process_session_id: identity.pane_process_session_id,
        foreground_pid: identity.foreground_pid,
        foreground_start_ticks: identity.foreground_start_ticks,
        foreground_process_group_id: identity.foreground_process_group_id,
        foreground_process_session_id: identity.foreground_process_session_id,
    }
}

fn durable_cortana_owner(
    owner: tmux::ManagedRuntimeOwnerToken,
) -> crate::cortana_reconcile::CortanaManagedOwnerToken {
    let tools = durable_cortana_tools(&owner.tools);
    crate::cortana_reconcile::CortanaManagedOwnerToken {
        version: owner.version,
        unit_name: owner.unit_name,
        invocation_id: owner.invocation_id,
        cgroup_path: owner.cgroup_path,
        cgroup_inode: owner.cgroup_inode,
        launcher_pid: owner.launcher_pid,
        launcher_start_ticks: owner.launcher_start_ticks,
        launch_nonce: owner.launch_nonce,
        tools,
        tmux: durable_cortana_effect_identity(owner.tmux),
    }
}

fn durable_cortana_tools(
    tools: &tmux::ManagedSystemTools,
) -> crate::cortana_reconcile::CortanaManagedSystemTools {
    let durable_tool = |tool: &tmux::ManagedExecutableIdentity| {
        crate::cortana_reconcile::CortanaExecutableIdentity {
            path: tool.path.clone(),
            device: tool.device,
            inode: tool.inode,
        }
    };
    crate::cortana_reconcile::CortanaManagedSystemTools {
        python: durable_tool(&tools.python),
        systemctl: durable_tool(&tools.systemctl),
        systemd_run: durable_tool(&tools.systemd_run),
    }
}

fn tmux_cortana_launch(
    launch: &crate::cortana_reconcile::CortanaManagedLaunchIntent,
) -> tmux::ManagedRuntimeLaunchSpec {
    let tmux_tool = |tool: &crate::cortana_reconcile::CortanaExecutableIdentity| {
        tmux::ManagedExecutableIdentity {
            path: tool.path.clone(),
            device: tool.device,
            inode: tool.inode,
        }
    };
    tmux::ManagedRuntimeLaunchSpec {
        unit_name: launch.unit_name.clone(),
        launch_nonce: launch.launch_nonce.clone(),
        tools: tmux::ManagedSystemTools {
            python: tmux_tool(&launch.tools.python),
            systemctl: tmux_tool(&launch.tools.systemctl),
            systemd_run: tmux_tool(&launch.tools.systemd_run),
        },
    }
}

fn cleanup_cortana_managed_launch(
    ctx: &ControlContext,
    launch: &crate::cortana_reconcile::CortanaManagedLaunchIntent,
    owner: Option<&tmux::ManagedRuntimeOwnerToken>,
) -> Result<(), String> {
    match owner {
        Some(owner) => {
            tmux::retire_managed_runtime(&launch.tmux_target, owner).map_err(|error| {
                cortana_tmux_observation_error("exact observed owner cleanup failed", error)
            })?
        }
        None => tmux::retire_prepared_managed_runtime(&tmux_cortana_launch(launch)).map_err(
            |error| cortana_tmux_observation_error("exact prepared owner cleanup failed", error),
        )?,
    }
    ctx.captains.clear_prepared_cortana_managed_launch(launch)?;
    Ok(())
}

fn tmux_cortana_owner(
    owner: &crate::cortana_reconcile::CortanaManagedOwnerToken,
) -> tmux::ManagedRuntimeOwnerToken {
    let tmux_tool = |tool: &crate::cortana_reconcile::CortanaExecutableIdentity| {
        tmux::ManagedExecutableIdentity {
            path: tool.path.clone(),
            device: tool.device,
            inode: tool.inode,
        }
    };
    tmux::ManagedRuntimeOwnerToken {
        version: owner.version,
        unit_name: owner.unit_name.clone(),
        invocation_id: owner.invocation_id.clone(),
        cgroup_path: owner.cgroup_path.clone(),
        cgroup_inode: owner.cgroup_inode,
        launcher_pid: owner.launcher_pid,
        launcher_start_ticks: owner.launcher_start_ticks,
        launch_nonce: owner.launch_nonce.clone(),
        tools: tmux::ManagedSystemTools {
            python: tmux_tool(&owner.tools.python),
            systemctl: tmux_tool(&owner.tools.systemctl),
            systemd_run: tmux_tool(&owner.tools.systemd_run),
        },
        tmux: tmux_cortana_effect_identity(&owner.tmux),
    }
}

fn stale_legacy_cortana_control_env(
    control_file: Option<&str>,
    control_addr: Option<&str>,
    control_token: Option<&str>,
    current_addr: &str,
    current_token: &str,
) -> bool {
    control_file.is_none()
        && !current_addr.is_empty()
        && !current_token.is_empty()
        && control_addr.is_some_and(|addr| !addr.is_empty() && addr != current_addr)
        && control_token
            .is_some_and(|token| !token.is_empty() && !ct_token_eq(token, current_token))
}

fn discover_cortana_runtimes(
    ctx: &ControlContext,
    home: &str,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
) -> Result<Vec<crate::cortana_reconcile::CortanaRuntimeCandidate>, String> {
    let canonical_control_file = discovery_file_for_spawn();
    let panes = tmux::pane_info().map_err(|error| {
        retryable_error(format!(
            "reconcile_cortana: terminal inspection failed: {error}"
        ))
    })?;
    let mut by_terminal = std::collections::BTreeMap::new();
    for pane in panes {
        if pane.cwd.trim_end_matches('/') != home.trim_end_matches('/') {
            continue;
        }
        let terminal_id = pane
            .session
            .strip_prefix("th_")
            .ok_or_else(|| {
                format!(
                    "reconcile_cortana: reserved-scope runtime session '{}' is not an exact T-Hub terminal target",
                    pane.session
                )
            })?
            .to_string();
        exact_cortana_tmux_target(&terminal_id)?;
        if by_terminal.contains_key(&terminal_id) {
            continue;
        }
        let detected = pane.command.trim().to_ascii_lowercase();
        let harness = if matches!(detected.as_str(), "codex" | "claude") {
            detected
        } else {
            durable
                .harness
                .clone()
                .or_else(|| detected_harness(&terminal_id))
                .unwrap_or_else(|| "codex".into())
        };
        let session_token =
            tmux::session_environment(&pane.session, crate::identity::SESSION_TOKEN_ENV).map_err(
                |error| {
                    retryable_error(format!(
                    "reconcile_cortana: identity inspection failed for '{terminal_id}': {error}"
                ))
                },
            )?;
        let identity = session_token
            .as_deref()
            .and_then(|token| ctx.identity.resolve(token));
        let unresolved_session_bearer = session_token
            .as_deref()
            .is_some_and(|token| !token.is_empty())
            && identity.is_none();
        let trusted_cortana_identity = identity
            .as_ref()
            .is_some_and(|identity| identity.role == crate::identity::Role::Cortana);
        let identity_bound_to_terminal = identity
            .as_ref()
            .and_then(|identity| identity.session_tile.as_deref())
            == Some(terminal_id.as_str());
        let control_file = tmux::session_environment(&pane.session, "T_HUB_CONTROL_FILE")
            .map_err(|error| {
                retryable_error(format!(
                    "reconcile_cortana: control discovery inspection failed for '{terminal_id}': {error}"
                ))
            })?;
        let control_addr = tmux::session_environment(&pane.session, "T_HUB_CONTROL_ADDR")
            .map_err(|error| {
                retryable_error(format!(
                    "reconcile_cortana: rotating address inspection failed for '{terminal_id}': {error}"
                ))
            })?;
        let control_token = tmux::session_environment(&pane.session, "T_HUB_CONTROL_TOKEN")
            .map_err(|error| {
                retryable_error(format!(
                    "reconcile_cortana: rotating token inspection failed for '{terminal_id}': {error}"
                ))
            })?;
        let stale_legacy_control_env = stale_legacy_cortana_control_env(
            control_file.as_deref(),
            control_addr.as_deref(),
            control_token.as_deref(),
            &ctx.addr,
            &ctx.token,
        );
        let generation = tmux::session_environment(&pane.session, CORTANA_GENERATION_ENV)
            .map_err(|error| {
                retryable_error(format!(
                    "reconcile_cortana: generation inspection failed for '{terminal_id}': {error}"
                ))
            })?
            .and_then(|generation| generation.parse::<u64>().ok())
            .unwrap_or_default();
        let provider_session_id = if harness == "claude" {
            ctx.status.session_for_terminal(&terminal_id)
        } else {
            tmux::session_environment(&pane.session, "CODEX_THREAD_ID")
                .map_err(|error| {
                    retryable_error(format!(
                        "reconcile_cortana: provider identity inspection failed for '{terminal_id}': {error}"
                    ))
                })?
                .filter(|value| !value.trim().is_empty())
        };
        let target = tmux_target(&terminal_id);
        let effect_identity = tmux::observe_session_effect_identity(&target)
            .ok()
            .map(durable_cortana_effect_identity);
        by_terminal.insert(
            terminal_id.clone(),
            crate::cortana_reconcile::CortanaRuntimeCandidate {
                terminal_id,
                identity_id: identity.as_ref().map(|identity| identity.id.clone()),
                generation,
                harness: harness.clone(),
                provider_session_id,
                terminal: runtime_evidence(tmux::session_liveness(&target)),
                harness_process: runtime_evidence(tmux::harness_liveness(&target, &harness)),
                identity_bound_to_terminal,
                canonical_control_file: control_file.as_deref()
                    == Some(canonical_control_file.as_str()),
                rotating_control_env_scrubbed: control_addr.as_deref().is_none_or(str::is_empty)
                    && control_token.as_deref().is_none_or(str::is_empty),
                stale_legacy_control_env,
                unresolved_session_bearer,
                effect_identity,
                // A durable Cortana identity can reacquire scoped authority. The
                // rotating global token is intentionally absent from its env.
                current_control_capability: trusted_cortana_identity,
                trusted_cortana_identity,
            },
        );
    }
    for quarantine in &durable.quarantine_ledger {
        match by_terminal.get(&quarantine.terminal_id) {
            Some(candidate)
                if candidate.generation == quarantine.generation
                    && candidate.harness == quarantine.harness
                    && candidate.terminal == crate::cortana_reconcile::RuntimeEvidence::Alive
                    && candidate.harness_process
                        == crate::cortana_reconcile::RuntimeEvidence::Alive
                    && candidate.identity_id.is_none()
                    && !candidate.identity_bound_to_terminal
                    && candidate.unresolved_session_bearer
                    && candidate.effect_identity.as_ref() == Some(&quarantine.tmux)
                    && !candidate.current_control_capability
                    && !candidate.trusted_cortana_identity => {}
            Some(_) => {
                return Err(format!(
                    "reconcile_cortana: quarantined runtime '{}' changed identity, authority, liveness, or process generation",
                    quarantine.terminal_id
                ));
            }
            None => match tmux::session_liveness(&tmux_target(&quarantine.terminal_id)) {
                tmux::SessionLiveness::Gone => {}
                tmux::SessionLiveness::Alive => {
                    return Err(format!(
                        "reconcile_cortana: quarantined runtime '{}' moved outside its reserved scope",
                        quarantine.terminal_id
                    ));
                }
                tmux::SessionLiveness::Unknown => {
                    return Err(retryable_error(format!(
                        "reconcile_cortana: quarantined runtime '{}' has uncertain liveness",
                        quarantine.terminal_id
                    )));
                }
            },
        }
    }
    Ok(by_terminal
        .into_values()
        .filter(|candidate| {
            !durable
                .quarantine_ledger
                .iter()
                .any(|quarantine| quarantine.terminal_id == candidate.terminal_id)
        })
        .collect())
}

fn same_cortana_tmux_generation(
    observed: &crate::cortana_reconcile::CortanaOrphanEffectIdentity,
    expected: &crate::cortana_reconcile::CortanaOrphanEffectIdentity,
) -> bool {
    observed.tmux_session_id == expected.tmux_session_id
        && observed.tmux_session_created == expected.tmux_session_created
        && observed.tmux_window_id == expected.tmux_window_id
        && observed.tmux_pane_id == expected.tmux_pane_id
        && observed.pane_pid == expected.pane_pid
        && observed.pane_start_ticks == expected.pane_start_ticks
}

fn observed_launch_matches_recovery(
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    launch: &crate::cortana_reconcile::CortanaManagedLaunchIntent,
) -> bool {
    match &durable.recovery {
        crate::cortana_reconcile::CortanaRecoveryState::Recovering { operation_id, .. } => {
            operation_id == &launch.operation_id
                && launch.generation == durable.generation.saturating_add(1)
                && durable
                    .identity_id
                    .as_deref()
                    .is_none_or(|identity_id| identity_id == launch.identity_id)
                && durable
                    .harness
                    .as_deref()
                    .is_none_or(|harness| harness == launch.harness)
        }
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
            operation_id,
            orphan_terminal_id,
            orphan_generation,
            harness,
            replacement_identity_id,
            ..
        } => {
            operation_id == &launch.operation_id
                && replacement_identity_id.as_deref() == Some(launch.identity_id.as_str())
                && launch.generation == orphan_generation.saturating_add(1)
                && launch.harness == *harness
                && launch.terminal_id != *orphan_terminal_id
        }
        crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
            operation_id,
            legacy_terminal_id,
            legacy_generation,
            replacement_identity_id,
            ..
        } => {
            operation_id == &launch.operation_id
                && replacement_identity_id.as_deref() == Some(launch.identity_id.as_str())
                && launch.generation == legacy_generation.saturating_add(1)
                && launch.terminal_id != *legacy_terminal_id
                && durable
                    .quarantine_ledger
                    .iter()
                    .find(|quarantine| {
                        quarantine.terminal_id == *legacy_terminal_id
                            && quarantine.generation == *legacy_generation
                    })
                    .is_some_and(|quarantine| quarantine.harness == launch.harness)
        }
        _ => false,
    }
}

fn exact_observed_cortana_claim(
    claim: &CaptainRecord,
    launch: &crate::cortana_reconcile::CortanaManagedLaunchIntent,
) -> bool {
    claim.role == FleetRole::Cortana
        && claim.state == ClaimState::Active
        && claim.terminal_id.as_deref() == Some(launch.terminal_id.as_str())
        && claim.provider.as_deref() == Some(launch.harness.as_str())
        && claim.harness.as_deref() == Some(launch.harness.as_str())
}

fn exact_observed_cortana_candidate<'a>(
    candidates: &'a [crate::cortana_reconcile::CortanaRuntimeCandidate],
    launch: &crate::cortana_reconcile::CortanaManagedLaunchIntent,
    owner: &crate::cortana_reconcile::CortanaManagedOwnerToken,
) -> Option<&'a crate::cortana_reconcile::CortanaRuntimeCandidate> {
    candidates
        .iter()
        .find(|candidate| candidate.terminal_id == launch.terminal_id)
        .filter(|candidate| {
            candidates.len() == 1
                && candidate.identity_id.as_deref() == Some(launch.identity_id.as_str())
                && candidate.generation == launch.generation
                && candidate.harness == launch.harness
                && candidate.terminal == crate::cortana_reconcile::RuntimeEvidence::Alive
                && candidate.identity_bound_to_terminal
                && candidate.canonical_control_file
                && candidate.rotating_control_env_scrubbed
                && candidate.current_control_capability
                && candidate.trusted_cortana_identity
                && candidate.effect_identity.as_ref().is_some_and(|effect| {
                    valid_cortana_effect_identity(effect)
                        && same_cortana_tmux_generation(effect, &owner.tmux)
                })
        })
}

fn cortana_harness_attestation_scope<'a>(
    ctx: &ControlContext,
    launch: &'a crate::cortana_reconcile::CortanaManagedLaunchIntent,
) -> Result<
    (
        Harness,
        &'a crate::harness::ExpectedHarnessLaunchProvenance,
        String,
    ),
    String,
> {
    let identity = ctx.identity.get(&launch.identity_id).ok_or_else(|| {
        format!(
            "reconcile_cortana: managed launch identity '{}' is unavailable or revoked",
            launch.identity_id
        )
    })?;
    if identity.role != crate::identity::Role::Cortana
        || identity.session_tile.as_deref() != Some(launch.terminal_id.as_str())
    {
        return Err(
            "reconcile_cortana: managed launch identity is not exactly bound to its terminal"
                .into(),
        );
    }
    let harness = match launch.harness.as_str() {
        "codex" => Harness::Codex,
        "claude" => Harness::Claude,
        _ => return Err("reconcile_cortana: managed launch Harness is unsupported".into()),
    };
    let expected = launch
        .expected_harness_launch_provenance
        .as_ref()
        .ok_or("reconcile_cortana: managed launch has no expected Harness provenance")?;
    Ok((harness, expected, identity.secret))
}

fn observe_cortana_harness_process(
    ctx: &ControlContext,
    launch: &crate::cortana_reconcile::CortanaManagedLaunchIntent,
    owner: &crate::cortana_reconcile::CortanaManagedOwnerToken,
) -> Result<crate::harness::HarnessProcessIdentity, String> {
    let (harness, expected, secret) = cortana_harness_attestation_scope(ctx, launch)?;
    crate::harness::observe_scoped_harness_process(
        &launch.tmux_target,
        harness,
        expected,
        &launch.identity_id,
        &secret,
        &owner.cgroup_path,
        owner.tmux.pane_start_ticks,
        Instant::now() + Duration::from_secs(2),
    )
    .map_err(|error| {
        cortana_harness_observation_error(
            "reconcile_cortana: managed Harness process attestation failed",
            error,
        )
    })
}

fn revalidate_cortana_managed_owner_after_process_observation(
    ctx: &ControlContext,
    launch: &crate::cortana_reconcile::CortanaManagedLaunchIntent,
    owner: &crate::cortana_reconcile::CortanaManagedOwnerToken,
    process: crate::harness::HarnessProcessIdentity,
) -> Result<CortanaManagedObservationEvidence, String> {
    #[cfg(test)]
    ctx.captains.pause_dispatch(match launch.phase {
        crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved => {
            "cortana_before_owner_revalidation_owner_observed"
        }
        crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed => {
            "cortana_before_owner_revalidation_observed"
        }
        crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed => {
            "cortana_before_owner_revalidation_claimed"
        }
        _ => "cortana_before_owner_revalidation_invalid",
    });
    #[cfg(not(test))]
    let _ = ctx;
    tmux::revalidate_managed_runtime_owner(&launch.tmux_target, &tmux_cortana_owner(owner))
        .map_err(|error| {
            cortana_tmux_observation_error(
                "reconcile_cortana: managed launch owner changed after Harness observation",
                error,
            )
        })?;
    Ok(CortanaManagedObservationEvidence {
        process,
        owner: owner.clone(),
    })
}

const CORTANA_HARNESS_CONFIRM_INTERVAL: Duration = Duration::from_millis(100);
const CORTANA_HARNESS_REQUIRED_CONFIRMATIONS: usize = 2;
const CORTANA_HARNESS_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const CORTANA_HARNESS_MAX_ATTEMPTS: usize = 64;

/// Require a newly observed Harness process generation to remain identical
/// across a bounded startup window before its identity can enter the durable
/// managed-launch WAL. A script runtime with a prebound native child is only a
/// transitional wrapper and can never become the durable Harness identity.
/// Retrying that allowed transition lets the legitimate topology settle, while
/// any foreign foreground process fails immediately through scoped attestation.
fn observe_stable_cortana_harness_process(
    ctx: &ControlContext,
    launch: &crate::cortana_reconcile::CortanaManagedLaunchIntent,
    owner: &crate::cortana_reconcile::CortanaManagedOwnerToken,
) -> Result<crate::harness::HarnessProcessIdentity, String> {
    let (harness, expected, secret) = cortana_harness_attestation_scope(ctx, launch)?;
    let required_child = expected.trusted_child_executable.as_ref();
    let deadline = Instant::now() + CORTANA_HARNESS_STARTUP_TIMEOUT;
    let observe = || {
        crate::harness::observe_scoped_harness_process(
            &launch.tmux_target,
            harness,
            expected,
            &launch.identity_id,
            &secret,
            &owner.cgroup_path,
            owner.tmux.pane_start_ticks,
            deadline,
        )
    };
    let retryable = |error| {
        matches!(
            error,
            crate::harness::LaunchAttestationError::UnreadableEvidence
                | crate::harness::LaunchAttestationError::ProcessChanged
                | crate::harness::LaunchAttestationError::AncestryChanged
        )
    };
    let mut baseline = None;
    let mut confirmations = 0;
    for _ in 0..CORTANA_HARNESS_MAX_ATTEMPTS {
        if Instant::now() >= deadline {
            break;
        }
        match observe() {
            Ok(observed) => {
                #[cfg(test)]
                ctx.captains
                    .pause_dispatch("cortana_harness_stability_observed");
                if required_child.is_some_and(|child| &observed.executable != child) {
                    baseline = None;
                    confirmations = 0;
                } else if baseline.as_ref() == Some(&observed) {
                    confirmations += 1;
                    if confirmations == CORTANA_HARNESS_REQUIRED_CONFIRMATIONS {
                        return Ok(observed);
                    }
                } else {
                    baseline = Some(observed);
                    confirmations = 0;
                }
            }
            Err(error) if retryable(error) => {
                baseline = None;
                confirmations = 0;
            }
            Err(error) => {
                return Err(format!(
                    "reconcile_cortana: managed Harness process attestation failed: {error}"
                ));
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(CORTANA_HARNESS_CONFIRM_INTERVAL.min(remaining));
    }
    Err(
        "reconcile_cortana: managed Harness process did not reach a stable startup generation"
            .into(),
    )
}

fn cortana_startup_command(
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    args: &Value,
    harness: Harness,
) -> String {
    let anchor = durable
        .provider_session_id
        .as_deref()
        .or(durable.conversation_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let command = anchor.map_or_else(
        || {
            harness.adapter().fresh_cortana_argv(
                "You are Cortana, T-Hub's singleton supervisor. First call cortana_bootstrap to restore the bounded durable fleet checkpoint and ship resume points. Delegate administrative execution and preserve ship boundaries.",
            )
        },
        |provider_session_id| {
            harness
                .adapter()
                .resume_cortana_argv(provider_session_id)
        },
    );
    #[cfg(test)]
    return arg_str(args, "testStartupCommand").unwrap_or(command);
    #[cfg(not(test))]
    {
        let _ = args;
        command
    }
}

fn resolve_cortana_expected_harness_launch(
    command: &str,
    harness: Harness,
    args: &Value,
) -> Result<crate::harness::ExpectedHarnessLaunchProvenance, String> {
    #[cfg(not(test))]
    let _ = args;
    let expected = crate::harness::resolve_expected_harness_launch_provenance(command, harness)
        .map_err(|error| format!("configured Harness launch provenance is untrusted: {error}"))?;
    if harness == Harness::Codex
        && expected.launch_policy_sha256.as_deref()
            != Some(crate::harness::cortana_codex_launch_policy_sha256().as_str())
    {
        #[cfg(test)]
        if args.get("testStartupCommand").is_some() {
            return Ok(expected);
        }
        return Err(
            "configured Codex launch does not grant exactly the cortana_bootstrap approval policy"
                .into(),
        );
    }
    Ok(expected)
}

#[cfg(all(test, unix))]
fn attest_cortana_managed_harness(
    ctx: &ControlContext,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
    let launch = durable
        .managed_launch
        .as_ref()
        .ok_or("reconcile_cortana: Harness attestation lost its managed launch")?;
    let owner = durable
        .owner
        .as_ref()
        .ok_or("reconcile_cortana: Harness attestation lost its managed owner")?;
    tmux::revalidate_managed_runtime_owner(&launch.tmux_target, &tmux_cortana_owner(owner))
        .map_err(|error| {
            cortana_tmux_observation_error(
                "reconcile_cortana: managed launch owner is unverifiable",
                error,
            )
        })?;
    let mut current = durable.clone();
    if launch.harness_process.is_none() {
        let observed = observe_stable_cortana_harness_process(ctx, launch, owner)?;
        current = ctx.captains.record_cortana_harness_process(
            &launch.operation_id,
            &launch.terminal_id,
            observed,
        )?;
    }
    let current_launch = current
        .managed_launch
        .as_ref()
        .ok_or("reconcile_cortana: attested managed launch disappeared")?;
    let expected = current_launch
        .harness_process
        .as_ref()
        .ok_or("reconcile_cortana: managed launch remains unattested")?;
    let observed = observe_cortana_harness_process(ctx, current_launch, owner)?;
    if &observed != expected {
        return Err(
            "reconcile_cortana: managed Harness process identity changed after attestation".into(),
        );
    }
    Ok(current)
}

fn attest_cortana_managed_harness_from_observation(
    ctx: &ControlContext,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    observation: Option<&CortanaReconcileObservation>,
) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
    let observed = managed_process_from_observation(observation, durable)?;
    let launch = durable
        .managed_launch
        .as_ref()
        .ok_or("reconcile_cortana: Harness attestation lost its managed launch")?;
    ctx.captains
        .record_cortana_harness_process(&launch.operation_id, &launch.terminal_id, observed)
}

fn revalidate_active_cortana_authority(
    ctx: &ControlContext,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
) -> Result<(), String> {
    let identity_id = durable
        .identity_id
        .as_deref()
        .ok_or("active Cortana authority has no durable identity")?;
    let terminal_id = durable
        .terminal_id
        .as_deref()
        .ok_or("active Cortana authority has no durable terminal")?;
    let harness = match durable.harness.as_deref() {
        Some("codex") => Harness::Codex,
        Some("claude") => Harness::Claude,
        _ => return Err("active Cortana authority has an unsupported Harness".into()),
    };
    let owner = durable
        .owner
        .as_ref()
        .ok_or("active Cortana authority has no managed owner")?;
    let attestation = durable
        .active_harness_attestation
        .as_ref()
        .ok_or("active Cortana authority has no Harness attestation")?;
    if !valid_cortana_active_harness_attestation(durable, attestation) {
        return Err("active Cortana Harness attestation is structurally invalid".into());
    }
    #[cfg(test)]
    if attestation.expected_launch_provenance
        == synthetic_cortana_expected_harness_launch(harness.as_provider())
        && attestation.process == synthetic_cortana_harness_process(owner, harness.as_provider())
    {
        return Ok(());
    }
    let identity = ctx
        .identity
        .get(identity_id)
        .filter(|identity| {
            identity.role == crate::identity::Role::Cortana
                && identity.session_tile.as_deref() == Some(terminal_id)
        })
        .ok_or("active Cortana identity binding changed")?;
    let target = tmux_target(terminal_id);
    tmux::revalidate_managed_runtime_owner(&target, &tmux_cortana_owner(owner)).map_err(
        |error| cortana_tmux_observation_error("active Cortana managed owner changed", error),
    )?;
    let observed = crate::harness::observe_scoped_harness_process(
        &target,
        harness,
        &attestation.expected_launch_provenance,
        identity_id,
        &identity.secret,
        &owner.cgroup_path,
        owner.tmux.pane_start_ticks,
        Instant::now() + Duration::from_secs(1),
    )
    .map_err(|error| {
        cortana_harness_observation_error("active Cortana Harness attestation failed", error)
    })?;
    if observed == attestation.process {
        Ok(())
    } else {
        Err("active Cortana Harness process identity changed".into())
    }
}

fn revalidate_unresolved_cortana_attestation(
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
) -> Result<(), String> {
    let Some(attestation) = durable.active_harness_attestation.as_ref() else {
        return Ok(());
    };
    let identity_id = durable
        .identity_id
        .as_deref()
        .ok_or("unresolved Cortana attestation has no durable identity")?;
    let terminal_id = durable
        .terminal_id
        .as_deref()
        .ok_or("unresolved Cortana attestation has no durable terminal")?;
    let harness = match durable.harness.as_deref() {
        Some("codex") => Harness::Codex,
        Some("claude") => Harness::Claude,
        _ => return Err("unresolved Cortana attestation has an unsupported Harness".into()),
    };
    let owner = durable
        .owner
        .as_ref()
        .ok_or("unresolved Cortana attestation has no managed owner")?;
    if !valid_cortana_active_harness_attestation(durable, attestation) {
        return Err("unresolved Cortana Harness attestation is structurally invalid".into());
    }
    let target = tmux_target(terminal_id);
    tmux::revalidate_managed_runtime_owner(&target, &tmux_cortana_owner(owner)).map_err(
        |error| cortana_tmux_observation_error("unresolved Cortana managed owner changed", error),
    )?;
    let bearer = tmux::session_environment(&target, crate::identity::SESSION_TOKEN_ENV)
        .map_err(|error| {
            cortana_tmux_observation_error("unresolved Cortana bearer inspection failed", error)
        })?
        .filter(|bearer| !bearer.is_empty())
        .ok_or("unresolved Cortana runtime has no retained session bearer")?;
    let observed = crate::harness::observe_scoped_harness_process(
        &target,
        harness,
        &attestation.expected_launch_provenance,
        identity_id,
        &bearer,
        &owner.cgroup_path,
        owner.tmux.pane_start_ticks,
        Instant::now() + Duration::from_secs(1),
    )
    .map_err(|error| {
        cortana_harness_observation_error("unresolved Cortana Harness attestation failed", error)
    })?;
    if observed == attestation.process {
        Ok(())
    } else {
        Err("unresolved Cortana Harness process identity changed".into())
    }
}

fn observe_live_cortana_with_expected_provenance(
    ctx: &ControlContext,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    expected: &crate::harness::ExpectedHarnessLaunchProvenance,
    deadline: Instant,
) -> Result<crate::harness::HarnessProcessIdentity, String> {
    let identity_id = durable
        .identity_id
        .as_deref()
        .ok_or("live Cortana has no durable identity")?;
    let terminal_id = durable
        .terminal_id
        .as_deref()
        .ok_or("live Cortana has no durable terminal")?;
    let harness = match durable.harness.as_deref() {
        Some("codex") => Harness::Codex,
        Some("claude") => Harness::Claude,
        _ => return Err("live Cortana has an unsupported Harness".into()),
    };
    if expected.provider != harness.as_provider() {
        return Err("configured Cortana provenance uses a different Harness".into());
    }
    let owner = durable
        .owner
        .as_ref()
        .ok_or("live Cortana has no managed owner")?;
    let identity = ctx
        .identity
        .get(identity_id)
        .filter(|identity| {
            identity.role == crate::identity::Role::Cortana
                && identity.session_tile.as_deref() == Some(terminal_id)
        })
        .ok_or("live Cortana identity binding changed")?;
    let target = tmux_target(terminal_id);
    tmux::revalidate_managed_runtime_owner(&target, &tmux_cortana_owner(owner)).map_err(
        |error| cortana_tmux_observation_error("live Cortana managed owner changed", error),
    )?;
    crate::harness::observe_scoped_harness_process(
        &target,
        harness,
        expected,
        identity_id,
        &identity.secret,
        &owner.cgroup_path,
        owner.tmux.pane_start_ticks,
        deadline,
    )
    .map_err(|error| {
        cortana_harness_observation_error("live Cortana Harness attestation failed", error)
    })
}

fn finalize_cortana_active_attestation_recovery_observation(
    ctx: &ControlContext,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
) -> Result<
    (
        crate::harness::ExpectedHarnessLaunchProvenance,
        crate::harness::HarnessProcessIdentity,
    ),
    String,
> {
    let recovery = durable
        .active_harness_attestation_recovery
        .as_ref()
        .ok_or("Cortana active attestation recovery WAL disappeared")?;
    let observed = observe_live_cortana_with_expected_provenance(
        ctx,
        durable,
        &recovery.expected_launch_provenance,
        Instant::now() + Duration::from_secs(5),
    )?;
    if observed != recovery.process {
        return Err("Cortana process changed during active attestation recovery".into());
    }
    Ok((recovery.expected_launch_provenance.clone(), observed))
}

fn quarantine_unattested_cortana_incumbent(
    ctx: &ControlContext,
    operation_id: &str,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    candidate: &crate::cortana_reconcile::CortanaRuntimeCandidate,
    reason: &str,
) -> Result<(), String> {
    let identity_id = durable
        .identity_id
        .as_deref()
        .filter(|identity| candidate.identity_id.as_deref() == Some(*identity))
        .ok_or("unattested Cortana incumbent identity is ambiguous")?;
    let terminal_id = durable
        .terminal_id
        .as_deref()
        .filter(|terminal| candidate.terminal_id == **terminal)
        .ok_or("unattested Cortana incumbent terminal is ambiguous")?;
    let harness = durable
        .harness
        .as_deref()
        .filter(|harness| candidate.harness == **harness)
        .ok_or("unattested Cortana incumbent Harness is ambiguous")?;
    let effect = candidate
        .effect_identity
        .filter(valid_cortana_effect_identity)
        .ok_or("unattested Cortana incumbent has no exact tmux generation")?;
    ctx.identity.revoke(identity_id)?;
    if ctx.identity.get(identity_id).is_some() || !ctx.identity.is_revoked(identity_id) {
        return Err("unattested Cortana incumbent bearer revocation was ambiguous".into());
    }
    ctx.captains.quarantine_legacy_cortana(
        operation_id,
        terminal_id,
        identity_id,
        durable.generation,
        harness,
        effect,
    )?;
    ctx.tabs.retire_tile_locked(terminal_id);
    let _ = ctx.captains.remove_session(terminal_id)?;
    audit_cortana_runtime_mutation(
        ctx,
        "quarantine-unattested-no-signal",
        operation_id,
        &[terminal_id.to_string()],
        &[],
        Some(reason),
    );
    Ok(())
}

fn finalize_observed_cortana_launch(
    ctx: &ControlContext,
    operation_id: &str,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    candidates: &[crate::cortana_reconcile::CortanaRuntimeCandidate],
    observation: Option<&CortanaReconcileObservation>,
) -> Result<Value, String> {
    let launch = durable
        .managed_launch
        .as_ref()
        .filter(|launch| {
            matches!(
                launch.phase,
                crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
                    | crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed
            )
        })
        .ok_or("reconcile_cortana: observed launch completion lost its durable WAL")?;
    if launch.operation_id != operation_id
        || durable.terminal_id.as_deref() != Some(launch.terminal_id.as_str())
        || !observed_launch_matches_recovery(durable, launch)
    {
        return Err(
            "reconcile_cortana: observed launch does not match its durable recovery intent".into(),
        );
    }
    let owner = durable
        .owner
        .as_ref()
        .ok_or("reconcile_cortana: observed launch lost its durable owner")?;
    if owner.unit_name != launch.unit_name || owner.launch_nonce != launch.launch_nonce {
        return Err("reconcile_cortana: observed launch and managed owner disagree".into());
    }
    let identity = ctx.identity.get(&launch.identity_id).ok_or_else(|| {
        format!(
            "reconcile_cortana: observed launch identity '{}' is unavailable or revoked",
            launch.identity_id
        )
    })?;
    if identity.role != crate::identity::Role::Cortana
        || identity.session_tile.as_deref() != Some(launch.terminal_id.as_str())
    {
        return Err(
            "reconcile_cortana: observed launch identity is not exactly bound to its terminal"
                .into(),
        );
    }
    let observed_process = managed_process_from_observation(observation, durable)?;
    if launch.harness_process.as_ref() != Some(&observed_process) {
        return Err("reconcile_cortana: observed launch process changed before commit".into());
    }
    let candidate = exact_observed_cortana_candidate(candidates, launch, owner).ok_or(
        "reconcile_cortana: observed launch runtime evidence is missing, extra, or mismatched",
    )?;
    let snapshot = ctx.captains.snapshot();
    let claims = snapshot
        .captains
        .iter()
        .filter(|claim| claim.role == FleetRole::Cortana && claim.state == ClaimState::Active)
        .collect::<Vec<_>>();
    if claims.len() > 1
        || claims
            .first()
            .is_some_and(|claim| !exact_observed_cortana_claim(claim, launch))
    {
        return Err(
            "reconcile_cortana: observed launch conflicts with durable Fleet authority".into(),
        );
    }
    if launch.phase == crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed {
        if claims.is_empty() {
            claim_cortana_runtime(ctx, candidate)?;
        }
        #[cfg(test)]
        ctx.captains.pause_dispatch("cortana_after_claim");
        ctx.captains
            .record_cortana_claimed_launch(operation_id, &launch.terminal_id)?;
        return Err(CORTANA_ATTESTATION_REQUIRED.into());
    }
    let action = if durable.identity_id.is_none()
        && durable.generation == 0
        && durable.quarantine_ledger.is_empty()
    {
        crate::cortana_reconcile::CortanaReconcileAction::Create
    } else {
        crate::cortana_reconcile::CortanaReconcileAction::Recover
    };
    let provider_session_id = candidate.provider_session_id.clone();
    let committed = ctx.captains.commit_cortana_runtime(
        operation_id,
        &launch.identity_id,
        launch.generation,
        &launch.terminal_id,
        &launch.harness,
        provider_session_id.as_deref(),
    )?;
    let quarantined = durable
        .quarantine_ledger
        .iter()
        .map(|quarantine| quarantine.terminal_id.clone())
        .collect();
    let _ = captains_sync_apply(ctx);
    Ok(cortana_reconcile_response(
        operation_id,
        action,
        committed,
        quarantined,
        Vec::new(),
        None,
    ))
}

fn claim_cortana_runtime(
    ctx: &ControlContext,
    candidate: &crate::cortana_reconcile::CortanaRuntimeCandidate,
) -> Result<CaptainRecord, String> {
    // reconcile_cortana already owns the shared identity transaction.  Use the
    // internal claim path to avoid recursively taking the non-reentrant lock.
    let result = claim_captain_locked(
        ctx,
        &json!({
            "captainSessionId": candidate.terminal_id,
            "role": "cortana",
            "provider": candidate.harness,
            "providerSessionId": candidate.provider_session_id,
        }),
        None,
        true,
        None,
        true,
    )?;
    serde_json::from_value(result["captain"].clone())
        .map_err(|error| format!("reconcile_cortana: invalid Fleet claim result: {error}"))
}

fn exact_cortana_tmux_target(terminal_id: &str) -> Result<String, String> {
    if terminal_id.len() != 8
        || !terminal_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!(
            "reconcile_cortana: discovered terminal id '{terminal_id}' is outside the exact T-Hub target contract"
        ));
    }
    Ok(format!("th_{terminal_id}"))
}

fn audit_cortana_runtime_mutation(
    ctx: &ControlContext,
    decision: &str,
    operation_id: &str,
    requested_terminal_ids: &[String],
    affected_terminal_ids: &[String],
    error: Option<&str>,
) {
    let quarantined_terminal_ids = if decision.starts_with("quarantine") {
        affected_terminal_ids
    } else {
        &[]
    };
    let migrated_terminal_ids = if decision.starts_with("generation-migrat") {
        affected_terminal_ids
    } else {
        &[]
    };
    let args = json!({
        "operationId": operation_id,
        "requestedTerminalIds": requested_terminal_ids,
        "affectedTerminalIds": affected_terminal_ids,
        "quarantinedTerminalIds": quarantined_terminal_ids,
        "migratedTerminalIds": migrated_terminal_ids,
        "authorityClaimed": false,
    });
    ctx.audit.record(
        "reconcile_cortana_runtime_mutation",
        CommandTier::ProcessChanging.label(),
        decision,
        &args,
        AuditMeta {
            peer: if ctx.peer_is_loopback {
                "loopback"
            } else {
                "remote"
            },
            token_tier: Capability::Full.tier_label(),
            session: None,
            spawned_by: None,
            error,
        },
    );
}

fn quarantine_cortana_runtimes(
    ctx: &ControlContext,
    operation_id: &str,
    requested_terminal_ids: &[String],
    candidates: &[crate::cortana_reconcile::CortanaRuntimeCandidate],
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
) -> Result<Vec<String>, String> {
    let requested = requested_terminal_ids
        .iter()
        .map(|terminal_id| {
            let candidate = candidates
                .iter()
                .find(|candidate| candidate.terminal_id == *terminal_id)
                .ok_or_else(|| {
                    format!(
                        "reconcile_cortana: quarantine target '{terminal_id}' was not present in the authoritative discovery snapshot"
                    )
                })?;
            exact_cortana_tmux_target(terminal_id)?;
            Ok(candidate)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut quarantined = requested
        .iter()
        .map(|candidate| candidate.terminal_id.clone())
        .collect::<Vec<_>>();
    quarantined.sort();
    for candidate in &requested {
        if let Some(identity_id) = candidate.identity_id.as_deref() {
            ctx.identity.revoke(identity_id)?;
            if ctx.identity.get(identity_id).is_some() || !ctx.identity.is_revoked(identity_id) {
                return Err(format!(
                    "reconcile_cortana: ambiguous bearer '{identity_id}' could not be durably revoked"
                ));
            }
        }
    }
    let _ = durable;
    audit_cortana_runtime_mutation(
        ctx,
        "quarantine-ambiguous-no-signal",
        operation_id,
        requested_terminal_ids,
        &[],
        Some("ambiguous runtimes were preserved and authority publication was refused"),
    );
    Ok(quarantined)
}

fn cortana_reconcile_response(
    operation_id: &str,
    action: crate::cortana_reconcile::CortanaReconcileAction,
    durable: crate::cortana_reconcile::CortanaDurableIdentity,
    retired: Vec<String>,
    quarantined: Vec<String>,
    reason: Option<String>,
) -> Value {
    json!({
        "accepted": "reconcile_cortana",
        "operationId": operation_id,
        "action": action,
        "healthy": action != crate::cortana_reconcile::CortanaReconcileAction::Degraded,
        "terminalId": durable.terminal_id,
        "identityId": durable.identity_id,
        "generation": durable.generation,
        "harness": durable.harness,
        "providerSessionId": durable.provider_session_id,
        "recovery": durable.recovery,
        "retiredTerminalIds": retired,
        "quarantinedTerminalIds": quarantined,
        "degradedReason": reason,
        "audited": true,
    })
}

fn reconcile_cortana(
    ctx: &ControlContext,
    args: &Value,
    trusted_internal: bool,
) -> Result<Value, String> {
    ctx.tabs
        .require_authoritative_startup()
        .map_err(retryable_error)?;
    reconcile_cortana_with_transition_count(ctx, args, trusted_internal, 0)
}

fn reconcile_cortana_with_transition_count(
    ctx: &ControlContext,
    args: &Value,
    trusted_internal: bool,
    transition_count: usize,
) -> Result<Value, String> {
    const MAX_ATTESTATION_TRANSITIONS: usize = 6;
    if transition_count > MAX_ATTESTATION_TRANSITIONS {
        let durable = ctx.captains.cortana_identity();
        return Err(format!(
            "reconcile_cortana: attestation state machine did not advance after {MAX_ATTESTATION_TRANSITIONS} transitions (managedPhase={:?}, activeRecovery={})",
            durable.managed_launch.as_ref().map(|launch| launch.phase),
            durable.active_harness_attestation_recovery.is_some(),
        ));
    }
    if !trusted_internal {
        return Err("acl: reconcile_cortana requires the trusted in-process app host".into());
    }
    let requested_operation_id = arg_str(args, "operationId")
        .or_else(|| arg_str(args, "requestId"))
        .filter(|value| !value.trim().is_empty())
        .ok_or("reconcile_cortana requires a stable non-empty operationId")?;
    let observation = observe_cortana_reconcile_outside_locks(ctx, args);
    let operation_id;
    {
        let _identity_transaction = ctx.tabs.identity_transaction();
        let _provision = ctx.captains.provision_guard();
        let existing = ctx.captains.cortana_identity();
        operation_id = if let Some(launch) = existing.managed_launch.as_ref() {
            launch.operation_id.clone()
        } else {
            match &existing.recovery {
                crate::cortana_reconcile::CortanaRecoveryState::Recovering {
                    operation_id, ..
                }
                | crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
                    operation_id,
                    ..
                }
                | crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
                    operation_id,
                    ..
                } => operation_id.clone(),
                _ => requested_operation_id,
            }
        };
        let durable = ctx.captains.begin_cortana_recovery(&operation_id)?;
        let result = reconcile_cortana_inner(
            ctx,
            args,
            &operation_id,
            durable,
            false,
            observation.as_ref(),
        );
        if matches!(
            result.as_ref().err().map(String::as_str),
            Some(CORTANA_ATTESTATION_REQUIRED)
        ) {
            drop(_provision);
            drop(_identity_transaction);
            return reconcile_cortana_with_transition_count(
                ctx,
                args,
                true,
                transition_count.saturating_add(1),
            );
        }
        if !matches!(
            result.as_ref().err().map(String::as_str),
            Some(CORTANA_SPAWN_ADMISSION_REQUIRED)
        ) {
            if let Err(error) = &result {
                if is_retryable_error(error) {
                    return result;
                }
                let _ = ctx.captains.mark_cortana_degraded(&operation_id, error);
            }
            return result;
        }
    }

    // The inspection-only pass found that a replacement is required. Retry
    // while holding the one global dual-lock order: dispatch admission, then
    // provisioning. Capacity and rate are evaluated only if the second pass
    // still reaches the exact spawn boundary.
    let _admission_lock = ctx
        .dispatch_admission
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _identity_transaction = ctx.tabs.identity_transaction();
    let _provision = ctx.captains.provision_guard();
    let durable = ctx.captains.begin_cortana_recovery(&operation_id)?;
    let result = reconcile_cortana_inner(
        ctx,
        args,
        &operation_id,
        durable,
        true,
        observation.as_ref(),
    );
    if matches!(
        result.as_ref().err().map(String::as_str),
        Some(CORTANA_ATTESTATION_REQUIRED)
    ) {
        drop(_provision);
        drop(_identity_transaction);
        drop(_admission_lock);
        return reconcile_cortana_with_transition_count(
            ctx,
            args,
            true,
            transition_count.saturating_add(1),
        );
    }
    if let Err(error) = &result {
        if is_retryable_error(error) {
            return result;
        }
        let _ = ctx.captains.mark_cortana_degraded(&operation_id, error);
    }
    result
}

const CORTANA_SPAWN_ADMISSION_REQUIRED: &str =
    "internal: Cortana replacement requires ordered spawn admission";
const CORTANA_ATTESTATION_REQUIRED: &str =
    "internal: Cortana attestation requires an outside-lock observation";

#[derive(Clone)]
struct CortanaManagedObservationEvidence {
    process: crate::harness::HarnessProcessIdentity,
    owner: crate::cortana_reconcile::CortanaManagedOwnerToken,
}

#[derive(Clone)]
struct CortanaReconcileObservation {
    durable_basis: crate::cortana_reconcile::CortanaDurableIdentity,
    managed_result: Option<Result<CortanaManagedObservationEvidence, String>>,
    active_result: Option<Result<(), String>>,
    legacy_result: Option<
        Result<
            (
                crate::harness::ExpectedHarnessLaunchProvenance,
                crate::harness::HarnessProcessIdentity,
            ),
            String,
        >,
    >,
}

fn same_cortana_attestation_basis(
    left: &crate::cortana_reconcile::CortanaDurableIdentity,
    right: &crate::cortana_reconcile::CortanaDurableIdentity,
) -> bool {
    left.identity_id == right.identity_id
        && left.generation == right.generation
        && left.terminal_id == right.terminal_id
        && left.harness == right.harness
        && left.owner == right.owner
        && left.managed_launch == right.managed_launch
        && left.active_harness_attestation == right.active_harness_attestation
        && left.active_harness_attestation_recovery == right.active_harness_attestation_recovery
}

fn managed_process_from_observation(
    observation: Option<&CortanaReconcileObservation>,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
) -> Result<crate::harness::HarnessProcessIdentity, String> {
    let observed = observation.ok_or(CORTANA_ATTESTATION_REQUIRED)?;
    let evidence = observed
        .managed_result
        .as_ref()
        .ok_or(CORTANA_ATTESTATION_REQUIRED)?
        .clone()?;
    if observed.durable_basis != *durable {
        let mut advanced_basis = observed.durable_basis.clone();
        let prior_phase = advanced_basis
            .managed_launch
            .as_ref()
            .map(|launch| launch.phase);
        let current_launch = durable.managed_launch.as_ref();
        if let (Some(advanced_launch), Some(current_launch)) =
            (advanced_basis.managed_launch.as_mut(), current_launch)
        {
            advanced_launch.phase = current_launch.phase;
            advanced_launch.harness_process = current_launch.harness_process.clone();
        }
        let current_phase = current_launch.map(|launch| launch.phase);
        let phase_advanced = matches!(
            (prior_phase, current_phase),
            (
                Some(crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved),
                Some(crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed)
                    | Some(crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed)
            ) | (
                Some(crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed),
                Some(crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed)
            )
        );
        let process_advanced_exactly = current_launch
            .and_then(|launch| launch.harness_process.as_ref())
            .is_some_and(|process| process == &evidence.process);
        if phase_advanced
            && process_advanced_exactly
            && advanced_basis == *durable
            && durable.owner.as_ref() == Some(&evidence.owner)
        {
            return Err(CORTANA_ATTESTATION_REQUIRED.into());
        }
        return Err("Cortana managed launch changed after outside-lock observation".into());
    }
    if durable.owner.as_ref() != Some(&evidence.owner) {
        return Err("Cortana managed owner evidence changed before commit".into());
    }
    Ok(evidence.process)
}

fn observe_cortana_reconcile_outside_locks(
    ctx: &ControlContext,
    args: &Value,
) -> Option<CortanaReconcileObservation> {
    let durable = ctx.captains.cortana_identity();
    if let (Some(launch), Some(owner)) = (durable.managed_launch.as_ref(), durable.owner.as_ref()) {
        if matches!(
            launch.phase,
            crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
                | crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
                | crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed
        ) {
            let result = if launch.phase
                == crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
            {
                wait_for_harness_started(&launch.terminal_id, &launch.harness)
                    .and_then(|()| observe_stable_cortana_harness_process(ctx, launch, owner))
            } else {
                observe_cortana_harness_process(ctx, launch, owner)
            }
            .and_then(|process| {
                revalidate_cortana_managed_owner_after_process_observation(
                    ctx, launch, owner, process,
                )
            });
            return Some(CortanaReconcileObservation {
                durable_basis: durable,
                managed_result: Some(result),
                active_result: None,
                legacy_result: None,
            });
        }
    }
    if durable.active_harness_attestation_recovery.is_some() {
        let result = finalize_cortana_active_attestation_recovery_observation(ctx, &durable);
        return Some(CortanaReconcileObservation {
            durable_basis: durable,
            managed_result: None,
            active_result: None,
            legacy_result: Some(result),
        });
    }
    if durable.active_harness_attestation.is_some() {
        let result = revalidate_active_cortana_authority(ctx, &durable);
        return Some(CortanaReconcileObservation {
            durable_basis: durable,
            managed_result: None,
            active_result: Some(result),
            legacy_result: None,
        });
    }
    if durable.owner.is_some()
        && durable.identity_id.is_some()
        && durable.terminal_id.is_some()
        && durable.harness.is_some()
    {
        let result = (|| {
            let harness = match durable.harness.as_deref() {
                Some("codex") => Harness::Codex,
                Some("claude") => Harness::Claude,
                _ => return Err("legacy incumbent has an unsupported Harness".into()),
            };
            let command = cortana_startup_command(&durable, args, harness);
            let expected = resolve_cortana_expected_harness_launch(&command, harness, args)
                .map_err(|error| {
                    format!("configured legacy incumbent provenance is untrusted: {error}")
                })?;
            let deadline = Instant::now() + Duration::from_secs(5);
            let first =
                observe_live_cortana_with_expected_provenance(ctx, &durable, &expected, deadline)?;
            std::thread::sleep(
                Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())),
            );
            let second =
                observe_live_cortana_with_expected_provenance(ctx, &durable, &expected, deadline)?;
            if first != second {
                return Err("legacy incumbent process identity did not remain stable".into());
            }
            let required_executable = expected
                .trusted_child_executable
                .as_ref()
                .unwrap_or(&expected.executable);
            if &second.executable != required_executable {
                return Err(
                    "legacy incumbent did not reach its exact configured native executable".into(),
                );
            }
            Ok((expected, second))
        })();
        return Some(CortanaReconcileObservation {
            durable_basis: durable,
            managed_result: None,
            active_result: None,
            legacy_result: Some(result),
        });
    }
    None
}

fn retirable_legacy_cortana_orphan(
    ctx: &ControlContext,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    candidates: &[crate::cortana_reconcile::CortanaRuntimeCandidate],
) -> Option<crate::cortana_reconcile::CortanaRuntimeCandidate> {
    let identity_id = durable.identity_id.as_deref()?;
    let harness = durable.harness.as_deref()?;
    let provenance = durable.legacy_orphan_provenance.as_ref()?;
    if durable.generation == 0
        || ctx.identity.get(identity_id).is_some()
        || candidates.len() != 1
        || ctx.captains.snapshot().captains.iter().any(|captain| {
            captain.role == FleetRole::Cortana && captain.state == ClaimState::Active
        })
    {
        return None;
    }
    let candidate = &candidates[0];
    (provenance.version == crate::cortana_reconcile::LEGACY_ORPHAN_PROVENANCE_VERSION
        && provenance.source_schema_version == 18
        && provenance.identity_id == identity_id
        && provenance.terminal_id == candidate.terminal_id
        && provenance.generation == durable.generation
        && provenance.harness == harness
        && !provenance.healthy_operation_id.trim().is_empty()
        && durable
            .terminal_id
            .as_deref()
            .is_none_or(|terminal_id| candidate.terminal_id == terminal_id)
        && candidate.identity_id.is_none()
        && candidate.generation == durable.generation
        && candidate.harness == harness
        && candidate.terminal == crate::cortana_reconcile::RuntimeEvidence::Alive
        && candidate.harness_process == crate::cortana_reconcile::RuntimeEvidence::Alive
        && !candidate.identity_bound_to_terminal
        && candidate.unresolved_session_bearer
        && candidate
            .effect_identity
            .as_ref()
            .is_some_and(valid_cortana_effect_identity)
        && ((candidate.canonical_control_file && candidate.rotating_control_env_scrubbed)
            || candidate.stale_legacy_control_env)
        && !candidate.current_control_capability
        && !candidate.trusted_cortana_identity)
        .then(|| candidate.clone())
}

fn retirable_unattested_managed_cortana_incumbent(
    ctx: &ControlContext,
    durable: &crate::cortana_reconcile::CortanaDurableIdentity,
    candidates: &[crate::cortana_reconcile::CortanaRuntimeCandidate],
) -> Option<crate::cortana_reconcile::CortanaRuntimeCandidate> {
    let identity_id = durable.identity_id.as_deref()?;
    let terminal_id = durable.terminal_id.as_deref()?;
    let harness = durable.harness.as_deref()?;
    let owner = durable.owner.as_ref()?;
    if durable.generation == 0
        || durable.managed_launch.is_some()
        || durable.active_harness_attestation_recovery.is_some()
        || ctx.identity.get(identity_id).is_some()
        || candidates.len() != 1
    {
        return None;
    }
    let active_claims = ctx
        .captains
        .snapshot()
        .captains
        .into_iter()
        .filter(|captain| captain.role == FleetRole::Cortana && captain.state == ClaimState::Active)
        .collect::<Vec<_>>();
    if active_claims.len() != 1 || active_claims[0].terminal_id.as_deref() != Some(terminal_id) {
        return None;
    }
    let candidate = &candidates[0];
    (candidate.terminal_id == terminal_id
        && candidate.identity_id.is_none()
        && candidate.generation == durable.generation
        && candidate.harness == harness
        && candidate.terminal == crate::cortana_reconcile::RuntimeEvidence::Alive
        && candidate.harness_process == crate::cortana_reconcile::RuntimeEvidence::Alive
        && !candidate.identity_bound_to_terminal
        && candidate.unresolved_session_bearer
        && candidate
            .effect_identity
            .as_ref()
            .is_some_and(|effect| same_cortana_tmux_generation(effect, &owner.tmux))
        && valid_cortana_effect_identity(&owner.tmux)
        && candidate.canonical_control_file
        && candidate.rotating_control_env_scrubbed
        && !candidate.stale_legacy_control_env
        && !candidate.current_control_capability
        && !candidate.trusted_cortana_identity)
        .then(|| candidate.clone())
}

fn exact_unresolved_managed_cortana_candidate(
    candidate: &crate::cortana_reconcile::CortanaRuntimeCandidate,
    terminal_id: &str,
    generation: u64,
    harness: &str,
    effect_identity: &crate::cortana_reconcile::CortanaOrphanEffectIdentity,
) -> bool {
    candidate.terminal_id == terminal_id
        && candidate.identity_id.is_none()
        && candidate.generation == generation
        && candidate.harness == harness
        && candidate.terminal == crate::cortana_reconcile::RuntimeEvidence::Alive
        && candidate.harness_process == crate::cortana_reconcile::RuntimeEvidence::Alive
        && !candidate.identity_bound_to_terminal
        && candidate.unresolved_session_bearer
        && candidate.effect_identity.as_ref() == Some(effect_identity)
        && candidate.canonical_control_file
        && candidate.rotating_control_env_scrubbed
        && !candidate.stale_legacy_control_env
        && !candidate.current_control_capability
        && !candidate.trusted_cortana_identity
}

fn reconcile_cortana_inner(
    ctx: &ControlContext,
    args: &Value,
    operation_id: &str,
    mut durable: crate::cortana_reconcile::CortanaDurableIdentity,
    dispatch_admission_held: bool,
    observation: Option<&CortanaReconcileObservation>,
) -> Result<Value, String> {
    let home = orchestrator_home(args)?;
    if home.is_empty() || !home.starts_with('/') {
        return Err("reconcile_cortana: orchestrator home must be an absolute POSIX path".into());
    }
    std::fs::create_dir_all(files::to_host_path(&home))
        .map_err(|error| format!("reconcile_cortana: could not create '{home}': {error}"))?;
    if let Some(recovery) = durable.active_harness_attestation_recovery.as_ref() {
        let observed = observation
            .filter(|observed| same_cortana_attestation_basis(&observed.durable_basis, &durable))
            .and_then(|observed| observed.legacy_result.as_ref())
            .ok_or(CORTANA_ATTESTATION_REQUIRED)?
            .clone()?;
        if observed.0 != recovery.expected_launch_provenance || observed.1 != recovery.process {
            return Err("Cortana staged attestation observation changed before commit".into());
        }
        let committed = ctx
            .captains
            .commit_cortana_active_attestation_recovery(recovery)?;
        let _ = captains_sync_apply(ctx);
        return Ok(cortana_reconcile_response(
            operation_id,
            crate::cortana_reconcile::CortanaReconcileAction::Recover,
            committed,
            Vec::new(),
            Vec::new(),
            None,
        ));
    }
    if let Some(launch) = durable
        .managed_launch
        .as_ref()
        .filter(|launch| {
            launch.version < 4
                || launch
                    .expected_harness_launch_provenance
                    .as_ref()
                    .is_none_or(|expected| {
                        expected.version
                            < crate::harness::EXPECTED_HARNESS_LAUNCH_PROVENANCE_VERSION
                    })
        })
        .cloned()
    {
        let harness = match launch.harness.as_str() {
            "codex" => Harness::Codex,
            "claude" => Harness::Claude,
            _ => {
                return Err(
                    "reconcile_cortana: legacy managed launch declares an unsupported Harness"
                        .into(),
                )
            }
        };
        let command = cortana_startup_command(&durable, args, harness);
        let expected =
            resolve_cortana_expected_harness_launch(&command, harness, args).map_err(|error| {
            format!(
                "reconcile_cortana: configured Harness provenance cannot enrich the retained managed launch: {error}"
            )
        })?;
        ctx.captains
            .record_cortana_expected_harness_launch_provenance(
                operation_id,
                &launch.terminal_id,
                &launch.identity_id,
                launch.generation,
                expected,
            )?;
        return Err(CORTANA_ATTESTATION_REQUIRED.into());
    }
    if let Some(launch) = durable.managed_launch.clone() {
        if launch.operation_id != operation_id {
            return Err(
                "reconcile_cortana: a different durable managed launch is still active".into(),
            );
        }
        let target_liveness = tmux::session_liveness(&launch.tmux_target);
        match (launch.phase, target_liveness) {
            (
                crate::cortana_reconcile::CortanaManagedLaunchPhase::Prepared,
                tmux::SessionLiveness::Alive,
            ) => {
                let owner = tmux::observe_prepared_managed_runtime_owner(
                    &launch.tmux_target,
                    &tmux_cortana_launch(&launch),
                )
                .map_err(|error| {
                    cortana_tmux_observation_error(
                        "reconcile_cortana: prepared launch effect is alive but ownership is unverifiable",
                        error,
                    )
                })?;
                durable = ctx.captains.record_cortana_runtime_owner(
                    operation_id,
                    &launch.terminal_id,
                    durable_cortana_owner(owner),
                )?;
                durable =
                    attest_cortana_managed_harness_from_observation(ctx, &durable, observation)?;
            }
            (
                crate::cortana_reconcile::CortanaManagedLaunchPhase::Prepared,
                tmux::SessionLiveness::Gone,
            ) => {
                cleanup_cortana_managed_launch(ctx, &launch, None)?;
                let identity_is_reserved = durable.identity_id.as_deref()
                    == Some(launch.identity_id.as_str())
                    || matches!(
                        &durable.recovery,
                        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
                            replacement_identity_id: Some(identity_id),
                            ..
                        }
                        | crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
                            replacement_identity_id: Some(identity_id),
                            ..
                        } if identity_id == &launch.identity_id
                    );
                if !identity_is_reserved {
                    ctx.identity.retire(&launch.identity_id)?;
                }
                durable = ctx.captains.cortana_identity();
            }
            (
                crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
                | crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
                | crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed,
                tmux::SessionLiveness::Alive,
            ) => {
                let post_claim =
                    launch.phase == crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed;
                durable = attest_cortana_managed_harness_from_observation(
                    ctx,
                    &durable,
                    observation,
                )
                .map_err(|error| {
                    if error == CORTANA_ATTESTATION_REQUIRED {
                        error
                    } else if post_claim {
                        format!(
                            "reconcile_cortana: post-claim Harness revalidation failed; WAL and Fleet claim retained: {error}"
                        )
                    } else {
                        error
                    }
                })?;
                if launch.phase
                    == crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
                {
                    return Err(CORTANA_ATTESTATION_REQUIRED.into());
                }
            }
            (
                crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
                | crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
                | crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed,
                tmux::SessionLiveness::Gone,
            ) => {
                let owner = durable
                    .owner
                    .as_ref()
                    .ok_or("reconcile_cortana: observed launch lost its durable owner")?;
                cleanup_cortana_managed_launch(ctx, &launch, Some(&tmux_cortana_owner(owner)))?;
                durable = ctx.captains.cortana_identity();
            }
            (_, tmux::SessionLiveness::Unknown) => {
                return Err(retryable_error(
                    "reconcile_cortana: durable managed launch has uncertain tmux evidence",
                ));
            }
        }
    }
    let mut candidates = discover_cortana_runtimes(ctx, &home, &durable)?;
    if durable.managed_launch.as_ref().is_some_and(|launch| {
        matches!(
            launch.phase,
            crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
                | crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed
        )
    }) {
        return finalize_observed_cortana_launch(
            ctx,
            operation_id,
            &durable,
            &candidates,
            observation,
        );
    }
    if let Some(terminal_id) = durable.terminal_id.as_deref() {
        if !candidates
            .iter()
            .any(|candidate| candidate.terminal_id == terminal_id)
        {
            match tmux::session_liveness(&tmux_target(terminal_id)) {
                tmux::SessionLiveness::Gone => {
                    if let Some(owner) = durable.owner.clone() {
                        if let Err(error) = tmux::retire_managed_runtime(
                            &tmux_target(terminal_id),
                            &tmux_cortana_owner(&owner),
                        ) {
                            let reason = cortana_tmux_observation_error(
                                &format!(
                                    "managed owner for gone terminal '{terminal_id}' remains populated or unverifiable"
                                ),
                                error,
                            );
                            if is_retryable_error(&reason) {
                                return Err(reason);
                            }
                            ctx.captains.mark_cortana_degraded(operation_id, &reason)?;
                            return Err(format!("reconcile_cortana: {reason}"));
                        }
                        durable = ctx
                            .captains
                            .clear_gone_cortana_runtime_owner(operation_id, &owner)?;
                    }
                }
                tmux::SessionLiveness::Alive => {
                    return Err(format!(
                        "reconcile_cortana: incumbent '{terminal_id}' is alive but absent from reserved-scope runtime discovery"
                    ));
                }
                tmux::SessionLiveness::Unknown => {
                    return Err(retryable_error(format!(
                        "reconcile_cortana: incumbent '{terminal_id}' has uncertain terminal evidence"
                    )));
                }
            }
        }
    }

    if !matches!(
        durable.recovery,
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. }
    ) {
        if let Some(incumbent) =
            retirable_unattested_managed_cortana_incumbent(ctx, &durable, &candidates)
        {
            revalidate_unresolved_cortana_attestation(&durable)?;
            let identity_id = durable
                .identity_id
                .as_deref()
                .expect("managed quarantine requires a durable identity");
            let harness = durable
                .harness
                .as_deref()
                .expect("managed quarantine requires a durable harness");
            let effect_identity = incumbent
                .effect_identity
                .expect("managed quarantine requires exact process evidence");
            durable = ctx.captains.prepare_cortana_orphan_replacement(
                operation_id,
                &incumbent.terminal_id,
                identity_id,
                durable.generation,
                harness,
                effect_identity,
            )?;
        }
    }

    if let crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
        orphan_terminal_id,
        orphan_identity_id,
        orphan_generation,
        harness,
        effect_identity,
        managed_basis,
        ..
    } = durable.recovery.clone()
    {
        let snapshot = ctx.captains.snapshot();
        let active_claims = snapshot
            .captains
            .iter()
            .filter(|captain| {
                captain.role == FleetRole::Cortana && captain.state == ClaimState::Active
            })
            .collect::<Vec<_>>();
        if let Some(basis) = managed_basis.as_ref() {
            if active_claims.len() != 1
                || active_claims[0].ship_slug != basis.claim_ship_slug
                || active_claims[0].assignment_id != basis.claim_assignment_id
                || active_claims[0].terminal_id.as_deref() != Some(basis.claim_terminal_id.as_str())
                || active_claims[0].harness.as_deref() != Some(basis.claim_harness.as_str())
                || basis.claim_terminal_id != orphan_terminal_id
                || basis.claim_harness != harness
                || !same_cortana_tmux_generation(&basis.owner.tmux, &effect_identity)
                || basis.replacement_generation != orphan_generation.saturating_add(1)
                || basis.prior_ledger_count != durable.quarantine_ledger.len()
                || basis.prior_ledger_sha256
                    != cortana_quarantine_ledger_sha256(&durable.quarantine_ledger)
            {
                return Err(
                    "reconcile_cortana: prepared managed quarantine authority is ambiguous".into(),
                );
            }
            tmux::revalidate_managed_runtime_owner(
                &tmux_target(&orphan_terminal_id),
                &tmux_cortana_owner(&basis.owner),
            )
            .map_err(|error| {
                cortana_tmux_observation_error(
                    "reconcile_cortana: prepared managed incumbent owner changed after WAL",
                    error,
                )
            })?;
            revalidate_unresolved_cortana_attestation(&durable)?;
        } else if !active_claims.is_empty() {
            return Err(
                "reconcile_cortana: prepared legacy quarantine authority is ambiguous".into(),
            );
        }
        let fresh_candidates = discover_cortana_runtimes(ctx, &home, &durable)?;
        let candidate = fresh_candidates
            .iter()
            .find(|candidate| candidate.terminal_id == orphan_terminal_id);
        if managed_basis.is_some() {
            if fresh_candidates.len() != 1
                || candidate.is_none_or(|candidate| {
                    !exact_unresolved_managed_cortana_candidate(
                        candidate,
                        &orphan_terminal_id,
                        orphan_generation,
                        &harness,
                        &effect_identity,
                    )
                })
                || ctx.identity.get(&orphan_identity_id).is_some()
            {
                return Err(
                    "reconcile_cortana: prepared managed quarantine runtime changed after WAL"
                        .into(),
                );
            }
        } else if let Some(candidate) = candidate {
            if candidate.effect_identity.as_ref() != Some(&effect_identity)
                || candidate.current_control_capability
                || candidate.trusted_cortana_identity
                || ctx.identity.get(&orphan_identity_id).is_some()
            {
                return Err(
                    "reconcile_cortana: prepared legacy quarantine evidence is ambiguous".into(),
                );
            }
        } else if tmux::session_liveness(&tmux_target(&orphan_terminal_id))
            != tmux::SessionLiveness::Gone
        {
            return Err(
                "reconcile_cortana: prepared legacy quarantine target is unavailable or ambiguous"
                    .into(),
            );
        }
        if let Some(basis) = managed_basis.as_deref() {
            #[cfg(test)]
            ctx.captains
                .pause_dispatch("cortana_managed_quarantine_revalidated");
            ctx.captains.validate_cortana_managed_quarantine_basis(
                operation_id,
                &orphan_terminal_id,
                &orphan_identity_id,
                orphan_generation,
                &harness,
                &effect_identity,
                basis,
            )?;
        }
        ctx.identity.revoke(&orphan_identity_id)?;
        ctx.control_leases.revoke_identity(&orphan_identity_id);
        if ctx.identity.get(&orphan_identity_id).is_some()
            || !ctx.identity.is_revoked(&orphan_identity_id)
        {
            return Err("reconcile_cortana: prepared quarantine identity burn is ambiguous".into());
        }
        durable = ctx.captains.quarantine_legacy_cortana(
            operation_id,
            &orphan_terminal_id,
            &orphan_identity_id,
            orphan_generation,
            &harness,
            effect_identity,
        )?;
        ctx.tabs.retire_tile_locked(&orphan_terminal_id);
        audit_cortana_runtime_mutation(
            ctx,
            "managed-unattested-quarantined-no-signal",
            operation_id,
            std::slice::from_ref(&orphan_terminal_id),
            &[],
            None,
        );
    } else if durable.owner.is_none() && durable.terminal_id.is_some() {
        let terminal_id = durable.terminal_id.clone().unwrap();
        let identity_id = durable
            .identity_id
            .clone()
            .ok_or("reconcile_cortana: pre-owner runtime has no durable identity")?;
        let harness = durable
            .harness
            .clone()
            .ok_or("reconcile_cortana: pre-owner runtime has no durable harness")?;
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.terminal_id == terminal_id)
            .filter(|candidate| {
                candidates.len() == 1
                    && candidate.generation == durable.generation
                    && candidate.harness == harness
                    && candidate.terminal == crate::cortana_reconcile::RuntimeEvidence::Alive
                    && candidate.harness_process == crate::cortana_reconcile::RuntimeEvidence::Alive
                    && candidate.effect_identity.as_ref().is_some_and(valid_cortana_effect_identity)
                    && candidate
                        .identity_id
                        .as_deref()
                        .is_none_or(|candidate_identity| candidate_identity == identity_id)
            })
            .cloned()
            .ok_or(
                "reconcile_cortana: pre-owner runtime identity is ambiguous; authority was not changed",
            )?;
        if ctx.identity.get(&identity_id).is_some() {
            ctx.identity.retire(&identity_id)?;
        }
        if ctx.identity.get(&identity_id).is_some() {
            return Err(
                "reconcile_cortana: pre-owner authority revocation could not be confirmed".into(),
            );
        }
        let effect_identity = candidate
            .effect_identity
            .expect("pre-owner quarantine requires exact tmux generation");
        durable = ctx.captains.quarantine_legacy_cortana(
            operation_id,
            &terminal_id,
            &identity_id,
            durable.generation,
            &harness,
            effect_identity,
        )?;
        ctx.tabs.retire_tile_locked(&terminal_id);
        let _ = ctx.captains.remove_session(&terminal_id)?;
        audit_cortana_runtime_mutation(
            ctx,
            "legacy-unowned-quarantined",
            operation_id,
            std::slice::from_ref(&candidate.terminal_id),
            &[],
            None,
        );
    } else if durable.owner.is_none()
        && durable.terminal_id.is_none()
        && durable.identity_id.is_none()
        && candidates.len() == 1
        && candidates[0].generation > 0
    {
        let candidate = candidates[0].clone();
        let identity_id = candidate
            .identity_id
            .clone()
            .ok_or("reconcile_cortana: discovered pre-owner runtime has no identity")?;
        let effect_identity = candidate
            .effect_identity
            .filter(valid_cortana_effect_identity)
            .ok_or("reconcile_cortana: discovered pre-owner tmux generation is ambiguous")?;
        if candidate.terminal != crate::cortana_reconcile::RuntimeEvidence::Alive
            || candidate.harness_process != crate::cortana_reconcile::RuntimeEvidence::Alive
            || !candidate.identity_bound_to_terminal
            || !candidate.current_control_capability
            || !candidate.trusted_cortana_identity
        {
            return Err(
                "reconcile_cortana: discovered pre-owner authority is ambiguous and was preserved"
                    .into(),
            );
        }
        ctx.identity.retire(&identity_id)?;
        if ctx.identity.get(&identity_id).is_some() {
            return Err("reconcile_cortana: discovered pre-owner revocation is ambiguous".into());
        }
        durable = ctx.captains.quarantine_legacy_cortana(
            operation_id,
            &candidate.terminal_id,
            &identity_id,
            candidate.generation,
            &candidate.harness,
            effect_identity,
        )?;
        ctx.tabs.retire_tile_locked(&candidate.terminal_id);
        let _ = ctx.captains.remove_session(&candidate.terminal_id)?;
    } else if let Some(orphan) = retirable_legacy_cortana_orphan(ctx, &durable, &candidates) {
        let identity_id = durable
            .identity_id
            .clone()
            .expect("exact legacy quarantine requires a durable identity");
        let harness = durable
            .harness
            .clone()
            .expect("exact legacy quarantine requires a durable harness");
        let effect_identity = orphan
            .effect_identity
            .expect("exact legacy quarantine requires a tmux generation");
        ctx.identity.revoke(&identity_id)?;
        if ctx.identity.get(&identity_id).is_some() || !ctx.identity.is_revoked(&identity_id) {
            return Err("reconcile_cortana: legacy orphan identity revocation is ambiguous".into());
        }
        durable = ctx.captains.quarantine_legacy_cortana(
            operation_id,
            &orphan.terminal_id,
            &identity_id,
            durable.generation,
            &harness,
            effect_identity,
        )?;
        ctx.tabs.retire_tile_locked(&orphan.terminal_id);
        let _ = ctx.captains.remove_session(&orphan.terminal_id)?;
        audit_cortana_runtime_mutation(
            ctx,
            "legacy-unowned-quarantined",
            operation_id,
            std::slice::from_ref(&orphan.terminal_id),
            &[],
            None,
        );
    }

    let replacement = match &durable.recovery {
        crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
            legacy_terminal_id,
            legacy_generation,
            replacement_identity_id,
            ..
        } => durable
            .quarantine_ledger
            .iter()
            .find(|quarantine| {
                quarantine.terminal_id == *legacy_terminal_id
                    && quarantine.generation == *legacy_generation
            })
            .map(|quarantine| {
                (
                    quarantine.terminal_id.clone(),
                    quarantine.identity_id.clone(),
                    quarantine.generation,
                    quarantine.harness.clone(),
                    quarantine.tmux,
                    replacement_identity_id.clone(),
                )
            }),
        _ => None,
    };
    if let Some((orphan_terminal_id, _, orphan_generation, harness, _, replacement_identity_id)) =
        &replacement
    {
        candidates.retain(|candidate| candidate.terminal_id != *orphan_terminal_id);
        if !candidates.is_empty() {
            let replacement_candidate = candidates.iter().find(|candidate| {
                replacement_identity_id.as_deref() == candidate.identity_id.as_deref()
                    && candidate.generation == orphan_generation.saturating_add(1)
                    && candidate.harness == *harness
                    && candidate.terminal == crate::cortana_reconcile::RuntimeEvidence::Alive
                    && candidate.harness_process == crate::cortana_reconcile::RuntimeEvidence::Alive
                    && candidate.identity_bound_to_terminal
                    && candidate.canonical_control_file
                    && candidate.rotating_control_env_scrubbed
                    && candidate.current_control_capability
                    && candidate.trusted_cortana_identity
            });
            if candidates.len() != 1 || replacement_candidate.is_none() {
                return Err(
                    "reconcile_cortana: reserved scope changed during durable orphan replacement"
                        .into(),
                );
            }
            let candidate = replacement_candidate.expect("checked above");
            let owner = durable
                .owner
                .as_ref()
                .ok_or("reconcile_cortana: replacement has no durable managed owner")?;
            tmux::revalidate_managed_runtime_owner(
                &tmux_target(&candidate.terminal_id),
                &tmux_cortana_owner(owner),
            )
            .map_err(|error| {
                cortana_tmux_observation_error(
                    "reconcile_cortana: replacement owner could not be revalidated",
                    error,
                )
            })?;
            claim_cortana_runtime(ctx, candidate)?;
            let identity_id = candidate
                .identity_id
                .as_deref()
                .expect("trusted replacement has an identity");
            let durable = ctx.captains.commit_cortana_runtime(
                operation_id,
                identity_id,
                candidate.generation,
                &candidate.terminal_id,
                &candidate.harness,
                candidate.provider_session_id.as_deref(),
            )?;
            let _ = captains_sync_apply(ctx);
            return Ok(cortana_reconcile_response(
                operation_id,
                crate::cortana_reconcile::CortanaReconcileAction::Recover,
                durable,
                vec![orphan_terminal_id.clone()],
                Vec::new(),
                None,
            ));
        }
    }

    let plan = if let Some((_, _, orphan_generation, _, _, _)) = &replacement {
        crate::cortana_reconcile::CortanaReconcilePlan {
            operation_id: operation_id.to_string(),
            action: crate::cortana_reconcile::CortanaReconcileAction::Recover,
            authoritative: None,
            retire_terminal_ids: Vec::new(),
            quarantine_terminal_ids: Vec::new(),
            next_generation: orphan_generation.saturating_add(1),
            degraded_reason: None,
        }
    } else {
        crate::cortana_reconcile::plan_reconciliation(&durable, operation_id, &candidates)
    };
    if plan.action == crate::cortana_reconcile::CortanaReconcileAction::Degraded {
        let quarantined = if plan.quarantine_terminal_ids.is_empty() {
            Vec::new()
        } else {
            quarantine_cortana_runtimes(
                ctx,
                operation_id,
                &plan.quarantine_terminal_ids,
                &candidates,
                &durable,
            )?
        };
        let reason = plan
            .degraded_reason
            .clone()
            .unwrap_or_else(|| "Cortana recovery evidence is ambiguous".into());
        ctx.captains.mark_cortana_degraded(operation_id, &reason)?;
        return Ok(cortana_reconcile_response(
            operation_id,
            plan.action,
            ctx.captains.cortana_identity(),
            Vec::new(),
            quarantined,
            Some(reason),
        ));
    }

    if !plan.retire_terminal_ids.is_empty() {
        let reason = "lower Cortana generations lack durable managed owner tokens; they were preserved and authority publication was refused".to_string();
        ctx.captains.mark_cortana_degraded(operation_id, &reason)?;
        return Ok(cortana_reconcile_response(
            operation_id,
            crate::cortana_reconcile::CortanaReconcileAction::Degraded,
            ctx.captains.cortana_identity(),
            Vec::new(),
            plan.retire_terminal_ids,
            Some(reason),
        ));
    }

    if let Some(candidate) = plan.authoritative.as_ref() {
        if durable.identity_id.is_none()
            && durable.terminal_id.is_none()
            && durable.generation == 0
            && plan.action == crate::cortana_reconcile::CortanaReconcileAction::Adopt
        {
            return Err(
                "reconcile_cortana: generation-zero runtime predates managed ownership and was preserved"
                    .into(),
            );
        }
        let same_incumbent = plan.action == crate::cortana_reconcile::CortanaReconcileAction::Keep
            && durable.identity_id.as_deref() == candidate.identity_id.as_deref()
            && durable.terminal_id.as_deref() == Some(candidate.terminal_id.as_str())
            && durable.generation == candidate.generation
            && durable.harness.as_deref() == Some(candidate.harness.as_str());
        if same_incumbent {
            if durable.active_harness_attestation.is_some() {
                let active_result = observation
                    .filter(|observed| {
                        same_cortana_attestation_basis(&observed.durable_basis, &durable)
                    })
                    .and_then(|observed| observed.active_result.as_ref())
                    .ok_or(CORTANA_ATTESTATION_REQUIRED)?
                    .clone();
                match separate_retryable_cortana_observation(active_result)? {
                    Ok(()) => {
                        let committed =
                            ctx.captains.complete_cortana_keep(operation_id, &durable)?;
                        let _ = captains_sync_apply(ctx);
                        return Ok(cortana_reconcile_response(
                            operation_id,
                            plan.action,
                            committed,
                            Vec::new(),
                            Vec::new(),
                            None,
                        ));
                    }
                    Err(error) => {
                        quarantine_unattested_cortana_incumbent(
                            ctx,
                            operation_id,
                            &durable,
                            candidate,
                            &error,
                        )?;
                        return Err(CORTANA_SPAWN_ADMISSION_REQUIRED.into());
                    }
                }
            }

            let attestation = observation
                .filter(|observed| {
                    same_cortana_attestation_basis(&observed.durable_basis, &durable)
                })
                .and_then(|observed| observed.legacy_result.as_ref())
                .ok_or(CORTANA_ATTESTATION_REQUIRED)?
                .clone();
            match separate_retryable_cortana_observation(attestation)? {
                Ok((expected_launch_provenance, process)) => {
                    let recovery =
                        crate::cortana_reconcile::CortanaActiveHarnessAttestationRecovery {
                            version: crate::cortana_reconcile::ACTIVE_HARNESS_ATTESTATION_RECOVERY_VERSION,
                            operation_id: operation_id.to_string(),
                            identity_id: candidate
                                .identity_id
                                .clone()
                                .expect("trusted incumbent has identity"),
                            generation: candidate.generation,
                            terminal_id: candidate.terminal_id.clone(),
                            harness: candidate.harness.clone(),
                            expected_launch_provenance,
                            process,
                        };
                    let staged = ctx
                        .captains
                        .prepare_cortana_active_attestation_recovery(recovery)?;
                    #[cfg(test)]
                    ctx.captains
                        .pause_dispatch("cortana_active_attestation_recovery_prepared");
                    let _ = staged;
                    return Err(CORTANA_ATTESTATION_REQUIRED.into());
                }
                Err(error) => {
                    quarantine_unattested_cortana_incumbent(
                        ctx,
                        operation_id,
                        &durable,
                        candidate,
                        &error,
                    )?;
                    return Err(CORTANA_SPAWN_ADMISSION_REQUIRED.into());
                }
            }
        }

        let reason = "authoritative Cortana candidate has no observed managed-launch WAL";
        quarantine_unattested_cortana_incumbent(ctx, operation_id, &durable, candidate, reason)?;
        return Err(CORTANA_SPAWN_ADMISSION_REQUIRED.into());
    }

    if ctx.apply_sink.is_none() && ctx.fanout.subscriber_count() == 0 {
        return Err("reconcile_cortana: no UI is connected to adopt a recovered runtime".into());
    }
    if !dispatch_admission_held {
        #[cfg(test)]
        ctx.captains
            .pause_dispatch("cortana_spawn_admission_required");
        return Err(CORTANA_SPAWN_ADMISSION_REQUIRED.into());
    }
    let _capacity = evaluate_spawn_capacity(ctx, &SpawnPurpose::Cortana, 1, None)
        .map_err(|refusal| format!("reconcile_cortana: {}", refusal.message))?;
    let harness_name = arg_str(args, "harness")
        .or_else(|| durable.harness.clone())
        .unwrap_or_else(|| "codex".into())
        .trim()
        .to_ascii_lowercase();
    if !matches!(harness_name.as_str(), "codex" | "claude") {
        return Err(format!(
            "reconcile_cortana: unsupported harness '{harness_name}'"
        ));
    }
    if durable
        .harness
        .as_deref()
        .is_some_and(|durable_harness| durable_harness != harness_name)
    {
        return Err("reconcile_cortana: changing the durable harness requires an explicit administrative update".into());
    }
    let harness = Harness::from_provider(&harness_name);
    let startup_command = cortana_startup_command(&durable, args, harness);
    let expected_harness_launch_provenance = resolve_cortana_expected_harness_launch(
        &startup_command,
        harness,
        args,
    )
    .map_err(|error| {
        format!("reconcile_cortana: configured Harness launch provenance is untrusted: {error}")
    })?;
    #[cfg(test)]
    let effect_startup_command =
        arg_str(args, "testEffectStartupCommand").unwrap_or_else(|| startup_command.clone());
    #[cfg(not(test))]
    let effect_startup_command = startup_command.clone();

    let (identity, _newly_minted) = if let Some((_, _, _, _, _, replacement_identity_id)) =
        &replacement
    {
        match replacement_identity_id.as_deref() {
            Some(identity_id) => {
                let identity = ctx.identity.get(identity_id).ok_or_else(|| {
                    format!(
                        "reconcile_cortana: reserved replacement identity '{identity_id}' is unavailable or revoked"
                    )
                })?;
                if identity.role != crate::identity::Role::Cortana {
                    return Err(
                        "reconcile_cortana: reserved replacement identity no longer has the Cortana role"
                            .into(),
                    );
                }
                (identity, false)
            }
            None => {
                let identity = ctx.identity.mint(crate::identity::Role::Cortana)?;
                if let Err(error) = ctx
                    .captains
                    .bind_cortana_orphan_replacement_identity(operation_id, &identity.id)
                {
                    let _ = ctx.identity.retire(&identity.id);
                    return Err(error);
                }
                (identity, true)
            }
        }
    } else {
        match durable.identity_id.as_deref() {
            Some(identity_id) => {
                let identity = ctx.identity.get(identity_id).ok_or_else(|| {
                    format!(
                        "reconcile_cortana: durable identity '{identity_id}' is unavailable or revoked"
                    )
                })?;
                if identity.role != crate::identity::Role::Cortana {
                    return Err(
                        "reconcile_cortana: durable identity no longer has the Cortana role".into(),
                    );
                }
                (identity, false)
            }
            None => (ctx.identity.mint(crate::identity::Role::Cortana)?, true),
        }
    };
    if let Some(previous_terminal_id) = durable.terminal_id.as_deref() {
        if tmux::session_liveness(&tmux_target(previous_terminal_id)) == tmux::SessionLiveness::Gone
        {
            ctx.tabs.retire_tile_locked(previous_terminal_id);
            let _ = ctx.captains.remove_session(previous_terminal_id)?;
        }
    }
    for candidate in &candidates {
        if candidate.terminal == crate::cortana_reconcile::RuntimeEvidence::Alive
            && candidate.harness_process == crate::cortana_reconcile::RuntimeEvidence::Gone
            && candidate.current_control_capability
            && candidate.identity_id.as_deref() == Some(identity.id.as_str())
        {
            let owner = durable.owner.as_ref().ok_or_else(|| {
                format!(
                    "reconcile_cortana: failed runtime '{}' has no durable managed owner and was preserved",
                    candidate.terminal_id
                )
            })?;
            tmux::retire_managed_runtime(
                &tmux_target(&candidate.terminal_id),
                &tmux_cortana_owner(owner),
            )
            .map_err(|error| {
                cortana_tmux_observation_error(
                    &format!(
                        "reconcile_cortana: failed managed runtime '{}' could not be retired",
                        candidate.terminal_id
                    ),
                    error,
                )
            })?;
            ctx.tabs.retire_tile_locked(&candidate.terminal_id);
            let _ = ctx.captains.remove_session(&candidate.terminal_id)?;
        }
    }
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let terminal_id = suffix[..8].to_string();
    ctx.identity.bind_tile(&identity.id, &terminal_id)?;
    let spawn_args = json!({
        "cwd": home,
        "name": "Cortana",
        "startupCommand": effect_startup_command,
        "tabId": CAPTAIN_WORKSPACE_ID,
    });
    let mut elevation = elevation_env(ctx, &spawn_args);
    audit_control_spawn(ctx, "reconcile_cortana", &spawn_args);
    elevation.push((
        crate::identity::SESSION_TOKEN_ENV.to_string(),
        identity.secret.clone(),
    ));
    elevation.push((
        CORTANA_GENERATION_ENV.to_string(),
        plan.next_generation.to_string(),
    ));
    elevation.push((
        PROVIDER_SESSION_ENV.to_string(),
        pending_provider_marker(&harness_name),
    ));
    let pane = crate::commands::pane_command(None, Some(&effect_startup_command));
    let tmux_cwd = files::posix_form(&home);
    let launch = tmux::prepare_managed_runtime_launch().map_err(|error| {
        format!("reconcile_cortana: managed launch preparation failed: {error}")
    })?;
    ctx.captains.prepare_cortana_managed_launch(
        operation_id,
        &terminal_id,
        &identity.id,
        plan.next_generation,
        harness.as_provider(),
        &launch,
        expected_harness_launch_provenance,
    )?;
    let prepared_launch = ctx
        .captains
        .cortana_identity()
        .managed_launch
        .expect("managed launch was just prepared");
    let (_, _tmux_session, owner) = match spawn_managed_tmux_terminal_with_id(
        &terminal_id,
        &tmux_cwd,
        pane.as_deref(),
        &elevation,
        &launch,
    ) {
        Ok(runtime) => runtime,
        Err(error) => {
            return Err(cortana_tmux_observation_error(
                "reconcile_cortana: terminal startup failed with durable prepared cleanup pending",
                error,
            ));
        }
    };
    let durable_owner = durable_cortana_owner(owner.clone());
    if let Err(error) =
        ctx.captains
            .record_cortana_runtime_owner(operation_id, &terminal_id, durable_owner)
    {
        cleanup_cortana_managed_launch(ctx, &prepared_launch, Some(&owner)).map_err(|cleanup| {
            format!(
                "reconcile_cortana: managed runtime owner durability failed: {error}; {cleanup}; prepared WAL retained"
            )
        })?;
        return Err(format!(
            "reconcile_cortana: managed runtime owner could not be made durable: {error}"
        ));
    }
    Err(CORTANA_ATTESTATION_REQUIRED.into())
}

fn trusted_provider_session_id(
    ctx: &ControlContext,
    terminal_id: &str,
    provider: &str,
    presented: Option<String>,
) -> Result<Option<String>, String> {
    let runtime = if provider == "claude" {
        ctx.status.session_for_terminal(terminal_id)
    } else {
        tmux::session_environment(&tmux_target(terminal_id), "CODEX_THREAD_ID")
            .map_err(|error| format!("could not inspect Codex runtime identity: {error}"))?
            .filter(|value| !value.trim().is_empty())
    };
    if let (Some(runtime), Some(presented)) = (&runtime, &presented) {
        if runtime != presented {
            return Err(format!(
                "providerSessionId does not match the {provider} identity reported by the target runtime"
            ));
        }
    }
    if runtime.is_none() && presented.is_some() {
        return Err(format!(
            "providerSessionId cannot be trusted because the {provider} runtime has not reported an identity"
        ));
    }
    Ok(runtime.or(presented))
}

mod handlers_comms;
use handlers_comms::*;

mod handlers_admin;
use handlers_admin::*;

mod handlers_worktrees;
use handlers_worktrees::*;

mod handlers_tabs;
use handlers_tabs::*;

mod handlers_captains;
use handlers_captains::*;
// `recover_pending_fleet_operations` is a public control entry point (called from
// `lib.rs` as `control::recover_pending_fleet_operations`); re-export it so moving
// its definition into the submodule preserves that path (a private glob does not).
pub use handlers_captains::recover_pending_fleet_operations;

mod handlers_fleet;
use handlers_fleet::*;

mod handlers_spawn;
use handlers_spawn::*;

mod handlers_terminal;
use handlers_terminal::*;

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
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
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
        json!({
            "memTotalKib": mem_total,
            "memAvailableKib": mem_avail,
            "swapTotalKib": swap_total,
            "swapFreeKib": swap_free,
            "cpuCount": cpu_count,
            "loadAvg": load_avg,
            "processCount": process_count,
            "distro": distro,
            "capturedAtMs": now_ms,
        })
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
        #[cfg(not(test))]
        let provider_capacity: ProviderCapacityFn = Arc::new(|| {
            provider_capacity_from_environment(std::env::var("T_HUB_PROVIDER_SESSION_CAPACITY"))
        });
        #[cfg(not(test))]
        let provider_live_sessions: ProviderLiveSessionsFn =
            Arc::new(inspect_provider_live_sessions);
        // Most existing unit tests exercise unrelated control behavior. They get
        // explicit deterministic provider evidence, while dedicated regressions
        // replace this closure with unavailable and exhausted evidence.
        #[cfg(test)]
        let provider_capacity: ProviderCapacityFn = Arc::new(|| {
            Ok(ProviderCapacityEvidence {
                session_capacity: crate::governor::HARD_SESSION_CEILING,
                status: crate::governor::ProviderCapacityStatus {
                    source: "deterministic-test-evidence".into(),
                    degraded: false,
                    detail: None,
                },
            })
        });
        #[cfg(test)]
        let provider_live_sessions: ProviderLiveSessionsFn = Arc::new(|_, sessions| {
            Ok(sessions
                .iter()
                .filter(|session| session.starts_with("th_"))
                .count())
        });
        Self {
            status,
            history: crate::history::HistoryService::from_env(),
            preview_control: Arc::new(|command, _, _| {
                Err(format!(
                    "Preview operation '{command}' is unavailable until a backend runtime is attached"
                ))
            }),
            supervisor,
            files: Arc::new(files::FileIndexState::new()),
            apply_sink: None,
            fanout: Arc::new(EventFanout::new()),
            metrics: None,
            provider_capacity,
            provider_live_sessions,
            live_sessions: Arc::new(|| tmux::list_sessions().map_err(|error| error.to_string())),
            tabs: Arc::new(TabRegistry::new()),
            captains: Arc::new(CaptainsRegistry::new()),
            dispatch_admission: Arc::new(Mutex::new(())),
            fleet_watches: Arc::new(crate::fleet::FleetWatchRegistry::new()),
            idle_timeout: CONN_READ_TIMEOUT,
            attach_write_timeout: ATTACH_WRITE_TIMEOUT,
            max_attach_forwarders: MAX_ATTACH_FORWARDERS,
            attach_keepalive_interval: ATTACH_KEEPALIVE_INTERVAL,
            peer_is_loopback: true,
            token,
            read_token: String::new(),
            host_token: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            addr: String::new(),
            listener_instance_id: uuid::Uuid::new_v4().simple().to_string(),
            listener_generation: Arc::new(AtomicU64::new(0)),
            bound_listener_generation: 0,
            governor: Arc::new(SpawnGovernor::from_env()),
            audit: Arc::new(AuditLog::from_env()),
            requests: Arc::new(RequestCache::new()),
            rebind: Arc::new(RebindController::new(REBIND_MIN_INTERVAL)),
            identity: Arc::new(crate::identity::IdentityStore::ephemeral()),
            control_leases: Arc::new(CaptainControlLeases::default()),
            inbox: Arc::new(crate::inbox::Inbox::ephemeral()),
            authz: Arc::new(crate::authz::AuthzStore::ephemeral()),
            delegated_admin: Arc::new(crate::delegated_admin::DelegatedAdminStore::ephemeral()),
            worktrees: Arc::new(crate::worktree_coordinator::WorktreeCoordinator::ephemeral()),
        }
    }

    /// Attach the per-launch **read** capability token (socket-gate Phase 2).
    /// `lib.rs` mints it alongside the control token; headless tests set a known
    /// value so they can exercise read-only capability resolution.
    pub fn with_read_token(mut self, read_token: String) -> Self {
        self.read_token = read_token;
        self
    }

    /// Replace the [`SpawnGovernor`] (tests inject tiny limits; production keeps the
    /// env-configured one from [`new`](Self::new)).
    #[cfg(test)]
    pub fn with_governor(mut self, governor: Arc<SpawnGovernor>) -> Self {
        self.governor = governor;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_provider_capacity(
        mut self,
        provider_capacity: impl Fn() -> Result<usize, String> + Send + Sync + 'static,
    ) -> Self {
        self.provider_capacity = Arc::new(move || {
            provider_capacity().map(|session_capacity| ProviderCapacityEvidence {
                session_capacity,
                status: crate::governor::ProviderCapacityStatus {
                    source: "injected-test-evidence".into(),
                    degraded: false,
                    detail: None,
                },
            })
        });
        self
    }

    #[cfg(test)]
    fn with_provider_capacity_evidence(
        mut self,
        provider_capacity: impl Fn() -> Result<ProviderCapacityEvidence, String> + Send + Sync + 'static,
    ) -> Self {
        self.provider_capacity = Arc::new(provider_capacity);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_provider_live_sessions(
        mut self,
        provider_live_sessions: impl Fn(&[String]) -> Result<usize, String> + Send + Sync + 'static,
    ) -> Self {
        self.provider_live_sessions = Arc::new(move |_, sessions| provider_live_sessions(sessions));
        self
    }

    #[cfg(test)]
    pub(crate) fn with_live_sessions(
        mut self,
        live_sessions: impl Fn() -> Result<Vec<String>, String> + Send + Sync + 'static,
    ) -> Self {
        self.live_sessions = Arc::new(live_sessions);
        self
    }

    /// Replace the [`AuditLog`]. Tests point it at a temp dir so they never write to
    /// the real `~/.t-hub/audit`; item-3 also uses it in production to SHARE one audit
    /// sink between the control server and the Tauri UI spawn path (single hash chain).
    pub fn with_audit(mut self, audit: Arc<AuditLog>) -> Self {
        self.audit = audit;
        self
    }

    /// The shared tab registry (TASK C / #22). `lib.rs` grabs this before starting
    /// the listener and `.manage()`s the same `Arc` so the `report_workspace_tabs`
    /// Tauri command feeds reports into the very registry `list_tabs` reads.
    pub fn tab_registry(&self) -> Arc<TabRegistry> {
        self.tabs.clone()
    }

    /// Attach an externally-shared [`TabRegistry`] (so the Tauri report command and
    /// the control listener see one registry). Builder-style; headless tests keep
    /// the private empty one from [`new`](Self::new).
    pub fn with_tab_registry(mut self, tabs: Arc<TabRegistry>) -> Self {
        self.tabs = tabs;
        self
    }

    /// Attach a persistent [`CaptainsRegistry`] (captain-chat phase 2). `lib.rs`
    /// builds it with [`CaptainsRegistry::load`] over [`captains_path`] so claims
    /// survive app restarts; headless tests keep the in-memory one from
    /// [`new`](Self::new).
    pub fn with_captains_registry(mut self, captains: Arc<CaptainsRegistry>) -> Self {
        self.captains = captains;
        self
    }

    /// Replace the History service. Tests use isolated provider roots; production
    /// keeps the provider-home service created by [`new`](Self::new).
    pub fn with_history_service(mut self, history: Arc<crate::history::HistoryService>) -> Self {
        self.history = history;
        self
    }

    /// Attach the single backend Preview service adapter shared by desktop,
    /// control, CLI, and MCP callers.
    pub fn with_preview_control(
        mut self,
        preview_control: impl Fn(&str, &Value, &PreviewRootAuthority) -> Result<Value, String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.preview_control = Arc::new(preview_control);
        self
    }

    /// Share the [`crate::fleet::FleetWatchRegistry`] with the fleet notifier so
    /// `watch_fleet` / `unwatch_fleet` arm the same registry the notifier reads.
    /// `lib.rs` builds the `Arc` once and hands the same clone to the notifier;
    /// headless tests keep the in-memory one from [`new`](Self::new).
    pub fn with_fleet_watches(mut self, watches: Arc<crate::fleet::FleetWatchRegistry>) -> Self {
        self.fleet_watches = watches;
        self
    }

    /// Attach the persistent per-session [`crate::identity::IdentityStore`]
    /// (comms-plane Phase 2). `lib.rs` builds it with `IdentityStore::load` over
    /// `identities.json` so bindings survive restarts and shares the same `Arc`;
    /// headless tests keep the ephemeral one from [`new`](Self::new).
    pub fn with_identity_store(mut self, identity: Arc<crate::identity::IdentityStore>) -> Self {
        self.identity = identity;
        self
    }

    /// Attach the durable [`crate::inbox::Inbox`] (comms-plane Phase 2). `lib.rs`
    /// builds it with `Inbox::open` over `~/.t-hub/inbox/` and shares the same `Arc`
    /// with the fleet notifier (the inbox's first client); headless tests keep the
    /// ephemeral one from [`new`](Self::new).
    pub fn with_inbox(mut self, inbox: Arc<crate::inbox::Inbox>) -> Self {
        self.inbox = inbox;
        self
    }

    /// Attach the process-durable inbox resolved from `T_HUB_INBOX_DIR` or the
    /// normal user data location. Headless control hosts use this to preserve the
    /// same restart semantics as the desktop application.
    pub fn with_durable_inbox(mut self) -> Self {
        self.inbox = Arc::new(crate::inbox::Inbox::open_default());
        self
    }

    /// Attach the durable [`crate::authz::AuthzStore`] (comms-plane Phase 3 delegation-
    /// gate carrier). `lib.rs` builds it with `AuthzStore::load` over
    /// `authorizations.json` and shares the same `Arc`; headless tests keep the
    /// ephemeral one from [`new`](Self::new).
    pub fn with_authz(mut self, authz: Arc<crate::authz::AuthzStore>) -> Self {
        self.authz = authz;
        self
    }

    pub fn with_delegated_admin(
        mut self,
        delegated_admin: Arc<crate::delegated_admin::DelegatedAdminStore>,
    ) -> Self {
        self.delegated_admin = delegated_admin;
        self
    }

    pub fn with_worktree_coordinator(
        mut self,
        worktrees: Arc<crate::worktree_coordinator::WorktreeCoordinator>,
    ) -> Self {
        self.worktrees = worktrees;
        self
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
mod tests;
