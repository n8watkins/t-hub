//! Core-side **agent bridge** (PLAN.md Workstream A, core half).
//!
//! Owns the long-lived connection to the WSL-side `t-hub-agent`:
//!   - launches `wsl.exe -d <distro> -- t-hub-agent --stdio` on Windows, or
//!     `t-hub-agent --stdio` directly on a unix dev box ([`launch_argv`]);
//!   - performs the [`Hello`]/[`Ready`] handshake;
//!   - correlates [`AgentRequest`]s with [`AgentResponse`]s by [`RequestId`];
//!   - consumes streamed/replayed [`EventJournalEntry`]s, advances the journal
//!     cursor, feeds [`crate::supervision::Supervisor`], and fans entries out to
//!     the UI via the [`crate::events`] journal/agent channels;
//!   - exposes WSL metrics / git / registry RPCs to the rest of the core.
//!
//! ## Status
//! This file defines the **contract** for the bridge: the public types,
//! [`AgentBridge`] handle, and method signatures the Tauri commands call. The
//! transport internals (spawning the child, reader/writer threads, the priority
//! scheduler that exploits [`t_hub_protocol::Channel`]/`Priority`, reconnect +
//! replay) are implemented by SUBAGENT(agent-bridge). The stubs compile and
//! return a clear "not yet connected" error so the command surface is wired and
//! typecheckable today.
//!
//! Boundary: SUBAGENT(agent-bridge) owns this directory (`agent/`). It must not
//! change `t-hub-protocol`, `model.rs`, or `supervision.rs` (it *calls* them).

mod connection;
pub mod emit;

pub use connection::ConnectionState;
pub use emit::{EventEmitter, TauriEmitter};

use std::sync::{mpsc, Arc};

use parking_lot::Mutex;
use t_hub_protocol::{
    AgentRequest, AgentResponse, Channel, CoreFrame, CoreToAgent, EventJournalEntry, Hello,
    HostMetrics, Priority, WorktreeInfo, PROTOCOL_VERSION,
};

use crate::supervision::Supervisor;
use connection::{spawn_child, spawn_reader, write_frame, TransportHandles};
use emit::{
    JournalEventPayload, SessionStatusPayload, SessionTitlePayload, EVT_AGENT_STATE, EVT_JOURNAL,
    EVT_SESSION_STATUS, EVT_STATUS_SNAPSHOT, EVT_SUPERVISION, EVT_TITLE,
};

/// How the core reaches the agent on this platform.
///
/// On Windows the agent runs inside the distro via `wsl.exe`; on unix (dev) it
/// is spawned directly so the whole spine is exercisable in this shell.
///
/// ## Windows agent resolution
///
/// The bundled `t-hub-agent` is installed to `~/.local/bin/t-hub-agent`
/// inside the distro. A bare `wsl.exe -d <distro> -- t-hub-agent` runs a
/// **non-login, non-interactive** shell, so the user's profile is never sourced
/// and `~/.local/bin` is *not* on `PATH` — the spawn fails and no live WSL
/// health / agent state ever reaches the sidebar. To make resolution robust we
/// launch through a **login shell** (`bash -lc`), which sources the profile and
/// puts `~/.local/bin` on `PATH`:
///
/// ```text
/// wsl.exe -d <distro> --cd ~ -e bash -lc "exec t-hub-agent --stdio"
/// ```
///
/// `exec` replaces the login shell with the agent so there's no extra process
/// in the tree and stdio is wired straight through. The `--cd ~` lands the
/// child in the user's home, matching the profile-relative `~/.local/bin`.
///
/// The `T_HUB_AGENT_BIN` escape hatch (honored here on Windows and in
/// [`connection::spawn_child`] on every platform) bypasses the login-shell hop
/// entirely: when set, its value is used **verbatim** as the program to spawn,
/// so a developer can point the bridge at an arbitrary binary without touching
/// PATH or the distro.
///
/// Called by SUBAGENT(agent-bridge)'s transport when it spawns the child.
#[allow(dead_code)]
pub fn launch_argv(distro: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        // Escape hatch: if T_HUB_AGENT_BIN is set, spawn it verbatim (no
        // wsl.exe / login-shell hop). This keeps the override usable on Windows
        // where it would otherwise be misapplied as wsl.exe's argv[0].
        if let Some(bin) = std::env::var("T_HUB_AGENT_BIN")
            .ok()
            .filter(|s| !s.is_empty())
        {
            return vec![bin, "--stdio".to_string()];
        }
        // Login shell so the user's profile is sourced and `~/.local/bin`
        // (where the orchestrator installs the agent) is on PATH. `exec` so the
        // agent replaces the shell rather than running as a child of it. `-e`
        // makes wsl.exe exec bash DIRECTLY — a bare `--` routes the command through
        // the user's DEFAULT login shell (zsh here), NOT bash (see the note on
        // tmux.rs::pane_info_command). bash's login PATH also has ~/.local/bin, so
        // the agent still resolves; `-e` just keeps us in the shell we intend.
        vec![
            "wsl.exe".to_string(),
            "-d".to_string(),
            distro.to_string(),
            "--cd".to_string(),
            "~".to_string(),
            "-e".to_string(),
            "bash".to_string(),
            "-lc".to_string(),
            "exec t-hub-agent --stdio".to_string(),
        ]
    }
    #[cfg(unix)]
    {
        let _ = distro; // distro is irrelevant when launching directly.
        vec!["t-hub-agent".to_string(), "--stdio".to_string()]
    }
}

/// Shared handle to the agent connection + the supervision reducer it feeds.
/// Cloneable (`Arc` inside) so Tauri-managed state and the reader thread share
/// one connection.
#[derive(Clone)]
pub struct AgentBridge {
    inner: Arc<BridgeInner>,
}

struct BridgeInner {
    /// The supervision reducer, fed by incoming journal events. Shared so the
    /// supervision Tauri commands can read snapshots without a round-trip.
    supervisor: Mutex<Supervisor>,
    /// Connection state machine. SUBAGENT(agent-bridge) drives this from the
    /// transport threads.
    state: Mutex<ConnectionState>,
    /// Highest journal sequence the core has durably consumed (the replay
    /// cursor). Advanced as entries arrive; persisted by workstream G later.
    journal_cursor: Mutex<u64>,
    /// Live transport handles (stdin writer + correlation map). `None` when
    /// disconnected. Set by `connect()`, read by `request()`.
    transport: Mutex<Option<Arc<TransportHandles>>>,
    /// The live UI event sink. `None` until [`AgentBridge::set_emitter`] installs
    /// it from the Tauri `setup()` hook (the bridge is built before the
    /// `AppHandle` exists, and unit tests never install one). All emission goes
    /// through [`BridgeInner::emit`] / [`BridgeInner::emit_json`], which are
    /// no-ops while this is `None`.
    emitter: Mutex<Option<Arc<dyn EventEmitter>>>,
    /// The status bridge, so a `StatusSnapshot` journal entry can be ingested and
    /// re-emitted as `status://snapshot` from the single journal-consume path.
    /// `None` until wired in `setup()` (and under unit tests). Held as a trait-
    /// free `Arc<StatusBridge>` to avoid a cycle with `claude`.
    status: Mutex<Option<Arc<crate::claude::StatusBridge>>>,
}

impl BridgeInner {
    /// Emit a `Serialize` payload on `channel` if an emitter is installed; a
    /// no-op otherwise (pre-`setup()` and under unit tests). Best-effort: the
    /// emitter swallows transport errors so UI delivery never blocks ingestion.
    fn emit<T: serde::Serialize>(&self, channel: &str, payload: &T) {
        let emitter = self.emitter.lock().clone();
        if let Some(e) = emitter {
            e.emit(channel, payload);
        }
    }
}

impl Default for AgentBridge {
    fn default() -> Self {
        Self {
            inner: Arc::new(BridgeInner {
                supervisor: Mutex::new(Supervisor::new()),
                state: Mutex::new(ConnectionState::Disconnected),
                journal_cursor: Mutex::new(0),
                transport: Mutex::new(None),
                emitter: Mutex::new(None),
                status: Mutex::new(None),
            }),
        }
    }
}

impl AgentBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current connection state (for the UI health area / diagnostics).
    pub fn state(&self) -> ConnectionState {
        *self.inner.state.lock()
    }

    /// The core's journal replay cursor (highest consumed seq).
    pub fn journal_cursor(&self) -> u64 {
        *self.inner.journal_cursor.lock()
    }

    /// Run a closure against the supervision reducer (read or mutate). Used by
    /// the supervision Tauri commands and by the journal consumer.
    pub fn with_supervisor<R>(&self, f: impl FnOnce(&mut Supervisor) -> R) -> R {
        f(&mut self.inner.supervisor.lock())
    }

    /// Install the live UI event sink (called once from the Tauri `setup()` hook,
    /// after the `AppHandle` exists). Idempotent: a later call replaces the sink.
    /// Emits an initial `agent://state` so the UI reflects the current connection
    /// state the moment the sink is wired, without waiting for a transition.
    pub fn set_emitter(&self, emitter: Arc<dyn EventEmitter>) {
        *self.inner.emitter.lock() = Some(emitter);
        // Push the current state immediately so a UI that mounts after a connect
        // already happened still gets a live `agent://state`.
        self.emit_agent_state();
    }

    /// Wire the status bridge so `StatusSnapshot` journal entries flowing through
    /// [`AgentBridge::consume_journal_entry`] are ingested + re-emitted as
    /// `status://snapshot`. Called once from `setup()` alongside the emitter.
    pub fn set_status_bridge(&self, status: Arc<crate::claude::StatusBridge>) {
        *self.inner.status.lock() = Some(status);
    }

    /// Transition the connection state **and** emit `agent://state` so the UI's
    /// health area is always live. Centralizing the write here guarantees no
    /// transition can silently skip the emit (the historical bug was that
    /// `connect()` mutated `state` directly and nothing was emitted).
    fn set_state(&self, next: ConnectionState) {
        {
            let mut guard = self.inner.state.lock();
            if *guard == next {
                return; // no change → no emit (avoid event spam on reconnect loops)
            }
            *guard = next;
        }
        self.emit_agent_state();
    }

    /// Emit the current `agent://state` payload (connection + journal cursor).
    fn emit_agent_state(&self) {
        let payload = crate::commands_05::AgentStateInfo {
            connection: self.state(),
            journal_cursor: self.journal_cursor(),
        };
        self.inner.emit(EVT_AGENT_STATE, &payload);
    }

    /// Launch the agent and complete the handshake.
    ///
    /// Spawns the child from [`launch_argv`] with piped stdin/stdout (stderr
    /// inherited). Starts a reader thread that dispatches incoming
    /// [`t_hub_protocol::AgentToCore`] frames. Sends `Hello`, waits for
    /// `Ready`, and if the agent's `journal_head_seq` is ahead of our cursor,
    /// sends `ReplayJournal` and waits for `ReplayComplete` before setting the
    /// state to `Live`.
    ///
    /// The `T_HUB_AGENT_BIN` env var overrides argv[0] for tests / dev
    /// (see [`connection::spawn_child`]).
    pub fn connect(&self, distro: &str) -> Result<(), String> {
        // Build argv and spawn child.
        let argv = launch_argv(distro);
        let mut child = spawn_child(argv).map_err(|e| format!("failed to spawn agent: {e}"))?;

        // Take ownership of the stdio handles before the child handle moves
        // into TransportHandles.
        let child_stdin = child
            .stdin
            .take()
            .ok_or("child has no stdin pipe")?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or("child has no stdout pipe")?;

        // Build the shared correlation map and next-id counter.
        let pending = Arc::new(connection::CorrelationMap::new(
            std::collections::HashMap::new(),
        ));
        let next_id = Arc::new(std::sync::atomic::AtomicU64::new(1));

        // Build transport handles (Arc so request() can clone a reference).
        let handles = Arc::new(TransportHandles {
            stdin: Mutex::new(child_stdin),
            pending: Arc::clone(&pending),
            next_id: Arc::clone(&next_id),
            child: Mutex::new(child),
        });

        // One-shot channels for the handshake/replay synchronisation.
        let (ready_tx, ready_rx) = mpsc::channel::<u64>();
        let (replay_done_tx, replay_done_rx) = mpsc::channel::<u64>();

        // Spawn the reader thread.  It captures a clone of `self` (AgentBridge
        // is Clone/Arc-backed) so it can call consume_journal_entry.
        spawn_reader(
            child_stdout,
            Arc::clone(&pending),
            self.clone(),
            ready_tx,
            replay_done_tx,
        );

        // Set state and store transport handles.
        self.set_state(ConnectionState::Handshaking);
        *self.inner.transport.lock() = Some(Arc::clone(&handles));

        // --- Handshake: send Hello ---
        {
            let hello = CoreFrame {
                channel: Channel::Control,
                msg: CoreToAgent::Hello(Hello {
                    protocol_version: PROTOCOL_VERSION,
                    core_version: format!("t-hub {}", env!("CARGO_PKG_VERSION")),
                }),
            };
            let mut stdin_guard = handles.stdin.lock();
            write_frame(&mut *stdin_guard, &hello)
                .map_err(|e| format!("failed to write Hello: {e}"))?;
        }

        // Wait for Ready (10 s timeout). On failure, mark Failed (and emit) so
        // the UI shows the dead connection rather than a stuck "handshaking".
        let journal_head_seq = match ready_rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(seq) => seq,
            Err(_) => {
                self.set_state(ConnectionState::Failed);
                return Err("timed out waiting for Ready from agent".to_string());
            }
        };

        // If the agent has journal entries we haven't consumed, request replay.
        let cursor = self.journal_cursor();
        if journal_head_seq > cursor {
            self.set_state(ConnectionState::Replaying);

            let replay_frame = CoreFrame {
                channel: Channel::Control,
                msg: CoreToAgent::ReplayJournal { after_seq: cursor },
            };
            {
                let mut stdin_guard = handles.stdin.lock();
                write_frame(&mut *stdin_guard, &replay_frame)
                    .map_err(|e| format!("failed to write ReplayJournal: {e}"))?;
            }

            // Wait for ReplayComplete (30 s — replay can be large).
            if replay_done_rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .is_err()
            {
                self.set_state(ConnectionState::Failed);
                return Err("timed out waiting for ReplayComplete from agent".to_string());
            }
        }

        self.set_state(ConnectionState::Live);
        Ok(())
    }

    /// Tear down the live connection so a fresh [`connect`](Self::connect) can't
    /// leak the old reader thread or orphan in-flight senders. Safe to call when
    /// already disconnected (it's a no-op then). Used by [`reconnect`](Self::reconnect)
    /// and callable directly from a tray "reconnect" action.
    ///
    /// Order matters (an earlier audit flagged a reader-thread / pending-sender
    /// leak on reconnect), so we do, in sequence:
    ///   1. **Take** the old `TransportHandles` out of `self.inner.transport`
    ///      (leaving it `None`), so any concurrent `request()` immediately sees
    ///      "not connected" rather than writing to a dying stdin.
    ///   2. **Clear the `pending` correlation map.** Dropping each one-shot
    ///      `Sender` wakes its blocked `request()` caller right away (its
    ///      `recv_timeout` returns `Err` instead of hanging the full 10 s), so no
    ///      in-flight sender is orphaned waiting on a reply that can never come.
    ///   3. **Kill the child** (`Child::kill`). Closing its stdout is what makes
    ///      the detached `agent-reader` thread hit EOF and exit — that's how the
    ///      old reader thread is reclaimed. `spawn_reader` returns no `JoinHandle`
    ///      (the thread is detached and self-terminating on EOF), so we cannot
    ///      `join()` it; killing the child is the deterministic teardown signal.
    ///      We then `wait()` to reap the zombie.
    ///
    /// LIMITATION: the reader thread is detached, so we can't *block* until it has
    /// fully unwound — we kill the child (its EOF trigger) and rely on it exiting
    /// promptly. In practice it returns from `BufReader::lines()` the moment stdout
    /// closes. Because each `connect()` builds a brand-new `pending` map + reader
    /// thread (nothing is shared with the previous connection), a not-yet-exited
    /// old reader can only touch its OWN now-empty map — it can't corrupt the new
    /// connection's state.
    pub fn disconnect(&self) {
        // 1. Detach the live transport so new requests can't use it.
        let old = self.inner.transport.lock().take();
        let Some(handles) = old else {
            // Already disconnected: still normalize the state and bail.
            self.set_state(ConnectionState::Disconnected);
            return;
        };

        // 2. Clear pending correlations: dropping the senders unblocks any waiting
        //    request() callers (recv_timeout -> Err) instead of orphaning them.
        handles.pending.lock().clear();

        // 3. Kill + reap the child so its stdout closes and the detached reader
        //    thread hits EOF and exits. Best-effort: a kill on an already-dead
        //    child is a benign error.
        {
            let mut child = handles.child.lock();
            let _ = child.kill();
            let _ = child.wait();
        }

        // Drop this Arc reference. If a concurrent request() still holds a clone,
        // the struct's memory outlives this call, but the child is already killed
        // + reaped above, so teardown is complete regardless; the new connect()
        // allocates entirely fresh handles either way.
        drop(handles);
        self.set_state(ConnectionState::Disconnected);
    }

    /// Re-establish the agent connection without touching terminals: safely tear
    /// the old connection down (see [`disconnect`](Self::disconnect)) and then
    /// [`connect`](Self::connect) again. Fixes a wedged bridge ("supervision /
    /// cost stopped updating") where the reader thread died or the agent went
    /// away, with no full app restart.
    ///
    /// The journal cursor is intentionally preserved across the reconnect, so the
    /// fresh handshake only replays entries newer than what we already consumed
    /// (no duplicate ingestion).
    pub fn reconnect(&self, distro: &str) -> Result<(), String> {
        self.disconnect();
        self.connect(distro)
    }

    /// Send a request and await its correlated response (blocking, 10 s timeout).
    ///
    /// Allocates the next [`t_hub_protocol::RequestId`] from an atomic
    /// counter, registers a one-shot [`mpsc`] sender in the correlation map,
    /// serializes the [`CoreFrame`] to the child's stdin (behind a `Mutex` so
    /// concurrent callers don't interleave bytes), then blocks on the receiver.
    ///
    /// **Channel / Priority**: `Channel::Control` and `Priority::Normal` are
    /// used for all requests today. A future scheduler can inspect the request
    /// body to select the appropriate channel and priority before writing.
    pub fn request(&self, req: AgentRequest) -> Result<AgentResponse, String> {
        // Grab the transport handles (returns an error if not connected).
        let handles = {
            let guard = self.inner.transport.lock();
            guard.as_ref()
                .cloned()
                .ok_or_else(|| "agent bridge not connected".to_string())?
        };

        // Allocate a unique request id.
        let id = handles.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Register the one-shot channel before writing so the reader thread
        // can never race ahead of us.
        let (tx, rx) = mpsc::channel::<AgentResponse>();
        handles.pending.lock().insert(id, tx);

        // Build and write the request frame.
        // NOTE: Channel::Control and Priority::Normal are used for all ops
        // today. Channel and Priority are fully serialized and echoed by the
        // agent; a future priority scheduler uses them to reorder the outbound
        // queue without protocol changes.
        let frame = CoreFrame {
            channel: Channel::Control,
            msg: CoreToAgent::Request {
                id,
                priority: Priority::Normal,
                body: req,
            },
        };

        {
            let mut stdin_guard = handles.stdin.lock();
            write_frame(&mut *stdin_guard, &frame).map_err(|e| {
                // Remove the dangling correlation entry on write failure.
                handles.pending.lock().remove(&id);
                format!("failed to write request id={id}: {e}")
            })?;
        }

        // Block until the reader delivers the response or we time out.
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(response) => Ok(response),
            Err(_) => {
                // Clean up the correlation entry so the reader doesn't deliver
                // a stale response after we've given up.
                handles.pending.lock().remove(&id);
                Err(format!("request id={id} timed out after 10 seconds"))
            }
        }
    }

    /// Convenience: fetch a host metrics snapshot.
    pub fn metrics(&self) -> Result<HostMetrics, String> {
        match self.request(AgentRequest::Metrics)? {
            AgentResponse::Metrics(m) => Ok(m),
            other => Err(format!("unexpected response to metrics: {other:?}")),
        }
    }

    /// Convenience: derive the current git branch for `cwd` (statusline lacks it).
    pub fn git_branch(&self, cwd: &str) -> Result<Option<String>, String> {
        match self.request(AgentRequest::GitBranch { cwd: cwd.to_string() })? {
            AgentResponse::GitBranch { branch } => Ok(branch),
            other => Err(format!("unexpected response to git_branch: {other:?}")),
        }
    }

    /// Convenience: list worktrees for the repo containing `cwd`.
    pub fn git_worktrees(&self, cwd: &str) -> Result<Vec<WorktreeInfo>, String> {
        match self.request(AgentRequest::GitWorktrees { cwd: cwd.to_string() })? {
            AgentResponse::GitWorktrees { worktrees } => Ok(worktrees),
            other => Err(format!("unexpected response to git_worktrees: {other:?}")),
        }
    }

    /// Consume one journal entry: advance the cursor, feed supervision, emit the
    /// live UI events, and return the affected session id.
    ///
    /// This is the core's single ingestion point for the spine. It is where the
    /// previously-missing **live emit** happens — when an entry is consumed it
    /// now fans out:
    ///   - `agent://journal` (the entry, forwarded verbatim);
    ///   - `agent://state` (if the cursor advanced — the health area shows it);
    ///   - `supervision://tree` + `session://status` (for the affected session,
    ///     after the supervision reducer has updated);
    ///   - `status://snapshot` (when the entry is a `StatusSnapshot`, routed
    ///     through the status bridge if one is wired).
    ///
    /// All emission is best-effort and a no-op before [`AgentBridge::set_emitter`]
    /// (so the unit tests that call this directly still pass). Returns the
    /// affected session id for callers/tests that want it.
    pub fn consume_journal_entry(&self, entry: &EventJournalEntry) -> Option<String> {
        let cursor_advanced = {
            let mut cursor = self.inner.journal_cursor.lock();
            if entry.seq > *cursor {
                *cursor = entry.seq;
                true
            } else {
                false
            }
        };

        // 1. Forward the raw journal entry to the UI (snake_case, verbatim — it's
        //    the protocol type). Serialize once and reuse the value.
        self.inner.emit(
            EVT_JOURNAL,
            &JournalEventPayload { entry },
        );

        // 2. If the replay cursor moved, the health area's journalCursor changed.
        if cursor_advanced {
            self.emit_agent_state();
        }

        // 3. A statusline snapshot rides the journal too (JournalSource::Status).
        //    Route it through the status bridge so `status://snapshot` goes live
        //    and the snapshot is queryable via the status_snapshot command.
        if matches!(entry.event_type, t_hub_protocol::JournalEventType::StatusSnapshot) {
            self.ingest_status_from_journal(entry);
        }

        // 3b. Derive a Claude-suggested title for the session (GOAL NAMES) and
        //     emit `agent://title`. The strongest signal is `UserPromptSubmit`'s
        //     prompt (what the user just asked Claude to do); `SessionStart`
        //     gives the project/cwd as a fallback. The UI prefers this over the
        //     raw command·cwd label. Carries `cwd` so the frontend can correlate
        //     the Claude session id to a T-Hub terminal.
        self.emit_session_title(entry);

        // 4. Feed the supervision reducer. Pull the subagent base fields out of
        //    the payload (hooks put `agent_id` / `agent_type` in stdin inside
        //    subagents — REVIEW base fields).
        let session_id = entry
            .entity_id
            .as_deref()
            .or_else(|| entry.payload.get("session_id").and_then(|v| v.as_str()));
        let agent_id = entry.payload.get("agent_id").and_then(|v| v.as_str());
        let agent_type = entry.payload.get("agent_type").and_then(|v| v.as_str());

        let affected = self.with_supervisor(|s| {
            s.ingest(session_id, agent_id, agent_type, entry.event_type, entry.timestamp_ms)
        });

        // 5. Emit the fresh tree + status for the affected session so the sidebar
        //    re-renders live (this is the headline FR-012 path).
        if let Some(sid) = affected.as_deref() {
            self.emit_session(sid);
        }

        affected
    }

    /// Emit `supervision://tree` and `session://status` for one session from the
    /// current reducer state. Public-in-crate so the status bridge / commands can
    /// re-emit a session after an out-of-band status change.
    pub(crate) fn emit_session(&self, session_id: &str) {
        let (tree, status) = self.with_supervisor(|s| (s.tree(session_id), s.status(session_id)));
        if let Some(tree) = tree {
            self.inner.emit(EVT_SUPERVISION, &tree);
        }
        self.inner.emit(
            EVT_SESSION_STATUS,
            &SessionStatusPayload {
                session_id: session_id.to_string(),
                status,
            },
        );
    }

    /// Derive a Claude-suggested title from a title-bearing hook entry and emit
    /// `agent://title` for the session (GOAL NAMES). No-op for entries that carry
    /// no usable title signal, or that have no session id. Best-effort + behind
    /// the optional emitter, like every other emit on this path.
    fn emit_session_title(&self, entry: &EventJournalEntry) {
        let session_id = entry
            .entity_id
            .as_deref()
            .or_else(|| entry.payload.get("session_id").and_then(|v| v.as_str()));
        let Some(sid) = session_id else { return };
        let Some(title) = derive_session_title(entry.event_type, &entry.payload) else {
            return;
        };
        let cwd = entry
            .payload
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned());
        self.inner.emit(
            EVT_TITLE,
            &SessionTitlePayload {
                session_id: sid.to_string(),
                cwd,
                title,
            },
        );
    }

    /// Route a `StatusSnapshot` journal entry into the status bridge (if wired)
    /// and emit `status://snapshot`. The payload carries the raw statusline JSON
    /// (the hook/agent put it there); we ingest it under the entry's session id.
    fn ingest_status_from_journal(&self, entry: &EventJournalEntry) {
        let Some(status_bridge) = self.inner.status.lock().clone() else {
            return; // no status bridge wired (pre-setup / tests)
        };
        let session_id = entry
            .entity_id
            .as_deref()
            .or_else(|| entry.payload.get("session_id").and_then(|v| v.as_str()));
        let Some(sid) = session_id else { return };
        // The raw statusline lives under `payload.status` when the agent wraps it;
        // fall back to the whole payload for forward-compat.
        let raw = entry
            .payload
            .get("status")
            .unwrap_or(&entry.payload);
        let snap = status_bridge.ingest(sid, raw, entry.timestamp_ms);
        self.inner.emit(EVT_STATUS_SNAPSHOT, &snap);
    }
}

/// Max characters for a derived session title before we ellipsize it. Long
/// enough to carry a useful task summary, short enough to fit a tile/tab label.
const TITLE_MAX_CHARS: usize = 60;

/// Derive a short, human-readable title for a session from a lifecycle hook
/// payload (GOAL NAMES). Pure (no I/O), so it is unit-tested directly.
///
/// Signal preference, strongest first:
///   - **`UserPromptSubmit`**: the user's `prompt` — what they just asked Claude
///     to do. Its first non-empty line, trimmed + capped, is the best label.
///   - **`SessionStart`**: the project basename (last path segment of `cwd`) —
///     a stable fallback so a fresh session still gets a meaningful name before
///     the first prompt.
///
/// Returns `None` for events we don't title or when no usable text is present
/// (the caller then emits nothing and the existing command·cwd label stands).
///
// TODO(claude-title): when a per-session summary signal is available (e.g. a
// `Stop`/`SessionEnd` payload carrying a model-written one-line summary, or a
// transcript tail), prefer it here over the raw prompt's first line.
fn derive_session_title(
    event_type: t_hub_protocol::JournalEventType,
    payload: &serde_json::Value,
) -> Option<String> {
    use t_hub_protocol::JournalEventType as E;
    let raw = match event_type {
        E::UserPromptSubmit => payload.get("prompt").and_then(|v| v.as_str()),
        E::SessionStart => {
            // Fallback to the project (cwd basename) so a brand-new session is
            // labelled before the user's first prompt arrives.
            let cwd = payload.get("cwd").and_then(|v| v.as_str())?;
            return cwd_basename(cwd).map(|s| s.to_string());
        }
        _ => None,
    }?;

    // First non-empty line, collapsed whitespace, capped length.
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(cap_title(line))
}

/// The last non-empty path segment of `cwd` (POSIX or Windows separators), or
/// `None` if there is none or it is just `~`.
fn cwd_basename(cwd: &str) -> Option<&str> {
    let last = cwd
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty() && *s != "~")?;
    Some(last)
}

/// Collapse internal whitespace runs to single spaces and cap to
/// [`TITLE_MAX_CHARS`] characters (ellipsizing on a char boundary).
fn cap_title(s: &str) -> String {
    let collapsed: String = {
        let mut out = String::with_capacity(s.len());
        let mut prev_ws = false;
        for ch in s.chars() {
            if ch.is_whitespace() {
                if !prev_ws {
                    out.push(' ');
                }
                prev_ws = true;
            } else {
                out.push(ch);
                prev_ws = false;
            }
        }
        out.trim().to_string()
    };
    if collapsed.chars().count() <= TITLE_MAX_CHARS {
        return collapsed;
    }
    let truncated: String = collapsed.chars().take(TITLE_MAX_CHARS - 1).collect();
    format!("{}…", truncated.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use t_hub_protocol::{JournalEventType, JournalSource};

    fn entry(
        seq: u64,
        session: &str,
        agent: Option<&str>,
        ev: JournalEventType,
    ) -> EventJournalEntry {
        let mut payload = serde_json::json!({ "session_id": session });
        if let Some(a) = agent {
            payload["agent_id"] = serde_json::json!(a);
        }
        EventJournalEntry {
            seq,
            timestamp_ms: seq,
            source: JournalSource::Hook,
            entity_id: Some(session.to_string()),
            event_type: ev,
            payload,
            result: None,
        }
    }

    #[test]
    fn launch_argv_shape() {
        #[cfg(unix)]
        {
            let argv = launch_argv("Ubuntu-24.04");
            assert_eq!(argv, vec!["t-hub-agent", "--stdio"]);
        }
        #[cfg(windows)]
        {
            // Default Windows path: a login shell so `~/.local/bin` is on PATH.
            std::env::remove_var("T_HUB_AGENT_BIN");
            let argv = launch_argv("Ubuntu-24.04");
            assert_eq!(
                argv,
                vec![
                    "wsl.exe",
                    "-d",
                    "Ubuntu-24.04",
                    "--cd",
                    "~",
                    "-e",
                    "bash",
                    "-lc",
                    "exec t-hub-agent --stdio",
                ]
            );

            // Escape hatch: T_HUB_AGENT_BIN is spawned verbatim (no wsl.exe).
            std::env::set_var("T_HUB_AGENT_BIN", "C:/tmp/t-hub-agent.exe");
            let argv = launch_argv("Ubuntu-24.04");
            assert_eq!(argv, vec!["C:/tmp/t-hub-agent.exe", "--stdio"]);
            std::env::remove_var("T_HUB_AGENT_BIN");
        }
    }

    #[test]
    fn consume_journal_advances_cursor_and_feeds_supervision() {
        let bridge = AgentBridge::new();
        assert_eq!(bridge.journal_cursor(), 0);

        bridge.consume_journal_entry(&entry(1, "o1", None, JournalEventType::SessionStart));
        bridge.consume_journal_entry(&entry(
            2,
            "o1",
            Some("a1"),
            JournalEventType::SubagentStart,
        ));
        let affected = bridge.consume_journal_entry(&entry(3, "o1", None, JournalEventType::Stop));
        assert_eq!(affected.as_deref(), Some("o1"));
        assert_eq!(bridge.journal_cursor(), 3);

        // Supervision saw the events → WaitingOnSubagents.
        let tree = bridge.with_supervisor(|s| s.tree("o1")).unwrap();
        assert_eq!(tree.status, crate::model::SessionStatus::WaitingOnSubagents);
        assert_eq!(tree.children.len(), 1);
    }

    #[test]
    fn cursor_does_not_regress_on_out_of_order_seq() {
        let bridge = AgentBridge::new();
        bridge.consume_journal_entry(&entry(5, "o1", None, JournalEventType::SessionStart));
        assert_eq!(bridge.journal_cursor(), 5);
        // A late/duplicate lower seq must not move the cursor backwards.
        bridge.consume_journal_entry(&entry(3, "o1", None, JournalEventType::UserPromptSubmit));
        assert_eq!(bridge.journal_cursor(), 5);
    }

    /// A recording emitter for the live-emit tests: captures (channel, payload).
    #[derive(Default, Clone)]
    struct RecordingEmitter {
        events: Arc<parking_lot::Mutex<Vec<(String, serde_json::Value)>>>,
    }
    impl super::EventEmitter for RecordingEmitter {
        fn emit_json(&self, channel: &str, payload: &serde_json::Value) {
            self.events
                .lock()
                .push((channel.to_string(), payload.clone()));
        }
    }

    #[test]
    fn consume_emits_journal_supervision_and_status_live() {
        // The #1 0.5 gap: consuming a journal entry must fan out live events.
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        bridge.set_emitter(Arc::new(rec.clone()));

        // set_emitter pushes an initial agent://state.
        {
            let evs = rec.events.lock();
            assert_eq!(evs.len(), 1, "set_emitter should emit one agent://state");
            assert_eq!(evs[0].0, super::EVT_AGENT_STATE);
        }
        rec.events.lock().clear();

        // SessionStart → Working: expect journal + agent://state (cursor moved) +
        // supervision://tree + session://status.
        bridge.consume_journal_entry(&entry(1, "o1", None, JournalEventType::SessionStart));
        let channels: Vec<String> = rec.events.lock().iter().map(|(c, _)| c.clone()).collect();
        assert!(channels.contains(&super::EVT_JOURNAL.to_string()), "journal: {channels:?}");
        assert!(channels.contains(&super::EVT_AGENT_STATE.to_string()), "state: {channels:?}");
        assert!(channels.contains(&super::EVT_SUPERVISION.to_string()), "tree: {channels:?}");
        assert!(
            channels.contains(&super::EVT_SESSION_STATUS.to_string()),
            "status: {channels:?}"
        );

        // The session://status payload must carry the camelCase status string.
        let status_ev = rec
            .events
            .lock()
            .iter()
            .find(|(c, _)| c == super::EVT_SESSION_STATUS)
            .cloned()
            .unwrap();
        assert_eq!(status_ev.1["sessionId"], "o1");
        assert_eq!(status_ev.1["status"], "working");

        // The supervision://tree payload must carry the session + status.
        let tree_ev = rec
            .events
            .lock()
            .iter()
            .find(|(c, _)| c == super::EVT_SUPERVISION)
            .cloned()
            .unwrap();
        assert_eq!(tree_ev.1["sessionId"], "o1");
        assert_eq!(tree_ev.1["status"], "working");
    }

    #[test]
    fn waiting_on_subagents_surfaces_via_session_status_emit() {
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        bridge.set_emitter(Arc::new(rec.clone()));
        rec.events.lock().clear();

        bridge.consume_journal_entry(&entry(1, "o1", None, JournalEventType::SessionStart));
        bridge.consume_journal_entry(&entry(2, "o1", Some("a1"), JournalEventType::SubagentStart));
        // Main agent Stop while the subagent is still running → WaitingOnSubagents.
        bridge.consume_journal_entry(&entry(3, "o1", None, JournalEventType::Stop));

        // The last session://status emit must be waitingOnSubagents (FR-012).
        let last_status = rec
            .events
            .lock()
            .iter()
            .filter(|(c, _)| c == super::EVT_SESSION_STATUS)
            .last()
            .cloned()
            .unwrap();
        assert_eq!(last_status.1["status"], "waitingOnSubagents");
    }

    #[test]
    fn consume_session_end_emits_terminal_status_not_unknown() {
        // REGRESSION (HIGH): evicting the session on `SessionEnd` made
        // `emit_session` read `status()` for an absent session — which defaults to
        // `Unknown` — so the UI's last `session://status` was `unknown` instead of
        // the real terminal status. The supervision reducer tests passed because
        // they assert on `ingest()`'s `status()` in isolation; this asserts on the
        // EMITTED payload from the `consume_journal_entry` → `emit_session` path.
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        bridge.set_emitter(Arc::new(rec.clone()));
        rec.events.lock().clear();

        // Clean-completed path: a main-agent Stop with no outstanding subagents
        // classifies Completed, and SessionEnd keeps that terminal status.
        bridge.consume_journal_entry(&entry(1, "o1", None, JournalEventType::SessionStart));
        bridge.consume_journal_entry(&entry(2, "o1", None, JournalEventType::Stop));
        bridge.consume_journal_entry(&entry(3, "o1", None, JournalEventType::SessionEnd));

        // The LAST session://status emit for this session — the one the UI renders
        // after the session ends — must be the terminal status, never `unknown`.
        let last_status = rec
            .events
            .lock()
            .iter()
            .filter(|(c, _)| c == super::EVT_SESSION_STATUS)
            .last()
            .cloned()
            .expect("a session://status must be emitted on SessionEnd");
        assert_eq!(last_status.1["sessionId"], "o1");
        assert_eq!(
            last_status.1["status"], "completed",
            "SessionEnd after a clean Stop must emit the terminal Completed status"
        );
        assert_ne!(
            last_status.1["status"], "unknown",
            "evicting on SessionEnd must never let emit_session broadcast Unknown"
        );

        // Abnormal path: a Stop with an outstanding subagent is WaitingOnSubagents,
        // so SessionEnd downgrades to Failed (non-Completed → Failed). Still never
        // `unknown` on the emitted payload.
        let rec2 = RecordingEmitter::default();
        bridge.set_emitter(Arc::new(rec2.clone()));
        rec2.events.lock().clear();

        bridge.consume_journal_entry(&entry(4, "o2", None, JournalEventType::SessionStart));
        bridge.consume_journal_entry(&entry(5, "o2", Some("a1"), JournalEventType::SubagentStart));
        bridge.consume_journal_entry(&entry(6, "o2", None, JournalEventType::Stop));
        bridge.consume_journal_entry(&entry(7, "o2", None, JournalEventType::SessionEnd));

        let last_status2 = rec2
            .events
            .lock()
            .iter()
            .filter(|(c, _)| c == super::EVT_SESSION_STATUS)
            .last()
            .cloned()
            .expect("a session://status must be emitted on SessionEnd");
        assert_eq!(last_status2.1["sessionId"], "o2");
        assert_eq!(
            last_status2.1["status"], "failed",
            "SessionEnd while waiting on a subagent must emit the terminal Failed status"
        );
        assert_ne!(last_status2.1["status"], "unknown");
    }

    #[test]
    fn status_snapshot_journal_entry_routes_to_status_bridge_and_emits() {
        use t_hub_protocol::{JournalSource, EventJournalEntry};
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        let status = Arc::new(crate::claude::StatusBridge::new());
        bridge.set_emitter(Arc::new(rec.clone()));
        bridge.set_status_bridge(Arc::clone(&status));
        rec.events.lock().clear();

        // A StatusSnapshot journal entry whose payload carries the raw statusline.
        let entry = EventJournalEntry {
            seq: 1,
            timestamp_ms: 100,
            source: JournalSource::Status,
            entity_id: Some("o1".to_string()),
            event_type: JournalEventType::StatusSnapshot,
            payload: serde_json::json!({
                "session_id": "o1",
                "status": { "context_window": { "used_percentage": 55.0 } }
            }),
            result: None,
        };
        bridge.consume_journal_entry(&entry);

        // status://snapshot emitted with the derived context %.
        let snap_ev = rec
            .events
            .lock()
            .iter()
            .find(|(c, _)| c == super::EVT_STATUS_SNAPSHOT)
            .cloned()
            .expect("status://snapshot must be emitted");
        assert_eq!(snap_ev.1["sessionId"], "o1");
        assert_eq!(snap_ev.1["contextUsedPct"], 55.0);
        // And the status bridge holds it (queryable via the command).
        assert_eq!(status.get("o1").unwrap().context_used_pct, Some(55.0));
    }

    // -----------------------------------------------------------------------
    // GOAL NAMES: derive_session_title + agent://title emit
    // -----------------------------------------------------------------------

    #[test]
    fn user_prompt_submit_titles_from_first_prompt_line() {
        let p = serde_json::json!({
            "session_id": "s1",
            "prompt": "Fix the WSL hooks install path\n\nlots of detail follows"
        });
        let t = super::derive_session_title(JournalEventType::UserPromptSubmit, &p).unwrap();
        assert_eq!(t, "Fix the WSL hooks install path");
    }

    #[test]
    fn user_prompt_submit_caps_long_prompts() {
        let long = "a ".repeat(80);
        let p = serde_json::json!({ "session_id": "s1", "prompt": long });
        let t = super::derive_session_title(JournalEventType::UserPromptSubmit, &p).unwrap();
        assert!(t.chars().count() <= super::TITLE_MAX_CHARS, "got {} chars", t.chars().count());
        assert!(t.ends_with('…'));
    }

    #[test]
    fn session_start_falls_back_to_cwd_basename() {
        let p = serde_json::json!({ "session_id": "s1", "cwd": "/home/natkins/n8builds/tools/" });
        let t = super::derive_session_title(JournalEventType::SessionStart, &p).unwrap();
        assert_eq!(t, "tools");
    }

    #[test]
    fn no_title_for_unrelated_events_or_empty_signal() {
        // Stop carries no title signal.
        assert!(super::derive_session_title(
            JournalEventType::Stop,
            &serde_json::json!({ "session_id": "s1" })
        )
        .is_none());
        // Empty prompt -> no title.
        assert!(super::derive_session_title(
            JournalEventType::UserPromptSubmit,
            &serde_json::json!({ "session_id": "s1", "prompt": "   \n  " })
        )
        .is_none());
        // SessionStart with no cwd -> no title.
        assert!(super::derive_session_title(
            JournalEventType::SessionStart,
            &serde_json::json!({ "session_id": "s1" })
        )
        .is_none());
    }

    #[test]
    fn consume_emits_agent_title_with_cwd_for_correlation() {
        let bridge = AgentBridge::new();
        let rec = RecordingEmitter::default();
        bridge.set_emitter(Arc::new(rec.clone()));

        let entry = EventJournalEntry {
            seq: 1,
            timestamp_ms: 1,
            source: JournalSource::Hook,
            entity_id: Some("sess-7".into()),
            event_type: JournalEventType::UserPromptSubmit,
            payload: serde_json::json!({
                "session_id": "sess-7",
                "cwd": "/home/u/proj",
                "prompt": "Wire the hook titles\nmore"
            }),
            result: None,
        };
        bridge.consume_journal_entry(&entry);

        let title_ev = rec
            .events
            .lock()
            .iter()
            .find(|(c, _)| c == super::EVT_TITLE)
            .cloned()
            .expect("agent://title must be emitted");
        assert_eq!(title_ev.1["sessionId"], "sess-7");
        assert_eq!(title_ev.1["cwd"], "/home/u/proj");
        assert_eq!(title_ev.1["title"], "Wire the hook titles");
    }
}
