// --- 0.1 nucleus (unchanged) ---
mod commands;
mod events;
mod pty;
mod tmux;

// --- 0.5 additions ---
mod agent; // core-side agent bridge (Workstream A, core half)
mod claude; // Claude adapter: hooks + status bridge (Workstream B)
mod commands_05; // the 0.5 Tauri command surface (agent/supervision/status)
mod model; // data-model structs (PRD §8)
mod supervision; // orchestrator->subagent tree + status (Workstream C)

use agent::AgentBridge;
use claude::StatusBridge;
use commands::TerminalManager;

/// App-wide 0.5 state, managed alongside the 0.1 [`TerminalManager`]. Grouped so
/// the command surface can pull exactly what it needs from Tauri-managed state.
#[derive(Clone)]
pub struct AppState {
    /// The core↔agent connection + supervision reducer.
    pub agent: AgentBridge,
    /// The Claude statusline status bridge (latest snapshot per session id).
    pub status: std::sync::Arc<StatusBridge>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            agent: AgentBridge::new(),
            status: std::sync::Arc::new(StatusBridge::new()),
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        .manage(TerminalManager::default())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            // 0.1 nucleus
            commands::spawn_terminal,
            commands::attach_terminal,
            commands::write_terminal,
            commands::resize_terminal,
            commands::close_terminal,
            commands::kill_terminal,
            commands::list_terminals,
            // 0.5 surface
            commands_05::agent_state,
            commands_05::host_metrics,
            commands_05::git_branch,
            commands_05::supervision_tree,
            commands_05::supervision_session_ids,
            commands_05::session_status,
            commands_05::status_snapshot,
            commands_05::ingest_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running TermHub");
}
