// --- 0.1 nucleus (unchanged) ---
mod commands;
mod events;
mod pty;
mod tmux;

// --- 0.5 additions ---
mod agent; // core-side agent bridge (Workstream A, core half)
mod claude; // Claude adapter: hooks + status bridge (Workstream B)
mod commands_05; // the 0.5 Tauri command surface (agent/supervision/status)
mod control; // MCP control listener: dispatches `{command,args}` over loopback (PRD §9.6)
mod files; // file index + fuzzy search + shallow tree + capped reader (PRD §6.8/§9.7)
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

/// Build the [`control::ControlContext`] from app state and start the loopback
/// control listener (PRD §9.6 MCP). The context shares the status bridge and a
/// supervisor-visitor closure (so `control` reads supervision snapshots without
/// reaching into `AgentBridge` internals). The per-launch token is a fresh UUID;
/// it is written to the handshake file alongside the bound port so `termhub-mcp`
/// can discover + authenticate to the channel. An explicit `TERMHUB_CONTROL_TOKEN`
/// overrides the generated token (useful for test harnesses).
fn start_control_listener(state: &AppState) {
    let token = std::env::var("TERMHUB_CONTROL_TOKEN")
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());

    // A visitor closure that locks the bridge's Supervisor and runs `f`. Capturing
    // a clone of the bridge keeps `control` decoupled from `agent` internals.
    let bridge = state.agent.clone();
    let supervisor: std::sync::Arc<
        dyn Fn(&mut dyn FnMut(&supervision::Supervisor)) + Send + Sync,
    > = std::sync::Arc::new(move |f: &mut dyn FnMut(&supervision::Supervisor)| {
        bridge.with_supervisor(|s| f(s));
    });

    let ctx = control::ControlContext::new(state.status.clone(), supervisor, token);
    match control::start(ctx) {
        Ok(h) => eprintln!(
            "termhub: control listener on {} (handshake: {})",
            h.addr,
            control::handshake_path().display()
        ),
        Err(e) => eprintln!("termhub: control listener failed to start: {e}"),
    }
}

pub fn run() {
    tauri::Builder::default()
        .manage(TerminalManager::default())
        .manage(AppState::default())
        .manage(files::FileIndexState::new())
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
            // Start the MCP control listener so `termhub-mcp` can forward
            // `tools/call` over the local control channel (PRD §9.6). A bind
            // failure is logged and does not abort startup (the channel is
            // optional, like the agent bridge).
            start_control_listener(&state);
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
            // Files: index + search + tree + reader (PRD §6.8/§9.7)
            files::index_project,
            files::search_files,
            files::list_dir,
            files::read_text_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running TermHub");
}
