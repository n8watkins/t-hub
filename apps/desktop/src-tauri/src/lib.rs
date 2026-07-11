// --- 0.1 nucleus (unchanged) ---
mod bounded_exec; // shared bounded-subprocess exec (drain+kill+reap on timeout); the single choke point tmux.rs + git.rs route every child through so no control handler parks forever
mod commands;
mod events;
mod pty;
mod tmux;

// --- 0.5 additions ---
mod agent; // core-side agent bridge (Workstream A, core half)
mod audit; // control-socket audit log with teeth (socket-gate Phase 1, hash-chained JSONL)
mod claude; // Claude adapter: hooks + status bridge (Workstream B)
mod governor; // fleet spawn budget + rate limits for process-changing control commands (socket-gate Phase 1)
mod commands_05; // the 0.5 Tauri command surface (agent/supervision/status)
pub mod control; // MCP control listener: dispatches `{command,args}` over loopback (PRD §9.6). `pub` so the end-to-end integration test can stand up a real listener.
mod control_client; // server-split M1: client-side socket transport (control_request command + event forwarder)
mod db; // durable SQLite copy of the workspace layout (#sqlite phase 1)
mod devserver; // feat/dev-runner: managed `npm run dev` per-project runner (Dev tab)
mod diag; // runtime diagnostics sink: diag_log/diag_clear -> fixed file (feat/diag)
mod hangwatch; // host main-thread hang watchdog (sporadic Not-Responding/ghost hunt)
mod dropin; // feat/terminal-input (Lane C): clipboard-image -> temp PNG for image paste
mod files; // file index + fuzzy search + shallow tree + capped reader (PRD §6.8/§9.7)
mod fleet; // orchestrator wake: FleetWatchRegistry + FleetNotifier (server-side push on supervised transitions)
// --- feat/git-panel ---
mod git; // git awareness for the Files panel: branch/worktree info + commit
// ----------------------
mod model; // data-model structs (PRD §8)
mod plane; // comms-plane Phase 1: Single Write Authority primary-writer seam (funnel + attribution for agent/automation input; NOT yet durable/ACL'd/typing-gated)
mod inbox; // comms-plane Phase 2: durable inbox (per-recipient segmented store + seq + at-least-once + receipt state machine); the fleet wake is its first client
mod identity; // comms-plane Phase 2: per-session identity slice (mint/bind/resolve a per-session token for unforgeable-across-sessions attribution)
mod remote_pty; // server-split M2a: client-side remote-PTY transport (terminal tiles over the control socket)
// --- feat/projects-sidebar (Agent A) ---------------------------------------
mod recent; // recent recallable Claude sessions for the sidebar "Recent" list
// ---------------------------------------------------------------------------
mod supervision; // orchestrator->subagent tree + status (Workstream C)
mod theme; // live theming contract: get_theme/set_theme + theme://changed (MCP-facing)
mod tray; // system-tray icon + close-to-tray (hide instead of quit) (#17)
mod usage; // Claude plan usage via `claude -p /usage` (sidebar Usage strip)
mod codex; // Codex plan usage, read from ~/.codex/sessions rollout files (sidebar)
mod voice; // Settings > Voice: voice.json persistence + loopback Piper TTS proxy (no browser Origin)
mod engine_supervisor; // managed Kokoro lifecycle: spawn/health-watch/auto-restart + auto-fallback to Piper (default-off flag; proposal /tmp/flap-probe/LIFECYCLE-PROPOSAL.md)
mod scribe; // Scribe voice-gate: v1 status endpoint via ~/.scribe/control.json, status.json fallback ("is the general dictating?")
mod win_snap; // Windows 11 Snap Layouts + native edge-resize on the frameless window (no-op on unix)

use agent::AgentBridge;
use claude::StatusBridge;
use commands::TerminalManager;

// --- Test/proof seams (used by `tests/mcp_e2e.rs`) -------------------------
// The end-to-end MCP proof seeds a real `Supervisor` + `StatusBridge`, starts a
// real control listener, and drives the real `t-hub-mcp` binary against it.
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
/// `T_HUB_DISTRO` env var; ignored on unix (the agent is launched directly).
fn default_distro() -> String {
    std::env::var("T_HUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string())
}

/// Connect the agent bridge on startup, off the main thread so app launch is
/// never blocked on the WSL hop / handshake. A connect failure is logged (the
/// UI shows the connection state); it does not abort startup. The bridge owns
/// reconnect behavior internally.
fn spawn_agent_connect(state: &AppState) {
    let bridge = state.agent.clone();
    let distro = default_distro();
    std::thread::Builder::new()
        .name("t-hub-agent-connect".into())
        .spawn(move || {
            // Log the resolved launch argv up front so a missing/unresolvable
            // agent binary is diagnosable from the core's stderr. On Windows
            // this is the `wsl.exe -d <distro> --cd ~ -e bash -lc "exec
            // t-hub-agent --stdio"` login-shell form — `-e` execs REAL bash, not
            // the default login shell (zsh); see agent::launch_argv. (Or the
            // verbatim T_HUB_AGENT_BIN override.) On unix it's the direct spawn.
            let argv = agent::launch_argv(&distro);
            eprintln!(
                "t-hub: connecting agent bridge (distro={distro:?}) via {argv:?}"
            );
            if let Err(e) = bridge.connect(&distro) {
                // A failure here never aborts startup: the bridge degrades to a
                // Failed/Disconnected state the sidebar renders. The most common
                // cause is the agent binary not being on the login-shell PATH
                // (install it to ~/.local/bin) — surface that hint.
                eprintln!(
                    "t-hub: agent bridge connect failed: {e} \
                     (is `t-hub-agent` installed to ~/.local/bin inside the \
                     distro, or T_HUB_AGENT_BIN set?)"
                );
            }
        })
        .ok();
}

/// Best-effort, off-the-main-path startup reconcile of Claude hooks. Mirrors
/// [`spawn_agent_connect`]: a detached `std::thread` so app launch is never
/// blocked on the WSL hop / settings.json write, and any error is swallowed +
/// logged so it can NEVER abort startup.
///
/// [`claude::install::reconcile_managed_hooks`] is itself a no-op when the user
/// has no T-Hub-managed hooks (so we never install without prior consent); when
/// they DO (including stale `__termhub_managed__` entries from an upgraded
/// `termhub` build), it migrates them to the current marker + resolved agent
/// path. The outcome is summarized via `diag::diag_log`.
fn spawn_reconcile_managed_hooks() {
    std::thread::Builder::new()
        .name("t-hub-claude-reconcile".into())
        .spawn(|| match claude::install::reconcile_managed_hooks() {
            Ok(()) => diag::diag_log(
                "claude/reconcile: startup reconcile ok (migrated managed hooks if any; \
                 no-op when none were installed)"
                    .to_string(),
            ),
            Err(e) => diag::diag_log(format!(
                "claude/reconcile: startup reconcile failed (non-fatal, launch continues): {e}"
            )),
        })
        .ok();
}

/// Build the [`control::ControlContext`] from app state and start the loopback
/// control listener (PRD §9.6 MCP). The context shares the status bridge and a
/// supervisor-visitor closure (so `control` reads supervision snapshots without
/// reaching into `AgentBridge` internals). The per-launch token is a fresh UUID;
/// it is written to the handshake file alongside the bound port so `t-hub-mcp`
/// can discover + authenticate to the channel. An explicit `T_HUB_CONTROL_TOKEN`
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
        use tauri::{Emitter, Manager};
        // Headless-org: control-spawned sessions are created SERVER-side (the id
        // rides the forward), bypassing `commands::spawn_terminal`'s bookkeeping.
        // Recreate it here so the adopted tile behaves exactly like a "+" spawn:
        // mark the id FRESH (first attach returns empty scrollback → one clean
        // prompt) and emit Live so the tile skips the "starting" placeholder.
        if matches!(command, "spawn_terminal" | "add_worktree_workspace") {
            let id = args
                .get("id")
                .or_else(|| args.get("terminalId"))
                .and_then(|v| v.as_str());
            if let Some(id) = id {
                let remote = self.app.state::<remote_pty::RemotePtyManager>();
                remote.fresh.lock().insert(id.to_string());
                let _ = self.app.emit(
                    events::STATE,
                    &events::StateEvent {
                        id: id.to_string(),
                        state: commands::TerminalState::Live,
                    },
                );
            }
        }
        self.app
            .emit(
                CONTROL_APPLY_EVENT,
                serde_json::json!({ "command": command, "args": args }),
            )
            .map_err(|e| e.to_string())
    }
}

/// The frontend's tab up-sync (TASK C #22, headless-org). `src/ipc/controlBridge.ts`
/// calls this on every layout change, reporting the FULL live tab list, the active
/// tab, and the last registry revision it applied (`baseSeq`). The SERVER registry
/// is authoritative: a stale report (a server-side mutation the UI has not applied
/// yet) is rejected and answered with the authoritative snapshot so the UI
/// converges instead of clobbering the mutation.
#[tauri::command]
fn report_workspace_tabs(
    app: tauri::AppHandle,
    tabs: Vec<control::TabRecord>,
    active_tab_id: Option<String>,
    base_seq: Option<u64>,
    registry: tauri::State<'_, std::sync::Arc<control::TabRegistry>>,
    captains: tauri::State<'_, std::sync::Arc<control::CaptainsRegistry>>,
    fanout: tauri::State<'_, std::sync::Arc<control::EventFanout>>,
) -> serde_json::Value {
    match registry.report(tabs, active_tab_id, base_seq) {
        control::ReportOutcome::Accepted { seq, removed_tab_ids } => {
            // Captain-chat phase 2: the webview's normal tab-close lands here (not
            // the socket close_tab), so a closed tab must be pruned from every
            // captain's workspaceTabIds here too - else it lingers as a phantom
            // controlled-workspace in the persistent captains.json. Forward a
            // captains snapshot (webview + socket clients) when anything changed.
            let mut pruned = false;
            for tab_id in &removed_tab_ids {
                pruned |= captains.prune_tab(tab_id);
            }
            if pruned {
                commands::forward_captains_sync(&app, &captains, &fanout);
            }
            serde_json::json!({ "seq": seq })
        }
        control::ReportOutcome::Stale(snap) => serde_json::json!({
            "stale": true,
            "seq": snap.seq,
            "activeTabId": snap.active_tab_id,
            "tabs": snap.tabs,
        }),
    }
}

fn start_control_listener(
    state: &AppState,
    app: &tauri::AppHandle,
    fanout: std::sync::Arc<control::EventFanout>,
    tab_registry: std::sync::Arc<control::TabRegistry>,
    captains_registry: std::sync::Arc<control::CaptainsRegistry>,
    fleet_watches: std::sync::Arc<fleet::FleetWatchRegistry>,
    identity_store: std::sync::Arc<identity::IdentityStore>,
    inbox: std::sync::Arc<inbox::Inbox>,
) -> Option<control::ControlHandshake> {
    // The control auth token. Server-split M2b: a PERSISTENT key (stable across
    // restarts) so a remote client paired once doesn't have to re-pair every launch.
    // An explicit T_HUB_CONTROL_TOKEN still overrides (test harnesses / the dev
    // isolation). For loopback the MCP/client rediscover it from the handshake file
    // each launch, so persistence is invisible there; it matters only once M2b binds
    // a network interface and a remote client knows the key out-of-band.
    let token = std::env::var("T_HUB_CONTROL_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(control::persistent_key);

    // socket-gate Phase 2: a distinct, persistent READ capability token minted
    // alongside the control token. Published in control.json as `read_token`; grants
    // the Read tier only. An explicit T_HUB_CONTROL_READ_TOKEN overrides (harnesses).
    let read_token = std::env::var("T_HUB_CONTROL_READ_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(control::persistent_read_key);

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

    // Host-metrics RPC (server-split M3, overlay source #5): a closure that fetches
    // the WSL agent's `/proc` snapshot via the bridge. The control `host_metrics`
    // handler prefers this over the daemon's local `/proc` (which is the Windows
    // host = zeros on the current topology). `metrics()` blocks ~10s on the agent
    // transport, so it runs on the per-connection blocking thread, never here.
    let metrics_bridge = state.agent.clone();
    let metrics: std::sync::Arc<
        dyn Fn() -> Result<t_hub_protocol::HostMetrics, String> + Send + Sync,
    > = std::sync::Arc::new(move || metrics_bridge.metrics());

    // Share the event fanout (server-split M1) so a subscribed control connection
    // receives the same stream the backend emits through the SocketEmitter.
    let ctx = control::ControlContext::new(state.status.clone(), supervisor, token)
        .with_read_token(read_token)
        .with_apply_sink(apply_sink)
        .with_event_fanout(fanout)
        .with_metrics(metrics)
        // TASK C (#22): share the addressable tab registry with the control listener
        // so `list_tabs` reads what the `report_workspace_tabs` command writes.
        .with_tab_registry(tab_registry)
        // Captain-chat phase 2: the persistent captains registry (claims survive
        // restarts server-side; the UI's localStorage keeps only view state).
        .with_captains_registry(captains_registry)
        // Orchestrator wake: share the SAME watch registry the notifier reads, so
        // `watch_fleet` / `unwatch_fleet` arm the wakes the notifier delivers.
        .with_fleet_watches(fleet_watches)
        // Comms-plane Phase 2: share the per-session identity store (spawn mints +
        // binds; `inbox_ack` resolves against it) and the durable inbox (the fleet
        // notifier enqueues/drains; `inbox_ack` / `inbox_status` reach the same
        // queues) - one Arc each across the notifier and every connection handler.
        .with_identity_store(identity_store)
        .with_inbox(inbox);
    match control::start(ctx) {
        Ok(h) => {
            eprintln!(
                "t-hub: control listener on {} (handshake: {})",
                h.addr,
                control::handshake_path().display()
            );
            Some(h)
        }
        Err(e) => {
            eprintln!("t-hub: control listener failed to start: {e}");
            None
        }
    }
}

/// When built as the side-by-side DEV variant (`--features devbuild`), point the
/// app's runtime state at an isolated namespace BEFORE anything reads it, so a
/// dev build never collides with — or clobbers — a production T-Hub running at
/// the same time: a separate tmux socket (`t-hub-dev`, so dev terminals never
/// appear in / kill prod's live sessions) and a separate state dir
/// (`~/.t-hub-dev`) for the MCP control channel + diag log. This reuses the
/// existing `T_HUB_*` env hooks (tmux.rs / control.rs / diag.rs all read them
/// lazily on first use), so no path code changes; each var is set only when the
/// user hasn't already overridden it. No-op in production builds.
#[cfg(feature = "devbuild")]
fn apply_devbuild_isolation() {
    if std::env::var_os("T_HUB_TMUX_SOCKET").is_none() {
        std::env::set_var("T_HUB_TMUX_SOCKET", "t-hub-dev");
    }
    let dev_home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .map(|h| h.join(".t-hub-dev"));
    if let Some(dir) = dev_home {
        let _ = std::fs::create_dir_all(&dir);
        if std::env::var_os("T_HUB_CONTROL_FILE").is_none() {
            std::env::set_var("T_HUB_CONTROL_FILE", dir.join("control.json"));
        }
        if std::env::var_os("T_HUB_DIAG_FILE").is_none() {
            std::env::set_var("T_HUB_DIAG_FILE", dir.join("diag.log"));
        }
        if std::env::var_os("T_HUB_CAPTAINS_FILE").is_none() {
            std::env::set_var("T_HUB_CAPTAINS_FILE", dir.join("captains.json"));
        }
    }
}

#[cfg(not(feature = "devbuild"))]
fn apply_devbuild_isolation() {}

/// The user-facing app name — "T-Hub Dev" for the side-by-side dev build, "T-Hub"
/// for production. Single source so the dev build is visibly distinct everywhere it
/// is named (tray tooltip + menu; the window title is set from the same intent in
/// `setup`, and the frontend wordmark reads the Tauri `productName` via `getName()`).
pub fn brand_name() -> &'static str {
    #[cfg(feature = "devbuild")]
    {
        "T-Hub Dev"
    }
    #[cfg(not(feature = "devbuild"))]
    {
        "T-Hub"
    }
}

pub fn run() {
    // Must run before any `T_HUB_*`-backed LazyLock (socket/control/diag) is
    // first touched — i.e. before the Tauri builder spawns anything.
    apply_devbuild_isolation();

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
        // OS toast notifications for key session events (WS-2). The frontend's
        // lib/notify.ts dynamically imports @tauri-apps/plugin-notification and
        // calls into this; gated by the `notification:*` capabilities + the
        // `plugins.notification` block in tauri.conf.json.
        .plugin(tauri_plugin_notification::init())
        .manage(TerminalManager::default())
        // Server-split M2a: live remote-PTY connections (terminal tiles streamed
        // over the control socket) live here instead of the in-process
        // TerminalManager. Both are managed during the migration; the streaming
        // path (attach/write/resize/close) is now backed by this one.
        .manage(remote_pty::RemotePtyManager::default())
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
            // Arm the host main-thread hang watchdog (catches the sporadic
            // Not-Responding/Alt-Tab-ghost freeze that the renderer-side JS detector
            // can't see). Logs {"t":"hang","src":"rust-main",...} to the diag file.
            hangwatch::spawn(app.handle().clone());
            // Server-split: backend bridge events now flow over the loopback
            // control socket ONLY. The SocketEmitter writes each into this shared
            // EventFanout; the forwarder thread (control_client) reads them back and
            // re-emits a single `control://event` envelope, which the frontend
            // demuxes by channel (ipc/controlClient.ts). The same fanout `Arc` is
            // handed to the control listener below, so a connection that subscribes
            // receives exactly this stream.
            //
            // The M1 migration is COMPLETE: every bridge channel (journal /
            // supervision / session-status / agent-state / status-snapshot / title)
            // is consumed via that demux, so the old in-process TauriEmitter leg (a
            // raw `app.emit(channel)`) had NO remaining listener. Running it
            // alongside the socket leg (the former TeeEmitter) double-emitted every
            // event into the webview for nothing — ~doubling the event volume that
            // pinned the UI. So we install the SocketEmitter ALONE. (The dual-leg
            // TeeEmitter + in-process TauriEmitter have been removed entirely.)
            let control_fanout = std::sync::Arc::new(control::EventFanout::new());
            let emitter: std::sync::Arc<dyn agent::EventEmitter> = std::sync::Arc::new(
                control_client::SocketEmitter::new(control_fanout.clone()),
            );
            // Manage the SAME fanout Arc so UI-driven Tauri commands that mutate
            // the captains registry (kill_terminal, the tab-close report) can
            // broadcast a captains snapshot to socket clients (the native cockpit)
            // as well as the webview - the Tauri-side twin of control.rs's
            // forward_apply (ApplySink + fanout broadcast).
            app.manage(control_fanout.clone());
            state.agent.set_emitter(emitter);
            state.agent.set_status_bridge(state.status.clone());
            // --- WS-6: open the durable DB now (before the status bridge ingests
            // any statusline) and manage it, so save/load_workspace_snapshot AND
            // the per-tile session-restore record share one handle. Wire it into
            // the status bridge so every ingested snapshot durably records its
            // tile→session binding for the boot-time restore catalog. A failed
            // open resolves to a no-op Db (logged inside), never aborting startup.
            let db = std::sync::Arc::new(db::init(&app.handle().clone()));
            state.status.set_db(db.clone());
            // Manage the SAME handle the workspace-persistence + recovery + WS-6
            // commands read via `app.state::<Arc<Db>>()` — one open connection,
            // shared by everything.
            app.manage(db);
            // Always-fires startup marker (proves the diag log is writable + shows
            // the resolved path; diagnoses "app runs but diag is stale").
            diag::log_startup();
            // Kick off the agent connection in the background once state exists.
            spawn_agent_connect(&state);

            // Managed TTS-engine lifecycle (proposal /tmp/flap-probe/
            // LIFECYCLE-PROPOSAL.md). The status snapshot is ALWAYS managed as
            // state so `engine_runtime_status` answers; the watcher thread that
            // actually spawns/health-watches/falls-back only starts behind the
            // default-OFF `T_HUB_MANAGED_KOKORO` flag (+ configured install
            // paths), so shipping this disturbs NOTHING live - the interim
            // unit-owned Kokoro keeps serving until the migration flip.
            let engine_snapshot = engine_supervisor::runtime::SharedSnapshot::new_unmanaged();
            let engine_snapshot_handle = engine_snapshot.handle();
            app.manage(engine_snapshot);
            if engine_supervisor::managed_enabled() {
                let selected = voice::current_engine();
                if selected != voice::VoiceEngine::Kokoro {
                    // F4 fail-safe: the platform layer (wsl.exe lifeline, the
                    // kokoro-tts.service unit name, the pid marker) is
                    // Kokoro-specific. Arming it with a non-Kokoro primary would,
                    // e.g., `disable --now` the Kokoro unit for no reason. Wave 1
                    // manages Kokoro only.
                    diag::diag_log(
                        "engine_supervisor: T_HUB_MANAGED_KOKORO set but the selected \
                         engine is not Kokoro - not starting (wave-1 manages Kokoro only)"
                            .to_string(),
                    );
                } else if let Some(opts) =
                    engine_supervisor::runtime::opts_from_env(selected)
                {
                    engine_supervisor::runtime::start(
                        control_fanout.clone(),
                        engine_snapshot_handle,
                        opts,
                    );
                } else {
                    diag::diag_log(
                        "engine_supervisor: T_HUB_MANAGED_KOKORO set but install \
                         paths (T_HUB_KOKORO_DIR / T_HUB_PIPER_EXE) missing - not \
                         starting (fail-safe)"
                            .to_string(),
                    );
                }
            }
            // Best-effort startup reconcile: if the user already has T-Hub-managed
            // Claude hooks/statusLine (including stale `__termhub_managed__` ones
            // from an upgraded `termhub` build), auto-migrate them to the current
            // marker + resolved `t-hub-agent` path. It NEVER installs where nothing
            // managed exists (no silent new consent). Detached + error-swallowed so
            // it can never block or abort launch (a WSL hop / file write runs here).
            spawn_reconcile_managed_hooks();
            // Start the MCP control listener so `t-hub-mcp` can forward
            // `tools/call` over the local control channel (PRD §9.6). A bind
            // failure is logged and does not abort startup (the channel is
            // optional, like the agent bridge). On success, install the
            // server-split M1 client transport: manage the endpoint (addr+token)
            // for the `control_request` command and start the event forwarder that
            // re-emits the socket's event stream into the webview.
            // TASK C (#22): the CORE's addressable tab registry. One Arc, shared
            // between the control listener (which reads it for list_tabs and updates
            // it on new_tab/move_tile/named placement) and the managed state the
            // `report_workspace_tabs` command writes the frontend's up-sync into.
            let tab_registry = std::sync::Arc::new(control::TabRegistry::new());
            app.manage(tab_registry.clone());
            // Captain-chat phase 2: the captains registry, loaded from its
            // persistence file so claims survive app restarts (unlike tabs, whose
            // layout the frontend re-seeds on boot).
            let captains_registry =
                std::sync::Arc::new(control::CaptainsRegistry::load(control::captains_path()));
            // Manage the SAME Arc so the Tauri `kill_terminal` command can drop a
            // dead session (captain or crew) from the registry - the UI kills tiles
            // via that command, not the control socket, so without this a killed
            // crew tile would leave a stale claim/crew entry in the persistent
            // captains.json to re-hydrate as a phantom pin after restart.
            app.manage(captains_registry.clone());
            // Orchestrator wake: the fleet watch registry (armed by `watch_fleet`)
            // and the notifier that reads it. The notifier observes every session
            // status edge and, when a watched session goes idle / needs-input /
            // completes, injects a wake prompt into the orchestrator's terminal -
            // re-invoking its agent loop instead of only painting a UI badge. All
            // opt-in: no armed watch => no wakes => no behaviour change.
            let fleet_watches = std::sync::Arc::new(fleet::FleetWatchRegistry::new());
            // Comms-plane Phase 2: the per-session identity store and the durable
            // inbox, both loaded from their persistence files so bindings + queued
            // messages survive restarts (identities.json; ~/.t-hub/inbox/). One Arc
            // each is shared between the fleet notifier (the inbox's first client) and
            // the control listener (identity resolve + inbox ack/status).
            let identity_store =
                std::sync::Arc::new(identity::IdentityStore::load_default());
            let inbox = std::sync::Arc::new(inbox::Inbox::open_default());
            {
                // The injector: type + submit a line into a tile's Claude session
                // over tmux (the only thing that re-invokes an idle agent loop).
                // comms-plane Phase 1: the wake is the plane's FIRST primary writer -
                // it routes through the plane (funnel + attribution) instead of
                // calling `tmux::send_text` directly. Behaviour is unchanged (still an
                // immediate, `Completed`-gated tmux write - no durability yet, that is
                // Phase 2). The construction lives in `fleet::production_wake_injector`
                // so the funnel is pinned by a test (a revert to a direct tmux write
                // there fails `production_wake_injector_routes_through_plane`).
                let inject: fleet::Injector = fleet::production_wake_injector();
                // Bonus UI/voice cue: fan out `fleet://wake` alongside the injection.
                let sink_fanout = control_fanout.clone();
                let event_sink: fleet::EventSink = std::sync::Arc::new(move |payload| {
                    sink_fanout.emit_event("fleet://wake", payload);
                });
                let mut notifier = fleet::FleetNotifier::new(
                    fleet_watches.clone(),
                    captains_registry.clone(),
                    state.status.clone(),
                    inject,
                )
                .with_event_sink(event_sink);
                // Comms-plane Phase 2: route wakes through the durable inbox (the
                // wake becomes the inbox's first client) unless the rollback flag
                // `T_HUB_INBOX_WAKE=0` is set, which falls back to the Phase-1
                // immediate `Completed`-gated wake (the rollback the design names).
                let wake_durable = std::env::var("T_HUB_INBOX_WAKE")
                    .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
                    .unwrap_or(true);
                if wake_durable {
                    notifier = notifier.with_inbox(inbox.clone());
                }
                let notifier = std::sync::Arc::new(notifier);
                // Install as the AgentBridge status observer. The Arc is kept alive
                // by the observer closure the bridge holds for the app's lifetime.
                let observer: agent::StatusObserver =
                    std::sync::Arc::new(move |uuid: &str, status| notifier.on_status(uuid, status));
                state.agent.set_status_observer(observer);
            }
            if let Some(handshake) = start_control_listener(
                &state,
                app.handle(),
                control_fanout,
                tab_registry,
                captains_registry,
                fleet_watches,
                identity_store,
                inbox,
            ) {
                control_client::install(app.handle(), &handshake);
            }
            // Install the system-tray icon + menu (#17). A tray build failure is
            // logged and does not abort startup; the app remains usable via its
            // window (close-to-tray still works regardless via on_window_event).
            if let Err(e) = tray::build(app.handle()) {
                eprintln!("t-hub: failed to build system tray: {e}");
            }
            // Restore Windows 11 Snap Layouts (hover-the-maximize-button flyout)
            // and native edge-resize on the frameless main window by subclassing
            // its HWND to answer WM_NCHITTEST (#snap). On unix this is a no-op. A
            // failure here is logged and never aborts startup; the window stays
            // fully usable, just without the native snap flyout / edge resize.
            if let Some(main) = app.get_webview_window("main") {
                // Restore Windows 11 Snap Layouts + native edge-resize on the
                // frameless main window by subclassing its HWND (see win_snap.rs).
                // win_snap was A/B-tested as the drag/resize-lag suspect (v0.3.5 shipped
                // with it OFF) and CONCLUSIVELY RULED OUT — the lag persisted without it
                // — so it's back on by default. The real cause was the opaque WebView2
                // redirection bitmap; fixed by `transparent: true` in tauri.conf.json
                // (tao then sets WS_EX_NOREDIRECTIONBITMAP, off the laggy redirection
                // path). The T_HUB_DISABLE_WIN_SNAP escape hatch remains for any future
                // A/B. A failure here is logged and never aborts startup.
                if std::env::var_os("T_HUB_DISABLE_WIN_SNAP").is_some() {
                    eprintln!("t-hub: win_snap DISABLED via T_HUB_DISABLE_WIN_SNAP");
                } else if let Err(e) = win_snap::install(&main) {
                    eprintln!("t-hub: failed to install Snap-Layouts hit-test hook: {e}");
                }
                // DEV variant: distinguish the window (alt-tab / taskbar tooltip)
                // from a production T-Hub that may be running alongside it.
                #[cfg(feature = "devbuild")]
                {
                    let _ = main.set_title("T-Hub Dev");
                }
            }
            // Force `mouse on` server-wide AND on every already-running session
            // (a session-local `mouse off` left by an older build overrides the
            // global flip, so the wheel still sent arrow keys in old panes). Run
            // off-thread: it spawns one `wsl.exe tmux` per live session, which we
            // never want to block the window's first paint on. Best-effort.
            std::thread::spawn(|| tmux::ensure_mouse_on());
            // (The durable workspace DB — app_data_dir/t-hub.db, WAL+NORMAL — is
            // opened + managed + wired into the status bridge above, before any
            // statusline ingest; see the WS-6 block.)
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
            commands::deliver_agent_input,
            commands::resize_terminal,
            commands::close_terminal,
            commands::kill_terminal,
            commands::list_terminals,
            // 0.5 surface
            commands_05::agent_state,
            commands_05::git_branch,
            commands_05::tmux_scroll,
            commands_05::tmux_exit_scroll,
            dropin::clipboard_image_to_temp,
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
            // WS-4: git worktrees as a first-class primitive (list/add/remove).
            git::git_worktree_list,
            git::git_worktree_add,
            git::git_worktree_remove,
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
            // #9: shared (all-variants) workspace layout at ~/.config/t-hub/workspaces.json.
            theme::load_shared_layout,
            theme::save_shared_layout,
            // #sqlite: durable workspace-layout persistence (mirrors localStorage).
            db::save_workspace_snapshot,
            db::load_workspace_snapshot,
            // #recovery: snapshot-history read commands for the Recovery review UI.
            db::list_snapshots,
            db::get_snapshot,
            // WS-6: native session-restore — list resumable orphans after an
            // app/backend/host restart. (Recording happens automatically via the
            // status bridge on every statusline ingest; there is no record command.)
            db::list_orphaned_sessions,
            // --- feat/projects-sidebar (Agent A) -------------------------------
            // Recent recallable Claude sessions for the sidebar "Recent" list.
            recent::recent_sessions,
            // Claude plan usage (`claude -p /usage`) for the sidebar Usage strip.
            usage::claude_usage,
            codex::codex_usage,
            // -------------------------------------------------------------------
            // feat/diag: runtime diagnostics sink (mirrors frontend logs to a file
            // the WSL-side orchestrator can read from a RELEASE build).
            diag::diag_log,
            diag::diag_clear,
            // Server-split M1: thin client transport — round-trip one control
            // command over the loopback socket (the wire M2 stretches to remote).
            control_client::control_request,
            // TASK C (#22): the frontend reports its live workspace-tab layout here
            // so the control/MCP tab registry (list_tabs) mirrors the UI.
            report_workspace_tabs,
            // #snap: the frontend reports the maximize button's live rect (physical
            // px, window-relative) so the Win32 WM_NCHITTEST returns HTMAXBUTTON
            // exactly over the visible button — what makes Win11 show the Snap
            // Layouts flyout on hover. No-op effect on unix (stored but unread).
            win_snap::set_maximize_button_rect,
            // Settings > Voice: voice.json read/write (shared with external
            // captain tooling) + the loopback TTS proxy (the server rejects
            // browser-Origin requests, so the webview never fetches it).
            voice::voice_settings_read,
            voice::voice_settings_write,
            voice::voice_list_voices,
            voice::voice_tts,
            // Bounded /health probe (2s) per engine - the Settings dual-engine
            // health display + selected-engine-down error state read this.
            voice::voice_health,
            // Managed-lifecycle status (active vs selected engine, degraded
            // level). Reports managed:false when the flag is off so the webview
            // falls back to its own #52 probes.
            engine_supervisor::engine_runtime_status,
            // Scribe voice-gate: "is the general dictating?" (fails open).
            scribe::scribe_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running T-Hub");
}
