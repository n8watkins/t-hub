// --- 0.1 nucleus (unchanged) ---
mod commands;
mod events;
mod pty;
mod tmux;

// --- 0.5 additions ---
mod agent; // core-side agent bridge (Workstream A, core half)
mod claude; // Claude adapter: hooks + status bridge (Workstream B)
mod commands_05; // the 0.5 Tauri command surface (agent/supervision/status)
pub mod control; // MCP control listener: dispatches `{command,args}` over loopback (PRD §9.6). `pub` so the end-to-end integration test can stand up a real listener.
mod db; // durable SQLite copy of the workspace layout (#sqlite phase 1)
mod devserver; // feat/dev-runner: managed `npm run dev` per-project runner (Dev tab)
mod diag; // runtime diagnostics sink: diag_log/diag_clear -> fixed file (feat/diag)
mod files; // file index + fuzzy search + shallow tree + capped reader (PRD §6.8/§9.7)
// --- feat/git-panel ---
mod git; // git awareness for the Files panel: branch/worktree info + commit
// ----------------------
mod model; // data-model structs (PRD §8)
// --- feat/projects-sidebar (Agent A) ---------------------------------------
mod recent; // recent recallable Claude sessions for the sidebar "Recent" list
// ---------------------------------------------------------------------------
mod supervision; // orchestrator->subagent tree + status (Workstream C)
mod theme; // live theming contract: get_theme/set_theme + theme://changed (MCP-facing)
mod tray; // system-tray icon + close-to-tray (hide instead of quit) (#17)
mod usage; // Claude plan usage via `claude -p /usage` (sidebar Usage strip)
mod win_snap; // Windows 11 Snap Layouts + native edge-resize on the frameless window (no-op on unix)

use agent::AgentBridge;
use claude::StatusBridge;
use commands::TerminalManager;

// --- Test/proof seams (used by `tests/mcp_e2e.rs`) -------------------------
// The end-to-end MCP proof seeds a real `Supervisor` + `StatusBridge`, starts a
// real control listener, and drives the real `termhub-mcp` binary against it.
// These thin constructors expose just enough of the otherwise-internal modules
// for that integration test without widening the general public surface.

/// Re-export the supervision reducer type for the e2e proof.
#[doc(hidden)]
pub use supervision::Supervisor;

/// Build a fresh, empty supervision reducer (for the e2e proof to seed).
#[doc(hidden)]
pub fn supervision_for_test() -> Supervisor {
    Supervisor::new()
}

/// Build a fresh status bridge (for the e2e proof to ingest a snapshot into).
#[doc(hidden)]
pub fn status_bridge_for_test() -> StatusBridge {
    StatusBridge::new()
}

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
            // Log the resolved launch argv up front so a missing/unresolvable
            // agent binary is diagnosable from the core's stderr. On Windows
            // this is the `wsl.exe -d <distro> --cd ~ -- bash -lc "exec
            // termhub-agent --stdio"` login-shell form (or the verbatim
            // TERMHUB_AGENT_BIN override); on unix it's the direct spawn.
            let argv = agent::launch_argv(&distro);
            eprintln!(
                "termhub: connecting agent bridge (distro={distro:?}) via {argv:?}"
            );
            if let Err(e) = bridge.connect(&distro) {
                // A failure here never aborts startup: the bridge degrades to a
                // Failed/Disconnected state the sidebar renders. The most common
                // cause is the agent binary not being on the login-shell PATH
                // (install it to ~/.local/bin) — surface that hint.
                eprintln!(
                    "termhub: agent bridge connect failed: {e} \
                     (is `termhub-agent` installed to ~/.local/bin inside the \
                     distro, or TERMHUB_AGENT_BIN set?)"
                );
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
// --- MCP control://apply forwarder (feat/mcp2) -----------------------------
// The Organization-tier MCP tools (`focus_session`, `move_tile`, `rename_tab`)
// apply a pure UI mutation. The control listener accepts + audits them, then
// forwards `{command, args}` to the frontend via this sink, which emits a Tauri
// `control://apply` event; `src/ipc/controlBridge.ts` subscribes and dispatches
// it into the workspace store. Kept here (a clearly separate block) so the sink
// stays out of `control.rs`'s tauri-free surface.
const CONTROL_APPLY_EVENT: &str = "control://apply";

struct AppHandleApplySink {
    app: tauri::AppHandle,
}

impl control::ApplySink for AppHandleApplySink {
    fn apply(&self, command: &str, args: &serde_json::Value) -> Result<(), String> {
        use tauri::Emitter;
        self.app
            .emit(
                CONTROL_APPLY_EVENT,
                serde_json::json!({ "command": command, "args": args }),
            )
            .map_err(|e| e.to_string())
    }
}

fn start_control_listener(state: &AppState, app: &tauri::AppHandle) {
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

    // Forward Organization-tier UI mutations to the frontend via control://apply.
    let apply_sink: std::sync::Arc<dyn control::ApplySink> =
        std::sync::Arc::new(AppHandleApplySink { app: app.clone() });

    let ctx = control::ControlContext::new(state.status.clone(), supervisor, token)
        .with_apply_sink(apply_sink);
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
        // Shell plugin: lets the frontend open URLs/paths in the OS default
        // browser (web-preview "Open externally"). Without it the JS open() is a
        // no-op. Paired with the `shell:allow-open` capability.
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        // --- Auto-updater (feat/auto-updater) ------------------------------
        // The updater plugin powers the in-app "Updates" settings section and
        // the on-launch silent install: it reads `latest.json` from the
        // GitHub Releases endpoint (see plugins.updater in tauri.conf.json),
        // verifies the signed NSIS artifact against the configured pubkey, and
        // downloads/installs it. The process plugin's relaunch() restarts the
        // app after an update is applied. Both are gated by the
        // `updater:default` / `process:default` capabilities.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(TerminalManager::default())
        .manage(AppState::default())
        .manage(files::FileIndexState::new())
        // Live theming state, seeded from the persisted theme file if present.
        // get_theme reads this; set_theme updates it, persists, and emits
        // theme://changed (the surface MCP forwards so Claude can retheme).
        .manage(theme::ThemeState::load())
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
            start_control_listener(&state, app.handle());
            // Install the system-tray icon + menu (#17). A tray build failure is
            // logged and does not abort startup; the app remains usable via its
            // window (close-to-tray still works regardless via on_window_event).
            if let Err(e) = tray::build(app.handle()) {
                eprintln!("termhub: failed to build system tray: {e}");
            }
            // Restore Windows 11 Snap Layouts (hover-the-maximize-button flyout)
            // and native edge-resize on the frameless main window by subclassing
            // its HWND to answer WM_NCHITTEST (#snap). On unix this is a no-op. A
            // failure here is logged and never aborts startup; the window stays
            // fully usable, just without the native snap flyout / edge resize.
            if let Some(main) = app.get_webview_window("main") {
                if let Err(e) = win_snap::install(&main) {
                    eprintln!("termhub: failed to install Snap-Layouts hit-test hook: {e}");
                }
            }
            // Force `mouse on` server-wide AND on every already-running session
            // (a session-local `mouse off` left by an older build overrides the
            // global flip, so the wheel still sent arrow keys in old panes). Run
            // off-thread: it spawns one `wsl.exe tmux` per live session, which we
            // never want to block the window's first paint on. Best-effort.
            std::thread::spawn(|| tmux::ensure_mouse_on());
            // --- #sqlite: open the durable workspace DB (app_data_dir/termhub.db,
            // WAL+NORMAL) and manage it so save/load_workspace_snapshot share one
            // handle. A failure resolves to a no-op Db (logged inside), never
            // aborting startup — the frontend keeps its localStorage mirror.
            app.manage(db::init(&app.handle().clone()));
            Ok(())
        })
        // Closing the main window hides it to the tray instead of quitting; only
        // the tray's "Quit" exits the app (#17).
        .on_window_event(tray::on_window_event)
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
            commands_05::tmux_scroll,
            commands_05::tmux_exit_scroll,
            commands_05::supervision_tree,
            commands_05::supervision_session_ids,
            commands_05::session_status,
            commands_05::status_snapshot,
            commands_05::ingest_status,
            commands_05::install_claude_hooks,
            commands_05::uninstall_claude_hooks,
            commands_05::claude_hooks_installed,
            commands_05::claude_hooks_managed,
            // Files: index + search + tree + reader (PRD §6.8/§9.7)
            files::index_project,
            files::search_files,
            files::list_dir,
            files::read_text_file,
            files::write_text_file,
            // --- feat/git-panel ---
            // Git awareness for the Files panel: branch/worktree info + commit.
            git::git_info,
            git::git_commit,
            // ----------------------
            // feat/dev-runner: managed per-project dev server (Dev tab). Self-
            // contained (its own process-global registry; no .manage() needed).
            // Streams output on `devserver://<terminal_id>`.
            devserver::start_dev_server,
            devserver::stop_dev_server,
            // feat/preview: WSL2 preview-reachability helpers. `preview_host`
            // returns the Windows-reachable host to substitute for a WSL
            // `localhost`; `probe_tcp` reports whether a host:port accepts a
            // connection (precise preview errors). See devserver.rs.
            devserver::preview_host,
            devserver::probe_tcp,
            // Theming contract (MCP-facing): read/write the active theme + emit
            // theme://changed.
            theme::get_theme,
            theme::set_theme,
            // #sqlite: durable workspace-layout persistence (mirrors localStorage).
            db::save_workspace_snapshot,
            db::load_workspace_snapshot,
            // #recovery: snapshot-history read commands for the Recovery review UI.
            db::list_snapshots,
            db::get_snapshot,
            // --- feat/projects-sidebar (Agent A) -------------------------------
            // Recent recallable Claude sessions for the sidebar "Recent" list.
            recent::recent_sessions,
            // Claude plan usage (`claude -p /usage`) for the sidebar Usage strip.
            usage::claude_usage,
            // -------------------------------------------------------------------
            // feat/diag: runtime diagnostics sink (mirrors frontend logs to a file
            // the WSL-side orchestrator can read from a RELEASE build).
            diag::diag_log,
            diag::diag_clear,
        ])
        .run(tauri::generate_context!())
        .expect("error while running TermHub");
}
