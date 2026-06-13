//! Agent connection state machine + transport internals.
//!
//! [`ConnectionState`] is contract (the UI health area renders it); the child
//! process + reader/writer threads + priority scheduler + reconnect/replay are
//! SUBAGENT(agent-bridge)'s to implement here. Keep [`ConnectionState`] stable.

use serde::{Deserialize, Serialize};

/// The lifecycle of the core↔agent connection (PLAN.md §A; surfaced in §H
/// health). Reconnect is a first-class state because a `wsl --shutdown` tears
/// the agent down and the core must re-handshake + replay the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionState {
    /// No agent process / pipe.
    Disconnected,
    /// Child spawned; `Hello` sent, awaiting `Ready`.
    Handshaking,
    /// Handshake done; replaying the journal up to the agent's head seq.
    Replaying,
    /// Live and serving requests + streaming the spine.
    Live,
    /// Lost the connection; backing off before a reconnect attempt.
    Reconnecting,
    /// Permanently failed (e.g. agent binary missing); needs user action.
    Failed,
}

impl Default for ConnectionState {
    fn default() -> Self {
        ConnectionState::Disconnected
    }
}

// SUBAGENT(agent-bridge): the transport implementation goes here — spawn the
// child from `super::launch_argv`, own stdin(writer)/stdout(reader) threads, a
// bounded priority queue keyed by termhub_protocol::{Channel, Priority} for the
// outbound side (so a slow CapturePane can't delay a Metrics/Ping), a
// RequestId->oneshot correlation map, and the reconnect+ReplayJournal loop that
// calls AgentBridge::consume_journal_entry for each replayed/streamed entry.
