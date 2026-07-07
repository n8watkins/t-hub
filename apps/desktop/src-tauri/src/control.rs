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
/// `{"scrollback":"<b64>"}`, then `{"out":"<b64>"}` / `{"exit":code}` down and
/// `{"write":"<b64>"}` / `{"resize":{cols,rows}}` up. With `"binary": true` it
/// speaks **v2** — length-prefixed binary frames ([`pty::binframe`]): a SCROLLBACK
/// frame opens, then OUT / EXIT down and WRITE / RESIZE up, with no base64 and no
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
/// through to `captains.json` under the registry lock, and `load` seeds from it.
pub struct CaptainsRegistry {
    inner: Mutex<CaptainsInner>,
    /// Persistence target; `None` = in-memory only (unit tests / headless proofs).
    path: Option<PathBuf>,
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
        Self { inner: Mutex::new(CaptainsInner::default()), path: None }
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
        Self { inner: Mutex::new(inner), path: Some(path) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CaptainsInner> {
        // Same poisoned-lock policy as TabRegistry: the data is a plain Vec, so
        // recovering the guard and continuing is safe.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Best-effort write-through, called under the registry lock so persisted
    /// snapshots are serialized and never interleave. A write failure is logged
    /// and never fails the mutation (the in-memory registry stays authoritative
    /// for this run; the next successful write heals the file).
    ///
    /// ATOMIC (temp + rename), mirroring `voice.rs`: the loader treats a corrupt
    /// file as empty (silently dropping every claim), so a crash mid-write must
    /// never leave a torn file. We write a full body to a unique temp path, then
    /// `rename` it over the target - `rename` replaces atomically (on Windows too,
    /// MOVEFILE_REPLACE_EXISTING), so a reader/loader always sees either the old
    /// complete file or the new complete file, never a partial one.
    fn persist_locked(&self, g: &CaptainsInner) {
        let Some(path) = &self.path else { return };
        let snap = CaptainsSnapshot { seq: g.seq, captains: g.captains.clone() };
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
        }
    }

    /// The full versioned snapshot (`list_captains` + every `sync_captains` forward).
    pub fn snapshot(&self) -> CaptainsSnapshot {
        let g = self.lock();
        CaptainsSnapshot { seq: g.seq, captains: g.captains.clone() }
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
            self.persist_locked(&g);
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
        self.persist_locked(&g);
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
        self.persist_locked(&g);
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
            self.persist_locked(&g);
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
            self.persist_locked(&g);
        }
        changed
    }
}

impl Default for CaptainsRegistry {
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

/// Defensive cap on concurrently live PTY attach forwarders (s27). Each
/// forwarder costs a PTY pair, a `tmux attach` client, a reader thread, and a
/// socket, and every one also holds an [`ACTIVE_CONNS`] slot - so a churn storm
/// of attaches must never be able to starve the request/event paths (cap is
/// well under [`MAX_CONNS`]). Generous: a full cockpit is ~14 attaches
/// (T10-measured), satellites included, so 64 fits 4+ complete clients.
const MAX_ATTACH_FORWARDERS: usize = 64;
static ACTIVE_ATTACH_FORWARDERS: AtomicUsize = AtomicUsize::new(0);

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
/// scrollback/out/exit/error frames written down AND the write/resize frames read
/// up — so a v1 client is byte-for-byte unchanged and a v2 client never sees base64.
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
        "invalidate_recent_cache" => invalidate_recent_cache(),
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
        match spawn_tmux_terminal(&worktree_path, None) {
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
    let (id, tmux_session) = spawn_tmux_terminal(&cwd_effective, pane.as_deref())?;

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
fn spawn_tmux_terminal(cwd: &str, command: Option<&str>) -> Result<(String, String), String> {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let id = suffix[..8].to_string();
    let tmux_session = format!("th_{id}");
    tmux::new_session(&tmux_session, cwd, command)
        .map_err(|e| format!("failed to create tmux session: {e}"))?;
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
    tmux::kill_session_tree(&target)
        .map_err(|e| format!("failed to close terminal '{session_id}': {e}"))?;
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
            idle_timeout: CONN_READ_TIMEOUT,
            attach_write_timeout: ATTACH_WRITE_TIMEOUT,
            max_attach_forwarders: MAX_ATTACH_FORWARDERS,
            peer_is_loopback: true,
            token,
        }
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
}
