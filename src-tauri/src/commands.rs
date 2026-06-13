//! Tauri command surface — the IPC contract, mirrored from `src/ipc/types.ts`.
//!
//! Command names and (camelCase) payload fields MUST stay in lockstep with the
//! frontend contract. The bodies below are stubs implemented by the Rust backend
//! subagent (task #9); they compile and return errors until then.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::Emitter;

use crate::events::{self, StateEvent};
use crate::pty::{self, PtySession};
use crate::tmux;

/// Default terminal geometry used when a tile is spawned before the frontend
/// has measured the xterm viewport. The first real `resize_terminal` from the
/// UI corrects this immediately.
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TerminalState {
    Starting,
    Live,
    Detached,
    Exited,
    Error,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnOptions {
    pub cwd: Option<String>,
    pub shell: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    pub id: String,
    pub tmux_session: String,
    pub cwd: String,
    pub title: String,
    pub state: TerminalState,
}

/// App-wide registry of live PTY/tmux-backed terminals, keyed by TermHub id.
#[derive(Default)]
pub struct TerminalManager {
    pub sessions: Mutex<HashMap<String, PtySession>>,
}

/// Resolve the working directory for a new terminal.
///
/// Honors an explicit `cwd`; otherwise falls back to `$HOME` (Unix / WSL) or, as
/// a last resort, the process working directory. This is the path tmux's
/// `new-session -c` roots the pane at; on Windows it is a WSL-side path because
/// the attach client runs inside the distro.
fn resolve_cwd(opts: &SpawnOptions) -> String {
    if let Some(cwd) = opts.cwd.as_ref().filter(|c| !c.trim().is_empty()) {
        return cwd.clone();
    }
    if let Some(home) = std::env::var_os("HOME") {
        return home.to_string_lossy().to_string();
    }
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/".to_string())
}

/// Pick the human-readable title for the tile: the caller's `name`, else the
/// shell preset, else a generic default.
fn resolve_title(opts: &SpawnOptions) -> String {
    opts.name
        .as_ref()
        .filter(|n| !n.trim().is_empty())
        .cloned()
        .or_else(|| opts.shell.clone())
        .unwrap_or_else(|| "terminal".to_string())
}

#[tauri::command]
pub async fn spawn_terminal(
    app: tauri::AppHandle,
    state: tauri::State<'_, TerminalManager>,
    opts: SpawnOptions,
) -> Result<TerminalInfo, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let tmux_session = format!("th_{}", &id[..8]);
    let cwd = resolve_cwd(&opts);
    let title = resolve_title(&opts);

    // Create the detached tmux session that backs this terminal. With
    // `command == None` tmux launches the user's login shell, which is what the
    // nucleus wants; an explicit `shell` preset becomes the pane's program.
    let command = opts.shell.as_deref().filter(|s| !s.trim().is_empty());
    tmux::new_session(&tmux_session, &cwd, command)
        .map_err(|e| format!("failed to create tmux session: {e}"))?;

    // Spawn the PTY attach client that streams this session to the frontend. If
    // it fails, tear down the tmux session we just created so we don't leak it.
    let session = match pty::spawn_attach_client(
        &app,
        &id,
        &tmux_session,
        &cwd,
        DEFAULT_COLS,
        DEFAULT_ROWS,
    ) {
        Ok(s) => s,
        Err(e) => {
            let _ = tmux::kill_session(&tmux_session);
            return Err(e);
        }
    };

    state.sessions.lock().insert(id.clone(), session);

    // The terminal is now live and streaming.
    let _ = app.emit(
        events::STATE,
        &StateEvent {
            id: id.clone(),
            state: TerminalState::Live,
        },
    );

    Ok(TerminalInfo {
        id,
        tmux_session,
        cwd,
        title,
        state: TerminalState::Live,
    })
}

#[tauri::command]
pub async fn attach_terminal(
    app: tauri::AppHandle,
    state: tauri::State<'_, TerminalManager>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    // Find the tmux session name for this id. It may be known via an existing
    // (possibly detached) in-memory PtySession; otherwise we reconstruct it
    // from the id, which is how `th_{prefix}` is derived in `spawn_terminal`.
    let tmux_session = {
        let sessions = state.sessions.lock();
        match sessions.get(&id) {
            Some(existing) => existing.tmux_session.clone(),
            None => format!("th_{}", &id[..id.len().min(8)]),
        }
    };

    // The tmux session must still exist to (re)attach; if it's gone the terminal
    // has been killed or exited and there is nothing to attach to.
    if !tmux::has_session(&tmux_session) {
        return Err(format!(
            "tmux session {tmux_session} for terminal {id} no longer exists"
        ));
    }

    // Ensure a live PTY client exists for this id. If one is already present
    // (the tile is visible and streaming) we keep it and just re-seed xterm with
    // the current scrollback. If not (first attach, or re-attach after
    // `close_terminal` detached it), create a fresh attach client.
    {
        let has_live = state.sessions.lock().contains_key(&id);
        if !has_live {
            // Best-effort cwd for the Windows attach path; the pane already has
            // its own cwd, so an empty string is acceptable on Unix.
            let cwd = std::env::var("HOME").unwrap_or_default();
            let session =
                pty::spawn_attach_client(&app, &id, &tmux_session, &cwd, cols, rows)?;
            state.sessions.lock().insert(id.clone(), session);
            let _ = app.emit(
                events::STATE,
                &StateEvent {
                    id: id.clone(),
                    state: TerminalState::Live,
                },
            );
        } else {
            // Already streaming — make sure the geometry matches the freshly
            // mounted xterm so the pane isn't stale.
            if let Some(session) = state.sessions.lock().get_mut(&id) {
                let _ = session.resize(cols, rows);
            }
        }
    }

    // Seed xterm with the current pane contents + scrollback (ANSI preserved).
    let scrollback = tmux::capture_pane(&tmux_session)
        .map_err(|e| format!("failed to capture scrollback: {e}"))?;
    Ok(STANDARD.encode(scrollback))
}

#[tauri::command]
pub async fn write_terminal(
    state: tauri::State<'_, TerminalManager>,
    id: String,
    data: String,
) -> Result<(), String> {
    let mut sessions = state.sessions.lock();
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| format!("no live terminal {id}"))?;
    session
        .write(data.as_bytes())
        .map_err(|e| format!("failed to write to terminal {id}: {e}"))
}

#[tauri::command]
pub async fn resize_terminal(
    state: tauri::State<'_, TerminalManager>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let mut sessions = state.sessions.lock();
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| format!("no live terminal {id}"))?;
    session.resize(cols, rows)
}

#[tauri::command]
pub async fn close_terminal(
    state: tauri::State<'_, TerminalManager>,
    id: String,
) -> Result<(), String> {
    // Detach the PTY client and remove it from the map, but DO NOT kill the
    // tmux session: the backing process keeps running so the terminal survives
    // the UI closing the tile. Re-attaching later via `attach_terminal` spins up
    // a fresh PTY client against the still-alive session.
    let session = state.sessions.lock().remove(&id);
    if let Some(session) = session {
        // Kills the attach client, drops master/writer, joins the reader thread.
        session.detach();
    }
    Ok(())
}

#[tauri::command]
pub async fn kill_terminal(
    state: tauri::State<'_, TerminalManager>,
    id: String,
) -> Result<(), String> {
    // Stop for real: kill the tmux session (terminating its process tree), then
    // drop the PTY client and remove it from the map.
    let session = state.sessions.lock().remove(&id);

    // Derive the session name from the live entry if present, else from the id.
    let tmux_session = session
        .as_ref()
        .map(|s| s.tmux_session.clone())
        .unwrap_or_else(|| format!("th_{}", &id[..id.len().min(8)]));

    let kill_result = tmux::kill_session(&tmux_session)
        .map_err(|e| format!("failed to kill tmux session {tmux_session}: {e}"));

    // Dropping the PtySession kills the attach client and joins the reader; do
    // this regardless of whether the tmux kill reported an error.
    drop(session);

    kill_result
}

#[tauri::command]
pub async fn list_terminals(
    state: tauri::State<'_, TerminalManager>,
) -> Result<Vec<TerminalInfo>, String> {
    // Source of truth for liveness is the tmux server on the `termhub` socket;
    // the in-memory map only tells us which terminals this UI currently has a
    // PTY client for (Live) vs. ones running detached (Detached).
    let live_sessions = tmux::list_sessions()
        .map_err(|e| format!("failed to list tmux sessions: {e}"))?;

    let sessions = state.sessions.lock();

    // Reconcile: every tmux session named `th_*` is a TermHub terminal. Reverse
    // any leftover in-memory entries whose tmux session vanished by NOT
    // reporting them (their reader thread will have emitted `exit`).
    let mut infos: Vec<TerminalInfo> = Vec::with_capacity(live_sessions.len());
    for tmux_session in &live_sessions {
        // Only surface sessions that belong to TermHub (the `th_` prefix), not
        // anything a user might have created on this socket out-of-band.
        if !tmux_session.starts_with("th_") {
            continue;
        }

        // Match back to an in-memory PtySession (if this UI has a client for
        // it) to recover the canonical id; otherwise the id is the session's
        // own suffix and the terminal is running detached from this UI.
        let entry = sessions
            .values()
            .find(|s| &s.tmux_session == tmux_session);

        let (id, state_val) = match entry {
            Some(s) => (s.id.clone(), TerminalState::Live),
            None => (
                tmux_session
                    .strip_prefix("th_")
                    .unwrap_or(tmux_session)
                    .to_string(),
                TerminalState::Detached,
            ),
        };

        infos.push(TerminalInfo {
            id,
            tmux_session: tmux_session.clone(),
            // tmux owns the real cwd of the pane; we don't track it server-side
            // after spawn, so report empty rather than a stale guess.
            cwd: String::new(),
            title: tmux_session.clone(),
            state: state_val,
        });
    }

    Ok(infos)
}
