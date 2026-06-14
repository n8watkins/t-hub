//! Core-side **agent bridge** (PLAN.md Workstream A, core half).
//!
//! Owns the long-lived connection to the WSL-side `termhub-agent`:
//!   - launches `wsl.exe -d <distro> -- termhub-agent --stdio` on Windows, or
//!     `termhub-agent --stdio` directly on a unix dev box ([`launch_argv`]);
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
//! scheduler that exploits [`termhub_protocol::Channel`]/`Priority`, reconnect +
//! replay) are implemented by SUBAGENT(agent-bridge). The stubs compile and
//! return a clear "not yet connected" error so the command surface is wired and
//! typecheckable today.
//!
//! Boundary: SUBAGENT(agent-bridge) owns this directory (`agent/`). It must not
//! change `termhub-protocol`, `model.rs`, or `supervision.rs` (it *calls* them).

mod connection;
pub mod emit;

pub use connection::ConnectionState;
pub use emit::{EventEmitter, TauriEmitter};

use std::sync::{mpsc, Arc};

use parking_lot::Mutex;
use termhub_protocol::{
    AgentRequest, AgentResponse, Channel, CoreFrame, CoreToAgent, EventJournalEntry, Hello,
    HostMetrics, Priority, WorktreeInfo, PROTOCOL_VERSION,
};

use crate::supervision::Supervisor;
use connection::{spawn_child, spawn_reader, write_frame, TransportHandles};
use emit::{
    JournalEventPayload, SessionStatusPayload, EVT_AGENT_STATE, EVT_JOURNAL, EVT_SESSION_STATUS,
    EVT_STATUS_SNAPSHOT, EVT_SUPERVISION,
};

/// How the core reaches the agent on this platform.
///
/// On Windows the agent runs inside the distro via `wsl.exe`; on unix (dev) it
/// is spawned directly so the whole spine is exercisable in this shell.
///
/// Called by SUBAGENT(agent-bridge)'s transport when it spawns the child; not
/// yet referenced elsewhere in the crate.
#[allow(dead_code)]
pub fn launch_argv(distro: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            "wsl.exe".to_string(),
            "-d".to_string(),
            distro.to_string(),
            "--".to_string(),
            "termhub-agent".to_string(),
            "--stdio".to_string(),
        ]
    }
    #[cfg(unix)]
    {
        let _ = distro; // distro is irrelevant when launching directly.
        vec!["termhub-agent".to_string(), "--stdio".to_string()]
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
    /// [`termhub_protocol::AgentToCore`] frames. Sends `Hello`, waits for
    /// `Ready`, and if the agent's `journal_head_seq` is ahead of our cursor,
    /// sends `ReplayJournal` and waits for `ReplayComplete` before setting the
    /// state to `Live`.
    ///
    /// The `TERMHUB_AGENT_BIN` env var overrides argv[0] for tests / dev
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
                    core_version: "termhub 0.5.0".to_string(),
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

    /// Send a request and await its correlated response (blocking, 10 s timeout).
    ///
    /// Allocates the next [`termhub_protocol::RequestId`] from an atomic
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
        if matches!(entry.event_type, termhub_protocol::JournalEventType::StatusSnapshot) {
            self.ingest_status_from_journal(entry);
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use termhub_protocol::{JournalEventType, JournalSource};

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
        let argv = launch_argv("Ubuntu-24.04");
        #[cfg(unix)]
        assert_eq!(argv, vec!["termhub-agent", "--stdio"]);
        #[cfg(windows)]
        assert_eq!(
            argv,
            vec!["wsl.exe", "-d", "Ubuntu-24.04", "--", "termhub-agent", "--stdio"]
        );
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
    fn status_snapshot_journal_entry_routes_to_status_bridge_and_emits() {
        use termhub_protocol::{JournalSource, EventJournalEntry};
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
}
