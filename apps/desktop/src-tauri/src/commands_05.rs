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
pub async fn git_branch(
    state: tauri::State<'_, AppState>,
    cwd: String,
) -> Result<Option<String>, String> {
    state.agent.git_branch(&cwd)
}

/// Scroll a tile's tmux scrollback by a page via copy-mode. `session` is the tmux
/// session name (`th_<terminalId>`); `down` pages toward the live prompt (and
/// auto-exits copy-mode at the bottom). The only way to scroll history when an
/// alt-screen app (claude/vim) owns the pane.
#[tauri::command]
pub async fn tmux_scroll(session: String, down: bool) -> Result<(), String> {
    crate::tmux::scroll_history(&session, down).map_err(|e| e.to_string())
}

/// Exit tmux copy-mode for a tile (back to the live prompt). Called the instant
/// the user types after scrolling, so paging up reads as a peek, not a mode.
#[tauri::command]
pub async fn tmux_exit_scroll(session: String) -> Result<(), String> {
    crate::tmux::exit_copy_mode(&session).map_err(|e| e.to_string())
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
/// point invokable from the frontend or a future native statusline hook). Emits
/// `status://snapshot` live so the UI's usage display updates without a poll.
#[tauri::command]
pub async fn ingest_status(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    session_id: String,
    payload: serde_json::Value,
) -> Result<StatusSnapshot, String> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let snap = state.status.ingest(&session_id, &payload, now_ms);
    // Live emit: the same channel the journal `StatusSnapshot` path uses, so a
    // statusline pushed directly from the UI/native hook surfaces immediately.
    use tauri::Emitter;
    let _ = app.emit(crate::agent::emit::EVT_STATUS_SNAPSHOT, snap.clone());
    Ok(snap)
}

// --- Claude hook installer (Workstream B; consent-gated, non-destructive) ---

/// Install T-Hub's hook handlers into `~/.claude/settings.json`. `consent`
/// MUST be true (collected explicitly in the UI) or this refuses.
#[tauri::command]
pub async fn install_claude_hooks(
    agent_bin: String,
    consent: bool,
    events: Vec<String>,
) -> Result<crate::claude::InstallReport, String> {
    // `events` is the user's selection; an empty vec means "all" (handled in the
    // installer). The managed set is reconciled to exactly this selection.
    crate::claude::install::install_hooks_events(&agent_bin, consent, &events)
        .map_err(|e| e.to_string())
}

/// Remove T-Hub's hook handlers (clean uninstall), leaving user hooks intact.
#[tauri::command]
pub async fn uninstall_claude_hooks() -> Result<crate::claude::InstallReport, String> {
    crate::claude::install::uninstall_hooks().map_err(|e| e.to_string())
}

/// Whether T-Hub hooks are currently installed (for the UI install state).
#[tauri::command]
pub async fn claude_hooks_installed() -> Result<bool, String> {
    crate::claude::install::hooks_installed().map_err(|e| e.to_string())
}

/// Which hook events T-Hub currently manages (so the UI can pre-check them).
#[tauri::command]
pub async fn claude_hooks_managed() -> Result<Vec<String>, String> {
    crate::claude::install::managed_event_names().map_err(|e| e.to_string())
}
