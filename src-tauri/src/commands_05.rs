//! The 0.5 Tauri command surface — the IPC contract for the agent bridge,
//! supervision tree, and status bridge. Mirrored in `src/ipc/types.ts`
//! (`Commands05` / event channels) and the payload types in `src/ipc/model.ts`.
//!
//! These are thin glue over [`crate::agent`], [`crate::supervision`], and
//! [`crate::claude`]. The supervision + status commands work today (their
//! reducers are implemented); the agent-RPC commands return the bridge's
//! "not connected" error until SUBAGENT(agent-bridge) lands the transport, at
//! which point they light up with no signature change.
//!
//! Command name ↔ identifier mapping (keep in lockstep with `Commands05`):
//!   - `agent_state`            → connection state + journal cursor
//!   - `host_metrics`           → WSL host metrics snapshot (RPC)
//!   - `git_branch`             → derive branch for a cwd (RPC)
//!   - `supervision_tree`       → read-only tree for one session
//!   - `supervision_session_ids`→ all supervised session ids
//!   - `session_status`         → FR-012 status for one session
//!   - `status_snapshot`        → latest statusline snapshot for a session
//!   - `ingest_status`          → push a raw statusline payload (status bridge)

use serde::Serialize;

use crate::claude::StatusSnapshot;
use crate::model::{SessionStatus, SupervisionTree};
use crate::AppState;
use termhub_protocol::HostMetrics;

/// Current core↔agent connection state for the UI health area.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStateInfo {
    pub connection: crate::agent::ConnectionState,
    pub journal_cursor: u64,
}

#[tauri::command]
pub async fn agent_state(state: tauri::State<'_, AppState>) -> Result<AgentStateInfo, String> {
    Ok(AgentStateInfo {
        connection: state.agent.state(),
        journal_cursor: state.agent.journal_cursor(),
    })
}

#[tauri::command]
pub async fn host_metrics(state: tauri::State<'_, AppState>) -> Result<HostMetrics, String> {
    state.agent.metrics()
}

#[tauri::command]
pub async fn git_branch(
    state: tauri::State<'_, AppState>,
    cwd: String,
) -> Result<Option<String>, String> {
    state.agent.git_branch(&cwd)
}

#[tauri::command]
pub async fn supervision_tree(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<Option<SupervisionTree>, String> {
    Ok(state.agent.with_supervisor(|s| s.tree(&session_id)))
}

#[tauri::command]
pub async fn supervision_session_ids(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    Ok(state.agent.with_supervisor(|s| s.session_ids()))
}

#[tauri::command]
pub async fn session_status(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<SessionStatus, String> {
    Ok(state.agent.with_supervisor(|s| s.status(&session_id)))
}

#[tauri::command]
pub async fn status_snapshot(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<Option<StatusSnapshot>, String> {
    Ok(state.status.get(&session_id))
}

/// Ingest a raw statusline JSON payload for a session (the status bridge entry
/// point invokable from the frontend or a future native statusline hook).
#[tauri::command]
pub async fn ingest_status(
    state: tauri::State<'_, AppState>,
    session_id: String,
    payload: serde_json::Value,
) -> Result<StatusSnapshot, String> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Ok(state.status.ingest(&session_id, &payload, now_ms))
}
