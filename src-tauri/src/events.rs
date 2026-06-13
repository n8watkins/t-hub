//! Event payload structs emitted from the backend to the frontend.
//!
//! These mirror the channels and payload shapes declared in `src/ipc/types.ts`
//! and MUST stay in lockstep with it. All payloads use `rename_all =
//! "camelCase"` to match the TypeScript interfaces.
//!
//! Channels:
//!   - [`OUTPUT`] `terminal://output` → [`OutputEvent`]
//!   - [`STATE`]  `terminal://state`  → [`StateEvent`]
//!   - [`EXIT`]   `terminal://exit`   → [`ExitEvent`]

use serde::Serialize;

use crate::commands::TerminalState;

/// Channel for streamed PTY output.
pub const OUTPUT: &str = "terminal://output";
/// Channel for terminal lifecycle/state transitions.
pub const STATE: &str = "terminal://state";
/// Channel for terminal process exit.
pub const EXIT: &str = "terminal://exit";

/// `terminal://output` — a chunk of raw PTY bytes, base64-encoded so it is
/// binary-safe across UTF-8 boundaries.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputEvent {
    pub id: String,
    /// base64-encoded raw PTY bytes.
    pub base64: String,
}

/// `terminal://state` — a terminal transitioned to a new lifecycle state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateEvent {
    pub id: String,
    pub state: TerminalState,
}

/// `terminal://exit` — the terminal's process exited; `code` is the OS exit
/// code when known (`None` if it was signalled or the code is unavailable).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitEvent {
    pub id: String,
    pub code: Option<i32>,
}
