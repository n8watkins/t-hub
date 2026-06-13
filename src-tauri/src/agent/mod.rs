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

pub use connection::ConnectionState;

use std::sync::Arc;

use parking_lot::Mutex;
use termhub_protocol::{AgentRequest, AgentResponse, EventJournalEntry, HostMetrics, WorktreeInfo};

use crate::supervision::Supervisor;

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
}

impl Default for AgentBridge {
    fn default() -> Self {
        Self {
            inner: Arc::new(BridgeInner {
                supervisor: Mutex::new(Supervisor::new()),
                state: Mutex::new(ConnectionState::Disconnected),
                journal_cursor: Mutex::new(0),
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

    /// Launch the agent and complete the handshake.
    ///
    /// SUBAGENT(agent-bridge): spawn [`launch_argv`], wire reader/writer threads,
    /// send `Hello`, await `Ready`, then (if the agent's `journal_head_seq` >
    /// our cursor) issue a `ReplayJournal`. Set [`ConnectionState`] accordingly.
    pub fn connect(&self, _distro: &str) -> Result<(), String> {
        Err("agent bridge transport not yet implemented (SUBAGENT(agent-bridge))".to_string())
    }

    /// Send a request and await its correlated response (blocking with a
    /// timeout). The priority hint lets the scheduler interleave it appropriately.
    ///
    /// SUBAGENT(agent-bridge): implement correlation by [`RequestId`].
    pub fn request(&self, _req: AgentRequest) -> Result<AgentResponse, String> {
        Err("agent bridge not connected (SUBAGENT(agent-bridge))".to_string())
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

    /// Consume one journal entry: advance the cursor, feed supervision, and
    /// return the affected session id (for the caller to emit a tree snapshot).
    ///
    /// This is the core's single ingestion point for the spine and is fully
    /// implemented so the supervision path works the moment the transport
    /// delivers entries. SUBAGENT(agent-bridge) calls this from the reader.
    pub fn consume_journal_entry(&self, entry: &EventJournalEntry) -> Option<String> {
        {
            let mut cursor = self.inner.journal_cursor.lock();
            if entry.seq > *cursor {
                *cursor = entry.seq;
            }
        }
        // Pull the subagent base fields out of the payload (hooks put `agent_id`
        // / `agent_type` in stdin inside subagents — REVIEW base fields).
        let session_id = entry
            .entity_id
            .as_deref()
            .or_else(|| entry.payload.get("session_id").and_then(|v| v.as_str()));
        let agent_id = entry.payload.get("agent_id").and_then(|v| v.as_str());
        let agent_type = entry.payload.get("agent_type").and_then(|v| v.as_str());

        self.with_supervisor(|s| {
            s.ingest(session_id, agent_id, agent_type, entry.event_type, entry.timestamp_ms)
        })
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
}
