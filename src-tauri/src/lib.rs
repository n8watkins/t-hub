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

/// The default WSL distro to launch the agent in. Overridable via the
/// `TERMHUB_DISTRO` env var; ignored on unix (the agent is launched directly).
fn default_distro() -> String {
    std::env::var("TERMHUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string())
}

/// Connect the agent bridge on startup, off the main thread so app launch is
/// never blocked on the WSL hop / handshake. A connect failure is logged (the
/// UI shows the connection state); it does not abort startup. The bridge owns
/// reconnect behavior internally.
fn spawn_agent_connect(state: &AppState) {
    let bridge = state.agent.clone();
    let distro = default_distro();
    std::thread::Builder::new()
        .name("termhub-agent-connect".into())
        .spawn(move || {
            if let Err(e) = bridge.connect(&distro) {
                eprintln!("termhub: agent bridge connect failed: {e}");
            }
        })
        .ok();
}

pub fn run() {
    tauri::Builder::default()
        .manage(TerminalManager::default())
        .manage(AppState::default())
        .setup(|app| {
            // Wire the live UI event sink now that the AppHandle exists (the
            // bridge + status bridge were built earlier in AppState::default(),
            // before any AppHandle). This closes the #1 0.5 gap: the frontend
            // subscribes to agent://journal / supervision://tree / session://status
            // / agent://state / status://snapshot, and from here on the backend
            // actually emits on them.
            use tauri::Manager;
            let state = app.state::<AppState>().inner().clone();
            let emitter = std::sync::Arc::new(agent::TauriEmitter::new(app.handle().clone()));
            state.agent.set_emitter(emitter);
            state.agent.set_status_bridge(state.status.clone());
            // Kick off the agent connection in the background once state exists.
            spawn_agent_connect(&state);
            Ok(())
        })
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
            commands_05::install_claude_hooks,
            commands_05::uninstall_claude_hooks,
            commands_05::claude_hooks_installed,
        ])
        .run(tauri::generate_context!())
        .expect("error while running TermHub");
}
