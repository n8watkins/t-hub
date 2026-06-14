//! The live event sink that fans backend state changes out to the UI.
//!
//! ## Why this exists (the #1 0.5 gap)
//! The frontend subscribes to `agent://journal`, `supervision://tree`,
//! `session://status`, `agent://state`, and `status://snapshot` (see
//! `src/ipc/client05.ts` / `Events05` in `src/ipc/types.ts`), but the Rust side
//! historically never emitted on them: no [`tauri::AppHandle`] was threaded into
//! the agent bridge, so the journal reader consumed entries and dropped the
//! affected session id on the floor (`let _ = seq`). The sidebar pulled one
//! snapshot on mount and went stale.
//!
//! This module closes that gap with a tiny [`EventEmitter`] trait so the
//! [`crate::agent::AgentBridge`] (and the status bridge) can emit without taking
//! a hard dependency on `tauri::AppHandle` — which matters because:
//!   - the bridge is constructed in `AppState::default()`, *before* the Tauri
//!     `AppHandle` exists (the handle is installed later in `setup()` via
//!     [`crate::agent::AgentBridge::set_emitter`]); and
//!   - the bridge's unit tests call `consume_journal_entry` directly with no app
//!     handle at all — emission must degrade to a no-op there.
//!
//! The Tauri-backed implementation is [`TauriEmitter`]; tests can use any
//! [`EventEmitter`] (including a recording fake).

use serde::Serialize;

// ---------------------------------------------------------------------------
// Channel names (single source of truth, mirrored in src/ipc/types.ts Events05)
// ---------------------------------------------------------------------------

/// `agent://journal` — a durable journal entry the core consumed (streamed or
/// replayed). Payload: [`JournalEventPayload`].
pub const EVT_JOURNAL: &str = "agent://journal";
/// `supervision://tree` — a supervision tree snapshot changed for a session.
/// Payload: [`crate::model::SupervisionTree`].
pub const EVT_SUPERVISION: &str = "supervision://tree";
/// `session://status` — a session's FR-012 status changed. Payload:
/// [`SessionStatusPayload`].
pub const EVT_SESSION_STATUS: &str = "session://status";
/// `agent://state` — the core↔agent connection state changed. Payload:
/// [`crate::commands_05::AgentStateInfo`].
pub const EVT_AGENT_STATE: &str = "agent://state";
/// `status://snapshot` — a new statusline snapshot was ingested. Payload:
/// [`crate::claude::StatusSnapshot`].
pub const EVT_STATUS_SNAPSHOT: &str = "status://snapshot";

// ---------------------------------------------------------------------------
// Payload shapes (mirrored in src/ipc/protocol.ts)
// ---------------------------------------------------------------------------

/// Payload of the `agent://journal` event. Wraps the entry in an object so the
/// shape matches `JournalEvent` in `src/ipc/protocol.ts` (`{ entry }`) and can
/// grow without a breaking change.
///
/// NOTE: the inner `EventJournalEntry` is the `termhub-protocol` type, which the
/// core forwards to the UI verbatim — so it serializes with the protocol crate's
/// default **snake_case** keys (matching `src/ipc/protocol.ts`), *not* the
/// camelCase used by `model.rs`.
#[derive(Debug, Clone, Serialize)]
pub struct JournalEventPayload<'a> {
    pub entry: &'a termhub_protocol::EventJournalEntry,
}

/// Payload of the `session://status` event (mirrors `SessionStatusEvent` in
/// `src/ipc/protocol.ts`). camelCase to match the TS interface.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatusPayload {
    pub session_id: String,
    pub status: crate::model::SessionStatus,
}

// ---------------------------------------------------------------------------
// EventEmitter trait + Tauri implementation
// ---------------------------------------------------------------------------

/// A minimal sink the backend uses to push events to the UI. Implemented by
/// [`TauriEmitter`] in production; any type works in tests.
///
/// It is `Send + Sync` because the agent reader thread (which calls it as it
/// consumes journal entries) is a separate OS thread from the one that installs
/// the emitter.
pub trait EventEmitter: Send + Sync {
    /// Emit `payload` (serialized to JSON) on `channel`. Best-effort: a transport
    /// error is logged, never propagated — a dropped UI event must not break the
    /// journal-consumption path.
    fn emit_json(&self, channel: &str, payload: &serde_json::Value);
}

impl dyn EventEmitter {
    /// Convenience: serialize any `Serialize` value and emit it. Centralizes the
    /// "serialization failed" handling so call sites stay terse.
    pub fn emit<T: Serialize>(&self, channel: &str, payload: &T) {
        match serde_json::to_value(payload) {
            Ok(v) => self.emit_json(channel, &v),
            Err(e) => {
                eprintln!("agent-emit: failed to serialize payload for {channel}: {e}");
            }
        }
    }
}

/// The production [`EventEmitter`]: a thin wrapper over a Tauri [`AppHandle`]
/// that emits app-global events (delivered to every window's listeners).
pub struct TauriEmitter {
    app: tauri::AppHandle,
}

impl TauriEmitter {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

impl EventEmitter for TauriEmitter {
    fn emit_json(&self, channel: &str, payload: &serde_json::Value) {
        use tauri::Emitter;
        if let Err(e) = self.app.emit(channel, payload.clone()) {
            eprintln!("agent-emit: failed to emit {channel}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    /// A recording fake emitter for tests: captures (channel, payload) pairs.
    #[derive(Default, Clone)]
    pub struct RecordingEmitter {
        pub events: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    }

    impl EventEmitter for RecordingEmitter {
        fn emit_json(&self, channel: &str, payload: &serde_json::Value) {
            self.events
                .lock()
                .push((channel.to_string(), payload.clone()));
        }
    }

    #[test]
    fn emit_serializes_and_records() {
        let rec = RecordingEmitter::default();
        let dynref: &dyn EventEmitter = &rec;
        dynref.emit(
            EVT_SESSION_STATUS,
            &SessionStatusPayload {
                session_id: "s1".into(),
                status: crate::model::SessionStatus::Working,
            },
        );
        let events = rec.events.lock();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, EVT_SESSION_STATUS);
        assert_eq!(events[0].1["sessionId"], "s1");
        assert_eq!(events[0].1["status"], "working");
    }
}
