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

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit::{AuditLog, AuditMeta};
use crate::claude::StatusBridge;
use crate::governor::SpawnGovernor;
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
    /// present it must be `<=` [`PROTOCOL_VERSION`] or the server rejects the request.
    /// A LOWER version is accepted (the protocol is backward-compatible: v2 added
    /// only the opt-in binary PTY framing of T13); only a HIGHER, unknown-future
    /// version is rejected.
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
    /// Per-launch shared secret the client must present. Backward-compatible: this
    /// is the full-power **control** token by default (every existing caller that
    /// reads `token` keeps full power). The Phase 3 harden flag
    /// (`T_HUB_CONTROL_HARDEN`, default OFF) flips it to publish only the read token
    /// here; Phase 2 never flips it.
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
        let Ok(mut subs) = self.subs.lock() else {
            return 0;
        };
        subs.retain_mut(|s| {
            s.writer
                .write_all(&frame)
                .and_then(|()| s.writer.flush())
                .is_ok()
        });
        subs.len()
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabRecord {
    pub id: String,
    pub name: String,
    /// Tile ids in this tab, in order. Accepts the frontend's `order` field as an
    /// alias so either spelling deserializes.
    #[serde(default, alias = "order")]
    pub tile_ids: Vec<String>,
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

#[derive(Default)]
struct RegistryInner {
    tabs: Vec<TabRecord>,
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
#[derive(Default)]
pub struct TabRegistry {
    inner: Mutex<RegistryInner>,
}

impl TabRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RegistryInner> {
        // A poisoned registry lock means a panic mid-mutation; the data is a plain
        // Vec so continuing with it is safe (same policy as recovering the guard).
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Replace the whole registry (legacy up-sync; no staleness check). Kept for
    /// reporters that predate `baseSeq` (native cockpit) and for tests.
    pub fn replace(&self, tabs: Vec<TabRecord>) {
        let mut g = self.lock();
        g.tabs = tabs;
        g.seq += 1;
    }

    /// A UI up-sync with optimistic-concurrency: accepted (and revision bumped)
    /// only when `base_seq` matches the current revision; `None` means a legacy
    /// reporter and is accepted unconditionally.
    pub fn report(
        &self,
        tabs: Vec<TabRecord>,
        active_tab_id: Option<String>,
        base_seq: Option<u64>,
    ) -> ReportOutcome {
        let mut g = self.lock();
        if let Some(base) = base_seq {
            if base != g.seq {
                return ReportOutcome::Stale(RegistrySnapshot {
                    seq: g.seq,
                    active_tab_id: g.active_tab_id.clone(),
                    tabs: g.tabs.clone(),
                });
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
        ReportOutcome::Accepted { seq: g.seq, removed_tab_ids }
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

    /// The tab currently holding `tile_id`, if any (captains: a claim with no
    /// explicit `workspaceTabIds` defaults to the tab the captain's tile lives in).
    fn tab_for_tile(&self, tile_id: &str) -> Option<String> {
        self.lock()
            .tabs
            .iter()
            .find(|t| t.tile_ids.iter().any(|x| x == tile_id))
            .map(|t| t.id.clone())
    }

    /// Record a new (empty) tab so its id is addressable immediately. No-op (no
    /// revision bump) if a tab with this id already exists.
    fn insert_tab(&self, id: &str, name: &str) {
        let mut g = self.lock();
        if !g.tabs.iter().any(|t| t.id == id) {
            g.tabs.push(TabRecord {
                id: id.to_string(),
                name: name.to_string(),
                tile_ids: Vec::new(),
            });
            g.seq += 1;
        }
    }

    /// Move a tile into `tab_id`: drop it from every tab, then append. Errors when
    /// the target tab is unknown (the old silent no-op is exactly how a headless
    /// `move_tile` got accepted-then-lost). A tile id not currently placed anywhere
    /// is still placed (it may be a live session the UI has not adopted yet).
    fn move_tile(&self, tile_id: &str, tab_id: &str) -> Result<(), String> {
        let mut g = self.lock();
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
        let target = tab_id
            .filter(|id| g.tabs.iter().any(|t| &t.id == id))
            .map(str::to_string)
            .or_else(|| {
                g.active_tab_id
                    .clone()
                    .filter(|id| g.tabs.iter().any(|t| &t.id == id))
            })
            .or_else(|| g.tabs.first().map(|t| t.id.clone()))?;
        for t in g.tabs.iter_mut() {
            t.tile_ids.retain(|x| x != tile_id);
        }
        if let Some(t) = g.tabs.iter_mut().find(|t| t.id == target) {
            t.tile_ids.push(tile_id.to_string());
        }
        g.seq += 1;
        Some(target)
    }

    /// Drop a tile from every tab (a terminal was closed). Returns true (and bumps
    /// the revision) only if the tile was actually placed somewhere.
    fn remove_tile(&self, tile_id: &str) -> bool {
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

    /// Rename a tab. Errors when the tab is unknown.
    fn rename_tab(&self, tab_id: &str, name: &str) -> Result<(), String> {
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

    /// Close a tab (headless tab lifecycle). Policy:
    ///   - unknown tab → error;
    ///   - the LAST tab is never closed (mirrors the UI's guard) → error;
    ///   - a NON-EMPTY tab is refused unless `force` (close its terminals first
    ///     via `close_terminal`, or pass `force: true` - the tab record is dropped
    ///     and any still-live sessions are re-adopted into the UI's active tab by
    ///     its reconciler, so nothing is orphaned).
    /// Returns the closed tab's tile ids.
    fn remove_tab(&self, tab_id: &str, force: bool) -> Result<Vec<String>, String> {
        let mut g = self.lock();
        let Some(idx) = g.tabs.iter().position(|t| t.id == tab_id) else {
            return Err(format!("close_tab: unknown tabId '{tab_id}'"));
        };
        if g.tabs.len() <= 1 {
            return Err("close_tab: refusing to close the last tab".to_string());
        }
        if !g.tabs[idx].tile_ids.is_empty() && !force {
            return Err(format!(
                "close_tab: tab '{tab_id}' still holds {} tile(s); close its terminals first \
                 (close_terminal) or pass force: true",
                g.tabs[idx].tile_ids.len()
            ));
        }
        let removed = g.tabs.remove(idx);
        // Heal the active pointer under the SAME lock as the removal, and against
        // the post-removal tab set rather than just the removed id - a concurrent
        // close/focus interleaving must never leave it referencing a deleted tab.
        let active_valid = g
            .active_tab_id
            .as_ref()
            .is_some_and(|id| g.tabs.iter().any(|t| &t.id == id));
        if !active_valid {
            g.active_tab_id = g.tabs.first().map(|t| t.id.clone());
        }
        g.seq += 1;
        Ok(removed.tile_ids)
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
            .filter_map(|t| t.name.strip_prefix("Workspace ").and_then(|n| n.trim().parse().ok()))
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

/// One claimed captaincy as the control channel sees it (captain-chat phase 2):
/// the ship, the captain's terminal/session id (the same id every other control
/// command uses - the tmux session is `th_<id>`), the workspace tabs the captain
/// controls, and the crew sessions it spawned (recorded at the
/// `spawn_terminal`/`create_worktree` paths via `spawnedBy`).
///
/// Serialized camelCase in BOTH directions: the persistence file, `list_captains`,
/// and every `sync_captains` forward all carry this exact shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptainRecord {
    pub ship_slug: String,
    pub captain_session_id: String,
    #[serde(default)]
    pub workspace_tab_ids: Vec<String>,
    #[serde(default)]
    pub crew: Vec<String>,
}

/// A full, versioned copy of the captains registry: what `list_captains` returns,
/// what every `sync_captains` forward carries down to the UI (the UI renders FROM
/// this, exactly like the tab [`RegistrySnapshot`]), and the on-disk persistence
/// shape (so a restart resumes at the same revision).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptainsSnapshot {
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub captains: Vec<CaptainRecord>,
}

#[derive(Default)]
struct CaptainsInner {
    captains: Vec<CaptainRecord>,
    /// Monotonic revision, bumped on every accepted mutation - the same
    /// convergence contract as [`RegistryInner::seq`]. Persisted, so it stays
    /// monotonic across app restarts.
    seq: u64,
}

/// The CORE's authoritative captains registry (captain-chat phase 2).
///
/// Captain identity previously lived in two disconnected places: the UI's
/// localStorage designation and the captain's own ship files. This registry is
/// the ONE source of truth the UI and MCP both read: pinning in the UI is a
/// `claim_captain` server mutation, captains self-register over MCP the same
/// way, and every mutation forwards a seq'd [`CaptainsSnapshot`] to the UI
/// exactly like the tab registry does.
///
/// Unlike [`TabRegistry`] this IS persistent (the phases doc: "survives restarts
/// server-side; localStorage keeps only view state"): every mutation is written
/// through to `captains.json`, and `load` seeds from it. The write-through happens
/// AFTER the registry lock is dropped (see [`persist`](Self::persist)) so a slow
/// state-file write never wedges a reader on the registry lock.
pub struct CaptainsRegistry {
    inner: Mutex<CaptainsInner>,
    /// Persistence target; `None` = in-memory only (unit tests / headless proofs).
    path: Option<PathBuf>,
    /// Serializes disk write-throughs WITHOUT holding `inner`, guarding the last
    /// revision that reached disk so an out-of-order write (a slower older
    /// snapshot racing a newer one after both dropped `inner`) can never regress
    /// the file. Held ONLY across the file write, and NEVER while `inner` is
    /// locked - so a stalled Windows/OneDrive-backed state write can't wedge a
    /// registry reader (`list_captains`, `get_status`) or the spawn hot path on
    /// the `inner` lock. That coupling - disk I/O under the registry lock - was
    /// the Incident-D flapping wedge (one slow persist parked every
    /// captains-touching command, and its handler thread, until it drained).
    persist: Mutex<u64>,
    /// Test-only injection point: a callback run INSIDE [`persist`](Self::persist)
    /// while it holds the `persist` mutex (never `inner`), so a test can SIMULATE a
    /// stalled disk write and assert a concurrent reader/mutator on `inner` is not
    /// blocked by it. `None` in every non-test path.
    #[cfg(test)]
    persist_hook: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

/// Normalize a caller-supplied ship name into a slug: lowercase, runs of
/// non-alphanumerics collapse to single dashes, trimmed. Empty in = empty out
/// (the caller falls back to a derived slug).
fn slugify_ship(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

impl CaptainsRegistry {
    /// An empty, in-memory registry (tests / headless proofs - no persistence).
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(CaptainsInner::default()),
            path: None,
            persist: Mutex::new(0),
            #[cfg(test)]
            persist_hook: Mutex::new(None),
        }
    }

    /// Load the registry from `path`, seeding from the persisted snapshot when
    /// present + parseable (a missing or corrupt file starts empty - never a
    /// startup failure). Every subsequent mutation writes back through.
    pub fn load(path: PathBuf) -> Self {
        let inner = std::fs::read_to_string(&path)
            .ok()
            .and_then(|body| serde_json::from_str::<CaptainsSnapshot>(&body).ok())
            .map(|snap| CaptainsInner { captains: snap.captains, seq: snap.seq })
            .unwrap_or_default();
        // N3: seed the persist guard from the LOADED seq, not 0, so a stale
        // in-memory snapshot (seq <= what's already on disk) can't rewrite the file
        // redundantly on startup - the monotonic guard is correct from the first
        // write, not just after the first mutation.
        let loaded_seq = inner.seq;
        Self {
            inner: Mutex::new(inner),
            path: Some(path),
            persist: Mutex::new(loaded_seq),
            #[cfg(test)]
            persist_hook: Mutex::new(None),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CaptainsInner> {
        // Same poisoned-lock policy as TabRegistry: the data is a plain Vec, so
        // recovering the guard and continuing is safe.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Snapshot the registry for persistence - a cheap clone taken under the
    /// caller's already-held `inner` lock. The (potentially slow) disk write then
    /// happens in [`persist`](Self::persist) AFTER the lock is dropped.
    fn snapshot_for_persist(g: &CaptainsInner) -> CaptainsSnapshot {
        CaptainsSnapshot { seq: g.seq, captains: g.captains.clone() }
    }

    /// Best-effort write-through of a snapshot to disk, WITHOUT the `inner` lock
    /// held (Incident D). Serialized by the dedicated `persist` mutex - never
    /// taken together with `inner` - so a stalled state write can't wedge a
    /// registry reader or the spawn hot path. The `persist` mutex also guards the
    /// last revision that reached disk: a snapshot older than what already landed
    /// is dropped, so two writers that dropped `inner` in one order but reach disk
    /// in the other never regress the file. A write failure is logged and never
    /// fails the mutation (the in-memory registry stays authoritative for this
    /// run; the next successful write heals the file).
    ///
    /// ATOMIC (temp + rename), mirroring `voice.rs`: the loader treats a corrupt
    /// file as empty (silently dropping every claim), so a crash mid-write must
    /// never leave a torn file. We write a full body to a unique temp path, then
    /// `rename` it over the target - `rename` replaces atomically (on Windows too,
    /// MOVEFILE_REPLACE_EXISTING), so a reader/loader always sees either the old
    /// complete file or the new complete file, never a partial one.
    fn persist(&self, snap: CaptainsSnapshot) {
        let Some(path) = &self.path else { return };
        // The ONLY lock held across the disk write. Never nested inside `inner`.
        let mut last = self.persist.lock().unwrap_or_else(|p| p.into_inner());
        if snap.seq < *last {
            // A newer revision already reached disk; this stale snapshot must not
            // clobber it.
            return;
        }
        // Test seam: stand in for a slow/stalled disk write, holding `persist` but
        // NOT `inner`, so a test can prove a concurrent reader/mutator is unblocked.
        #[cfg(test)]
        if let Some(hook) = self.persist_hook.lock().unwrap().as_ref() {
            hook();
        }
        let body = match serde_json::to_vec_pretty(&snap) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("t-hub-control: captains registry serialize failed: {e}");
                return;
            }
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // A unique temp name (pid + a process-wide counter) so two writers can
        // never interleave on the same temp file - each renames its own complete
        // body; last rename wins whole.
        static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
        let tmp = path.with_extension(format!(
            "json.{}.{}.tmp",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        if let Err(e) = std::fs::write(&tmp, &body) {
            eprintln!(
                "t-hub-control: captains registry temp write to {} failed: {e}",
                tmp.display()
            );
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            eprintln!(
                "t-hub-control: captains registry rename to {} failed: {e}",
                path.display()
            );
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        *last = snap.seq;
    }

    /// Install the test-only persist hook (see [`persist_hook`](Self::persist_hook)).
    #[cfg(test)]
    fn set_persist_hook(&self, hook: Box<dyn Fn() + Send + Sync>) {
        *self.persist_hook.lock().unwrap() = Some(hook);
    }

    /// The full versioned snapshot (`list_captains` + every `sync_captains` forward).
    pub fn snapshot(&self) -> CaptainsSnapshot {
        let g = self.lock();
        CaptainsSnapshot { seq: g.seq, captains: g.captains.clone() }
    }

    /// The captain record for a given captain session id (a tile id), if that
    /// session currently holds a captaincy. Used by the fleet notifier to label a
    /// transition as belonging to a captain (and name its ship).
    pub fn captain_for_session(&self, session_id: &str) -> Option<CaptainRecord> {
        self.lock()
            .captains
            .iter()
            .find(|c| c.captain_session_id == session_id)
            .cloned()
    }

    /// Claim captaincy (UPSERT by captain session id):
    ///   - a NEW captain gets a record `{shipSlug, captainSessionId, workspaceTabIds}`
    ///     (crew starts empty);
    ///   - RE-claiming by the same captain updates its ship slug / workspace tabs
    ///     and keeps its crew (idempotent designation refresh);
    ///   - a ship slug already held by a DIFFERENT captain is refused (fleet
    ///     doctrine: one captain per ship - release first, explicitly).
    ///
    /// `ship_slug` is slugified; empty/absent falls back to `ship-<sessionId>` so
    /// a UI pin (which has no ship name) always claims something addressable.
    ///
    /// Idempotent: a re-claim that would change NOTHING (same slug, and no new
    /// workspace tabs) does not bump the revision or persist - the caller sees an
    /// unchanged `seq` and skips the redundant `sync_captains` forward.
    pub fn claim(
        &self,
        captain_session_id: &str,
        ship_slug: Option<&str>,
        workspace_tab_ids: Vec<String>,
    ) -> Result<CaptainRecord, String> {
        if captain_session_id.trim().is_empty() {
            return Err("claim_captain requires a non-empty 'captainSessionId'".into());
        }
        let slug = ship_slug
            .map(slugify_ship)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| slugify_ship(&format!("ship-{captain_session_id}")));
        let mut g = self.lock();
        if let Some(other) = g
            .captains
            .iter()
            .find(|c| c.ship_slug == slug && c.captain_session_id != captain_session_id)
        {
            return Err(format!(
                "claim_captain: ship '{slug}' is already captained by session '{}' \
                 (release_captain it first - one captain per ship)",
                other.captain_session_id
            ));
        }
        let mut changed = true;
        let record = match g
            .captains
            .iter_mut()
            .find(|c| c.captain_session_id == captain_session_id)
        {
            Some(c) => {
                // Would this re-claim actually change anything? An empty
                // workspace_tab_ids means "leave the tabs as they are".
                let tabs_change =
                    !workspace_tab_ids.is_empty() && c.workspace_tab_ids != workspace_tab_ids;
                if c.ship_slug == slug && !tabs_change {
                    changed = false;
                } else {
                    c.ship_slug = slug;
                    if !workspace_tab_ids.is_empty() {
                        c.workspace_tab_ids = workspace_tab_ids;
                    }
                }
                c.clone()
            }
            None => {
                let record = CaptainRecord {
                    ship_slug: slug,
                    captain_session_id: captain_session_id.to_string(),
                    workspace_tab_ids,
                    crew: Vec::new(),
                };
                g.captains.push(record.clone());
                record
            }
        };
        if changed {
            g.seq += 1;
            let snap = Self::snapshot_for_persist(&g);
            drop(g);
            self.persist(snap);
        }
        Ok(record)
    }

    /// Release a captaincy, addressed by captain session id OR ship slug.
    /// Unknown target is an error (strict, like the tab mutations - a silent
    /// no-op is how state drifts). Returns the released record.
    pub fn release(&self, target: &str) -> Result<CaptainRecord, String> {
        let mut g = self.lock();
        let Some(idx) = g
            .captains
            .iter()
            .position(|c| c.captain_session_id == target || c.ship_slug == target)
        else {
            return Err(format!(
                "release_captain: no claim matches '{target}' (list_captains shows \
                 captainSessionId + shipSlug of every claim)"
            ));
        };
        let removed = g.captains.remove(idx);
        g.seq += 1;
        let snap = Self::snapshot_for_persist(&g);
        drop(g);
        self.persist(snap);
        Ok(removed)
    }

    /// Record a spawned crew session under its captain (`spawnedBy` at the
    /// `spawn_terminal`/`create_worktree` paths). Returns true (revision bumped)
    /// when the captain is claimed and the crew id was newly added; false when
    /// the captain has no claim (the spawn still proceeds - crew linkage simply
    /// requires the captain to have claimed first) or the id is already crew.
    pub fn record_crew(&self, spawned_by: &str, crew_session_id: &str) -> bool {
        let mut g = self.lock();
        let Some(c) = g
            .captains
            .iter_mut()
            .find(|c| c.captain_session_id == spawned_by)
        else {
            return false;
        };
        if c.crew.iter().any(|id| id == crew_session_id) {
            return false;
        }
        c.crew.push(crew_session_id.to_string());
        g.seq += 1;
        let snap = Self::snapshot_for_persist(&g);
        drop(g);
        self.persist(snap);
        true
    }

    /// Lifecycle cleanup for a closed/killed session: drop its captaincy (a dead
    /// captain must not hold a ship) and remove it from every crew list. Returns
    /// true (revision bumped) if anything changed.
    pub fn remove_session(&self, session_id: &str) -> bool {
        let mut g = self.lock();
        let before_caps = g.captains.len();
        g.captains.retain(|c| c.captain_session_id != session_id);
        let mut changed = g.captains.len() != before_caps;
        for c in g.captains.iter_mut() {
            let before = c.crew.len();
            c.crew.retain(|id| id != session_id);
            changed |= c.crew.len() != before;
        }
        if changed {
            g.seq += 1;
            let snap = Self::snapshot_for_persist(&g);
            drop(g);
            self.persist(snap);
        }
        changed
    }

    /// Drop a closed workspace tab from every captain's `workspaceTabIds` (the
    /// registry must never advertise ownership of a tab that no longer exists).
    /// The claim itself survives - a captain can control zero tabs. Returns true
    /// (revision bumped) if anything changed.
    pub fn prune_tab(&self, tab_id: &str) -> bool {
        let mut g = self.lock();
        let mut changed = false;
        for c in g.captains.iter_mut() {
            let before = c.workspace_tab_ids.len();
            c.workspace_tab_ids.retain(|id| id != tab_id);
            changed |= c.workspace_tab_ids.len() != before;
        }
        if changed {
            g.seq += 1;
            let snap = Self::snapshot_for_persist(&g);
            drop(g);
            self.persist(snap);
        }
        changed
    }
}

impl Default for CaptainsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Idempotency: completed-request outcome cache (ask #1)
// ---------------------------------------------------------------------------

/// How many completed request outcomes to retain before evicting the oldest.
/// Spawn-class traffic is low volume (a fleet spawns dozens, not thousands, of
/// sessions), so a few hundred entries covers every realistic in-flight retry
/// window with a trivial memory cost.
const REQUEST_CACHE_CAPACITY: usize = 512;

/// How long a completed outcome stays queryable via `get_request_status`. Longer
/// than any client's overall retry deadline so a caller recovering from an
/// ambiguous response leg can always still learn what happened; short enough that
/// the cache is self-cleaning without the eviction cap ever being the sole bound.
const REQUEST_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// Default window an InFlight reservation survives before it is presumed DEAD and
/// reaped, so a handler thread that panicked or hung (e.g. a wedged `git worktree
/// add`, Incident D) cannot leave a request id permanently blocking every retry.
///
/// 600s (matching [`REQUEST_CACHE_TTL`]) deliberately sits WELL above any realistic
/// slow spawn - including a `git worktree add` against the OneDrive-backed store
/// (the very slow-I/O surface Incident D was about). At 120s a slow-but-ALIVE
/// create_worktree could be reaped mid-flight, letting a retry see `Fresh` and both
/// apply -> the exact A/B duplicate (each spawn mints a fresh uuid). 600s makes a
/// still-running op far less plausible than a truly dead one; the env override
/// (`T_HUB_REQUEST_INFLIGHT_REAP_SECS`) lets an operator tune it.
///
/// This window is now the OUTER BOUND, not the only guard: the full fix landed as
/// [`reprobe_reaped_request`] - on reaping a reservation, a same-id retry re-probes
/// reality (`git worktree list` for a `create_worktree`) BEFORE re-applying, so a
/// reaped-but-alive op resolves against what actually happened instead of being
/// blindly duplicated regardless of the window. The window still bounds how long a
/// truly-dead reservation blocks retries; the re-probe removes the duplicate risk.
const REQUEST_INFLIGHT_REAP_DEFAULT: std::time::Duration = std::time::Duration::from_secs(600);

/// The effective InFlight reap window: `$T_HUB_REQUEST_INFLIGHT_REAP_SECS` (seconds)
/// if set to a positive integer, else [`REQUEST_INFLIGHT_REAP_DEFAULT`].
fn inflight_reap_window() -> std::time::Duration {
    std::env::var("T_HUB_REQUEST_INFLIGHT_REAP_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or(REQUEST_INFLIGHT_REAP_DEFAULT)
}

/// The state of a request id in the [`RequestCache`].
enum RequestSlot {
    /// A first caller reserved this id and is running the command now. A
    /// concurrent duplicate (a retry that raced the original, Incident B) sees
    /// this and must NOT run the command again.
    InFlight { since: std::time::Instant },
    /// The command finished; its outcome is cached for replay to a retry.
    Done {
        at: std::time::Instant,
        outcome: Result<Value, String>,
    },
}

/// What [`RequestCache::begin`] decided for an incoming request id.
enum BeginOutcome {
    /// This id is new (never seen): reserved InFlight, the caller must run the
    /// command and then call [`RequestCache::finish`].
    Fresh,
    /// This id was a still-InFlight reservation that aged PAST the reap window and
    /// was just presumed-dead + re-reserved for this caller (M1 full fix). Behaves
    /// like [`Fresh`] EXCEPT the caller must first RE-PROBE reality
    /// ([`reprobe_reaped_request`]): a slow-but-alive original (e.g. a `git worktree
    /// add` on the OneDrive-backed store) may have actually LANDED before the reap,
    /// so blindly re-applying would duplicate it. If the artifact already exists,
    /// resolve the retry against it; otherwise the original truly died - apply fresh.
    FreshAfterReap,
    /// This exact request already completed - replay its outcome, do NOT re-run.
    Duplicate(Result<Value, String>),
    /// This exact request is still running on another connection - do NOT re-run;
    /// the caller should poll `get_request_status` (or retry) until it completes.
    InFlight,
}

/// The queryable status of a request id (`get_request_status`).
enum RequestStatus {
    Unknown,
    InFlight,
    Completed(Result<Value, String>),
}

/// A bounded, TTL'd cache of spawn-class request outcomes keyed by a
/// client-supplied `requestId` (ask #1). It makes a spawn-class command safely
/// RETRYABLE across an ambiguous response leg: the server applies the side effect
/// exactly once per id, and a retry of the same id replays the stored outcome
/// instead of double-applying (the Incident A/B duplicate-maker). A concurrent
/// duplicate that races the original is told InFlight rather than spawning again.
///
/// Keyed only when the client opts in by supplying a `requestId`; a request with
/// no id behaves exactly as before (no dedup), preserving backward compatibility.
pub struct RequestCache {
    inner: Mutex<RequestCacheInner>,
    capacity: usize,
    ttl: std::time::Duration,
    /// Window after which a still-InFlight reservation is presumed dead and reaped
    /// (see [`inflight_reap_window`]). A field (not the bare const) so a test can
    /// drive a tiny one and an operator can tune it via env.
    inflight_reap: std::time::Duration,
}

#[derive(Default)]
struct RequestCacheInner {
    slots: std::collections::HashMap<String, RequestSlot>,
    /// Insertion order of ids, for capacity eviction (oldest first).
    order: std::collections::VecDeque<String>,
}

impl RequestCache {
    fn new() -> Self {
        Self {
            inner: Mutex::new(RequestCacheInner::default()),
            capacity: REQUEST_CACHE_CAPACITY,
            ttl: REQUEST_CACHE_TTL,
            inflight_reap: inflight_reap_window(),
        }
    }

    /// Test-only constructor with explicit bounds so eviction/TTL/reap behavior can
    /// be exercised without inserting the full production capacity or waiting out
    /// the real windows.
    #[cfg(test)]
    fn with_bounds(
        capacity: usize,
        ttl: std::time::Duration,
        inflight_reap: std::time::Duration,
    ) -> Self {
        Self {
            inner: Mutex::new(RequestCacheInner::default()),
            capacity,
            ttl,
            inflight_reap,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RequestCacheInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Drop entries that have aged out: a Done entry past the TTL, or an InFlight
    /// reservation past `inflight_reap` (presumed dead - a panicked or hung handler
    /// must never leave an id permanently blocking retries).
    fn evict_expired(
        inner: &mut RequestCacheInner,
        now: std::time::Instant,
        ttl: std::time::Duration,
        inflight_reap: std::time::Duration,
    ) {
        let RequestCacheInner { slots, order } = inner;
        order.retain(|id| {
            let expired = match slots.get(id) {
                Some(RequestSlot::Done { at, .. }) => now.duration_since(*at) >= ttl,
                Some(RequestSlot::InFlight { since }) => {
                    now.duration_since(*since) >= inflight_reap
                }
                None => true,
            };
            if expired {
                slots.remove(id);
            }
            !expired
        });
    }

    /// Reserve `id` for a first caller, or report that it is a duplicate/in-flight.
    /// The reservation (InFlight) and the completed-outcome lookup are one atomic
    /// step so two racing retries can never both reserve the same id.
    fn begin(&self, id: &str) -> BeginOutcome {
        let now = std::time::Instant::now();
        let mut inner = self.lock();
        // M1 full fix: was THIS id a reservation that just aged out? Capture it
        // BEFORE `evict_expired` removes it, so the re-reservation below can tell a
        // genuinely-new request (Fresh) from a reaped-but-maybe-alive retry
        // (FreshAfterReap) that must re-probe reality before re-applying.
        let reaped = matches!(
            inner.slots.get(id),
            Some(RequestSlot::InFlight { since }) if now.duration_since(*since) >= self.inflight_reap
        );
        Self::evict_expired(&mut inner, now, self.ttl, self.inflight_reap);
        match inner.slots.get(id) {
            Some(RequestSlot::Done { outcome, .. }) => BeginOutcome::Duplicate(outcome.clone()),
            Some(RequestSlot::InFlight { .. }) => BeginOutcome::InFlight,
            None => {
                inner
                    .slots
                    .insert(id.to_string(), RequestSlot::InFlight { since: now });
                inner.order.push_back(id.to_string());
                // Capacity bound: evict the oldest COMPLETED entries (never an
                // in-flight reservation) until back under the cap.
                while inner.order.len() > self.capacity {
                    let Some(oldest) = inner.order.front().cloned() else { break };
                    match inner.slots.get(&oldest) {
                        Some(RequestSlot::Done { .. }) | None => {
                            inner.order.pop_front();
                            inner.slots.remove(&oldest);
                        }
                        // Oldest is still running: stop evicting (the cap is soft
                        // under a burst of concurrent in-flight requests).
                        Some(RequestSlot::InFlight { .. }) => break,
                    }
                }
                if reaped {
                    BeginOutcome::FreshAfterReap
                } else {
                    BeginOutcome::Fresh
                }
            }
        }
    }

    /// Record the outcome of a request reserved by [`begin`](Self::begin), and
    /// return it (cloned) so the caller can respond with the very value now cached
    /// for any future retry.
    fn finish(&self, id: &str, outcome: Result<Value, String>) -> Result<Value, String> {
        let mut inner = self.lock();
        // M2: normally `begin` already put the id in `order`, so we must NOT
        // double-insert. BUT if the reservation outlived the reap window (a
        // >`inflight_reap` handler still running), `evict_expired` already dropped
        // the id from BOTH maps - so this Done entry would be recorded in `slots`
        // with no `order` membership: never TTL/capacity-evictable, a permanent
        // leak that also breaches the cap and reports `completed` forever. Re-
        // establish order membership when (and only when) it is missing.
        if !inner.order.iter().any(|x| x == id) {
            inner.order.push_back(id.to_string());
        }
        inner.slots.insert(
            id.to_string(),
            RequestSlot::Done {
                at: std::time::Instant::now(),
                outcome: outcome.clone(),
            },
        );
        outcome
    }

    /// Release an InFlight reservation WITHOUT recording an outcome - used when a
    /// pre-side-effect gate (the spawn governor) refuses a reserved request, so a
    /// later retry (after budget frees) is not permanently stuck seeing InFlight /
    /// a cached refusal. A no-op if the id already completed.
    fn cancel(&self, id: &str) {
        let mut inner = self.lock();
        if matches!(inner.slots.get(id), Some(RequestSlot::InFlight { .. })) {
            inner.slots.remove(id);
            inner.order.retain(|x| x != id);
        }
    }

    /// Query the status of a request id (`get_request_status`).
    fn status(&self, id: &str) -> RequestStatus {
        let now = std::time::Instant::now();
        let mut inner = self.lock();
        Self::evict_expired(&mut inner, now, self.ttl, self.inflight_reap);
        match inner.slots.get(id) {
            None => RequestStatus::Unknown,
            Some(RequestSlot::InFlight { .. }) => RequestStatus::InFlight,
            Some(RequestSlot::Done { outcome, .. }) => RequestStatus::Completed(outcome.clone()),
        }
    }
}

impl Default for RequestCache {
    fn default() -> Self {
        Self::new()
    }
}

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
    /// The loopback address the listener bound to (`127.0.0.1:<port>`), set in
    /// [`start`] after bind. Injected (with a capability token) into the
    /// environment of spawned sessions so their in-session MCP/clients authenticate
    /// as the capability the spawn was granted (Phase 2b). Empty in headless tests
    /// (then no capability env is injected, and spawns behave exactly as before).
    addr: String,
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
/// read-only consumer paired once keeps working. Read from [`read_key_path`] if
/// present + non-empty, else a fresh UUID is minted and written (best-effort
/// `0600`). Always returns a usable in-memory key on any I/O failure.
pub fn persistent_read_key() -> String {
    let path = read_key_path();
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

/// Phase 3 hardening flag (socket-gate). When ON, [`start`] stops publishing the
/// control token to `control.json` and publishes only the read token there, so a
/// process that merely scrapes the discovery file gets read-only; elevated sessions
/// then rely on the control token injected down the spawn tree (Phase 2b). DEFAULT
/// OFF - opt in with `T_HUB_CONTROL_HARDEN=1` (or `true`).
///
/// NOTE (2026-07-07 incident): the default was briefly flipped ON (0.3.47) and
/// reverted the same day. The app's OWN frontend authenticates to the control
/// socket with the token published in `control.json`; hardening downgraded that to
/// the read token, so the webview lost control and terminals could not re-attach
/// ("session detached - reconnecting"). Phase 3 stays behind this flag until the
/// frontend receives the control token via a trusted internal channel (a Tauri
/// command / spawn-injected env) instead of scraping the discovery file. See
/// `docs/SOCKET-AUTH-DESIGN.md`.
fn phase3_harden_enabled() -> bool {
    std::env::var("T_HUB_CONTROL_HARDEN")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Pick the token published in the `token` field of `control.json`. With hardening
/// ON (the Phase 3 default) this is the read token (ambient discovery becomes
/// read-only); with hardening OFF (`T_HUB_CONTROL_HARDEN=0`) it is the full-power
/// control token (Phase 2 backward-compatible behavior). An empty read token falls
/// back to the control token even when hardening is ON, so a context that never
/// minted a read token (e.g. a bare probe server) is never locked out. Pure so it
/// is directly unit-testable.
fn select_published_token<'a>(control_token: &'a str, read_token: &'a str, harden: bool) -> &'a str {
    if harden && !read_token.is_empty() {
        read_token
    } else {
        control_token
    }
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
pub fn start(mut ctx: ControlContext) -> std::io::Result<ControlHandshake> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    // Phase 2b: record the bound loopback address on the context so `spawn_terminal`
    // can inject it (with a capability token) into the sessions it spawns.
    ctx.addr = addr.to_string();
    // Phase 2 / Phase 3: `token` stays the full-power control token by DEFAULT
    // (the app's own frontend authenticates to this socket with the published token,
    // so publishing read-only breaks terminal attach - see the 2026-07-07 incident
    // note on `phase3_harden_enabled`). Opt in with `T_HUB_CONTROL_HARDEN=1` to
    // publish only the read token; elevated sessions then receive the control token
    // via the Phase 2b spawn-tree env injection (T_HUB_CONTROL_ADDR +
    // T_HUB_CONTROL_TOKEN), not this file. `read_token` is always published so a
    // least-privilege consumer can discover a read-only credential.
    let harden = phase3_harden_enabled();
    let handshake = ControlHandshake {
        addr: addr.to_string(),
        token: select_published_token(&ctx.token, &ctx.read_token, harden).to_string(),
        read_token: ctx.read_token.clone(),
        pid: std::process::id(),
        protocol_version: PROTOCOL_VERSION,
        // The full-power control token, carried ONLY in this returned struct (never
        // serialized - see the field's `#[serde(skip_serializing)]`). Under Phase 3
        // hardening `token` above is the read token, so the trusted local frontend
        // takes its credential from here to keep terminal attach working while
        // `control.json` still withholds full power from external scrapers.
        local_control_token: ctx.token.clone(),
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
                    if !token_is_valid(&ctx, &req.token) {
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
/// the PTY-runs-`tmux attach` streaming output frames down (via a clone of the
/// writer — the reader thread owns those writes, so they never interleave with the
/// scrollback we send first), then read write/resize frames from the client until
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
    let framing = if args.get("binary").and_then(|v| v.as_bool()).unwrap_or(false) {
        pty::PtyFraming::V2Binary
    } else {
        pty::PtyFraming::V1Json
    };

    let session_id = match arg_str(args, "sessionId").or_else(|| arg_str(args, "session_id")) {
        Some(s) => s,
        None => {
            return send_attach_error(writer, framing, "attach_pty requires a 'sessionId' argument");
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
    writer.set_write_timeout(Some(ctx.attach_write_timeout)).ok();

    if !tmux::has_session(&tmux_session) {
        return send_attach_error(
            writer,
            framing,
            format!(
                "attach_pty: tmux session {tmux_session} for terminal {session_id} no longer exists"
            ),
        );
    }

    // Scrollback seed as the opening frame — sent BEFORE the stream starts so the
    // reader thread's output frames never race it. v1: `{"scrollback":"<b64>"}`;
    // v2: a binary SCROLLBACK frame carrying the raw capture bytes.
    let scrollback = tmux::capture_pane(&tmux_session).unwrap_or_default();
    match framing {
        pty::PtyFraming::V1Json => {
            write_json_line(writer, &json!({ "scrollback": STANDARD.encode(&scrollback) }))?
        }
        pty::PtyFraming::V2Binary => {
            pty::write_bin_frame(writer, pty::binframe::SCROLLBACK, &scrollback)?
        }
    }

    // Spawn the PTY streaming output to a clone of this connection, in the same
    // framing. `on_stream_end` shuts the SOCKET down when the stream is over, so
    // the input loop below unblocks promptly whether the stream died because the
    // client vanished (sink error) or because the tmux session exited under a
    // still-connected client - without it, teardown waited on the client.
    let sink = writer.try_clone()?;
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
        pty::PtyFraming::V1Json => read_pty_input_v1(reader, &mut handle, cols, rows),
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
    let mut body = serde_json::to_vec(resp).unwrap_or_else(|_| {
        br#"{"ok":false,"error":"failed to serialize response"}"#.to_vec()
    });
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
        "spawn_terminal" | "send_text" | "send_keys" | "close_terminal" => {
            CommandTier::ProcessChanging
        }
        "focus_session" | "move_tile" | "rename_tab" | "new_tab" | "close_tab" | "remove_tab"
        | "focus_tab" | "open_file" | "create_worktree" | "remove_worktree"
        | "archive_recent_project" | "claim_captain" | "release_captain" | "watch_fleet"
        | "unwatch_fleet" => CommandTier::Organization,
        _ => CommandTier::Read,
    }
}

/// A resolved caller capability (socket-gate Phase 2). The read token resolves to
/// [`ReadOnly`](Capability::ReadOnly) (Read tier only); the control token to
/// [`Full`](Capability::Full) (every tier).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capability {
    ReadOnly,
    Full,
}

impl Capability {
    /// Whether this capability may run a command of the given required tier. The
    /// read token is strictly Read-only (the general's chosen default); everything
    /// else requires the full control token.
    fn allows(self, tier: CommandTier) -> bool {
        match self {
            Capability::Full => true,
            Capability::ReadOnly => tier == CommandTier::Read,
        }
    }

    /// The audit `tokenTier` label for this capability.
    fn tier_label(self) -> &'static str {
        match self {
            Capability::Full => "control",
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

/// The capability-token env injected into a spawned session so its in-session
/// MCP/clients authenticate as the capability the spawn was granted (Phase 2b).
///
/// DEFAULT is the full control token - every spawned session can orchestrate,
/// exactly as today (where the in-session MCP reads the control token from
/// `control.json`), so nothing that spawns today breaks. An explicit
/// `capability: "read"` spawn arg downgrades the new session to the read token
/// (a pure-work crew that can observe but not spawn/type/kill). Any other/absent
/// `capability` value fails SAFE to the control token (a typo never silently
/// under-privileges a session that needed to orchestrate).
///
/// Injects BOTH `T_HUB_CONTROL_ADDR` and `T_HUB_CONTROL_TOKEN` because the MCP's
/// env override is all-or-nothing (it needs both, else it falls back to
/// `control.json` and ignores the env token). Empty when the bound addr is unknown
/// (headless tests) - then nothing is injected and the session behaves as before.
fn elevation_env(ctx: &ControlContext, args: &Value) -> Vec<(String, String)> {
    if ctx.addr.is_empty() {
        return Vec::new();
    }
    let read_only = arg_str(args, "capability")
        .map(|c| c.eq_ignore_ascii_case("read"))
        .unwrap_or(false);
    let token = if read_only && !ctx.read_token.is_empty() {
        ctx.read_token.clone()
    } else {
        ctx.token.clone()
    };
    vec![
        ("T_HUB_CONTROL_ADDR".to_string(), ctx.addr.clone()),
        ("T_HUB_CONTROL_TOKEN".to_string(), token),
    ]
}

/// The authoritative count of live `th_*` tmux sessions, reconciled from the tmux
/// source of truth on every spawn (never a free-running counter that drifts when a
/// session dies without a `close_terminal`).
///
/// Fails OPEN (returns 0) when tmux cannot be queried, because the hard constraint
/// is that a transient tmux hiccup must NOT block legitimate orchestration - and
/// the spawn-rate token bucket still bounds runaway spawning to 20/min regardless
/// of the count, so the concurrent cap/ceiling degrading to the rate limiter is a
/// bounded, deliberate fallback. The failure is logged (not silent) so a query
/// outage that softens the cap is observable in the audit/stderr trail.
fn live_session_count() -> usize {
    match tmux::list_sessions() {
        Ok(sessions) => sessions.iter().filter(|n| n.starts_with("th_")).count(),
        Err(e) => {
            eprintln!(
                "t-hub-control: could not derive live-session count from tmux ({e}); \
                 spawn concurrent-cap/ceiling fall back to the spawn-rate limiter for this spawn"
            );
            0
        }
    }
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
fn governor_gate(ctx: &ControlContext, command: &str, args: &Value) -> Result<(), crate::governor::Refusal> {
    let now = std::time::Instant::now();
    match command {
        "spawn_terminal" => ctx.governor.check_spawn(live_session_count(), now),
        "close_terminal" => ctx.governor.check_destructive(now),
        "send_keys" if keys_are_kill_style(args) => ctx.governor.check_destructive(now),
        _ => Ok(()),
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
    ctx.audit.record(
        &req.command,
        tier.label(),
        decision,
        &req.args,
        AuditMeta {
            peer: if ctx.peer_is_loopback { "loopback" } else { "remote" },
            // Phase 2: the capability the presented token resolved to.
            token_tier: cap.tier_label(),
            session,
            spawned_by,
            error,
        },
    );
}

/// Resolve capability, gate + audit, then dispatch. A bad token is rejected before
/// any command runs (byte-identical message, no leak of which commands exist).
///
/// Order (§3): (1) resolve the presented token to a [`Capability`]; (2) the
/// command's [`required_tier`] must be covered by that capability, else refuse
/// `refused-authz` and audit it; (3) for ProcessChanging the fleet governor runs
/// (Phase 1, refuse-past-ceiling); (4) dispatch. Every Organization/ProcessChanging
/// command - allowed or refused - lands in the audit log with its `tokenTier`, and
/// a refusal is mirrored live onto the event fanout.
fn dispatch_authenticated(ctx: &ControlContext, req: ControlRequest) -> ControlResponse {
    let Some(cap) = resolve_capability(ctx, &req.token) else {
        return ControlResponse::err("unauthorized: bad control token");
    };

    let tier = required_tier(&req.command);

    // Phase 2 capability gate: the presented token's capability must cover the
    // command's required tier. The read token authorizes Read only; Organization
    // and ProcessChanging require the control token.
    if !cap.allows(tier) {
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
    if let Some(id) = &request_id {
        match ctx.requests.begin(id) {
            // This exact request already completed: replay its stored outcome. Do
            // NOT re-run, re-charge, or re-audit - the side effect is already done.
            BeginOutcome::Duplicate(outcome) => return replay_response(outcome),
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
                if let Some(outcome) = reprobe_reaped_request(ctx, &req.command, &req.args) {
                    let outcome = ctx.requests.finish(id, outcome);
                    return replay_response(outcome);
                }
            }
        }
    }

    // Phase 1 fleet gate: budget + rate limits for process-changing commands only.
    // Read/Organization tiers never touch the governor.
    if tier == CommandTier::ProcessChanging {
        if let Err(refusal) = governor_gate(ctx, &req.command, &req.args) {
            // A pre-side-effect gate refusal is not an applied outcome: release the
            // reservation so a retry after the budget frees can still succeed
            // (rather than being permanently stuck replaying the refusal).
            if let Some(id) = &request_id {
                ctx.requests.cancel(id);
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

    // Dispatch, then record the outcome under the requestId (if any) so a later
    // retry replays exactly this result. `finish` returns the outcome back.
    let outcome = dispatch(ctx, &req.command, &req.args);
    let outcome = match &request_id {
        Some(id) => ctx.requests.finish(id, outcome),
        None => outcome,
    };
    let response = match outcome {
        Ok(value) => ControlResponse::ok(value),
        Err(e) => ControlResponse::err(e),
    };

    // Audit every Organization + ProcessChanging command on the allowed path,
    // capturing the dispatch outcome. Read-tier commands are not audited.
    if tier != CommandTier::Read {
        let err = if response.ok {
            None
        } else {
            response.error.as_deref()
        };
        audit_command(ctx, &req, tier, cap, "allowed", err);
    }

    response
}

/// Commands whose side effects are process/filesystem mutations we make idempotent
/// via a client `requestId` (ask #1). Deliberately narrow: only the spawn-class
/// commands from the field incidents (a create-then-register that can leave a
/// ghost, or a spawn that can duplicate on retry). Read/organization commands are
/// naturally re-runnable and need no dedup.
fn is_idempotent_command(command: &str) -> bool {
    matches!(command, "spawn_terminal" | "create_worktree")
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
        // Server-minted artifact id (see doc comment): nothing in args to probe by.
        _ => None,
    }
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
fn dispatch(ctx: &ControlContext, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        // ---- Read tier (PRD §11.2: allowed) --------------------------------
        "list_terminals" => list_terminals(),
        "get_status" => get_status(ctx, args),
        // Idempotency (ask #1): "what happened to request X?" - resolves an
        // ambiguous spawn-class response leg without guessing (Read tier).
        "get_request_status" => get_request_status(ctx, args),
        "wait_for_status" => wait_for_status(ctx, args),
        "supervision_tree" => supervision_tree(ctx, args),
        "supervision_session_ids" => supervision_session_ids(ctx),
        "wsl_health" => wsl_health(ctx),
        "recent_sessions" => recent_sessions(),
        "invalidate_recent_cache" => invalidate_recent_cache(),
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
        "list_fleet_watches" => list_fleet_watches(ctx),
        // T12: the socket twin of the `report_workspace_tabs` Tauri command - a
        // socket UI (the native cockpit) reports its tab layout into the same
        // registry the webview reports into, so `list_tabs` stays truthful
        // whichever client is attached.
        "report_workspace_tabs" => report_workspace_tabs(ctx, args),
        "read_terminal" | "capture_pane" => read_terminal(args),

        // ---- Organization tier (PRD §11.2: allowed, audited) ---------------
        // These are surfaced by the MCP server and accepted here, but the
        // process-changing subset (spawn) is gated behind the confirmation flag
        // in the MCP tool description AND refused here unless explicitly enabled,
        // so the dev-box proof never spawns/kills anything by accident.
        "focus_session" => organization_apply(ctx, "focus_session", args),
        // Headless-org: the organization mutations below apply to the SERVER tab
        // registry first (authoritative; hard error on an invalid target) and then
        // forward the registry snapshot for the UI to render from.
        "move_tile" => move_tile(ctx, args),
        "rename_tab" => rename_tab(ctx, args),
        // new_tab mints the tab id CORE-side so it can return it (TASK C:
        // addressable tabs) and forwards that id for the frontend to adopt.
        "new_tab" => new_tab(ctx, args),
        "close_tab" | "remove_tab" => close_tab(ctx, args),
        "focus_tab" => focus_tab(ctx, args),
        "open_file" => open_file(ctx, args),
        // WS-4 git worktrees: create runs git here then forwards the tab+spawn to
        // the UI; remove forwards to the UI so it detaches live tiles BEFORE git
        // tears the dir down (no orphaned processes). list (T-B) is the read-only
        // socket twin of the `git_worktree_list` Tauri command, for a socket UI's
        // worktree list/re-open/remove flows.
        "create_worktree" => create_worktree(ctx, args),
        "remove_worktree" => remove_worktree(ctx, args),
        "list_worktrees" | "git_worktree_list" => list_worktrees(ctx, args),
        // Recent list × made durable: move a project's transcripts out of the
        // scanned catalog into projects-archive (reversible). App-initiated from
        // the sidebar; filesystem-mutating like the worktree ops above.
        "archive_recent_project" => archive_recent_project(args),
        // Captain-chat phase 2: captaincy is a SERVER mutation (audited) - the
        // UI's pin action and an MCP captain's self-registration both land here,
        // and every mutation forwards the authoritative captains snapshot.
        "claim_captain" => claim_captain(ctx, args),
        "release_captain" => release_captain(ctx, args),
        // Orchestrator wake: arm/disarm a server-side push that re-invokes the
        // orchestrator's loop when a watched session goes idle / needs-input /
        // completes. Organization tier (audited); the wake itself injects via the
        // same backend send_text path the ProcessChanging tier gates.
        "watch_fleet" => watch_fleet(ctx, args),
        "unwatch_fleet" => unwatch_fleet(ctx, args),

        // ---- Process-changing tier (PRD §11.2: confirmation required) ------
        // `spawn_terminal` is confirmation-gated (its MCP description carries the
        // CONFIRMATION REQUIRED contract), but functional: it routes through the
        // SAME ApplySink adoption path create_worktree uses, so the frontend spawns
        // a real tile + live session it OWNS (no untracked tmux session). Refused
        // only when no UI is connected to adopt the tile. The session-targeted
        // process actions — typing into / interrupting / closing an *existing*
        // session — execute directly against tmux (they only act on a `th_*`
        // session the app already owns).
        "spawn_terminal" => spawn_terminal(ctx, args),
        "send_text" => send_text(args),
        "send_keys" => send_keys(args),
        "close_terminal" => close_terminal(ctx, args),

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

/// `get_status`: FR-012 status for one session id (from the supervision reducer)
/// plus the latest statusline snapshot (context %, rate-limit windows) if one
/// has been ingested. `args.sessionId` selects the session.
fn get_status(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("get_status requires a 'sessionId' argument")?;
    let key = resolve_supervisor_key(ctx, &session_id);
    let status = ctx.with_supervisor(|s| s.status(&key));
    let snapshot = ctx.status.get(&key);
    Ok(json!({
        "sessionId": session_id,
        "resolvedSessionId": key,
        "status": status,
        "snapshot": snapshot,
    }))
}

/// `get_request_status` (Read tier; ask #1): resolve "what happened to request X?"
/// for a spawn-class `requestId`, so a caller whose response leg failed can learn
/// the true outcome instead of guessing (and risking a duplicate). Returns:
///   - `{status:"completed", ok:true,  result}`  the command applied; here is its result
///   - `{status:"completed", ok:false, error}`   the command ran and failed
///   - `{status:"inFlight"}`                      still running; do not retry yet
///   - `{status:"unknown"}`                       never seen / evicted: the command
///                                                did NOT land under this id, so a
///                                                retry with the same id is safe.
/// Args: `requestId` (required).
fn get_request_status(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let request_id = arg_str(args, "requestId")
        .or_else(|| arg_str(args, "request_id"))
        .ok_or("get_request_status requires a 'requestId' argument")?;
    let body = match ctx.requests.status(&request_id) {
        RequestStatus::Unknown => json!({ "requestId": request_id, "status": "unknown" }),
        RequestStatus::InFlight => json!({ "requestId": request_id, "status": "inFlight" }),
        RequestStatus::Completed(Ok(result)) => json!({
            "requestId": request_id,
            "status": "completed",
            "ok": true,
            "result": result,
        }),
        RequestStatus::Completed(Err(error)) => json!({
            "requestId": request_id,
            "status": "completed",
            "ok": false,
            "error": error,
        }),
    };
    Ok(body)
}

/// Resolve a caller-supplied `sessionId` to the supervision reducer's key (a Claude
/// session UUID). The reducer keys sessions by the Claude UUID, but callers routinely
/// pass a T-Hub **tile id** — that is what `list_terminals` / `list_captains` expose
/// (a captain's `captainSessionId` is a tile id). If the id is already a known
/// supervisor key we keep it; otherwise we map `tile -> live UUID` via the status
/// bridge; otherwise we return it unchanged (an unknown id still resolves to
/// `Unknown` / `null`, exactly as before this bridge existed). This closes the split
/// where `get_status` / `supervision_tree` / `wait_for_status` returned `unknown` for
/// a captain addressed by its `captainSessionId`.
fn resolve_supervisor_key(ctx: &ControlContext, id: &str) -> String {
    if ctx.with_supervisor(|s| s.knows(id)) {
        return id.to_string();
    }
    if let Some(uuid) = ctx.status.session_for_terminal(id) {
        return uuid;
    }
    id.to_string()
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
        // Resolve the caller id to a supervisor key each iteration: a captain passed
        // by its tile id may not have a `tile -> uuid` binding yet on the first poll
        // (no statusline ingested), but it appears once the session emits — so a wait
        // armed a hair early still latches on. Resolved OUTSIDE `with_supervisor`
        // because the resolver itself takes the supervisor lock.
        let key = resolve_supervisor_key(ctx, &session_id);
        // (a) current status, and (b) any transition edge for this session since
        // `consumed` that matches a target — both read under one lock acquisition.
        // We advance `consumed` past every inspected edge so we never re-scan.
        let (status, edge_match) = ctx.with_supervisor(|s| {
            let status = s.status(&key);
            let edge = s.matched_since(&key, &target_enums, consumed);
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
    let key = resolve_supervisor_key(ctx, &session_id);
    let tree = ctx.with_supervisor(|s| s.tree(&key));
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

/// `scribe_status` (read tier): the Scribe voice-gate - asks Scribe's v1
/// status endpoint (loopback HTTP, discovered via `~/.scribe/control.json`)
/// whether the general is inside a dictation cycle, falling back to Scribe's
/// status.json file (pid + 15s updatedAt TTL) only when the endpoint is
/// unavailable. Returns `{listening, status, since, source}` - `listening` is
/// sourced from the snapshot's level-triggered `busy` flag - and fails open to
/// `listening: false` whenever it cannot positively confirm an active
/// dictation (see crate::scribe). Lets an agent ask "is the general dictating
/// right now?".
fn scribe_status() -> Result<Value, String> {
    serde_json::to_value(crate::scribe::read_scribe_status()).map_err(|e| e.to_string())
}

/// `invalidate_recent_cache` (Tier 3 reap): drop the recent-sessions cache so a
/// just-closed workspace's sessions show in Recent immediately, not after the 15s TTL.
fn invalidate_recent_cache() -> Result<Value, String> {
    crate::recent::invalidate_recent_cache();
    Ok(Value::Bool(true))
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

/// `list_tabs`: the live workspace tabs from the CORE tab registry (TASK C / #22),
/// each `{id, name, tileIds}`. The frontend reports its full tab layout up (the
/// `report_workspace_tabs` Tauri command) so this reflects UI-created tabs and real
/// tile membership; MCP-driven `new_tab` / `move_tile` / named placement update it
/// optimistically so a just-created tab is addressable immediately. This is the
/// minimal in-memory registry that makes headless tab ops (discover an id, then
/// `move_tile` / `focus_tab` into it) work — NOT the PRD §8 persistence layer.
fn list_tabs(ctx: &ControlContext) -> Result<Value, String> {
    let snap = ctx.tabs.snapshot_full();
    Ok(json!({
        "tabs": snap.tabs,
        "count": snap.tabs.len(),
        "seq": snap.seq,
        "activeTabId": snap.active_tab_id,
    }))
}

/// `list_captains`: the claimed captains from the CORE captains registry
/// (captain-chat phase 2), each `{shipSlug, captainSessionId, workspaceTabIds,
/// crew}` plus the registry revision - the same versioned-snapshot contract as
/// `list_tabs`. This is the ONE source of truth the UI's sidebar/overlay and an
/// MCP captain both read; ship files remain the captain-side roster only.
fn list_captains(ctx: &ControlContext) -> Result<Value, String> {
    let snap = ctx.captains.snapshot();
    Ok(json!({
        "captains": snap.captains,
        "count": snap.captains.len(),
        "seq": snap.seq,
    }))
}

/// `report_workspace_tabs` (T12 / headless-org): a UI client up-syncs its live tab
/// layout - the control-socket twin of the Tauri command of the same name (the
/// native cockpit is a socket client and cannot call Tauri). Consistency model
/// (headless-org): the SERVER registry is authoritative; a report carrying
/// `baseSeq` is accepted only when it matches the current revision, otherwise it
/// is rejected as stale and answered with the authoritative snapshot so the
/// reporter converges instead of clobbering a server-side mutation it has not
/// applied yet. A report WITHOUT `baseSeq` (legacy reporter) is accepted
/// unconditionally. Args: `tabs`: `[{id, name, tileIds}]`; `activeTabId`
/// (optional); `baseSeq` (optional).
fn report_workspace_tabs(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let tabs: Vec<TabRecord> = serde_json::from_value(
        args.get("tabs")
            .cloned()
            .ok_or("report_workspace_tabs requires a 'tabs' array")?,
    )
    .map_err(|e| format!("report_workspace_tabs: bad 'tabs' shape: {e}"))?;
    let count = tabs.len();
    let active = arg_str(args, "activeTabId").or_else(|| arg_str(args, "active_tab_id"));
    let base_seq = args
        .get("baseSeq")
        .or_else(|| args.get("base_seq"))
        .and_then(|v| v.as_u64());
    match ctx.tabs.report(tabs, active, base_seq) {
        ReportOutcome::Accepted { seq, removed_tab_ids } => {
            // Captain-chat phase 2: a normally-closed tab (the primary UI path)
            // must leave every captain's workspaceTabIds too - prune, and forward
            // a captains snapshot when anything changed so the UI/native cockpit
            // converge.
            let mut pruned = false;
            for tab_id in &removed_tab_ids {
                pruned |= ctx.captains.prune_tab(tab_id);
            }
            if pruned {
                let _ = captains_sync_apply(ctx);
            }
            Ok(json!({ "reported": count, "seq": seq }))
        }
        ReportOutcome::Stale(snap) => Ok(json!({
            "stale": true,
            "seq": snap.seq,
            "activeTabId": snap.active_tab_id,
            "tabs": snap.tabs,
            "note": "report based on a stale revision; adopt the returned snapshot \
                     and re-report on the next local change.",
        })),
    }
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
    // Captain-chat phase 2: a captain staging a crew worktree identifies itself
    // so the worktree terminal is recorded as crew (same contract as
    // spawn_terminal's spawnedBy).
    let spawned_by = arg_str(args, "spawnedBy").or_else(|| arg_str(args, "spawned_by"));

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

    // Resolve the TARGET TAB by NAME deterministically (TASK C / #22): the tile
    // must land in a tab identified by name, NOT in whatever tab is focused. Reuse
    // an existing tab with this name; otherwise mint a fresh id CORE-side. We record
    // it in the registry now (so it's addressable immediately) and forward the
    // chosen `tabId` so the frontend places the tile in THAT tab (creating it with
    // this id+name if needed) rather than defaulting to the active workspace.
    let effective_tab_name = tab_name
        .clone()
        .or_else(|| branch.clone())
        .unwrap_or_else(|| final_path_component(&worktree_path));
    let tab_id = ctx
        .tabs
        .id_for_name(&effective_tab_name)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    ctx.tabs.insert_tab(&tab_id, &effective_tab_name);

    // Headless-org: spawn the worktree terminal SERVER-side (the server owns tmux
    // either way - the webview's spawnTerminal IPC lands in this same process) and
    // place the tile in the named tab in the authoritative registry, so placement
    // holds even when that tab is hidden or the window is minimized, and the
    // terminal id is returned synchronously. With NO UI at all (no sink, no
    // subscribers), keep the headless behavior: worktree created on disk, tab
    // recorded, no terminal spawned (nothing would render it).
    let has_ui = ctx.apply_sink.is_some() || ctx.fanout.subscriber_count() > 0;
    let mut terminal_id: Option<String> = None;
    let mut tab_id = tab_id;
    if has_ui {
        // Phase 2b: elevate the worktree's terminal the same way spawn_terminal does
        // (control token by default; `capability: "read"` downgrades it).
        let elevation = elevation_env(ctx, args);
        match spawn_tmux_terminal(&worktree_path, None, &elevation) {
            Ok((id, _)) => {
                // Atomic placement with fallback: if the named tab was closed in
                // the race window between resolution and placement, the tile
                // lands in the active (else first) tab - never orphaned outside
                // the registry. tab_id then reflects the ACTUAL placement.
                if let Some(placed) = ctx.tabs.place_tile_with_fallback(&id, Some(&tab_id)) {
                    tab_id = placed;
                }
                terminal_id = Some(id);
            }
            Err(e) => {
                eprintln!("t-hub-control: create_worktree: worktree terminal spawn failed: {e}")
            }
        }
    }

    // Captain-chat phase 2: link the spawned worktree terminal to its captain.
    // No terminal (headless boot / spawn failure) = no crew session to record.
    let crew_recorded = match (&spawned_by, &terminal_id) {
        (Some(cap), Some(id)) => ctx.captains.record_crew(cap, id),
        _ => false,
    };
    if crew_recorded {
        let _ = captains_sync_apply(ctx);
    }

    // Forward the UI orchestration (open/reuse the named tab + adopt the spawned
    // terminal, rendered from the attached registry snapshot). The git worktree
    // already exists, so `alreadyCreated: true` tells any legacy consumer not to
    // run `gitWorktreeAdd` again.
    let forward = with_sync(
        ctx,
        json!({
            "worktreePath": worktree_path,
            "repoRoot": repo_root,
            "branch": branch,
            "tabId": tab_id,
            "tabName": effective_tab_name,
            "terminalId": terminal_id,
            "alreadyCreated": true,
        }),
    );
    let applied = has_ui && forward_apply(ctx, "add_worktree_workspace", &forward);
    Ok(json!({
        "accepted": "create_worktree",
        "worktreePath": worktree_path,
        "branch": branch,
        "tabId": tab_id,
        "tabName": effective_tab_name,
        "terminalId": terminal_id,
        "gitOutput": git_output,
        "spawnedBy": spawned_by,
        "crewRecorded": crew_recorded,
        "audited": true,
        "applied": applied,
        "note": if applied {
            "worktree created on disk; the terminal was spawned server-side and \
             placed in the tab identified by tabName in the authoritative registry \
             (the user's active tab is not switched)."
        } else {
            "worktree created on disk; the UI tab/terminal forward was not \
             delivered (headless/no sink)."
        },
    }))
}

/// The final non-empty path component of a POSIX path (the worktree dir's name),
/// used as a fallback tab name when neither `tabName` nor `branch` was given.
fn final_path_component(path: &str) -> String {
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// `remove_worktree` (WS-4): remove a git worktree WITHOUT orphaning processes.
/// With a webview attached we do NOT run `git worktree remove` here, because any
/// live tiles whose cwd is inside the worktree must be detached FIRST (their tmux
/// session survives a detach; killing the dir out from under a running process
/// would orphan it). So we forward a `remove_worktree_workspace` command to the
/// frontend, which (in the workspace store) detaches every tile rooted in the
/// worktree dir AND THEN calls `gitWorktreeRemove` — keeping the detach→remove
/// ordering correct.
///
/// T-B (closing T12 deviation 2): with NO sink but socket event subscribers
/// present (a native cockpit), the ordering moves SERVER-side: broadcast the
/// same `remove_worktree_workspace` forward (the native apply module detaches
/// every tile rooted in the dir — a layout-only mutation; the tmux sessions
/// survive exactly as on the webview path), then run `git worktree remove` here.
/// The broadcast is queued to each subscriber's socket before git runs, and the
/// detach never depends on the dir still existing, so the removal need not wait
/// on the client. With neither sink nor subscribers (headless), keep refusing:
/// nothing would even witness the removal. Args: `repoRoot`, `worktreePath`
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
            // T12: a native cockpit attached to this same server detaches its
            // own tiles rooted in the worktree in parallel; the detach->git
            // ordering and the git removal itself stay webview-owned. (With no
            // sink there is still no removal path - the refusal below - because
            // a socket client cannot run the git side; documented T12 deviation,
            // revisited at the T14 cutover.)
            let _ = broadcast_apply(ctx, "remove_worktree_workspace", &forward);
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
        None => {
            // No UI at all ⇒ refuse rather than orphan a process unwitnessed.
            if ctx.fanout.subscriber_count() == 0 {
                return Err(
                    "remove_worktree: no UI is connected to detach the worktree's live \
                     tiles first; refusing to remove it to avoid orphaning a running \
                     process (the app must be running for worktree removal)"
                        .to_string(),
                );
            }
            // T-B native path: detach broadcast FIRST (queued to every
            // subscriber's socket), then the git removal server-side. A git
            // failure (e.g. dirty tree without force) surfaces verbatim — the
            // detach has still been requested, exactly like the webview path
            // where gitWorktreeRemove rejects after the tiles detached.
            let applied = broadcast_apply(ctx, "remove_worktree_workspace", &forward) > 0;
            git::worktree_remove(&repo_root, &worktree_path, force)?;
            Ok(json!({
                "accepted": "remove_worktree",
                "worktreePath": worktree_path,
                "force": force,
                "audited": true,
                "applied": applied,
                "removed": true,
                "note": "no webview is attached; the detach forward was broadcast to \
                         socket UI subscribers (the native cockpit detaches its tiles \
                         rooted in the worktree) and the server then ran `git worktree \
                         remove` itself. The removal IS confirmed: the worktree is gone.",
            }))
        }
    }
}

/// `list_worktrees` (T-B, read-only): the worktrees of the repo containing `cwd`
/// — the socket twin of the `git_worktree_list` Tauri command, sharing its
/// implementation (`git::worktree_list`), so a socket UI can build the worktree
/// list/re-open/remove flow the webview drives via IPC. Best-effort like the
/// IPC twin: a non-repo yields an empty list. Args: `cwd` (or `path`/`repoRoot`).
/// Remote peers are allowlist-gated exactly like `git_info` (the probe leaks
/// repo topology for an arbitrary host path).
fn list_worktrees(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let cwd = arg_str(args, "cwd")
        .or_else(|| arg_str(args, "path"))
        .or_else(|| arg_str(args, "repoRoot"))
        .or_else(|| arg_str(args, "repo_root"))
        .ok_or("list_worktrees requires a 'cwd' argument")?;
    let cwd = if ctx.peer_is_loopback {
        cwd
    } else {
        files::scoped_create_path(&cwd, true, files::remote_file_roots())?
            .to_string_lossy()
            .into_owned()
    };
    let list = git::worktree_list(&cwd)?;
    Ok(json!({ "worktrees": list }))
}

/// Organization-tier actions whose effect is a pure UI mutation
/// (`focus_session`, `move_tile`, `rename_tab`). We **accept and audit** them
/// (PRD §11.2: "allowed with visible audit event") AND apply them: the accepted
/// `{command, args}` is forwarded to the frontend through the [`ApplySink`]
/// (a Tauri `control://apply` event), where `controlBridge.ts` dispatches it into
/// the workspace store. `applied` reflects whether the forward happened — `true`
/// once the app has wired its sink (the normal app path), `false` in the headless
/// proof/tests that run the listener without an `AppHandle` (still audited).
/// Broadcast one accepted forward to event subscribers on
/// [`APPLY_EVENT_CHANNEL`] (T12: the native client's delivery path). Returns how
/// many subscribers received it. Zero subscribers is a cheap no-op, so this runs
/// unconditionally next to every [`ApplySink`] forward.
fn broadcast_apply(ctx: &ControlContext, command: &str, args: &Value) -> usize {
    ctx.fanout
        .emit_event(APPLY_EVENT_CHANNEL, &json!({ "command": command, "args": args }))
}

/// Forward one command + args to the frontend through the [`ApplySink`], returning
/// whether the forward was delivered. A forward failure is non-fatal (logged), and
/// no sink (headless proof/tests) is simply `false`. Shared by every
/// Organization-tier handler that mutates the UI.
///
/// T12: the forward is ALSO broadcast to event subscribers (the native client's
/// path). With a sink wired (the Tauri app), `applied` keeps meaning exactly what
/// it always did — the sink delivered — so the webview path is unchanged; with no
/// sink (a headless server fronting the native cockpit), reaching at least one
/// event subscriber counts as delivery.
fn forward_apply(ctx: &ControlContext, command: &str, args: &Value) -> bool {
    let sink_applied = match &ctx.apply_sink {
        Some(sink) => match sink.apply(command, args) {
            Ok(()) => Some(true),
            Err(e) => {
                eprintln!("t-hub-control: failed to forward '{command}' to the UI: {e}");
                Some(false)
            }
        },
        None => None,
    };
    let subscribers = broadcast_apply(ctx, command, args);
    sink_applied.unwrap_or(subscribers > 0)
}

fn organization_apply(ctx: &ControlContext, command: &str, args: &Value) -> Result<Value, String> {
    let applied = forward_apply(ctx, command, args);
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

/// Merge the authoritative registry snapshot into a forward's args (under `sync`)
/// so the UI renders FROM the registry instead of re-deriving the mutation -
/// the headless-org apply contract. Applied AFTER the registry mutation, so the
/// snapshot already reflects it.
fn with_sync(ctx: &ControlContext, mut args: Value) -> Value {
    let snap = ctx.tabs.snapshot_full();
    args["sync"] = serde_json::to_value(&snap).unwrap_or(Value::Null);
    args
}

/// Registry-first organization mutation: the registry was already updated; forward
/// the args + authoritative snapshot to the UI and answer with the new revision.
fn organization_sync_apply(
    ctx: &ControlContext,
    command: &str,
    args: Value,
) -> Result<Value, String> {
    let forward = with_sync(ctx, args);
    let applied = forward_apply(ctx, command, &forward);
    let snap = ctx.tabs.snapshot_full();
    Ok(json!({
        "accepted": command,
        "audited": true,
        "applied": applied,
        "seq": snap.seq,
        "tabs": snap.tabs,
        "note": "applied to the server tab registry (authoritative) and forwarded \
                 to the UI with the registry snapshot; a hidden tab or unfocused \
                 window cannot lose this update.",
    }))
}

/// `new_tab` (Organization, audited): create a new workspace tab. TASK C / #22 —
/// the CORE mints the tab id so it can RETURN it (`tabId`), making the tab
/// immediately addressable for `move_tile` / `focus_tab`, and forwards that id to
/// the frontend to adopt (rather than letting the frontend mint its own id the
/// caller never learns). The id is recorded in the registry optimistically so
/// `list_tabs` sees it before the frontend reports back. Args: `name` (optional;
/// auto-named "Workspace N" when omitted).
fn new_tab(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let name = arg_str(args, "name")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ctx.tabs.auto_name());
    let id = uuid::Uuid::new_v4().to_string();
    ctx.tabs.insert_tab(&id, &name);
    let mut res =
        organization_sync_apply(ctx, "new_tab", json!({ "id": id, "name": name }))?;
    res["tabId"] = json!(id);
    res["name"] = json!(name);
    Ok(res)
}

/// `close_tab` (Organization, audited; headless-org): close a workspace tab over
/// the socket - the missing half of the headless tab lifecycle (an auto-created
/// tab emptied by `close_terminal` was previously only closeable by hand in the
/// UI). Policy (see [`TabRegistry::remove_tab`]): unknown tab and the last tab
/// are errors; a non-empty tab is refused unless `force: true` (its still-live
/// sessions are then re-adopted into the UI's active tab, never orphaned).
/// Auto-created empty tabs are NOT reaped implicitly - an agent staging a
/// workspace may empty and refill a tab, so closing is always an explicit call.
/// Args: `tabId` (or `tabName` to resolve by exact name); `force` (optional).
fn close_tab(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let tab_id = arg_str(args, "tabId")
        .or_else(|| arg_str(args, "id"))
        .or_else(|| {
            arg_str(args, "tabName")
                .or_else(|| arg_str(args, "tab_name"))
                .and_then(|n| ctx.tabs.id_for_name(&n))
        })
        .ok_or("close_tab requires a 'tabId' (or a 'tabName' that resolves to one)")?;
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let orphaned = ctx.tabs.remove_tab(&tab_id, force)?;
    // Captains must never advertise ownership of a tab that no longer exists
    // (the claim itself survives - a captain can control zero tabs).
    if ctx.captains.prune_tab(&tab_id) {
        let _ = captains_sync_apply(ctx);
    }
    let mut res = organization_sync_apply(
        ctx,
        "close_tab",
        json!({ "tabId": tab_id, "force": force }),
    )?;
    res["tabId"] = json!(tab_id);
    res["orphanedTileIds"] = json!(orphaned);
    Ok(res)
}

/// `rename_tab` (Organization, audited; headless-org): rename a tab. Registry-
/// first + strict (unknown tab is an error), then forwards the snapshot so the
/// rename applies even when the tab is hidden or the window is unfocused.
/// Args: `tabId` (or `id`), `name`.
fn rename_tab(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let tab_id = arg_str(args, "tabId")
        .or_else(|| arg_str(args, "id"))
        .ok_or("rename_tab requires a 'tabId' argument")?;
    let name = arg_str(args, "name")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or("rename_tab requires a non-empty 'name' argument")?;
    ctx.tabs.rename_tab(&tab_id, &name)?;
    organization_sync_apply(ctx, "rename_tab", json!({ "tabId": tab_id, "name": name }))
}

/// Forward the authoritative captains snapshot to the UI as a `sync_captains`
/// apply (captain-chat phase 2) - the captains twin of [`with_sync`]'s tab
/// snapshot, emitted AFTER a registry mutation so the UI renders FROM the
/// registry. Rides the same [`forward_apply`] path (webview sink + T12 socket
/// broadcast). Returns whether the forward was delivered.
fn captains_sync_apply(ctx: &ControlContext) -> bool {
    let snap = ctx.captains.snapshot();
    let args = json!({ "sync": serde_json::to_value(&snap).unwrap_or(Value::Null) });
    forward_apply(ctx, "sync_captains", &args)
}

/// `claim_captain` (Organization, audited; captain-chat phase 2): claim captaincy
/// of a ship. The UI's pin action and an MCP captain's self-registration are the
/// SAME mutation - registry-first (strict: a ship already captained by another
/// session is refused), then the authoritative captains snapshot is forwarded via
/// `sync_captains` so every client renders from it. Args: `captainSessionId` (or
/// `sessionId`) required; `shipSlug` optional (slugified; defaults to
/// `ship-<sessionId>`); `workspaceTabIds` optional (defaults to the tab currently
/// holding the captain's tile, when the tab registry knows one).
///
/// LIVENESS: the session must be a LIVE terminal (`th_<id>` exists in tmux) - a
/// claim for a dead/unknown session would persist and linger forever (nothing
/// ever kills a session that never existed). A re-claim that changes nothing is
/// idempotent: `seq` is unchanged and no redundant `sync_captains` is forwarded.
fn claim_captain(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let captain_session_id = arg_str(args, "captainSessionId")
        .or_else(|| arg_str(args, "captain_session_id"))
        .or_else(|| arg_str(args, "sessionId"))
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("claim_captain requires a 'captainSessionId' argument")?;
    // Liveness: refuse a claim for a session with no live terminal, so a bogus
    // or raced id can never be persisted into captains.json to linger forever.
    if !tmux::has_session(&tmux_target(&captain_session_id)) {
        return Err(format!(
            "claim_captain: no live terminal for session '{captain_session_id}' \
             (spawn or attach it first - a claim for a dead session would linger)"
        ));
    }
    let ship_slug = arg_str(args, "shipSlug").or_else(|| arg_str(args, "ship_slug"));
    let workspace_tab_ids: Vec<String> = args
        .get("workspaceTabIds")
        .or_else(|| args.get("workspace_tab_ids"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_else(|| {
            // No explicit tabs: default to the tab the captain's tile lives in
            // (the same lookup the UI's liveness check does, but server-side).
            ctx.tabs
                .tab_for_tile(&captain_session_id)
                .into_iter()
                .collect()
        });
    let before_seq = ctx.captains.snapshot().seq;
    let record = ctx
        .captains
        .claim(&captain_session_id, ship_slug.as_deref(), workspace_tab_ids)?;
    let snap = ctx.captains.snapshot();
    // Idempotent re-claim (unchanged): the registry left `seq` alone, so skip the
    // redundant forward. A real change bumps `seq` and forwards the snapshot.
    let applied = snap.seq != before_seq && captains_sync_apply(ctx);
    Ok(json!({
        "accepted": "claim_captain",
        "audited": true,
        "applied": applied,
        "captain": record,
        "seq": snap.seq,
        "captains": snap.captains,
        "note": "captaincy claimed in the server captains registry (authoritative, \
                 persistent) and the snapshot forwarded to the UI (sync_captains).",
    }))
}

/// `release_captain` (Organization, audited; captain-chat phase 2): release a
/// claimed captaincy, addressed by `captainSessionId` (or `sessionId`) or
/// `shipSlug`. Strict (an unknown claim is an error), then the snapshot is
/// forwarded via `sync_captains` exactly like `claim_captain`.
fn release_captain(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let target = arg_str(args, "captainSessionId")
        .or_else(|| arg_str(args, "captain_session_id"))
        .or_else(|| arg_str(args, "sessionId"))
        .or_else(|| arg_str(args, "session_id"))
        .or_else(|| arg_str(args, "shipSlug"))
        .or_else(|| arg_str(args, "ship_slug"))
        .ok_or("release_captain requires a 'captainSessionId' (or 'shipSlug') argument")?;
    let released = ctx.captains.release(&target)?;
    let applied = captains_sync_apply(ctx);
    let snap = ctx.captains.snapshot();
    Ok(json!({
        "accepted": "release_captain",
        "audited": true,
        "applied": applied,
        "released": released,
        "seq": snap.seq,
        "captains": snap.captains,
    }))
}

/// Parse the `scope` argument of `watch_fleet` into a [`crate::fleet::WatchScope`].
/// Accepts the string `"captains"` (default) or `"all"`, or an array of tile ids
/// (an explicit session list). An empty/absent scope defaults to captains.
fn parse_watch_scope(args: &Value) -> Result<crate::fleet::WatchScope, String> {
    use crate::fleet::WatchScope;
    match args.get("scope") {
        None | Some(Value::Null) => Ok(WatchScope::Captains),
        Some(Value::String(s)) => match s.to_ascii_lowercase().as_str() {
            "captains" | "" => Ok(WatchScope::Captains),
            "all" => Ok(WatchScope::All),
            other => Err(format!(
                "watch_fleet: unknown scope '{other}' (use \"captains\", \"all\", or an array of session ids)"
            )),
        },
        Some(Value::Array(arr)) => {
            let ids: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            if ids.is_empty() {
                return Err("watch_fleet: scope array must contain at least one session id".into());
            }
            Ok(WatchScope::Sessions(ids))
        }
        Some(_) => Err(
            "watch_fleet: 'scope' must be \"captains\", \"all\", or an array of session ids".into(),
        ),
    }
}

/// `watch_fleet` (Organization, audited): arm an orchestrator wake. The CALLING
/// orchestrator (identified by its own tile id in `orchestratorSessionId`) asks to
/// be re-invoked - a wake prompt injected into its terminal - whenever a session in
/// `scope` (default: every claimed captain) transitions into one of `states`
/// (default: the actionable set - idle/turn-complete, needs-input, completed/exited).
/// Requires a live terminal (like `claim_captain`), so a bogus id can't arm a dead
/// watch. Idempotent: re-arming replaces the prior watch for that orchestrator.
fn watch_fleet(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let orchestrator = arg_str(args, "orchestratorSessionId")
        .or_else(|| arg_str(args, "orchestrator_session_id"))
        .or_else(|| arg_str(args, "sessionId"))
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("watch_fleet requires an 'orchestratorSessionId' argument (the orchestrator's own session id)")?;
    if !tmux::has_session(&tmux_target(&orchestrator)) {
        return Err(format!(
            "watch_fleet: no live terminal for orchestrator '{orchestrator}' \
             (a wake could never be delivered to a dead session)"
        ));
    }
    let scope = parse_watch_scope(args)?;
    // `states`: an array of camelCase status strings, or absent for the default
    // actionable set. Unrecognized strings are dropped (they can never match a real
    // status); an all-unrecognized list falls back to the default rather than a
    // watch that can never fire.
    let states = match args.get("states").and_then(|v| v.as_array()) {
        Some(arr) => {
            let strs: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            target_statuses(&strs)
        }
        None => Vec::new(),
    };
    let watch = ctx.fleet_watches.arm(&orchestrator, scope, states);
    Ok(json!({
        "accepted": "watch_fleet",
        "audited": true,
        "watch": watch,
        "note": "armed - this session will be woken (a prompt injected into its \
                 terminal) when a watched session transitions into a target state.",
    }))
}

/// `unwatch_fleet` (Organization, audited): disarm an orchestrator wake previously
/// armed by `watch_fleet`, addressed by `orchestratorSessionId`. Idempotent-ish:
/// reports whether a watch was actually removed.
fn unwatch_fleet(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let orchestrator = arg_str(args, "orchestratorSessionId")
        .or_else(|| arg_str(args, "orchestrator_session_id"))
        .or_else(|| arg_str(args, "sessionId"))
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("unwatch_fleet requires an 'orchestratorSessionId' argument")?;
    let removed = ctx.fleet_watches.disarm(&orchestrator);
    Ok(json!({
        "accepted": "unwatch_fleet",
        "audited": true,
        "removed": removed,
    }))
}

/// `list_fleet_watches` (Read): the armed orchestrator wakes.
fn list_fleet_watches(ctx: &ControlContext) -> Result<Value, String> {
    let watches = ctx.fleet_watches.snapshot();
    Ok(json!({
        "watches": watches,
        "count": watches.len(),
    }))
}

/// `focus_tab` (Organization, audited): activate a tab - the ONE organization
/// command that intentionally moves the user's view. Validates the tab against
/// the registry (strict), mirrors the new active tab there (so `list_tabs` and
/// default spawn placement track it), and forwards to the UI.
fn focus_tab(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let tab_id = arg_str(args, "tabId")
        .or_else(|| arg_str(args, "id"))
        .ok_or("focus_tab requires a 'tabId' argument")?;
    // Validate-and-set atomically (a focus racing a close must fail cleanly, not
    // leave the registry's active pointer on a deleted tab).
    if !ctx.tabs.set_active_tab(&tab_id) {
        return Err(format!("focus_tab: unknown tabId '{tab_id}'"));
    }
    organization_apply(ctx, "focus_tab", &json!({ "tabId": tab_id }))
}

/// `move_tile` (Organization, audited; headless-org): move a tile into another
/// tab. Registry-FIRST and STRICT: the server registry is updated (or the command
/// errors - an unknown `tabId` is a hard error now, not the silent accept-then-
/// lose of the mirror model), then the authoritative snapshot is forwarded so the
/// UI applies it even when the target tab is hidden. A `targetId`-only call is
/// the legacy within-tab reorder: forwarded for the UI to apply and report back
/// (visual order is a UI concern).
fn move_tile(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let tile = arg_str(args, "terminalId").or_else(|| arg_str(args, "id"));
    let tab = arg_str(args, "tabId");
    match (tile, tab) {
        (Some(tile), Some(tab)) => {
            ctx.tabs.move_tile(&tile, &tab)?;
            organization_sync_apply(
                ctx,
                "move_tile",
                json!({ "terminalId": tile, "tabId": tab }),
            )
        }
        _ => organization_apply(ctx, "move_tile", args),
    }
}

/// `spawn_terminal` (Process-changing, PRD §11.2: confirmation required).
/// Headless-org: the SERVER spawns the tmux session (same id minting + pane wrap
/// as the Tauri `commands::spawn_terminal`), resolves the target tab against the
/// authoritative registry - `tabName` reuses-or-creates a tab WITHOUT switching
/// the user's active tab - places the tile there, and forwards the registry
/// snapshot for the UI (webview sink and/or socket subscribers) to render. The
/// real terminal id is therefore returned synchronously, and a hidden target tab
/// or a minimized window cannot lose the spawn or its placement. Refused only
/// when NO UI is connected at all (nothing would render the tile). Its MCP
/// description still carries the CONFIRMATION REQUIRED contract (the user-facing
/// gate). Args: `cwd`, `name`, `shell`, `startupCommand` (T-B), `tabName`,
/// `tabId` (all optional; `tabId` must exist, default placement is the user's
/// active tab).
///
/// `startupCommand` is the socket twin of the webview "+" presets' field: the
/// command runs inside an interactive login shell the pane execs back into
/// (`commands::pane_command`, the same wrap the Tauri spawn uses), which is what
/// the native client's resume flow rides (`claude --resume <id>`). SECURITY: it
/// is process-changing surface and deliberately stays INSIDE this command's
/// existing confirmation-gate tier — same audit, same remote-peer cwd allowlist,
/// no new ungated path (a caller with this tier could already run commands via
/// the equally-gated `send_text`).
fn spawn_terminal(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let cwd = arg_str(args, "cwd");
    let name = arg_str(args, "name");
    let shell = arg_str(args, "shell");
    let startup_command =
        arg_str(args, "startupCommand").or_else(|| arg_str(args, "startup_command"));
    // Captain-chat phase 2: a captain spawning crew identifies itself so the
    // spawned session is recorded as crew in the captains registry.
    let spawned_by = arg_str(args, "spawnedBy").or_else(|| arg_str(args, "spawned_by"));

    // #27: a REMOTE peer may spawn ONLY with a cwd under the operator allowlist —
    // the spawn execs a shell SERVER-SIDE at a peer-controlled dir. Loopback (the
    // local frontend/MCP) is unrestricted. An absent cwd is fine (the UI spawns in
    // the shell's default dir).
    let cwd = match cwd {
        Some(c) if !ctx.peer_is_loopback => Some(
            files::scoped_create_path(&c, true, files::remote_file_roots())?
                .to_string_lossy()
                .into_owned(),
        ),
        other => other,
    };

    // A UI must exist to render the tile (webview sink or socket subscribers);
    // with neither, keep refusing rather than spawn a session nothing shows.
    if ctx.apply_sink.is_none() && ctx.fanout.subscriber_count() == 0 {
        return Err(
            "spawn_terminal: no UI is connected to adopt the new terminal tile; \
             refusing to spawn an untracked session (the app must be running to \
             spawn a terminal)"
                .to_string(),
        );
    }

    // Headless-org: resolve the TARGET TAB server-side, against the authoritative
    // registry, BEFORE spawning - `tabId` must exist (strict), `tabName` reuses an
    // existing tab or mints one (created hidden; the user's active tab is NOT
    // switched), and neither means the UI's active tab per the registry mirror.
    let tab_name = arg_str(args, "tabName").or_else(|| arg_str(args, "tab_name"));
    let tab_id = match (arg_str(args, "tabId").or_else(|| arg_str(args, "tab_id")), &tab_name) {
        (Some(id), _) => {
            if !ctx.tabs.has_tab(&id) {
                return Err(format!("spawn_terminal: unknown tabId '{id}'"));
            }
            Some(id)
        }
        (None, Some(name)) => Some(match ctx.tabs.id_for_name(name) {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                ctx.tabs.insert_tab(&id, name);
                id
            }
        }),
        // No target given: resolved atomically at placement time (active/first
        // tab) inside place_tile_with_fallback below.
        (None, None) => None,
    };

    // Spawn the tmux session SERVER-side (same id minting + pane wrap as the Tauri
    // `commands::spawn_terminal`) so the real id is known synchronously, the tile
    // can be placed in the registry atomically, and a hidden/suspended webview
    // cannot lose the spawn. Mirror `commands::resolve_cwd`'s unix arm ($HOME
    // fallback).
    let cwd_effective = cwd
        .clone()
        .unwrap_or_else(|| std::env::var("HOME").unwrap_or_default());
    let pane = crate::commands::pane_command(shell.as_deref(), startup_command.as_deref());
    // Phase 2b: grant this session its capability token via env (control by default,
    // read_token when `capability: "read"`), so its in-session MCP authenticates as
    // the granted capability.
    let elevation = elevation_env(ctx, args);
    let (id, tmux_session) = spawn_tmux_terminal(&cwd_effective, pane.as_deref(), &elevation)?;

    // Atomic placement with fallback: if the resolved tab was closed in the race
    // window between spawn and placement, the tile lands in the active (else
    // first) tab instead - never orphaned outside the registry. The response
    // carries the ACTUAL placement.
    let placed_tab = ctx.tabs.place_tile_with_fallback(&id, tab_id.as_deref());

    // Captain-chat phase 2: record the crew link under the spawning captain.
    // The spawn NEVER fails on this - an unclaimed spawnedBy simply records
    // nothing (crewRecorded: false tells the caller to claim_captain first).
    let crew_recorded = spawned_by
        .as_deref()
        .is_some_and(|cap| ctx.captains.record_crew(cap, &id));
    if crew_recorded {
        let _ = captains_sync_apply(ctx);
    }

    let forward = with_sync(
        ctx,
        json!({
            "id": id,
            "tmuxSession": tmux_session,
            "cwd": cwd_effective,
            "name": name,
            "shell": shell,
            "startupCommand": startup_command,
            "tabId": placed_tab,
            "tabName": tab_name,
            "spawnedBy": spawned_by,
        }),
    );
    let applied = forward_apply(ctx, "spawn_terminal", &forward);
    Ok(json!({
        "accepted": "spawn_terminal",
        "id": id,
        "tmuxSession": tmux_session,
        "cwd": cwd_effective,
        "name": name,
        "shell": shell,
        "startupCommand": startup_command,
        "tabId": placed_tab,
        "placed": placed_tab.is_some(),
        "spawnedBy": spawned_by,
        "crewRecorded": crew_recorded,
        "audited": true,
        "applied": applied,
        "note": "the server spawned the session, placed the tile in the target tab \
                 in the authoritative registry (without switching the user's active \
                 tab), and forwarded the snapshot for the UI to render. tabId is the \
                 ACTUAL placement (falls back to the active tab if the target was \
                 closed mid-spawn).",
    }))
}

/// Mint a terminal id + create its detached tmux session. The id IS the tmux
/// session's own suffix, exactly like `commands::spawn_terminal` (bug #16 there:
/// id and session name must never disagree). Shared by the T12 native-path arms
/// of `spawn_terminal` / `create_worktree`, where no webview exists to run the
/// spawn client-side.
fn spawn_tmux_terminal(
    cwd: &str,
    command: Option<&str>,
    env: &[(String, String)],
) -> Result<(String, String), String> {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let id = suffix[..8].to_string();
    let tmux_session = format!("th_{id}");
    // Phase 2b: `env` carries the capability token (+ addr) for the new session, set
    // via tmux `-e` so it is present BEFORE the first pane execs and never lands in
    // argv. Empty ⇒ a plain session (headless tests / addr unknown).
    tmux::new_session_with_env(&tmux_session, cwd, command, env)
        .map_err(|e| format!("failed to create tmux session: {e}"))?;
    // Registry-vs-reality (Incident A/B, ask #3): never hand back an id whose tmux
    // session did not actually materialize. `new-session` returning success is not
    // enough - a session can fail to appear (a raced server teardown, a wsl.exe
    // relaunch dropping the detached session), and the caller would then place +
    // record a GHOST tile keyed to a session that never existed. Verifying
    // has-session here means the id is live BEFORE it is placed/recorded, so a
    // spawn that didn't take fails loudly (and idempotently retryable) instead of
    // registering a phantom.
    if !tmux::has_session(&tmux_session) {
        // L1: a FALSE negative is possible (a has-session hiccup / TOCTOU) - the
        // session may in fact have come up. Returning Err WITHOUT tearing it down
        // would orphan it: a live pane with no tile, invisible to close_terminal,
        // and (under a requestId) the failure is cached so the retry won't adopt
        // it. Best-effort reap the maybe-live session before failing, so a spawn
        // that DID take is killed, not leaked. Idempotent: a truly-absent session
        // is a no-op.
        let _ = tmux::kill_session_tree(&tmux_session);
        return Err(format!(
            "tmux session '{tmux_session}' did not materialize after new-session \
             (the spawn did not take; any partial session was reaped and nothing \
             was registered)"
        ));
    }
    Ok((id, tmux_session))
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
/// `kill_session_tree` - the same guarantee the webview's `kill_terminal`
/// gives: `kill-session` alone only SIGHUPs, which agents like `claude`
/// survive and leak; the tree kill SIGKILLs the pane's descendants first.
/// Idempotent (already-gone ⇒ success).
///
/// Headless-org: the dead tile is also dropped from the server tab registry and
/// a `sync_tabs` snapshot is forwarded, so the tile leaves its tab cleanly even
/// when that tab is hidden or the window is minimized (previously removal relied
/// on the UI's ~5s live-terminal reconcile). Args: `sessionId` (required).
fn close_terminal(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("close_terminal requires a 'sessionId' argument")?;
    let target = tmux_target(&session_id);
    // Registry-vs-reality (Incident C, ask #3): `kill_session_tree` is idempotent -
    // it returns Ok for an already-gone session too - so a caller could never tell
    // a real kill from a phantom close (ghost ids f0f3207b / 709c7252). Probe
    // liveness BEFORE the kill so we can report an HONEST outcome. We check first
    // (not the kill's own status) because the tree sweep SIGKILLs the pane pids,
    // which can auto-destroy the session before `kill-session` runs, making a real
    // kill look already-gone. The kill stays idempotent; only the label is refined.
    let existed = tmux::has_session(&target);
    tmux::kill_session_tree(&target)
        .map_err(|e| format!("failed to close terminal '{session_id}': {e}"))?;
    let outcome = if existed { "killed" } else { "already_gone" };
    // The registry keys tiles by the bare terminal id; strip an already-prefixed
    // caller the same way tmux_target normalizes the other direction.
    let tile_id = session_id.strip_prefix("th_").unwrap_or(&session_id);
    if ctx.tabs.remove_tile(tile_id) {
        let _ = forward_apply(ctx, "sync_tabs", &with_sync(ctx, json!({})));
    }
    // Captain-chat phase 2: a dead session leaves the captains registry too -
    // its captaincy is released and it drops out of every crew list.
    if ctx.captains.remove_session(tile_id) {
        let _ = captains_sync_apply(ctx);
    }
    Ok(json!({
        "accepted": "close_terminal",
        "sessionId": session_id,
        "target": target,
        // killed = a live session was reaped; already_gone = nothing was there to
        // kill (idempotent no-op). ok:true either way, so a retry stays safe.
        "outcome": outcome,
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
            tabs: Arc::new(TabRegistry::new()),
            captains: Arc::new(CaptainsRegistry::new()),
            fleet_watches: Arc::new(crate::fleet::FleetWatchRegistry::new()),
            idle_timeout: CONN_READ_TIMEOUT,
            attach_write_timeout: ATTACH_WRITE_TIMEOUT,
            max_attach_forwarders: MAX_ATTACH_FORWARDERS,
            attach_keepalive_interval: ATTACH_KEEPALIVE_INTERVAL,
            peer_is_loopback: true,
            token,
            read_token: String::new(),
            addr: String::new(),
            governor: Arc::new(SpawnGovernor::from_env()),
            audit: Arc::new(AuditLog::from_env()),
            requests: Arc::new(RequestCache::new()),
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

    /// Replace the [`AuditLog`] (tests point it at a temp dir so they never write to
    /// the real `~/.t-hub/audit`).
    #[cfg(test)]
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

    /// Share the [`crate::fleet::FleetWatchRegistry`] with the fleet notifier so
    /// `watch_fleet` / `unwatch_fleet` arm the same registry the notifier reads.
    /// `lib.rs` builds the `Arc` once and hands the same clone to the notifier;
    /// headless tests keep the in-memory one from [`new`](Self::new).
    pub fn with_fleet_watches(mut self, watches: Arc<crate::fleet::FleetWatchRegistry>) -> Self {
        self.fleet_watches = watches;
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
        // Point the audit sink at a per-token temp dir so dispatch_authenticated
        // tests never write to the real ~/.t-hub/audit.
        let audit_dir = std::env::temp_dir().join(format!("t-hub-ctl-test-{token}"));
        // A known read token so capability tests can present it; distinct from the
        // control token so ReadOnly vs Full resolution is exercised.
        ControlContext::new(Arc::new(StatusBridge::new()), visitor, token.to_string())
            .with_read_token(format!("read-{token}"))
            .with_audit(Arc::new(crate::audit::AuditLog::new(audit_dir)))
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

    /// The id-namespace bridge: the supervisor keys by the Claude UUID, but callers
    /// address a captain by its tile id (`captainSessionId`). `get_status` must
    /// resolve tile -> UUID via the status bridge, so a captain's status is no longer
    /// a spurious `unknown`. A UUID passed directly is unchanged.
    #[test]
    fn get_status_resolves_a_captain_tile_id_to_its_claude_uuid() {
        use t_hub_protocol::JournalEventType;
        let supervisor = Arc::new(StdMutex::new(Supervisor::new()));
        supervisor.lock().unwrap().ingest(
            Some("uuid-abc"),
            None,
            None,
            JournalEventType::SessionStart,
            1,
        );
        let sup_for_closure = supervisor.clone();
        let visitor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync> =
            Arc::new(move |f: &mut dyn FnMut(&Supervisor)| {
                let guard = sup_for_closure.lock().unwrap();
                f(&guard);
            });
        let status = Arc::new(StatusBridge::new());
        // The tile `cap01234` currently hosts Claude session `uuid-abc`.
        status.ingest(
            "uuid-abc",
            &json!({ "cwd": "/p", "tmux_session": "th_cap01234" }),
            1,
        );
        let ctx = ControlContext::new(status, visitor, "t".to_string());

        // Poll by the CAPTAIN tile id -> resolves to the UUID, returns the real status.
        let v = get_status(&ctx, &json!({ "sessionId": "cap01234" })).unwrap();
        assert_eq!(
            v.get("resolvedSessionId").and_then(|x| x.as_str()),
            Some("uuid-abc"),
            "tile id must resolve to the Claude UUID"
        );
        assert_eq!(
            v.get("status").and_then(|x| x.as_str()),
            Some("working"),
            "status must be the real supervisor status, not 'unknown'"
        );
        // A UUID (already a supervisor key) is passed through untouched.
        let v2 = get_status(&ctx, &json!({ "sessionId": "uuid-abc" })).unwrap();
        assert_eq!(
            v2.get("resolvedSessionId").and_then(|x| x.as_str()),
            Some("uuid-abc")
        );
        // A genuinely unknown id still resolves to unknown (no regression).
        let v3 = get_status(&ctx, &json!({ "sessionId": "ghostzzzz" })).unwrap();
        assert_eq!(v3.get("status").and_then(|x| x.as_str()), Some("unknown"));
    }

    #[test]
    fn watch_fleet_requires_a_live_orchestrator_terminal() {
        let ctx = test_ctx("t");
        // No live tmux for this id -> the arm is refused so a bogus id can't arm a
        // watch that could never deliver.
        let err = watch_fleet(&ctx, &json!({ "orchestratorSessionId": "nolivetile" }))
            .unwrap_err();
        assert!(err.contains("no live terminal"), "got: {err}");
        // And it requires the id at all.
        assert!(watch_fleet(&ctx, &json!({})).unwrap_err().contains("orchestratorSessionId"));
    }

    #[test]
    fn unwatch_and_list_fleet_watches_on_empty_registry() {
        let ctx = test_ctx("t");
        let v = unwatch_fleet(&ctx, &json!({ "orchestratorSessionId": "whoever" })).unwrap();
        assert_eq!(v.get("removed").and_then(|x| x.as_bool()), Some(false));
        let list = list_fleet_watches(&ctx).unwrap();
        assert_eq!(list.get("count").and_then(|x| x.as_u64()), Some(0));
    }

    #[test]
    fn arm_then_list_and_disarm_a_watch_via_the_registry() {
        // The command's tmux liveness guard needs a real session, so exercise the
        // arm/list/disarm round-trip through the shared registry directly (the
        // command is a thin validate-then-arm wrapper over exactly this).
        let ctx = test_ctx("t");
        ctx.fleet_watches
            .arm("orc12345", crate::fleet::WatchScope::Captains, vec![]);
        let list = list_fleet_watches(&ctx).unwrap();
        assert_eq!(list.get("count").and_then(|x| x.as_u64()), Some(1));
        let removed = unwatch_fleet(&ctx, &json!({ "orchestratorSessionId": "orc12345" })).unwrap();
        assert_eq!(removed.get("removed").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(
            list_fleet_watches(&ctx).unwrap().get("count").and_then(|x| x.as_u64()),
            Some(0)
        );
    }

    #[test]
    fn parse_watch_scope_accepts_captains_all_and_explicit_lists() {
        use crate::fleet::WatchScope;
        assert_eq!(parse_watch_scope(&json!({})).unwrap(), WatchScope::Captains);
        assert_eq!(
            parse_watch_scope(&json!({ "scope": "all" })).unwrap(),
            WatchScope::All
        );
        assert_eq!(
            parse_watch_scope(&json!({ "scope": ["a", "b"] })).unwrap(),
            WatchScope::Sessions(vec!["a".into(), "b".into()])
        );
        assert!(parse_watch_scope(&json!({ "scope": "bogus" })).is_err());
        assert!(parse_watch_scope(&json!({ "scope": [] })).is_err());
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
    fn spawn_terminal_without_sink_refuses_untracked_session() {
        // No apply sink (headless): there is no UI to adopt the tile, so spawn is
        // refused rather than creating an untracked tmux session (#17).
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).unwrap_err();
        assert!(err.contains("no UI"), "got: {err}");
    }

    #[test]
    fn spawn_terminal_with_sink_spawns_places_and_returns_id() {
        // Headless-org: with a UI sink wired, the SERVER spawns the real session,
        // resolves `tabName` against the authoritative registry (minting a hidden
        // tab without switching the active one), places the tile there, returns
        // the real id synchronously, and forwards id + registry snapshot.
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());
        ctx.tab_registry().replace(vec![TabRecord {
            id: "tab-1".into(),
            name: "Main".into(),
            tile_ids: vec![],
        }]);
        let v = dispatch(
            &ctx,
            "spawn_terminal",
            &json!({"cwd": "/tmp", "name": "logs", "tabName": "hidden-ops"}),
        )
        .unwrap();
        assert_eq!(v["accepted"], "spawn_terminal");
        assert_eq!(v["audited"], true);
        let id = v["id"].as_str().expect("real id returned synchronously").to_string();
        assert_eq!(v["placed"], true);
        let tab_id = v["tabId"].as_str().unwrap().to_string();
        assert_ne!(tab_id, "tab-1", "a NEW hidden tab is minted for the new name");

        // The registry (authoritative) holds the placement, and the active tab
        // was NOT touched (no focus steal).
        let snap = ctx.tab_registry().snapshot_full();
        let tab = snap.tabs.iter().find(|t| t.id == tab_id).expect("tab minted");
        assert_eq!(tab.name, "hidden-ops");
        assert_eq!(tab.tile_ids, vec![id.clone()]);
        assert_eq!(snap.active_tab_id, None);

        // The forward carries the id + snapshot for the UI to render from.
        {
            let calls = sink.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].0, "spawn_terminal");
            assert_eq!(calls[0].1["id"], json!(id));
            assert_eq!(calls[0].1["cwd"], "/tmp");
            assert_eq!(calls[0].1["name"], "logs");
            assert_eq!(calls[0].1["tabId"], json!(tab_id));
            assert!(calls[0].1["sync"]["seq"].as_u64().is_some());
        }
        // Reap the real session this spawned.
        dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    }

    #[test]
    fn spawn_terminal_forwards_the_startup_command() {
        // T-B: the socket spawn carries the webview presets' `startupCommand`
        // (the resume flow's `claude --resume <id>` in production; a harmless
        // echo here - headless-org spawns the REAL session server-side now, so
        // the command actually runs). The forward must carry it verbatim.
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());
        let v = dispatch(
            &ctx,
            "spawn_terminal",
            &json!({"cwd": "/tmp", "startupCommand": "echo resume-proof"}),
        )
        .unwrap();
        assert_eq!(v["accepted"], "spawn_terminal");
        assert_eq!(v["startupCommand"], "echo resume-proof");
        let first_id = v["id"].as_str().unwrap().to_string();

        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls[0].0, "spawn_terminal");
        assert_eq!(calls[0].1["startupCommand"], "echo resume-proof");
        // The snake_case alias parses too (loose-args convention).
        drop(calls);
        let v2 = dispatch(
            &ctx,
            "spawn_terminal",
            &json!({"cwd": "/tmp", "startup_command": "echo alias-proof"}),
        )
        .unwrap();
        assert_eq!(
            sink.calls.lock().unwrap()[1].1["startupCommand"],
            "echo alias-proof"
        );
        // Reap the real sessions these spawned.
        for id in [first_id.as_str(), v2["id"].as_str().unwrap()] {
            dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
        }
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

    /// Scaffold a REAL throwaway git repo (initial commit) with one linked
    /// worktree, under the OS temp dir. Returns `(base, repo, worktree)`; the
    /// caller removes `base` when done (best-effort — a unique name per call
    /// keeps reruns clean either way).
    fn scratch_repo_with_worktree() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)
    {
        fn sh_git(cwd: &std::path::Path, args: &[&str]) {
            let out = std::process::Command::new("git")
                .current_dir(cwd)
                .args(args)
                .output()
                .expect("git spawns");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let base = std::env::temp_dir().join(format!(
            "t-hub-tb-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir repo");
        sh_git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "hi").expect("seed file");
        sh_git(&repo, &["add", "."]);
        sh_git(
            &repo,
            &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init"],
        );
        let wt = base.join("wt");
        sh_git(&repo, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        assert!(wt.exists(), "worktree dir created");
        (base, repo, wt)
    }

    #[test]
    fn remove_worktree_sinkless_with_subscribers_broadcasts_then_removes() {
        // T-B (closing T12 deviation 2): with no sink but a socket subscriber,
        // the server broadcasts the detach forward and then runs the git
        // removal ITSELF — the native-only path stops refusing.
        let (base, repo, wt) = scratch_repo_with_worktree();

        let fanout = Arc::new(EventFanout::new());
        let ctx = test_ctx("t").with_event_fanout(fanout.clone());
        let mut reader = subscribe_test_reader(&fanout);
        let v = dispatch(
            &ctx,
            "remove_worktree",
            &json!({"repoRoot": repo.to_str().unwrap(), "worktreePath": wt.to_str().unwrap()}),
        )
        .unwrap();
        assert_eq!(v["accepted"], "remove_worktree");
        assert_eq!(v["applied"], true);
        assert_eq!(v["removed"], true, "this path CONFIRMS the removal: {v:?}");

        // The detach forward was queued to the subscriber before git ran.
        let frame = read_event_frame(&mut reader);
        assert_eq!(frame["event"], APPLY_EVENT_CHANNEL);
        assert_eq!(frame["payload"]["command"], "remove_worktree_workspace");
        assert_eq!(
            frame["payload"]["args"]["worktreePath"],
            json!(wt.to_str().unwrap())
        );

        assert!(!wt.exists(), "the worktree dir must be gone");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn list_worktrees_lists_main_then_linked() {
        let (base, repo, wt) = scratch_repo_with_worktree();
        let ctx = test_ctx("t");
        let v = dispatch(&ctx, "list_worktrees", &json!({"cwd": repo.to_str().unwrap()})).unwrap();
        let list = v["worktrees"].as_array().expect("worktrees array");
        assert_eq!(list.len(), 2, "main + linked: {list:?}");
        assert_eq!(list[0]["isLinked"], false);
        assert_eq!(list[1]["isLinked"], true);
        assert_eq!(list[1]["path"], json!(wt.to_str().unwrap()));
        // The IPC-twin alias resolves to the same handler.
        let v2 =
            dispatch(&ctx, "git_worktree_list", &json!({"cwd": repo.to_str().unwrap()})).unwrap();
        assert_eq!(v2["worktrees"], v["worktrees"]);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn reprobe_reaped_create_worktree_resolves_against_reality() {
        // M1 full fix. A create_worktree whose InFlight reservation was reaped is
        // retried with the same requestId; before re-applying we RE-PROBE reality.
        let (base, repo, wt) = scratch_repo_with_worktree();
        let ctx = test_ctx("t");

        // The worktree EXISTS on disk (the original DID land): the re-probe must
        // resolve to a success outcome tagged reprobedAfterReap, NOT None (which
        // would let dispatch re-run git worktree add and duplicate/error).
        let args = json!({
            "repoRoot": repo.to_str().unwrap(),
            "worktreePath": wt.to_str().unwrap(),
        });
        let outcome = reprobe_reaped_request(&ctx, "create_worktree", &args)
            .expect("existing worktree must resolve against reality");
        let v = outcome.expect("resolved outcome is Ok");
        assert_eq!(v["accepted"], "create_worktree");
        assert_eq!(v["alreadyCreated"], true);
        assert_eq!(v["reprobedAfterReap"], true);

        // A worktree path that does NOT exist ⇒ None: the original truly died, so
        // dispatch proceeds to a fresh (re-checked) apply.
        let missing = json!({
            "repoRoot": repo.to_str().unwrap(),
            "worktreePath": base.join("never-created").to_str().unwrap(),
        });
        assert!(
            reprobe_reaped_request(&ctx, "create_worktree", &missing).is_none(),
            "an absent worktree must NOT resolve - it should re-apply fresh"
        );

        // spawn_terminal has a SERVER-minted id: nothing in args to probe by ⇒ None.
        assert!(
            reprobe_reaped_request(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).is_none(),
            "spawn_terminal has no probe-able artifact in its args"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn list_worktrees_requires_cwd_and_is_empty_outside_a_repo() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "list_worktrees", &json!({})).unwrap_err();
        assert!(err.contains("cwd"), "got: {err}");
        // Best-effort like the IPC twin: a non-repo dir yields an empty list.
        let v = dispatch(&ctx, "list_worktrees", &json!({"cwd": "/"})).unwrap();
        assert_eq!(v["worktrees"], json!([]));
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
        for cmd in ["create_worktree", "remove_worktree", "list_worktrees"] {
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
    fn focus_tab_is_organization_apply() {
        // Headless-org: focus_tab is STRICT (the tab must exist in the registry)
        // and mirrors the new active tab there. No sink (headless): accepted +
        // audited, but not applied.
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "focus_tab", &json!({"tabId": "tab-1"})).unwrap_err();
        assert!(err.contains("unknown tabId"), "got: {err}");

        ctx.tab_registry().replace(vec![TabRecord {
            id: "tab-1".into(),
            name: "Main".into(),
            tile_ids: vec![],
        }]);
        let v = dispatch(&ctx, "focus_tab", &json!({"tabId": "tab-1"})).unwrap();
        assert_eq!(v["accepted"], "focus_tab");
        assert_eq!(v["audited"], true);
        assert_eq!(v["applied"], false);
        assert_eq!(
            ctx.tab_registry().snapshot_full().active_tab_id.as_deref(),
            Some("tab-1")
        );
    }

    #[test]
    fn new_tab_returns_a_tab_id_and_registers_it() {
        // TASK C: new_tab mints an id CORE-side, returns it, and records it so
        // list_tabs sees it immediately (addressable before any frontend report).
        let ctx = test_ctx("t");
        let v = dispatch(&ctx, "new_tab", &json!({"name": "Logs"})).unwrap();
        assert_eq!(v["accepted"], "new_tab");
        assert_eq!(v["name"], "Logs");
        let tab_id = v["tabId"].as_str().expect("new_tab returns a tabId");
        assert!(!tab_id.is_empty());

        let tabs = dispatch(&ctx, "list_tabs", &Value::Null).unwrap();
        let arr = tabs["tabs"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], tab_id);
        assert_eq!(arr[0]["name"], "Logs");
        assert_eq!(arr[0]["tileIds"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn new_tab_auto_names_when_no_name_given() {
        let ctx = test_ctx("t");
        let v = dispatch(&ctx, "new_tab", &Value::Null).unwrap();
        assert_eq!(v["name"], "Workspace 1");
        let v2 = dispatch(&ctx, "new_tab", &Value::Null).unwrap();
        assert_eq!(v2["name"], "Workspace 2");
    }

    #[test]
    fn new_tab_then_move_tile_reflected_in_list_tabs() {
        // The headless acceptance for #22: new_tab -> get its id -> move_tile a
        // terminal into it -> list_tabs shows the tile in that tab.
        let ctx = test_ctx("t");
        let created = dispatch(&ctx, "new_tab", &json!({"name": "Target"})).unwrap();
        let tab_id = created["tabId"].as_str().unwrap().to_string();

        dispatch(
            &ctx,
            "move_tile",
            &json!({"terminalId": "term-xyz", "tabId": tab_id}),
        )
        .unwrap();

        let tabs = dispatch(&ctx, "list_tabs", &Value::Null).unwrap();
        let target = tabs["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == tab_id.as_str())
            .expect("target tab present");
        let tiles: Vec<&str> = target["tileIds"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(tiles, vec!["term-xyz"]);
    }

    #[test]
    fn stale_report_is_rejected_and_answers_with_the_snapshot() {
        // Headless-org acceptance for requirement 2: a server-side move_tile must
        // survive a UI report that predates it (the exact lost-update repro: three
        // accepted move_tile calls, registry silently reverted by the reporter).
        let ctx = test_ctx("t");
        // UI boots and reports its layout (legacy/no baseSeq → accepted).
        let v = dispatch(
            &ctx,
            "report_workspace_tabs",
            &json!({"tabs": [
                {"id": "t1", "name": "Workspace 1", "tileIds": ["aa"]},
                {"id": "t2", "name": "hidden", "tileIds": []},
            ], "activeTabId": "t1", "baseSeq": 0}),
        )
        .unwrap();
        let seq = v["seq"].as_u64().unwrap();

        // A headless move into the hidden tab bumps the revision.
        dispatch(&ctx, "move_tile", &json!({"terminalId": "aa", "tabId": "t2"})).unwrap();

        // The UI (which never applied the move - hidden tab, suspended webview…)
        // reports its stale layout: REJECTED, answered with the snapshot.
        let v = dispatch(
            &ctx,
            "report_workspace_tabs",
            &json!({"tabs": [
                {"id": "t1", "name": "Workspace 1", "tileIds": ["aa"]},
                {"id": "t2", "name": "hidden", "tileIds": []},
            ], "activeTabId": "t1", "baseSeq": seq}),
        )
        .unwrap();
        assert_eq!(v["stale"], true);
        let tabs = v["tabs"].as_array().unwrap();
        let t2 = tabs.iter().find(|t| t["id"] == "t2").unwrap();
        assert_eq!(t2["tileIds"], json!(["aa"]), "the move survives the stale report");

        // list_tabs stays truthful: the tile is in the hidden tab.
        let tabs = dispatch(&ctx, "list_tabs", &Value::Null).unwrap();
        let t2 = tabs["tabs"].as_array().unwrap().iter().find(|t| t["id"] == "t2").unwrap();
        assert_eq!(t2["tileIds"], json!(["aa"]));

        // A report based on the CURRENT revision is accepted (normal UI flow).
        let cur = tabs["seq"].as_u64().unwrap();
        let v = dispatch(
            &ctx,
            "report_workspace_tabs",
            &json!({"tabs": [
                {"id": "t1", "name": "Workspace 1", "tileIds": []},
                {"id": "t2", "name": "hidden", "tileIds": ["aa"]},
            ], "activeTabId": "t1", "baseSeq": cur}),
        )
        .unwrap();
        assert_eq!(v["reported"], 2);
    }

    #[test]
    fn close_tab_headless_lifecycle_policy() {
        // Requirement 3: tiles leave their tab on close_terminal, and an emptied
        // auto-created tab is closeable headlessly - with the documented guards.
        let ctx = test_ctx("t");
        ctx.tab_registry().replace(vec![
            TabRecord {
                id: "t1".into(),
                name: "Workspace 1".into(),
                tile_ids: vec!["keep".into()],
            },
            TabRecord {
                id: "t2".into(),
                name: "staging".into(),
                tile_ids: vec!["dead".into()],
            },
        ]);

        // A non-empty tab is refused without force.
        let err = dispatch(&ctx, "close_tab", &json!({"tabId": "t2"})).unwrap_err();
        assert!(err.contains("close its terminals first"), "got: {err}");

        // close_terminal drops the tile from its tab (tmux kill is idempotent on
        // an already-gone session, so this exercises the registry path headlessly).
        dispatch(&ctx, "close_terminal", &json!({"sessionId": "dead"})).unwrap();
        let t2 = ctx
            .tab_registry()
            .snapshot()
            .into_iter()
            .find(|t| t.id == "t2")
            .unwrap();
        assert!(t2.tile_ids.is_empty(), "the closed tile left its tab");

        // The emptied tab closes headlessly (by name here - id also works).
        let v = dispatch(&ctx, "close_tab", &json!({"tabName": "staging"})).unwrap();
        assert_eq!(v["accepted"], "close_tab");
        assert_eq!(v["tabId"], "t2");
        assert!(ctx.tab_registry().snapshot().iter().all(|t| t.id != "t2"));

        // The LAST tab is never closed.
        let err = dispatch(&ctx, "close_tab", &json!({"tabId": "t1"})).unwrap_err();
        assert!(err.contains("last tab"), "got: {err}");
    }

    #[test]
    fn placement_falls_back_when_the_target_tab_vanished() {
        // The tab-closed-during-spawn race, at the placement primitive: the tab
        // resolved before the tmux spawn may be gone by placement time. The tile
        // must ALWAYS land in the registry - active tab first, else first tab -
        // and the actual tab id is returned.
        let ctx = test_ctx("t");
        ctx.tab_registry().replace(vec![
            TabRecord {
                id: "t1".into(),
                name: "Workspace 1".into(),
                tile_ids: vec![],
            },
            TabRecord {
                id: "t2".into(),
                name: "Workspace 2".into(),
                tile_ids: vec![],
            },
        ]);
        assert!(ctx.tab_registry().set_active_tab("t2"));

        // Target vanished -> falls back to the ACTIVE tab.
        let placed = ctx
            .tab_registry()
            .place_tile_with_fallback("tile-a", Some("closed-mid-spawn"));
        assert_eq!(placed.as_deref(), Some("t2"));
        // Target vanished AND no active pointer -> first tab.
        ctx.tab_registry().replace(vec![TabRecord {
            id: "only".into(),
            name: "Solo".into(),
            tile_ids: vec![],
        }]);
        let placed = ctx
            .tab_registry()
            .place_tile_with_fallback("tile-b", Some("also-gone"));
        assert_eq!(placed.as_deref(), Some("only"));
        let snap = ctx.tab_registry().snapshot();
        assert_eq!(snap[0].tile_ids, vec!["tile-b"]);
        // Empty registry -> unplaced (None), the only case a tile stays out.
        ctx.tab_registry().replace(vec![]);
        assert_eq!(
            ctx.tab_registry().place_tile_with_fallback("tile-c", Some("x")),
            None
        );
    }

    #[test]
    fn spawn_survives_a_concurrent_close_of_its_target_tab() {
        // Dispatch-level tab-closed-during-spawn race: close_tab races the spawn's
        // resolve->tmux->place window. Whichever side wins, the invariant holds:
        // the spawned session ends up in EXACTLY ONE registry tab, and the
        // response's tabId names that tab (fallback placement is reflected).
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink);
        ctx.tab_registry().replace(vec![
            TabRecord {
                id: "keep".into(),
                name: "Workspace 1".into(),
                tile_ids: vec![],
            },
            TabRecord {
                id: "doomed".into(),
                name: "staging".into(),
                tile_ids: vec![],
            },
        ]);
        assert!(ctx.tab_registry().set_active_tab("keep"));

        let closer = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                // Either outcome is legal: the close wins (spawn falls back to
                // "keep") or the placement wins (close refuses the non-empty tab).
                let _ = dispatch(&ctx, "close_tab", &json!({"tabId": "doomed"}));
            })
        };
        let v = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp", "tabId": "doomed"}))
            .unwrap();
        closer.join().unwrap();

        let id = v["id"].as_str().unwrap().to_string();
        let placed_tab = v["tabId"].as_str().expect("always placed").to_string();
        assert_eq!(v["placed"], true);
        let owners: Vec<String> = ctx
            .tab_registry()
            .snapshot()
            .into_iter()
            .filter(|t| t.tile_ids.iter().any(|x| x == &id))
            .map(|t| t.id)
            .collect();
        assert_eq!(owners, vec![placed_tab], "tile in exactly the reported tab");

        dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    }

    #[test]
    fn back_to_back_close_tab_keeps_the_active_pointer_valid() {
        // A second close (or a close racing a focus) must never leave the
        // registry's activeTabId pointing at a deleted tab: removal + pointer
        // fixup are atomic under the registry lock, and focus_tab's validate+set
        // is a single atomic operation.
        let ctx = test_ctx("t");
        ctx.tab_registry().replace(vec![
            TabRecord { id: "a".into(), name: "A".into(), tile_ids: vec![] },
            TabRecord { id: "b".into(), name: "B".into(), tile_ids: vec![] },
            TabRecord { id: "c".into(), name: "C".into(), tile_ids: vec![] },
        ]);
        assert!(ctx.tab_registry().set_active_tab("c"));

        let active_is_valid = |ctx: &ControlContext| {
            let snap = ctx.tab_registry().snapshot_full();
            let active = snap.active_tab_id.expect("active pointer set");
            assert!(
                snap.tabs.iter().any(|t| t.id == active),
                "active '{active}' must reference an existing tab; tabs: {:?}",
                snap.tabs.iter().map(|t| t.id.clone()).collect::<Vec<_>>()
            );
        };

        // Close the ACTIVE tab, then immediately close the tab the pointer
        // healed onto - the pointer must stay valid after each step.
        dispatch(&ctx, "close_tab", &json!({"tabId": "c"})).unwrap();
        active_is_valid(&ctx);
        let healed = ctx.tab_registry().snapshot_full().active_tab_id.unwrap();
        dispatch(&ctx, "close_tab", &json!({"tabId": healed})).unwrap();
        active_is_valid(&ctx);

        // focus_tab on the now-deleted tab fails cleanly, pointer untouched.
        let err = dispatch(&ctx, "focus_tab", &json!({"tabId": "c"})).unwrap_err();
        assert!(err.contains("unknown tabId"), "got: {err}");
        active_is_valid(&ctx);

        // Concurrent closes from a 3-tab registry: whichever interleaving wins,
        // the surviving pointer references a live tab.
        ctx.tab_registry().replace(vec![
            TabRecord { id: "a".into(), name: "A".into(), tile_ids: vec![] },
            TabRecord { id: "b".into(), name: "B".into(), tile_ids: vec![] },
            TabRecord { id: "c".into(), name: "C".into(), tile_ids: vec![] },
        ]);
        assert!(ctx.tab_registry().set_active_tab("b"));
        let t1 = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = ctx.tab_registry().remove_tab("b", false);
            })
        };
        let t2 = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = ctx.tab_registry().remove_tab("c", false);
            })
        };
        t1.join().unwrap();
        t2.join().unwrap();
        active_is_valid(&ctx);
    }

    #[test]
    fn spawn_terminal_default_placement_is_the_active_tab_without_switching() {
        // No tabName/tabId: the tile lands in the tab the USER has active (per the
        // registry mirror) - matching the "+" menu - and never switches it.
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());
        dispatch(
            &ctx,
            "report_workspace_tabs",
            &json!({"tabs": [
                {"id": "t1", "name": "Workspace 1", "tileIds": []},
                {"id": "t2", "name": "Workspace 2", "tileIds": []},
            ], "activeTabId": "t2"}),
        )
        .unwrap();

        let v = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).unwrap();
        let id = v["id"].as_str().unwrap().to_string();
        assert_eq!(v["tabId"], "t2", "default placement is the active tab");
        let snap = ctx.tab_registry().snapshot_full();
        assert_eq!(snap.active_tab_id.as_deref(), Some("t2"), "focus untouched");
        dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    }

    #[test]
    fn report_workspace_tabs_replaces_the_registry() {
        // The frontend's up-sync (via the Tauri command, exercised here directly on
        // the shared registry) makes list_tabs reflect the live UI, including
        // UI-created tabs and real tile membership.
        let ctx = test_ctx("t");
        ctx.tab_registry().replace(vec![
            TabRecord {
                id: "t1".into(),
                name: "Main".into(),
                tile_ids: vec!["a".into(), "b".into()],
            },
            TabRecord {
                id: "t2".into(),
                name: "Side".into(),
                tile_ids: vec![],
            },
        ]);
        let tabs = dispatch(&ctx, "list_tabs", &Value::Null).unwrap();
        assert_eq!(tabs["count"], 2);
        assert_eq!(tabs["tabs"][0]["id"], "t1");
        assert_eq!(tabs["tabs"][0]["tileIds"][1], "b");
        assert_eq!(tabs["tabs"][1]["name"], "Side");
    }

    #[test]
    fn create_worktree_named_placement_reuses_a_tab_by_name() {
        // TASK C: a create_worktree with a tabName that already exists resolves to
        // the SAME tab id (no duplicate), and the forward carries that id so the
        // frontend places the tile deterministically, not into the focused tab.
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());
        // Seed an existing tab named "control-surface".
        ctx.tab_registry().replace(vec![TabRecord {
            id: "existing-tab".into(),
            name: "control-surface".into(),
            tile_ids: vec![],
        }]);
        // A create_worktree targeting that name should reuse `existing-tab`. We
        // resolve the tab BEFORE git runs by calling the registry directly for the
        // assertion (git::worktree_add needs a real repo, out of scope for a unit
        // test), mirroring the handler's own resolution.
        assert_eq!(
            ctx.tab_registry().id_for_name("control-surface"),
            Some("existing-tab".to_string())
        );
    }

    /// Live round-trip through dispatch: spawn a real tmux session, type a line
    /// via `send_text`, read it back via `read_terminal`, then `close_terminal`.
    /// Needs a real tmux on PATH (WSL2 dev shell; not the Windows CI target).
    #[test]
    fn live_send_read_close_roundtrip() {
        // The id must honor the production invariant "the id IS the tmux session
        // suffix, capped at 8 chars" (`tmux::target_for_id`) — the previous long
        // `mcp3test<nanos>` id created `th_mcp3test<nanos>` but dispatched
        // against `th_mcp3test`, so send_text hit a session that never existed.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let id = format!("{:08x}", (nanos as u64) & 0xffff_ffff);
        let target = tmux::target_for_id(&id);
        let _ = tmux::kill_session(&target);
        tmux::new_session(&target, "/tmp", None).expect("spawn session");
        // Deterministic geometry regardless of what the server's latest client
        // reports (the wedged-2x24 gotcha; see tmux::resize_window_for_tests).
        tmux::resize_window_for_tests(&target, 80, 24).expect("resize session");

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
            for _ in 0..4 {
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

        // A valid token but a version NEWER than the server speaks is rejected — the
        // gate fires before dispatch, with a clear, actionable message.
        let bad = send(json!({"token": "secret", "command": "list_tabs", "v": 999}));
        assert_eq!(bad["ok"], false);
        assert!(
            bad["error"]
                .as_str()
                .unwrap()
                .contains("protocol version too new"),
            "got: {bad}"
        );

        // The matching version passes the gate and dispatches normally.
        let good = send(json!({"token": "secret", "command": "list_tabs", "v": PROTOCOL_VERSION}));
        assert_eq!(good["ok"], true, "got: {good}");

        // A LOWER version (a v1 client against this v2 server) is still accepted —
        // the protocol is backward-compatible (T13 binary framing is opt-in), so the
        // live webview keeps working unchanged.
        let v1 = send(json!({"token": "secret", "command": "list_tabs", "v": 1}));
        assert_eq!(v1["ok"], true, "got: {v1}");

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
        // focus_session and a targetId-only move_tile (within-tab reorder) stay
        // legacy pass-through forwards.
        let ctx = test_ctx("t");
        for (cmd, args) in [
            ("focus_session", json!({"sessionId": "s1"})),
            ("move_tile", json!({"terminalId": "t1", "targetId": "t2"})),
        ] {
            let v = dispatch(&ctx, cmd, &args).unwrap();
            assert_eq!(v["accepted"], cmd);
            assert_eq!(v["audited"], true);
            assert_eq!(v["applied"], false);
        }
        // Headless-org: registry-first mutations are STRICT - an unknown target
        // is a hard error, not the old silent accept-then-lose.
        for (cmd, args) in [
            ("move_tile", json!({"terminalId": "t1", "tabId": "nope"})),
            ("rename_tab", json!({"tabId": "nope", "name": "x"})),
            ("close_tab", json!({"tabId": "nope"})),
        ] {
            let err = dispatch(&ctx, cmd, &args).unwrap_err();
            assert!(err.contains("unknown tabId"), "{cmd}: {err}");
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
        ctx.tab_registry().replace(vec![
            TabRecord {
                id: "tab-1".into(),
                name: "Main".into(),
                tile_ids: vec!["term-1".into()],
            },
            TabRecord {
                id: "tab-2".into(),
                name: "Side".into(),
                tile_ids: vec![],
            },
        ]);

        for (cmd, args) in [
            ("focus_session", json!({"sessionId": "term-1"})),
            ("move_tile", json!({"terminalId": "term-1", "tabId": "tab-2"})),
            ("rename_tab", json!({"tabId": "tab-2", "name": "Ops"})),
        ] {
            let v = dispatch(&ctx, cmd, &args).unwrap();
            assert_eq!(v["accepted"], cmd);
            assert_eq!(v["audited"], true);
            // With a sink wired, the action is forwarded to the UI and applied.
            assert_eq!(v["applied"], true, "expected applied:true for {cmd}");
        }

        // Every Organization-tier command reached the sink, in order, with args.
        let calls = sink.calls.lock().unwrap();
        let names: Vec<&str> = calls.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(names, ["focus_session", "move_tile", "rename_tab"]);
        assert_eq!(calls[0].1, json!({"sessionId": "term-1"}));

        // Headless-org: registry-first forwards carry the authoritative snapshot
        // (`sync.seq` / `sync.tabs`) so the UI renders FROM the registry - the
        // move is visible in the snapshot even before any UI report.
        let sync = &calls[1].1["sync"];
        assert!(sync["seq"].as_u64().unwrap() >= 1);
        let tabs = sync["tabs"].as_array().unwrap();
        let tab2 = tabs.iter().find(|t| t["id"] == "tab-2").unwrap();
        assert_eq!(tab2["tileIds"], json!(["term-1"]));
        assert_eq!(calls[2].1["name"], "Ops");
    }

    /// Register a real loopback socket as an event subscriber on `fanout`,
    /// returning a line reader over the client end (T12 broadcast tests).
    fn subscribe_test_reader(fanout: &EventFanout) -> impl std::io::BufRead {
        use std::io::BufReader;
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).expect("connect");
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let (server_side, _) = listener.accept().expect("accept");
        fanout.register(server_side);
        BufReader::new(client)
    }

    /// Read one `{"event":..,"payload":..}` frame from a subscriber reader.
    fn read_event_frame(reader: &mut impl std::io::BufRead) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read event frame");
        serde_json::from_str(line.trim()).expect("event frame is JSON")
    }

    #[test]
    fn apply_forwards_are_broadcast_to_event_subscribers() {
        // T12: every accepted Organization forward ALSO reaches event
        // subscribers on `control://apply`, while the webview sink keeps
        // receiving exactly what it always did.
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let fanout = Arc::new(EventFanout::new());
        let ctx = test_ctx("t")
            .with_apply_sink(sink.clone())
            .with_event_fanout(fanout.clone());
        ctx.tab_registry().replace(vec![TabRecord {
            id: "tab-1".into(),
            name: "Main".into(),
            tile_ids: vec![],
        }]);
        let mut reader = subscribe_test_reader(&fanout);

        // A plain organization apply: broadcast mirrors the sink call.
        let v = dispatch(&ctx, "focus_tab", &json!({"tabId": "tab-1"})).unwrap();
        assert_eq!(v["applied"], true);
        let frame = read_event_frame(&mut reader);
        assert_eq!(frame["event"], APPLY_EVENT_CHANNEL);
        assert_eq!(frame["payload"]["command"], "focus_tab");
        assert_eq!(frame["payload"]["args"], json!({"tabId": "tab-1"}));

        // new_tab: the broadcast carries the SAME core-minted id the caller got.
        let v = dispatch(&ctx, "new_tab", &json!({"name": "Logs"})).unwrap();
        let frame = read_event_frame(&mut reader);
        assert_eq!(frame["payload"]["command"], "new_tab");
        assert_eq!(frame["payload"]["args"]["id"], v["tabId"]);
        assert_eq!(frame["payload"]["args"]["name"], "Logs");

        // spawn_terminal: the server spawns + places (headless-org), so sink AND
        // subscribers both hear the forward WITH the real id + registry snapshot.
        let v = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp", "name": "n"})).unwrap();
        assert_eq!(v["accepted"], "spawn_terminal");
        let spawned_id = v["id"].as_str().unwrap().to_string();
        let frame = read_event_frame(&mut reader);
        assert_eq!(frame["payload"]["command"], "spawn_terminal");
        assert_eq!(frame["payload"]["args"]["cwd"], "/tmp");
        assert_eq!(frame["payload"]["args"]["id"], json!(spawned_id));
        assert!(frame["payload"]["args"]["sync"]["seq"].as_u64().is_some());

        // remove_worktree (sink path): subscribers hear the detach forward too.
        let v = dispatch(
            &ctx,
            "remove_worktree",
            &json!({"repoRoot": "/r", "worktreePath": "/r/wt"}),
        )
        .unwrap();
        assert_eq!(v["accepted"], "remove_worktree");
        let frame = read_event_frame(&mut reader);
        assert_eq!(frame["payload"]["command"], "remove_worktree_workspace");
        assert_eq!(frame["payload"]["args"]["worktreePath"], "/r/wt");

        // The sink saw every forward, unchanged by the broadcast riding along.
        let names: Vec<String> = sink.calls.lock().unwrap().iter().map(|(c, _)| c.clone()).collect();
        assert_eq!(
            names,
            ["focus_tab", "new_tab", "spawn_terminal", "remove_worktree_workspace"]
        );

        // Reap the real session the spawn created.
        dispatch(&ctx, "close_terminal", &json!({"sessionId": spawned_id})).unwrap();
    }

    #[test]
    fn forward_without_sink_counts_event_subscribers_as_delivery() {
        // T12: a headless server fronting the native cockpit has no ApplySink;
        // reaching an event subscriber is what "applied" means there.
        let fanout = Arc::new(EventFanout::new());
        let ctx = test_ctx("t").with_event_fanout(fanout.clone());
        ctx.tab_registry().replace(vec![TabRecord {
            id: "x".into(),
            name: "Main".into(),
            tile_ids: vec![],
        }]);
        let mut reader = subscribe_test_reader(&fanout);

        let v = dispatch(&ctx, "rename_tab", &json!({"tabId": "x", "name": "ops"})).unwrap();
        assert_eq!(v["applied"], true, "subscriber delivery counts without a sink");
        let frame = read_event_frame(&mut reader);
        assert_eq!(frame["payload"]["command"], "rename_tab");
        // (Sink-less AND subscriber-less stays applied:false - covered by
        // `organization_actions_are_accepted_and_audited`.)
    }

    #[test]
    fn report_workspace_tabs_replaces_the_registry_for_list_tabs() {
        // T12: the socket twin of the Tauri report command - the native client's
        // half of the registry mirror.
        let ctx = test_ctx("t");
        let v = dispatch(
            &ctx,
            "report_workspace_tabs",
            &json!({"tabs": [
                {"id": "t1", "name": "Workspace 1", "tileIds": ["aa", "bb"]},
                {"id": "t2", "name": "ops", "tileIds": []},
            ]}),
        )
        .unwrap();
        assert_eq!(v["reported"], 2);

        let tabs = dispatch(&ctx, "list_tabs", &json!({})).unwrap();
        assert_eq!(tabs["count"], 2);
        assert_eq!(tabs["tabs"][0]["id"], "t1");
        assert_eq!(tabs["tabs"][0]["tileIds"], json!(["aa", "bb"]));
        assert_eq!(tabs["tabs"][1]["name"], "ops");

        // A later report REPLACES wholesale (last writer wins, webview parity).
        dispatch(&ctx, "report_workspace_tabs", &json!({"tabs": []})).unwrap();
        assert_eq!(dispatch(&ctx, "list_tabs", &json!({})).unwrap()["count"], 0);

        // Malformed shapes are a clear error, not a partial replace.
        let err = dispatch(&ctx, "report_workspace_tabs", &json!({})).unwrap_err();
        assert!(err.contains("requires a 'tabs'"), "got: {err}");
        let err =
            dispatch(&ctx, "report_workspace_tabs", &json!({"tabs": [{"name": 7}]})).unwrap_err();
        assert!(err.contains("bad 'tabs' shape"), "got: {err}");
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
            read_token: "rdonly".into(),
            pid: 42,
            protocol_version: PROTOCOL_VERSION,
            local_control_token: "full".into(),
        };
        let s = serde_json::to_string(&h).unwrap();
        let back: ControlHandshake = serde_json::from_str(&s).unwrap();
        assert_eq!(back.addr, "127.0.0.1:5000");
        assert_eq!(back.token, "abc");
        assert_eq!(back.read_token, "rdonly");
        assert_eq!(back.pid, 42);
        assert_eq!(back.protocol_version, PROTOCOL_VERSION);
        // `local_control_token` is in-process-only: it is NEVER serialized, so it
        // does not survive the JSON round-trip and comes back empty (its default).
        assert!(!s.contains("local_control_token"), "field must not serialize");
        assert!(!s.contains("full"), "in-process token must not appear in JSON");
        assert_eq!(back.local_control_token, "");
    }

    #[test]
    fn old_handshake_without_read_token_still_parses() {
        // Backward-compat: a control.json written before Phase 2 (no read_token
        // field) must still deserialize - the field defaults to empty.
        let json = r#"{"addr":"127.0.0.1:9","token":"t","pid":1,"protocol_version":2}"#;
        let hs: ControlHandshake = serde_json::from_str(json).unwrap();
        assert_eq!(hs.token, "t");
        assert_eq!(hs.read_token, "");
        // The Phase-3 in-process field is absent from old files and defaults empty.
        assert_eq!(hs.local_control_token, "");
    }

    // ---- s27: attach path vs client churn -----------------------------------

    use std::time::Duration;

    /// The attach-churn tests share the process-global forwarder counter (and
    /// real tmux sessions), so they run serialized; everything else in this
    /// module stays parallel. Poison is ignored: a failed churn test must not
    /// cascade into the other one.
    static ATTACH_TEST_SERIAL: StdMutex<()> = StdMutex::new(());

    fn attach_serial_guard() -> std::sync::MutexGuard<'static, ()> {
        ATTACH_TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Stand up the REAL accept loop (`serve`, not per-connection `handle_conn`)
    /// on an ephemeral loopback port. The thread parks in accept for the process
    /// lifetime, exactly like the `control_probe_server` example.
    fn spawn_attach_listener(ctx: ControlContext) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || serve(listener, ctx));
        addr
    }

    /// A disposable real tmux session for attach tests; returns (id, tmux name).
    fn churn_tmux_session(tag: &str) -> (String, String) {
        let id = format!(
            "s27{tag}{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let target = format!("th_{id}");
        let _ = tmux::kill_session(&target);
        tmux::new_session(&target, "/tmp", None).expect("spawn churn tmux session");
        (id, target)
    }

    /// Send a v1 `attach_pty` request line on `stream`.
    fn send_attach_request(stream: &mut TcpStream, token: &str, session_id: &str) {
        let mut frame = serde_json::to_vec(&json!({
            "token": token,
            "command": ATTACH_PTY_COMMAND,
            "args": { "sessionId": session_id, "cols": 80, "rows": 24 },
        }))
        .unwrap();
        frame.push(b'\n');
        stream.write_all(&frame).expect("write attach_pty request");
    }

    /// Send a v1 `{"write":"<b64>"}` input frame (keystrokes) on `stream`.
    fn send_write_frame(stream: &mut TcpStream, keys: &str) {
        let mut frame = serde_json::to_vec(&json!({ "write": STANDARD.encode(keys) })).unwrap();
        frame.push(b'\n');
        stream.write_all(&frame).expect("write input frame");
    }

    /// Read one newline-delimited JSON frame; panics on EOF (caller expects one).
    fn read_json_frame(reader: &mut BufReader<TcpStream>) -> Value {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read frame");
        assert!(n > 0, "connection closed before the expected frame");
        serde_json::from_str(line.trim()).expect("frame is JSON")
    }

    /// Poll `ok` until it holds or `deadline` elapses (then panic with `what`).
    fn eventually(what: &str, deadline: Duration, mut ok: impl FnMut() -> bool) {
        let start = std::time::Instant::now();
        while start.elapsed() < deadline {
            if ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for {what}");
    }

    /// THE s27 regression: N clients die abruptly at every stage of the attach
    /// lifecycle - before speaking, mid-request, pre-seed, post-seed via RST,
    /// and the incident's exact shape: a client that starts a firehose, stops
    /// draining, and silently HOLDS its socket (the un-reaped CLOSE_WAIT
    /// forwarders that wedged the live server's new-attach path). The server
    /// must reap every forwarder on its own and keep serving fresh attaches.
    #[test]
    fn attach_path_survives_abrupt_client_churn() {
        let _serial = attach_serial_guard();
        eventually("forwarder table to drain before the test", Duration::from_secs(10), || {
            attach_forwarder_count() == 0
        });

        let mut ctx = test_ctx("churn-secret");
        ctx.idle_timeout = Duration::from_millis(500);
        ctx.attach_write_timeout = Duration::from_millis(300);
        let addr = spawn_attach_listener(ctx);
        let conns_baseline = ACTIVE_CONNS.load(Ordering::Relaxed);

        let (id, target) = churn_tmux_session("churn");

        // (a) Dies before speaking: reaped by the idle read timeout.
        drop(TcpStream::connect(addr).expect("connect"));
        // (b) Dies mid-request-line (no newline ever arrives).
        {
            let mut s = TcpStream::connect(addr).expect("connect");
            s.write_all(b"{\"token\":\"churn-secret\",\"comm").unwrap();
            drop(s);
        }
        // (c) Attaches to a MISSING session and dies without reading the refusal.
        {
            let mut s = TcpStream::connect(addr).expect("connect");
            send_attach_request(&mut s, "churn-secret", "s27-definitely-absent");
            drop(s);
        }
        // (d) Dies between the request and the seed (FIN lands mid-seed), x3.
        for _ in 0..3 {
            let mut s = TcpStream::connect(addr).expect("connect");
            send_attach_request(&mut s, "churn-secret", &id);
            drop(s);
        }
        // (e) Reads the seed, then dies with an abrupt RST (SO_LINGER 0), x3.
        for _ in 0..3 {
            let s = TcpStream::connect(addr).expect("connect");
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut w = s.try_clone().unwrap();
            send_attach_request(&mut w, "churn-secret", &id);
            let mut reader = BufReader::new(s);
            let seed = read_json_frame(&mut reader);
            assert!(seed.get("scrollback").is_some(), "expected a seed, got {seed}");
            socket2::SockRef::from(reader.get_ref())
                .set_linger(Some(Duration::from_secs(0)))
                .unwrap();
            // Dropping both clones now closes the socket -> RST, not FIN.
        }

        // (f) The incident wedge: a tiny-receive-buffer client attaches, starts a
        // firehose, stops reading, and HOLDS the socket open in silence. ~13 MB of
        // output against a 4 KiB client window and a <=4 MiB kernel send buffer
        // guarantees the forwarder's sink write blocks; the write timeout must
        // then tear the whole forwarder down while the client still holds its end.
        let wedge = {
            let sock =
                socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
            sock.set_recv_buffer_size(4096).unwrap();
            sock.connect(&addr.into()).expect("connect wedge client");
            TcpStream::from(sock)
        };
        wedge.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut wedge_writer = wedge.try_clone().unwrap();
        send_attach_request(&mut wedge_writer, "churn-secret", &id);
        let mut wedge_reader = BufReader::new(wedge);
        let seed = read_json_frame(&mut wedge_reader);
        assert!(seed.get("scrollback").is_some(), "expected a seed, got {seed}");
        send_write_frame(&mut wedge_writer, "yes S27-FIREHOSE | head -n 1000000\n");
        // Do NOT read, do NOT close. The server must reap the forwarder on its
        // own; every earlier case drains here too (EOF/RST paths are fast).
        eventually(
            "forwarder teardown while the wedged client still holds its socket",
            Duration::from_secs(20),
            || attach_forwarder_count() == 0,
        );

        // A FRESH attach must now succeed end to end - the exact operation that
        // failed for every client in the incident.
        let fresh = TcpStream::connect(addr).expect("connect fresh client");
        fresh.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut fresh_writer = fresh.try_clone().unwrap();
        send_attach_request(&mut fresh_writer, "churn-secret", &id);
        let mut fresh_reader = BufReader::new(fresh);
        let seed = read_json_frame(&mut fresh_reader);
        assert!(
            seed.get("scrollback").is_some(),
            "fresh attach after churn must get a seed, got {seed}"
        );
        send_write_frame(&mut fresh_writer, "echo S27_CHURN_OK\n");
        let mut seen = String::new();
        let sentinel_deadline = std::time::Instant::now() + Duration::from_secs(15);
        while !seen.contains("S27_CHURN_OK") {
            assert!(
                std::time::Instant::now() < sentinel_deadline,
                "sentinel never arrived on the fresh attach; saw: {seen:?}"
            );
            let mut line = String::new();
            let n = fresh_reader.read_line(&mut line).expect("read out frame");
            assert!(n > 0, "server closed the fresh attach early; saw: {seen:?}");
            let v: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(b64) = v.get("out").and_then(|x| x.as_str()) {
                if let Ok(bytes) = STANDARD.decode(b64) {
                    seen.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
        }

        // Teardown: with every client gone, BOTH tables return to baseline - no
        // leaked forwarder slot, no leaked connection slot.
        drop(fresh_reader);
        drop(fresh_writer);
        drop(wedge_reader);
        drop(wedge_writer);
        let _ = tmux::kill_session(&target);
        eventually("forwarder table back to baseline", Duration::from_secs(10), || {
            attach_forwarder_count() == 0
        });
        eventually("connection handlers to drain", Duration::from_secs(10), || {
            ACTIVE_CONNS.load(Ordering::Relaxed) <= conns_baseline
        });
    }

    /// The defensive forwarder-table bound: at the cap a new attach is refused
    /// with a clear error (not a silent close), and a released slot makes the
    /// attach path serviceable again.
    #[test]
    fn attach_forwarder_cap_refuses_then_recovers() {
        let _serial = attach_serial_guard();
        eventually("forwarder table to drain before the test", Duration::from_secs(10), || {
            attach_forwarder_count() == 0
        });

        let mut ctx = test_ctx("cap-secret");
        ctx.idle_timeout = Duration::from_millis(500);
        ctx.attach_write_timeout = Duration::from_secs(2);
        ctx.max_attach_forwarders = 1;
        let addr = spawn_attach_listener(ctx);

        let (id, target) = churn_tmux_session("cap");

        // First attach fills the size-1 table; reading the seed proves the slot
        // is held (the guard is acquired before the seed is written).
        let first = TcpStream::connect(addr).expect("connect");
        first.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut first_writer = first.try_clone().unwrap();
        send_attach_request(&mut first_writer, "cap-secret", &id);
        let mut first_reader = BufReader::new(first);
        assert!(read_json_frame(&mut first_reader).get("scrollback").is_some());
        assert_eq!(attach_forwarder_count(), 1);

        // Second attach: refused with an actionable error, then closed.
        let second = TcpStream::connect(addr).expect("connect");
        second.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut second_writer = second.try_clone().unwrap();
        send_attach_request(&mut second_writer, "cap-secret", &id);
        let mut second_reader = BufReader::new(second);
        let refusal = read_json_frame(&mut second_reader);
        assert_eq!(refusal["ok"], false, "expected a refusal, got {refusal}");
        assert!(
            refusal["error"]
                .as_str()
                .unwrap()
                .contains("forwarder table is full"),
            "got: {refusal}"
        );
        let mut rest = String::new();
        assert_eq!(
            second_reader.read_line(&mut rest).expect("read after refusal"),
            0,
            "the refused connection must be closed, not parked"
        );

        // Release the slot; the table must drain without any explicit detach call.
        drop(first_reader);
        drop(first_writer);
        eventually("slot release after client disconnect", Duration::from_secs(10), || {
            attach_forwarder_count() == 0
        });

        // And the attach path is serviceable again.
        let third = TcpStream::connect(addr).expect("connect");
        third.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut third_writer = third.try_clone().unwrap();
        send_attach_request(&mut third_writer, "cap-secret", &id);
        let mut third_reader = BufReader::new(third);
        assert!(
            read_json_frame(&mut third_reader).get("scrollback").is_some(),
            "attach must succeed once the table drained"
        );

        drop(third_reader);
        drop(third_writer);
        let _ = tmux::kill_session(&target);
        eventually("forwarder table drained at test end", Duration::from_secs(10), || {
            attach_forwarder_count() == 0
        });
    }

    /// THE s27 idle-leak regression: a client attached to an IDLE terminal that
    /// stops draining and then vanishes WITHOUT a clean close (no FIN reaches the
    /// server's input read) must still be reaped. The forwarder only ever noticed
    /// a dead client when it had real output to write; an idle terminal produces
    /// none, so the write path never fired and the forwarder parked forever on the
    /// silent PTY read - leaking the slot and, at scale, wedging the table so new
    /// cockpit tiles could not attach. The sibling churn test above never catches
    /// this because every one of its clients either closes (FIN/RST -> the input
    /// read unblocks) or drives a firehose (the sink write blocks -> write
    /// timeout); only a SILENT idle client exercises the gap. The periodic idle
    /// keepalive must now force the stalled client to surface (its socket buffers
    /// fill, the attach write timeout fires) so the forwarder reaps on its own.
    #[test]
    fn attach_reaps_idle_terminal_with_stalled_client() {
        let _serial = attach_serial_guard();
        eventually("forwarder table to drain before the test", Duration::from_secs(10), || {
            attach_forwarder_count() == 0
        });

        let mut ctx = test_ctx("idle-secret");
        ctx.idle_timeout = Duration::from_millis(500);
        ctx.attach_write_timeout = Duration::from_millis(300);
        // A short keepalive so the idle liveness probe fires within the test window
        // (production drives seconds). Without the probe an idle forwarder never
        // writes, so a stalled client is never noticed and the slot leaks forever.
        ctx.attach_keepalive_interval = Duration::from_millis(50);
        let addr = spawn_attach_listener(ctx);
        let conns_baseline = ACTIVE_CONNS.load(Ordering::Relaxed);

        let (id, target) = churn_tmux_session("idle");

        // A tiny-receive-buffer client attaches to an IDLE session, reads the seed,
        // then STOPS reading and holds the socket in silence - the idle analogue of
        // the firehose wedge (case f above), but with no output to force the issue.
        // Only the idle keepalive can fill the small buffer and trip the write
        // timeout; without it this forwarder never reaps.
        let stalled = {
            let sock =
                socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
            sock.set_recv_buffer_size(4096).unwrap();
            sock.connect(&addr.into()).expect("connect stalled client");
            TcpStream::from(sock)
        };
        stalled.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut stalled_writer = stalled.try_clone().unwrap();
        send_attach_request(&mut stalled_writer, "idle-secret", &id);
        let mut stalled_reader = BufReader::new(stalled);
        let seed = read_json_frame(&mut stalled_reader);
        assert!(seed.get("scrollback").is_some(), "expected a seed, got {seed}");
        assert_eq!(attach_forwarder_count(), 1, "forwarder up after attach");

        // Do NOT read, do NOT close: the client is gone but its socket lingers. The
        // server must reap this idle forwarder on its own, driven by the keepalive.
        eventually(
            "idle-terminal forwarder reaps a stalled client via the keepalive probe",
            Duration::from_secs(15),
            || attach_forwarder_count() == 0,
        );

        // Hold the client until AFTER the assertion so the reap is proven to be
        // driven by the server's probe, not by the socket finally closing.
        drop(stalled_reader);
        drop(stalled_writer);
        let _ = tmux::kill_session(&target);
        eventually("connection handlers to drain", Duration::from_secs(10), || {
            ACTIVE_CONNS.load(Ordering::Relaxed) <= conns_baseline
        });
    }

    // ---- Captains registry (captain-chat phase 2) -------------------------

    /// A unique temp path for a captains persistence file (removed by the caller).
    fn captains_tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "t-hub-captains-test-{tag}-{}.json",
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn claim_registers_updates_and_bumps_seq() {
        let reg = CaptainsRegistry::new();
        let rec = reg.claim("cap-1", Some("Ship Alpha!"), vec!["tab-1".into()]).unwrap();
        assert_eq!(rec.ship_slug, "ship-alpha");
        assert_eq!(rec.captain_session_id, "cap-1");
        assert_eq!(rec.workspace_tab_ids, vec!["tab-1".to_string()]);
        assert!(rec.crew.is_empty());
        let snap = reg.snapshot();
        assert_eq!(snap.seq, 1);
        assert_eq!(snap.captains.len(), 1);

        // Re-claim by the SAME captain is an upsert: slug/tabs refresh, crew kept.
        assert!(reg.record_crew("cap-1", "crew-1"));
        let rec = reg.claim("cap-1", Some("ship-beta"), vec!["tab-2".into()]).unwrap();
        assert_eq!(rec.ship_slug, "ship-beta");
        assert_eq!(rec.workspace_tab_ids, vec!["tab-2".to_string()]);
        assert_eq!(rec.crew, vec!["crew-1".to_string()]);
        let snap = reg.snapshot();
        assert_eq!(snap.captains.len(), 1, "upsert must not duplicate the claim");
        assert_eq!(snap.seq, 3);
    }

    #[test]
    fn claim_defaults_slug_and_refuses_a_taken_ship() {
        let reg = CaptainsRegistry::new();
        // No ship name (a UI pin): slug falls back to ship-<sessionId>.
        let rec = reg.claim("cap-1", None, vec![]).unwrap();
        assert_eq!(rec.ship_slug, "ship-cap-1");
        // One captain per ship: a DIFFERENT captain claiming the slug is refused.
        let err = reg.claim("cap-2", Some("ship-cap-1"), vec![]).unwrap_err();
        assert!(err.contains("already captained by session 'cap-1'"), "got: {err}");
        // Empty session id is refused before touching the registry.
        assert!(reg.claim("  ", None, vec![]).is_err());
        assert_eq!(reg.snapshot().seq, 1, "refusals must not bump the revision");
    }

    #[test]
    fn release_is_strict_and_addresses_by_id_or_slug() {
        let reg = CaptainsRegistry::new();
        reg.claim("cap-1", Some("alpha"), vec![]).unwrap();
        reg.claim("cap-2", Some("beta"), vec![]).unwrap();
        // By ship slug.
        assert_eq!(reg.release("alpha").unwrap().captain_session_id, "cap-1");
        // By captain session id.
        assert_eq!(reg.release("cap-2").unwrap().ship_slug, "beta");
        // Unknown target is an error, not a silent no-op.
        let err = reg.release("cap-2").unwrap_err();
        assert!(err.contains("no claim matches"), "got: {err}");
        assert!(reg.snapshot().captains.is_empty());
    }

    #[test]
    fn crew_lifecycle_record_dedupe_and_session_removal() {
        let reg = CaptainsRegistry::new();
        reg.claim("cap-1", Some("alpha"), vec![]).unwrap();
        // Recording under an UNclaimed captain is a no-op (spawn still proceeds).
        assert!(!reg.record_crew("cap-ghost", "crew-1"));
        assert!(reg.record_crew("cap-1", "crew-1"));
        assert!(!reg.record_crew("cap-1", "crew-1"), "duplicate crew must not re-add");
        assert!(reg.record_crew("cap-1", "crew-2"));
        assert_eq!(reg.snapshot().captains[0].crew.len(), 2);

        // A killed crew session leaves every crew list.
        assert!(reg.remove_session("crew-1"));
        assert_eq!(reg.snapshot().captains[0].crew, vec!["crew-2".to_string()]);
        // A killed CAPTAIN loses its claim.
        assert!(reg.remove_session("cap-1"));
        assert!(reg.snapshot().captains.is_empty());
        // Removing an unknown session changes nothing (no revision bump).
        let seq = reg.snapshot().seq;
        assert!(!reg.remove_session("nobody"));
        assert_eq!(reg.snapshot().seq, seq);
    }

    #[test]
    fn prune_tab_drops_the_tab_but_keeps_the_claim() {
        let reg = CaptainsRegistry::new();
        reg.claim("cap-1", Some("alpha"), vec!["tab-1".into(), "tab-2".into()]).unwrap();
        assert!(reg.prune_tab("tab-1"));
        let snap = reg.snapshot();
        assert_eq!(snap.captains[0].workspace_tab_ids, vec!["tab-2".to_string()]);
        assert!(!reg.prune_tab("tab-1"), "already-pruned tab must not bump the revision");
        assert!(reg.prune_tab("tab-2"));
        // Zero controlled tabs is a valid claim state.
        assert_eq!(reg.snapshot().captains.len(), 1);
    }

    #[test]
    fn registry_persists_across_reloads_including_seq() {
        let path = captains_tmp("roundtrip");
        {
            let reg = CaptainsRegistry::load(path.clone());
            reg.claim("cap-1", Some("alpha"), vec!["tab-1".into()]).unwrap();
            reg.record_crew("cap-1", "crew-1");
        }
        // A fresh load (an app restart) resumes the same claims AND revision.
        let reg = CaptainsRegistry::load(path.clone());
        let snap = reg.snapshot();
        assert_eq!(snap.seq, 2);
        assert_eq!(snap.captains.len(), 1);
        assert_eq!(snap.captains[0].ship_slug, "alpha");
        assert_eq!(snap.captains[0].crew, vec!["crew-1".to_string()]);
        // And keeps counting monotonically from there.
        reg.claim("cap-2", Some("beta"), vec![]).unwrap();
        assert_eq!(CaptainsRegistry::load(path.clone()).snapshot().seq, 3);

        // Atomic write discipline: the temp file is renamed over the target, so
        // no `.tmp` sibling is ever left behind after the writes above.
        let stem = path.file_name().unwrap().to_string_lossy().into_owned();
        let leftover_tmp = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with(&stem) && n.ends_with(".tmp")
            });
        assert!(!leftover_tmp, "atomic write must leave no .tmp file behind");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_or_missing_persistence_starts_empty() {
        let missing = CaptainsRegistry::load(captains_tmp("missing"));
        assert_eq!(missing.snapshot().seq, 0);
        assert!(missing.snapshot().captains.is_empty());

        let path = captains_tmp("corrupt");
        std::fs::write(&path, b"{not json").unwrap();
        let reg = CaptainsRegistry::load(path.clone());
        assert!(reg.snapshot().captains.is_empty());
        // The first mutation heals the file.
        reg.claim("cap-1", None, vec![]).unwrap();
        let healed = CaptainsRegistry::load(path.clone());
        assert_eq!(healed.snapshot().captains.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn list_captains_returns_the_versioned_snapshot() {
        let ctx = test_ctx("secret");
        ctx.captains.claim("cap-1", Some("alpha"), vec!["tab-1".into()]).unwrap();
        let v = dispatch(&ctx, "list_captains", &json!({})).unwrap();
        assert_eq!(v["count"], 1);
        assert_eq!(v["seq"], 1);
        assert_eq!(v["captains"][0]["shipSlug"], "alpha");
        assert_eq!(v["captains"][0]["captainSessionId"], "cap-1");
        assert_eq!(v["captains"][0]["workspaceTabIds"][0], "tab-1");
        assert_eq!(v["captains"][0]["crew"], json!([]));
    }

    #[test]
    fn scribe_status_dispatches_and_returns_a_listening_bool() {
        // The read-tier scribe voice-gate: dispatches to crate::scribe and
        // always returns an object with a boolean `listening` field, whatever
        // the on-disk file says (fail-open guarantees the shape). Asserting the
        // shape (not the value) keeps this deterministic whether or not a real
        // Scribe status file exists on the test machine.
        let ctx = test_ctx("secret");
        let v = dispatch(&ctx, "scribe_status", &Value::Null).unwrap();
        assert!(v.is_object());
        assert!(v["listening"].is_boolean());
    }

    #[test]
    fn claim_and_release_are_audited_and_forward_the_captains_snapshot() {
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());
        ctx.tab_registry().replace(vec![TabRecord {
            id: "tab-1".into(),
            name: "Main".into(),
            tile_ids: vec![],
        }]);
        // A LIVE terminal to claim (the liveness gate): spawn it into tab-1.
        let cap_id = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp", "tabId": "tab-1"}))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        // Claim with NO explicit workspaceTabIds: defaults to the tab holding
        // the captain's tile (the UI pin path sends exactly this shape).
        let v = dispatch(&ctx, "claim_captain", &json!({"captainSessionId": cap_id})).unwrap();
        assert_eq!(v["accepted"], "claim_captain");
        assert_eq!(v["audited"], true);
        assert_eq!(v["applied"], true);
        assert_eq!(v["captain"]["shipSlug"], format!("ship-{cap_id}"));
        assert_eq!(v["captain"]["workspaceTabIds"], json!(["tab-1"]));
        assert_eq!(v["captain"]["captainSessionId"], cap_id);

        let v = dispatch(&ctx, "release_captain", &json!({"captainSessionId": cap_id})).unwrap();
        assert_eq!(v["accepted"], "release_captain");
        assert_eq!(v["released"]["captainSessionId"], cap_id);
        assert_eq!(v["captains"], json!([]));

        // The claim + release each forwarded a sync_captains snapshot (filtering
        // out the spawn_terminal forward that seeded the live session).
        let sync_calls: Vec<_> = sink
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(c, _)| c == "sync_captains")
            .cloned()
            .collect();
        assert_eq!(sync_calls.len(), 2);
        assert_eq!(sync_calls[0].1["sync"]["captains"][0]["captainSessionId"], cap_id);
        assert_eq!(sync_calls[1].1["sync"]["captains"], json!([]));

        dispatch(&ctx, "close_terminal", &json!({"sessionId": cap_id})).unwrap();
    }

    #[test]
    fn claim_conflicts_liveness_and_bad_release_are_dispatch_errors() {
        let ctx = test_ctx("t").with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
        ctx.tab_registry().replace(vec![TabRecord {
            id: "tab-1".into(),
            name: "Main".into(),
            tile_ids: vec![],
        }]);
        let id1 = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp", "tabId": "tab-1"}))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let id2 = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp", "tabId": "tab-1"}))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        dispatch(&ctx, "claim_captain", &json!({"captainSessionId": id1, "shipSlug": "alpha"}))
            .unwrap();
        // A DIFFERENT live captain claiming the same ship is refused.
        let err = dispatch(
            &ctx,
            "claim_captain",
            &json!({"captainSessionId": id2, "shipSlug": "alpha"}),
        )
        .unwrap_err();
        assert!(err.contains("already captained"), "got: {err}");
        // A claim for a DEAD/unknown session is refused by the liveness gate
        // (else it would persist and linger forever).
        let err =
            dispatch(&ctx, "claim_captain", &json!({"captainSessionId": "nonexistent"})).unwrap_err();
        assert!(err.contains("no live terminal"), "got: {err}");
        let err = dispatch(&ctx, "release_captain", &json!({"shipSlug": "nope"})).unwrap_err();
        assert!(err.contains("no claim matches"), "got: {err}");
        assert!(dispatch(&ctx, "claim_captain", &json!({})).is_err());
        assert!(dispatch(&ctx, "release_captain", &json!({})).is_err());

        dispatch(&ctx, "close_terminal", &json!({"sessionId": id1})).unwrap();
        dispatch(&ctx, "close_terminal", &json!({"sessionId": id2})).unwrap();
    }

    #[test]
    fn idempotent_reclaim_does_not_bump_seq_or_forward() {
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());
        ctx.tab_registry().replace(vec![TabRecord {
            id: "tab-1".into(),
            name: "Main".into(),
            tile_ids: vec![],
        }]);
        let id = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp", "tabId": "tab-1"}))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        let v1 = dispatch(&ctx, "claim_captain", &json!({"captainSessionId": id})).unwrap();
        assert_eq!(v1["applied"], true);
        let seq1 = v1["seq"].as_u64().unwrap();
        // An identical re-claim changes nothing: seq stays put, no second forward.
        let v2 = dispatch(&ctx, "claim_captain", &json!({"captainSessionId": id})).unwrap();
        assert_eq!(v2["seq"].as_u64().unwrap(), seq1, "unchanged re-claim must not bump seq");
        assert_eq!(v2["applied"], false, "unchanged re-claim must not forward");
        let sync_count = sink
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(c, _)| c == "sync_captains")
            .count();
        assert_eq!(sync_count, 1, "only the first (changing) claim forwards");

        dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    }

    #[test]
    fn spawn_with_spawned_by_records_crew_and_close_terminal_removes_it() {
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());
        ctx.tab_registry().replace(vec![TabRecord {
            id: "tab-1".into(),
            name: "Main".into(),
            tile_ids: vec![],
        }]);
        ctx.captains.claim("cap-1", Some("alpha"), vec![]).unwrap();

        // A claimed captain spawns crew: the link is recorded + synced.
        let v = dispatch(
            &ctx,
            "spawn_terminal",
            &json!({"cwd": "/tmp", "spawnedBy": "cap-1"}),
        )
        .unwrap();
        assert_eq!(v["crewRecorded"], true);
        assert_eq!(v["spawnedBy"], "cap-1");
        let crew_id = v["id"].as_str().unwrap().to_string();
        let snap = ctx.captains.snapshot();
        assert_eq!(snap.captains[0].crew, vec![crew_id.clone()]);

        // The dead crew session leaves the registry (and forwards a sync).
        dispatch(&ctx, "close_terminal", &json!({"sessionId": crew_id.clone()})).unwrap();
        assert!(ctx.captains.snapshot().captains[0].crew.is_empty());

        // Forwards: sync_captains (crew add), spawn_terminal (with spawnedBy),
        // sync_tabs (tile drop), sync_captains (crew removal).
        let calls = sink.calls.lock().unwrap();
        let names: Vec<&str> = calls.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(
            names,
            ["sync_captains", "spawn_terminal", "sync_tabs", "sync_captains"]
        );
        assert_eq!(calls[0].1["sync"]["captains"][0]["crew"], json!([crew_id]));
        assert_eq!(calls[1].1["spawnedBy"], "cap-1");
        assert_eq!(calls[3].1["sync"]["captains"][0]["crew"], json!([]));
    }

    #[test]
    fn spawn_with_an_unclaimed_spawned_by_still_spawns_without_a_crew_link() {
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());
        let v = dispatch(
            &ctx,
            "spawn_terminal",
            &json!({"cwd": "/tmp", "spawnedBy": "cap-ghost"}),
        )
        .unwrap();
        assert_eq!(v["accepted"], "spawn_terminal");
        assert_eq!(v["crewRecorded"], false, "no claim = no crew link, spawn unaffected");
        assert!(ctx.captains.snapshot().captains.is_empty());
        let id = v["id"].as_str().unwrap().to_string();
        dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
        let calls = sink.calls.lock().unwrap();
        assert!(
            calls.iter().all(|(c, _)| c != "sync_captains"),
            "nothing captain-shaped changed, so no captains sync may be forwarded"
        );
    }

    #[test]
    fn close_terminal_of_a_captain_releases_its_claim() {
        let ctx = test_ctx("t");
        ctx.captains.claim("cap-1", Some("alpha"), vec![]).unwrap();
        // The captain's own session dies (already-gone tmux session: the kill
        // is idempotent, so dispatch succeeds and the registry cleanup runs).
        dispatch(&ctx, "close_terminal", &json!({"sessionId": "cap-1"})).unwrap();
        assert!(ctx.captains.snapshot().captains.is_empty());
    }

    #[test]
    fn report_workspace_tabs_prunes_closed_tabs_from_captains() {
        // The PRIMARY UI tab-close path is report_workspace_tabs (the webview
        // reports its new tab set), NOT the socket close_tab. A tab dropped from
        // the report must leave every captain's workspaceTabIds and forward a
        // captains snapshot - else it lingers as a phantom controlled-workspace.
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());
        ctx.tab_registry().replace(vec![
            TabRecord { id: "t1".into(), name: "Main".into(), tile_ids: vec![] },
            TabRecord { id: "t2".into(), name: "Side".into(), tile_ids: vec![] },
        ]);
        ctx.captains
            .claim("cap-1", Some("alpha"), vec!["t1".into(), "t2".into()])
            .unwrap();

        // Report a tab set WITHOUT t2 (the user closed it): t2 is pruned from the
        // captain, and a sync_captains forward carries the pruned snapshot.
        dispatch(
            &ctx,
            "report_workspace_tabs",
            &json!({"tabs": [{"id": "t1", "name": "Main", "tileIds": []}]}),
        )
        .unwrap();
        assert_eq!(
            ctx.captains.snapshot().captains[0].workspace_tab_ids,
            vec!["t1".to_string()],
        );
        let calls = sink.calls.lock().unwrap();
        assert!(
            calls.iter().any(|(c, a)| c == "sync_captains"
                && a["sync"]["captains"][0]["workspaceTabIds"] == json!(["t1"])),
            "a sync_captains forward must carry the pruned workspaceTabIds"
        );
    }

    #[test]
    fn close_tab_prunes_captain_workspace_ownership() {
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());
        ctx.tab_registry().replace(vec![
            TabRecord { id: "tab-1".into(), name: "Main".into(), tile_ids: vec![] },
            TabRecord { id: "tab-2".into(), name: "Side".into(), tile_ids: vec![] },
        ]);
        ctx.captains
            .claim("cap-1", Some("alpha"), vec!["tab-2".into()])
            .unwrap();

        dispatch(&ctx, "close_tab", &json!({"tabId": "tab-2"})).unwrap();
        let snap = ctx.captains.snapshot();
        assert_eq!(snap.captains[0].workspace_tab_ids, Vec::<String>::new());
        // The prune rode a sync_captains forward ahead of the close_tab apply.
        let calls = sink.calls.lock().unwrap();
        let names: Vec<&str> = calls.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(names, ["sync_captains", "close_tab"]);
    }

    // -----------------------------------------------------------------------
    // socket-gate Phase 1: fleet governor + audit wiring at dispatch_authenticated
    // -----------------------------------------------------------------------

    /// Read every audit record written under `dir` (order within a single day file
    /// is append order). Empty when nothing was audited.
    fn read_audit(dir: &std::path::Path) -> Vec<Value> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                if let Ok(txt) = std::fs::read_to_string(entry.path()) {
                    for line in txt.lines() {
                        if !line.trim().is_empty() {
                            out.push(serde_json::from_str(line).unwrap());
                        }
                    }
                }
            }
        }
        out
    }

    fn req(token: &str, command: &str, args: Value) -> ControlRequest {
        ControlRequest {
            token: token.to_string(),
            command: command.to_string(),
            args,
            v: None,
        }
    }

    #[test]
    fn normal_captain_fanout_burst_not_refused_at_gate() {
        // THE most important test (design spec): a captain fanning out 6 crew in an
        // instant burst must NOT be refused by the fleet gate. With the default
        // burst of 8 the governor admits all six; they fail downstream only because
        // this headless ctx has no UI sink, never because of the budget.
        let dir = std::env::temp_dir().join("t-hub-gate-burst");
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = test_ctx("burst")
            .with_governor(Arc::new(SpawnGovernor::default()))
            .with_audit(Arc::new(AuditLog::new(dir.clone())));
        for i in 0..6 {
            let resp = dispatch_authenticated(
                &ctx,
                req("burst", "spawn_terminal", json!({"cwd": "/tmp", "name": format!("crew-{i}")})),
            );
            let err = resp.error.clone().unwrap_or_default();
            assert!(!err.contains("rate limit"), "spawn {i} was rate-limited: {err}");
            assert!(
                !err.contains("concurrent-session cap"),
                "spawn {i} hit the concurrent cap: {err}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spawn_rate_limit_refuses_with_exact_message_and_audits() {
        // Burst 1: the first spawn spends the only token; the second is refused with
        // the exact §5 message and recorded as `refused-rate`.
        let dir = std::env::temp_dir().join("t-hub-gate-rate");
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = test_ctx("rate")
            .with_governor(Arc::new(SpawnGovernor::new(64, 20.0, 1.0)))
            .with_audit(Arc::new(AuditLog::new(dir.clone())));
        let r1 = dispatch_authenticated(&ctx, req("rate", "spawn_terminal", json!({"cwd": "/tmp"})));
        // Governor admitted r1; it fails downstream on the missing UI sink.
        assert!(r1.error.clone().unwrap_or_default().contains("no UI"), "got: {:?}", r1.error);
        let r2 = dispatch_authenticated(&ctx, req("rate", "spawn_terminal", json!({"cwd": "/tmp"})));
        assert!(
            r2.error.clone().unwrap().contains("spawn rate limit (20/min); retry shortly"),
            "got: {:?}",
            r2.error
        );

        let recs = read_audit(&dir);
        assert_eq!(recs.len(), 2, "expected an allowed + a refused record");
        assert_eq!(recs[0]["decision"], "allowed");
        assert_eq!(recs[0]["command"], "spawn_terminal");
        assert_eq!(recs[1]["decision"], "refused-rate");
        // The hash chain links the refusal to the prior line.
        assert_eq!(recs[1]["prev"], recs[0]["hash"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_tier_is_not_gated_or_audited() {
        // list_terminals is Read tier: it must never touch the governor or the audit
        // log, whether or not tmux is reachable in the test env.
        let dir = std::env::temp_dir().join("t-hub-gate-read");
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = test_ctx("read").with_audit(Arc::new(AuditLog::new(dir.clone())));
        let _ = dispatch_authenticated(&ctx, req("read", "list_terminals", json!({})));
        assert!(read_audit(&dir).is_empty(), "a read-tier command was audited");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_text_is_audited_with_redaction_through_gate() {
        // send_text is process-changing (audited) but NOT rate-limited. The literal
        // text must never reach the audit log - only a length + hash.
        let dir = std::env::temp_dir().join("t-hub-gate-sendtext");
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = test_ctx("st").with_audit(Arc::new(AuditLog::new(dir.clone())));
        let resp = dispatch_authenticated(
            &ctx,
            req("st", "send_text", json!({"sessionId": "ghost", "text": "SUPERSECRET", "enter": true})),
        );
        assert!(!resp.ok); // no such session, but the audit still lands
        let recs = read_audit(&dir);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0]["command"], "send_text");
        assert_eq!(recs[0]["decision"], "allowed");
        let blob = serde_json::to_string(&recs[0]).unwrap();
        assert!(!blob.contains("SUPERSECRET"), "literal text leaked into audit: {blob}");
        assert_eq!(recs[0]["args"]["textLen"], 11);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_token_is_rejected_and_not_audited() {
        // A bad token is rejected before the gate and never audited (no leak of the
        // process-changing surface to an unauthenticated probe).
        let dir = std::env::temp_dir().join("t-hub-gate-badtok");
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = test_ctx("good").with_audit(Arc::new(AuditLog::new(dir.clone())));
        let resp = dispatch_authenticated(&ctx, req("WRONG", "spawn_terminal", json!({})));
        assert!(resp.error.unwrap().contains("bad control token"));
        assert!(read_audit(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kill_style_send_keys_is_throttled_but_navigation_is_not() {
        // The destructive throttle covers kill-style keys (C-c) but not navigation
        // (Up/Enter) - proven at the classifier the gate uses.
        assert!(keys_are_kill_style(&json!({"keys": ["C-c"]})));
        assert!(keys_are_kill_style(&json!({"keys": ["Up", "C-d"]})));
        assert!(!keys_are_kill_style(&json!({"keys": ["Up", "Enter"]})));
        assert!(!keys_are_kill_style(&json!({"keys": []})));
    }

    #[test]
    fn command_tiers_are_classified() {
        assert_eq!(required_tier("spawn_terminal"), CommandTier::ProcessChanging);
        assert_eq!(required_tier("close_terminal"), CommandTier::ProcessChanging);
        assert_eq!(required_tier("send_text"), CommandTier::ProcessChanging);
        assert_eq!(required_tier("new_tab"), CommandTier::Organization);
        assert_eq!(required_tier("create_worktree"), CommandTier::Organization);
        assert_eq!(required_tier("remove_worktree"), CommandTier::Organization);
        assert_eq!(required_tier("list_terminals"), CommandTier::Read);
        assert_eq!(required_tier("get_status"), CommandTier::Read);
    }

    #[test]
    fn legit_spawn_send_close_through_gate_is_admitted_and_audited() {
        // End-to-end through dispatch_authenticated (governor + audit) against a
        // REAL tmux session: a legitimate crew spawn -> send_text -> close must all
        // be ADMITTED and audited allowed. This is the "legit orchestration still
        // works over the exact socket" guarantee, exercised through the gate.
        let dir = std::env::temp_dir().join("t-hub-gate-e2e");
        let _ = std::fs::remove_dir_all(&dir);
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("e2e")
            .with_apply_sink(sink.clone())
            .with_audit(Arc::new(AuditLog::new(dir.clone())));
        ctx.tab_registry().replace(vec![TabRecord {
            id: "tab-1".into(),
            name: "Main".into(),
            tile_ids: vec![],
        }]);

        // Spawn a real session through the authenticated gate.
        let sresp = dispatch_authenticated(
            &ctx,
            req("e2e", "spawn_terminal", json!({"cwd": "/tmp", "name": "crew", "tabId": "tab-1"})),
        );
        assert!(sresp.ok, "legit spawn was refused by the gate: {:?}", sresp.error);
        let id = sresp.result.as_ref().unwrap()["id"].as_str().unwrap().to_string();
        let target = tmux::target_for_id(&id);
        assert!(tmux::has_session(&target), "the real tmux session should exist");
        let _ = tmux::resize_window_for_tests(&target, 80, 24);

        // Type into it through the gate (send_text is not throttled).
        let tresp = dispatch_authenticated(
            &ctx,
            req("e2e", "send_text", json!({"sessionId": id, "text": "echo GATE_E2E_OK", "enter": true})),
        );
        assert!(tresp.ok, "legit send_text was refused: {:?}", tresp.error);

        // Close it through the gate (destructive, but the first teardown is under
        // the burst of 10 so it is admitted).
        let cresp = dispatch_authenticated(&ctx, req("e2e", "close_terminal", json!({"sessionId": id})));
        assert!(cresp.ok, "legit close_terminal was refused: {:?}", cresp.error);
        assert!(!tmux::has_session(&target), "session should be gone after close");

        // All three land in the audit log, allowed and hash-chained. send_text's
        // literal payload is NOT present (redacted).
        let recs = read_audit(&dir);
        assert_eq!(recs.len(), 3, "expected spawn+send+close audited: {recs:?}");
        let cmds: Vec<&str> = recs.iter().map(|r| r["command"].as_str().unwrap()).collect();
        assert_eq!(cmds, ["spawn_terminal", "send_text", "close_terminal"]);
        assert!(recs.iter().all(|r| r["decision"] == "allowed"), "a legit command was not allowed: {recs:?}");
        for w in recs.windows(2) {
            assert_eq!(w[1]["prev"], w[0]["hash"], "hash chain broken");
        }
        let blob = serde_json::to_string(&recs).unwrap();
        assert!(!blob.contains("GATE_E2E_OK"), "send_text literal leaked into audit");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // socket-gate Phase 2/2b: capability-scoped tokens
    // -----------------------------------------------------------------------

    #[test]
    fn capability_resolution_maps_each_token() {
        // control token -> Full; read token -> ReadOnly; anything else -> None.
        let ctx = test_ctx("t"); // control="t", read="read-t"
        assert_eq!(resolve_capability(&ctx, "t"), Some(Capability::Full));
        assert_eq!(resolve_capability(&ctx, "read-t"), Some(Capability::ReadOnly));
        assert_eq!(resolve_capability(&ctx, "nope"), None);
        assert_eq!(resolve_capability(&ctx, ""), None);
    }

    #[test]
    fn empty_read_token_authorizes_nothing() {
        // A ctx with no read token configured must not let an empty presented token
        // resolve to ReadOnly (the empty==empty trap).
        let ctx = ControlContext::new(
            Arc::new(StatusBridge::new()),
            Arc::new(|_: &mut dyn FnMut(&Supervisor)| {}),
            "ctl".to_string(),
        );
        assert!(ctx.read_token.is_empty());
        assert_eq!(resolve_capability(&ctx, ""), None);
        assert_eq!(resolve_capability(&ctx, "ctl"), Some(Capability::Full));
    }

    #[test]
    fn control_token_still_grants_full_power_backward_compat() {
        // THE make-or-break: the existing control token (published in control.json)
        // resolves to Full and is authorized for EVERY tier - zero client breakage.
        let ctx = test_ctx("t");
        assert!(Capability::Full.allows(CommandTier::Read));
        assert!(Capability::Full.allows(CommandTier::Organization));
        assert!(Capability::Full.allows(CommandTier::ProcessChanging));
        // Through the gate: a ProcessChanging command with the control token is NOT
        // authz-refused (it fails downstream only because this headless ctx has no
        // UI sink - proving authz passed).
        let resp = dispatch_authenticated(&ctx, req("t", "spawn_terminal", json!({"cwd": "/tmp"})));
        let err = resp.error.unwrap_or_default();
        assert!(!err.contains("requires the control capability"), "control token was authz-refused: {err}");
        assert!(err.contains("no UI"), "expected the downstream no-UI failure, got: {err}");
    }

    #[test]
    fn read_token_reads_but_cannot_spawn_or_kill() {
        let dir = std::env::temp_dir().join("t-hub-p2-readonly");
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = test_ctx("t").with_audit(Arc::new(AuditLog::new(dir.clone())));

        // Read tier: allowed (not authz-refused). May fail on tmux, but never authz.
        let r = dispatch_authenticated(&ctx, req("read-t", "list_terminals", json!({})));
        assert!(
            !r.error.clone().unwrap_or_default().contains("requires the control capability"),
            "read token was refused a Read command"
        );

        // ProcessChanging + Organization-destructive: authz-refused with the exact msg.
        for cmd in ["spawn_terminal", "send_text", "send_keys", "close_terminal", "create_worktree"] {
            let resp = dispatch_authenticated(&ctx, req("read-t", cmd, json!({"cwd": "/tmp", "sessionId": "x", "text": "y", "keys": ["C-c"]})));
            let err = resp.error.unwrap_or_default();
            assert!(
                err == format!("unauthorized: '{cmd}' requires the control capability (this token is read-only)"),
                "read token should be authz-refused for {cmd}, got: {err}"
            );
        }

        // Every refusal is audited with tokenTier=read and decision=refused-authz.
        let recs = read_audit(&dir);
        assert!(!recs.is_empty());
        assert!(recs.iter().all(|r| r["decision"] == "refused-authz"));
        assert!(recs.iter().all(|r| r["tokenTier"] == "read"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn control_token_command_audits_token_tier_control() {
        let dir = std::env::temp_dir().join("t-hub-p2-ctltier");
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = test_ctx("t").with_audit(Arc::new(AuditLog::new(dir.clone())));
        // An Organization command with the control token: allowed, audited control.
        let _ = dispatch_authenticated(&ctx, req("t", "new_tab", json!({"name": "T"})));
        let recs = read_audit(&dir);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0]["tokenTier"], "control");
        assert_eq!(recs[0]["decision"], "allowed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_peer_is_capped_to_read_even_with_control_token() {
        // Belt-and-suspenders (open Q4): a non-loopback peer presenting the CONTROL
        // token is capped to ReadOnly, so it cannot spawn/kill over the network bind.
        let mut ctx = test_ctx("t");
        ctx.peer_is_loopback = false;
        assert_eq!(resolve_capability(&ctx, "t"), Some(Capability::ReadOnly));
        // Read still works remotely; ProcessChanging is authz-refused.
        let spawn = dispatch_authenticated(&ctx, req("t", "spawn_terminal", json!({"cwd": "/tmp"})));
        assert!(spawn.error.unwrap().contains("requires the control capability"));
        let read = dispatch_authenticated(&ctx, req("t", "list_terminals", json!({})));
        assert!(!read.error.clone().unwrap_or_default().contains("requires the control capability"));
    }

    #[test]
    fn read_token_is_valid_for_subscribe() {
        // token_is_valid (the event-subscribe gate) accepts either capability so a
        // least-privilege monitor can subscribe; a bad token is rejected.
        let ctx = test_ctx("t");
        assert!(token_is_valid(&ctx, "t"));
        assert!(token_is_valid(&ctx, "read-t"));
        assert!(!token_is_valid(&ctx, "bad"));
    }

    #[test]
    fn phase3_flag_is_off_by_default_and_selects_control_token() {
        // Phase 3 hardening is OFF by default (unset ⇒ disabled), so `control.json`
        // keeps publishing the full control token - the app's own frontend depends on
        // that until it gets the control token via a trusted internal channel (see
        // the 2026-07-07 incident note on `phase3_harden_enabled`). Opt in with `1`/
        // `true`. This mutates a process-global env var; no other test reads this var,
        // and it is saved/restored around the mutation to stay hermetic.
        let saved = std::env::var("T_HUB_CONTROL_HARDEN").ok();
        std::env::remove_var("T_HUB_CONTROL_HARDEN");
        assert!(!phase3_harden_enabled(), "harden flag must default OFF");
        std::env::set_var("T_HUB_CONTROL_HARDEN", "1");
        assert!(phase3_harden_enabled(), "'1' must enable hardening");
        std::env::set_var("T_HUB_CONTROL_HARDEN", "true");
        assert!(phase3_harden_enabled(), "'true' must enable hardening");
        std::env::set_var("T_HUB_CONTROL_HARDEN", "0");
        assert!(!phase3_harden_enabled(), "'0' stays off");
        std::env::set_var("T_HUB_CONTROL_HARDEN", "yes");
        assert!(!phase3_harden_enabled(), "only 1/true enable; other values stay off");
        match saved {
            Some(v) => std::env::set_var("T_HUB_CONTROL_HARDEN", v),
            None => std::env::remove_var("T_HUB_CONTROL_HARDEN"),
        }

        // The pure selector: ON ⇒ read token, OFF ⇒ control token.
        assert_eq!(select_published_token("ctl", "rd", true), "rd");
        assert_eq!(select_published_token("ctl", "rd", false), "ctl");
        // Never an empty read token (falls back to control so a context that never
        // minted a read token is not locked out).
        assert_eq!(select_published_token("ctl", "", true), "ctl");
    }

    #[test]
    fn hardened_control_json_withholds_full_token_but_handshake_carries_it() {
        // The security-critical Phase-3-safety invariant. Build the handshake exactly
        // as `start` does with hardening ON, write it, and assert BOTH halves of the
        // contract:
        //   (a) the SERIALIZED control.json `token` == read_token (full token withheld
        //       from external scrapers), and the full token appears nowhere in the file;
        //   (b) the RETURNED handshake's `local_control_token` == the full control token,
        //       so the trusted in-process frontend still gets full power.
        let full = "FULL-SECRET-abc123";
        let read = "READ-only-xyz789";
        let handshake = ControlHandshake {
            addr: "127.0.0.1:5000".into(),
            // Mirrors `start`: published token is the read token under hardening.
            token: select_published_token(full, read, true).to_string(),
            read_token: read.into(),
            pid: 7,
            protocol_version: PROTOCOL_VERSION,
            local_control_token: full.into(),
        };

        // (a) Published discovery is read-only and never leaks the full token.
        assert_eq!(handshake.token, read, "published token must be the read token");
        let file = std::env::temp_dir()
            .join(format!("t-hub-ctl-harden-{}.json", std::process::id()));
        let prev = std::env::var("T_HUB_CONTROL_FILE").ok();
        std::env::set_var("T_HUB_CONTROL_FILE", &file);
        write_handshake(&handshake).expect("write handshake");
        let on_disk = std::fs::read_to_string(&file).expect("read control.json");
        match prev {
            Some(v) => std::env::set_var("T_HUB_CONTROL_FILE", v),
            None => std::env::remove_var("T_HUB_CONTROL_FILE"),
        }
        let _ = std::fs::remove_file(&file);

        assert!(
            !on_disk.contains(full),
            "control.json must NOT contain the full control token; got: {on_disk}"
        );
        assert!(
            !on_disk.contains("local_control_token"),
            "the in-process field must not be serialized; got: {on_disk}"
        );
        let parsed: ControlHandshake =
            serde_json::from_str(&on_disk).expect("control.json parses");
        assert_eq!(parsed.token, read, "on-disk token must be the read token");
        assert_eq!(
            parsed.local_control_token, "",
            "in-process token must not survive to disk"
        );

        // (b) The RETURNED handshake still carries the full token for the frontend.
        assert_eq!(
            handshake.local_control_token, full,
            "local frontend must receive the full control token in-process"
        );
    }

    #[test]
    fn phase3_hardened_publishes_read_token_yet_spawn_env_carries_control() {
        // With hardening ON (opt-in via `T_HUB_CONTROL_HARDEN=1`): what `control.json`
        // publishes as `token` is the READ token (so a raw scraper is read-only), while
        // the spawn-tree env injection still hands a spawned session the CONTROL token,
        // preserving orchestration. These two facts together are the whole Phase 3
        // contract - kept green so the flag is ready to re-enable once the frontend
        // stops depending on the published control token.
        let ctx = test_ctx("ctl"); // read token is "read-ctl" (see test_ctx)
        // Discovery, hardened: publishes the read token, NOT the control token.
        let published = select_published_token(&ctx.token, &ctx.read_token, true);
        assert_eq!(published, ctx.read_token, "hardened discovery must publish read token");
        assert_ne!(published, ctx.token, "hardened discovery must NOT publish control token");
        assert_eq!(
            resolve_capability(&ctx, published),
            Some(Capability::ReadOnly),
            "published token must resolve to read-only"
        );

        // Spawn-tree env injection: still carries the CONTROL token so the spawned
        // session's in-session MCP authenticates with full power.
        let mut ctx = ctx;
        ctx.addr = "127.0.0.1:4242".to_string();
        let env = elevation_env(&ctx, &json!({}));
        let injected = env
            .iter()
            .find(|(k, _)| k == "T_HUB_CONTROL_TOKEN")
            .map(|(_, v)| v.clone())
            .expect("spawn env injects T_HUB_CONTROL_TOKEN");
        assert_eq!(injected, ctx.token, "spawn env must inject the control token");
        assert_eq!(
            resolve_capability(&ctx, &injected),
            Some(Capability::Full),
            "injected token must grant full control"
        );
    }

    #[test]
    fn elevation_env_defaults_control_and_downgrades_to_read() {
        let mut ctx = test_ctx("t");
        ctx.addr = "127.0.0.1:4242".to_string();
        // Default: full control token injected (preserve current behavior).
        let def = elevation_env(&ctx, &json!({}));
        assert_eq!(def, vec![
            ("T_HUB_CONTROL_ADDR".to_string(), "127.0.0.1:4242".to_string()),
            ("T_HUB_CONTROL_TOKEN".to_string(), "t".to_string()),
        ]);
        // Opt-in read-only: the read token is injected instead.
        let ro = elevation_env(&ctx, &json!({"capability": "read"}));
        assert_eq!(ro[1], ("T_HUB_CONTROL_TOKEN".to_string(), "read-t".to_string()));
        // No bound addr (headless): nothing injected, so spawns behave as before.
        ctx.addr = String::new();
        assert!(elevation_env(&ctx, &json!({"capability": "read"})).is_empty());
    }

    // -----------------------------------------------------------------------
    // Idempotency: RequestCache (ask #1)
    // -----------------------------------------------------------------------

    #[test]
    fn request_cache_replays_a_completed_outcome() {
        let cache = RequestCache::new();
        // First sighting reserves the id and must run the command.
        assert!(matches!(cache.begin("r1"), BeginOutcome::Fresh));
        let stored = cache.finish("r1", Ok(json!({"id": "abc"})));
        assert_eq!(stored.unwrap()["id"], "abc");
        // A retry of the SAME id replays the stored outcome - it does NOT re-run.
        match cache.begin("r1") {
            BeginOutcome::Duplicate(Ok(v)) => assert_eq!(v["id"], "abc"),
            BeginOutcome::Duplicate(Err(e)) => panic!("expected Ok replay, got Err: {e}"),
            BeginOutcome::Fresh => panic!("a completed id must not be reserved Fresh again"),
            BeginOutcome::FreshAfterReap => {
                panic!("a completed id must replay, not reap-and-re-reserve")
            }
            BeginOutcome::InFlight => panic!("a completed id must replay, not report InFlight"),
        }
    }

    #[test]
    fn request_cache_reports_in_flight_for_a_concurrent_duplicate() {
        let cache = RequestCache::new();
        // A first caller reserved the id and is still running (no finish yet).
        assert!(matches!(cache.begin("r2"), BeginOutcome::Fresh));
        // A retry that races the original must NOT run the command again.
        assert!(matches!(cache.begin("r2"), BeginOutcome::InFlight));
        // Once it completes, the same id replays the outcome.
        let _ = cache.finish("r2", Ok(json!({"ok": true})));
        assert!(matches!(cache.begin("r2"), BeginOutcome::Duplicate(_)));
    }

    #[test]
    fn request_cache_cancel_frees_a_reservation_for_retry() {
        let cache = RequestCache::new();
        assert!(matches!(cache.begin("r3"), BeginOutcome::Fresh));
        // A governor refusal cancels the reservation (no outcome recorded)...
        cache.cancel("r3");
        // ...so a later retry is Fresh again (it can succeed once budget frees),
        // not stuck InFlight or replaying a refusal.
        assert!(matches!(cache.begin("r3"), BeginOutcome::Fresh));
    }

    #[test]
    fn request_cache_status_reports_unknown_inflight_and_completed() {
        let cache = RequestCache::new();
        assert!(matches!(cache.status("nope"), RequestStatus::Unknown));
        cache.begin("r4");
        assert!(matches!(cache.status("r4"), RequestStatus::InFlight));
        let _ = cache.finish("r4", Err("boom".to_string()));
        match cache.status("r4") {
            RequestStatus::Completed(Err(e)) => assert_eq!(e, "boom"),
            _ => panic!("expected Completed(Err)"),
        }
    }

    #[test]
    fn request_cache_evicts_oldest_completed_beyond_capacity() {
        let cache = RequestCache::with_bounds(
            2,
            std::time::Duration::from_secs(600),
            std::time::Duration::from_secs(600),
        );
        for id in ["a", "b", "c"] {
            cache.begin(id);
            let _ = cache.finish(id, Ok(json!({"id": id})));
        }
        // "a" was evicted when "c" pushed past the capacity of 2.
        assert!(matches!(cache.status("a"), RequestStatus::Unknown));
        assert!(matches!(cache.status("b"), RequestStatus::Completed(_)));
        assert!(matches!(cache.status("c"), RequestStatus::Completed(_)));
    }

    #[test]
    fn request_cache_evicts_a_done_entry_past_its_ttl() {
        // A completed outcome ages out of the cache after its TTL, keeping the cache
        // self-cleaning. (The same retain reaps a stale InFlight reservation past
        // REQUEST_INFLIGHT_REAP - the safety valve for a panicked/hung handler.)
        let cache = RequestCache::with_bounds(
            8,
            std::time::Duration::from_millis(1),
            std::time::Duration::from_secs(600),
        );
        cache.begin("done");
        let _ = cache.finish("done", Ok(json!({})));
        std::thread::sleep(std::time::Duration::from_millis(5));
        // status() runs eviction; the expired Done entry is gone -> Unknown, so a
        // fresh retry would be safe.
        assert!(matches!(cache.status("done"), RequestStatus::Unknown));
    }

    #[test]
    fn request_cache_reaps_a_stale_in_flight_reservation() {
        // The InFlight reap safety valve: a reservation that never finished (a
        // panicked/hung handler) is presumed dead after `inflight_reap` so a retry
        // is not blocked forever. Tiny reap window stands in for the 600s default.
        let cache = RequestCache::with_bounds(
            8,
            std::time::Duration::from_secs(600),
            std::time::Duration::from_millis(1),
        );
        cache.begin("stuck"); // reserved InFlight, never finished
        std::thread::sleep(std::time::Duration::from_millis(5));
        // A retry now sees FreshAfterReap (the dead reservation was reaped + re-
        // reserved), not a permanent InFlight. The `AfterReap` flavor tells dispatch
        // to RE-PROBE reality before re-applying (M1 full fix) - a genuinely-new id
        // would be plain Fresh.
        assert!(matches!(
            cache.begin("stuck"),
            BeginOutcome::FreshAfterReap
        ));
    }

    #[test]
    fn request_cache_reaped_id_yields_exactly_one_fresh_after_reap() {
        // F4 (one-reprobe-per-reap): after a reservation is reaped, TWO retries of
        // the same id must NOT both re-probe/re-apply. `begin` is atomic — the FIRST
        // retry consumes the reap (FreshAfterReap) AND re-reserves the id InFlight in
        // the same locked step, so the SECOND retry sees a live InFlight reservation,
        // not a second FreshAfterReap. That is what caps the M1 re-probe (and its
        // unbounded git worktree-list) at ONCE per reap: the loser is told InFlight
        // and polls/retries instead of issuing a duplicate reality probe + re-apply.
        //
        // A comfortably large reap window (relative to two back-to-back synchronous
        // `begin` calls) keeps this deterministic: the original ages PAST it, but the
        // freshly re-reserved slot is far YOUNGER than it when the second retry runs.
        let reap = std::time::Duration::from_millis(50);
        let cache = RequestCache::with_bounds(8, std::time::Duration::from_secs(600), reap);

        cache.begin("wt"); // original reservation, never finished (handler presumed dead)
        std::thread::sleep(reap * 2); // age it past the reap window

        // First retry: the dead reservation is reaped and re-reserved in one step.
        assert!(
            matches!(cache.begin("wt"), BeginOutcome::FreshAfterReap),
            "the first retry after a reap must re-probe reality (FreshAfterReap)"
        );
        // Second retry, immediately after: the just-re-reserved slot is still well
        // within the reap window, so this loser sees InFlight — NOT a second reprobe.
        assert!(
            matches!(cache.begin("wt"), BeginOutcome::InFlight),
            "a concurrent second retry must see InFlight, not a duplicate FreshAfterReap"
        );
        // And a third: still InFlight until the winner calls finish(). At no point
        // does a single reap yield two re-applies.
        assert!(matches!(cache.begin("wt"), BeginOutcome::InFlight));

        // Once the winner records the outcome, further retries replay it (Duplicate),
        // still never a second apply.
        let _ = cache.finish("wt", Ok(json!({"alreadyCreated": true})));
        assert!(matches!(cache.begin("wt"), BeginOutcome::Duplicate(_)));
    }

    #[test]
    fn request_cache_never_seen_id_is_fresh_not_fresh_after_reap() {
        // A first-ever id must be plain Fresh (no reap happened), so dispatch does
        // NOT waste a reality re-probe on it - FreshAfterReap is reserved for a
        // retry whose prior reservation actually aged out.
        let cache = RequestCache::new();
        assert!(matches!(cache.begin("brand-new"), BeginOutcome::Fresh));
    }

    #[test]
    fn request_cache_reap_after_completion_is_fresh_not_reap() {
        // A COMPLETED id that TTL-expires and is retried is a fresh apply, NOT a
        // reap: the reap flavor is strictly for an InFlight reservation that aged
        // out (the ambiguous "did it land?" case), not for a cleanly-finished one
        // whose cache entry simply expired.
        let cache = RequestCache::with_bounds(
            8,
            std::time::Duration::from_millis(1), // TTL
            std::time::Duration::from_secs(600), // reap window (irrelevant here)
        );
        cache.begin("done");
        let _ = cache.finish("done", Ok(json!({"id": "done"})));
        std::thread::sleep(std::time::Duration::from_millis(5)); // outlive the TTL
        assert!(matches!(cache.begin("done"), BeginOutcome::Fresh));
    }

    #[test]
    fn request_cache_finish_after_reap_does_not_leak_the_entry() {
        // M2: a handler that outlives the reap window has its reservation evicted
        // from BOTH maps; a late finish() must re-establish `order` membership so
        // the recorded outcome is still TTL/capacity-evictable - never a permanent
        // leak that breaches the cap and reports `completed` forever.
        let cache = RequestCache::with_bounds(
            1,
            std::time::Duration::from_secs(600),
            std::time::Duration::from_millis(1),
        );
        cache.begin("x");
        std::thread::sleep(std::time::Duration::from_millis(5));
        // status() reaps the stale InFlight "x" from slots AND order.
        assert!(matches!(cache.status("x"), RequestStatus::Unknown));
        // The handler finally finishes, AFTER its reservation was reaped.
        let _ = cache.finish("x", Ok(json!({"id": "x"})));
        assert!(matches!(cache.status("x"), RequestStatus::Completed(_)), "outcome preserved");
        // Crucially, "x" is back in `order`: capacity pressure now evicts it. With
        // the leak unfixed, "x" would linger in `slots` forever (never in `order`).
        cache.begin("y");
        let _ = cache.finish("y", Ok(json!({"id": "y"})));
        assert!(
            matches!(cache.status("x"), RequestStatus::Unknown),
            "finish must re-establish order membership so the entry stays evictable (M2)"
        );
    }

    #[test]
    fn spawn_terminal_retry_with_same_request_id_does_not_duplicate() {
        // Repro of Incident A/B at the dispatch layer: a spawn that is RETRIED with
        // the same requestId (the client's recovery from an ambiguous response leg)
        // must apply exactly once - one tmux session, one tile, one UI forward - and
        // the retry must replay the original outcome, never spawn a second session.
        let sink = Arc::new(RecordingSink { calls: StdMutex::new(Vec::new()) });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());
        let args = json!({"cwd": "/tmp", "requestId": "spawn-retry-1"});
        let first = dispatch_authenticated(&ctx, ControlRequest {
            token: "t".into(),
            command: "spawn_terminal".into(),
            args: args.clone(),
            v: None,
        });
        assert!(first.ok, "first spawn failed: {:?}", first.error);
        let id = first.result.as_ref().unwrap()["id"].as_str().unwrap().to_string();

        // The retry: identical requestId. It must NOT spawn again.
        let retry = dispatch_authenticated(&ctx, ControlRequest {
            token: "t".into(),
            command: "spawn_terminal".into(),
            args,
            v: None,
        });
        assert!(retry.ok, "retry failed: {:?}", retry.error);
        let retry_result = retry.result.unwrap();
        assert_eq!(retry_result["id"].as_str().unwrap(), id, "retry replays the same id");
        assert_eq!(retry_result["idempotentReplay"], json!(true), "retry is tagged a replay");

        // Exactly ONE real session materialized, and ONE UI forward was emitted.
        let live: Vec<String> = tmux::list_sessions()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s == &format!("th_{id}"))
            .collect();
        assert_eq!(live.len(), 1, "exactly one tmux session for the id");
        assert_eq!(sink.calls.lock().unwrap().len(), 1, "the retry did NOT re-forward a spawn");

        // Reap the real session.
        dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    }

    #[test]
    fn get_request_status_command_resolves_a_completed_spawn() {
        // The queryable half of ask #1: after a spawn with a requestId, a caller
        // whose response leg failed can learn the outcome (and the real id) without
        // guessing. An unknown id reports unknown (safe to retry).
        let sink = Arc::new(RecordingSink { calls: StdMutex::new(Vec::new()) });
        let ctx = test_ctx("t").with_apply_sink(sink);
        let spawn = dispatch_authenticated(&ctx, ControlRequest {
            token: "t".into(),
            command: "spawn_terminal".into(),
            args: json!({"cwd": "/tmp", "requestId": "spawn-status-1"}),
            v: None,
        });
        assert!(spawn.ok);
        let id = spawn.result.unwrap()["id"].as_str().unwrap().to_string();

        let status = dispatch(&ctx, "get_request_status", &json!({"requestId": "spawn-status-1"})).unwrap();
        assert_eq!(status["status"], "completed");
        assert_eq!(status["ok"], true);
        assert_eq!(status["result"]["id"].as_str().unwrap(), id);

        let unknown = dispatch(&ctx, "get_request_status", &json!({"requestId": "never-seen"})).unwrap();
        assert_eq!(unknown["status"], "unknown");

        dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    }

    // -----------------------------------------------------------------------
    // Registry-vs-reality: close_terminal outcome (ask #3, Incident C)
    // -----------------------------------------------------------------------

    #[test]
    fn close_terminal_reports_already_gone_for_a_phantom() {
        // Incident C: closing a session that never existed must not look like a real
        // kill. ok:true (idempotent) stays, but the outcome discriminates it.
        let ctx = test_ctx("t");
        let v = dispatch(&ctx, "close_terminal", &json!({"sessionId": "f0f3207b"})).unwrap();
        assert_eq!(v["accepted"], "close_terminal");
        assert_eq!(v["outcome"], "already_gone");
    }

    #[test]
    fn close_terminal_reports_killed_for_a_live_session() {
        // A real session reports outcome=killed, so a caller can tell a genuine kill
        // from a phantom close.
        let sink = Arc::new(RecordingSink { calls: StdMutex::new(Vec::new()) });
        let ctx = test_ctx("t").with_apply_sink(sink);
        let spawn = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).unwrap();
        let id = spawn["id"].as_str().unwrap().to_string();
        let closed = dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
        assert_eq!(closed["outcome"], "killed");
    }

    // -----------------------------------------------------------------------
    // Incident D: captains persistence no longer holds the registry lock
    // -----------------------------------------------------------------------

    #[test]
    fn captains_persist_writes_through_off_the_lock() {
        // The write-through still happens (durability preserved), now via the
        // off-lock `persist` path.
        let dir = std::env::temp_dir().join(format!("t-hub-captains-persist-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("captains.json");
        let _ = std::fs::remove_file(&path);
        let reg = CaptainsRegistry::load(path.clone());
        reg.claim("cap-1", Some("alpha"), vec![]).unwrap();
        let body = std::fs::read_to_string(&path).expect("captains.json written through");
        assert!(body.contains("alpha"), "persisted body must carry the claim: {body}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn captains_persist_is_monotonic_and_drops_a_stale_snapshot() {
        // Two writers that dropped `inner` in one order but reach disk in the other
        // must not regress the file: an older-seq snapshot is dropped.
        let dir = std::env::temp_dir().join(format!("t-hub-captains-mono-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("captains.json");
        let _ = std::fs::remove_file(&path);
        let reg = CaptainsRegistry::load(path.clone());
        reg.claim("cap-1", Some("alpha"), vec![]).unwrap(); // seq -> 1 on disk
        let newer = reg.snapshot(); // seq 1
        // Hand-persist a STALE snapshot (seq 0): it must be dropped, not clobber.
        reg.persist(CaptainsSnapshot { seq: 0, captains: vec![] });
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("alpha"), "stale seq-0 snapshot must not clobber the claim: {body}");
        // A NEWER snapshot (seq 1, already on disk) is allowed to (re)write.
        reg.persist(newer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stalled_persist_does_not_block_a_concurrent_registry_reader_or_mutator() {
        // The core Incident-D proof: with persistence moved OFF the `inner` lock, a
        // STALLED disk write (here a hook that blocks while holding only the
        // `persist` mutex) must NOT block a concurrent reader or mutator that only
        // touches `inner`. Under the OLD code (persist under the registry lock) the
        // reader below would hang for the duration of the stall - so this test would
        // TIME OUT and fail, which is exactly the regression guard we want.
        use std::sync::mpsc;
        let dir = std::env::temp_dir().join(format!("t-hub-captains-stall-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("captains.json");
        let _ = std::fs::remove_file(&path);
        let reg = Arc::new(CaptainsRegistry::load(path));

        // The hook stands in for a stalled OneDrive-backed write: it signals that a
        // persist is in progress, then blocks (holding `persist`, NOT `inner`) until
        // the test releases it.
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = StdMutex::new(release_rx);
        reg.set_persist_hook(Box::new(move || {
            let _ = started_tx.send(());
            let _ = release_rx.lock().unwrap().recv(); // block: the write is stalled
        }));

        // A background mutator triggers a persist and stalls inside it. Its `inner`
        // mutation (the claim) has ALREADY committed + released `inner` by the time
        // persist runs, so `inner` is free while this stalls.
        let writer_reg = reg.clone();
        let writer = std::thread::spawn(move || {
            writer_reg.claim("cap-1", Some("alpha"), vec![]).unwrap();
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the persist hook should have started (mutation reached persist)");

        // NOW, while the persist is stalled: a concurrent reader must return promptly
        // (it only takes `inner`). Run it on a thread so a REGRESSION (reader blocked
        // on `inner`) surfaces as a timeout instead of hanging the suite forever.
        let reader_reg = reg.clone();
        let (read_tx, read_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let snap = reader_reg.snapshot();
            let _ = read_tx.send(snap.captains.len());
        });
        let n = read_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("a reader was BLOCKED by a stalled persist (regression: persist holds `inner`)");
        assert_eq!(n, 1, "the reader sees the already-committed claim");

        // Release the stalled write; the mutator finishes cleanly.
        let _ = release_tx.send(());
        writer.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
