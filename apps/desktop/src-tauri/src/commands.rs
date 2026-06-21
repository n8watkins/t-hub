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
    /// Optional command to run in the new pane after the login shell starts
    /// (the "+" spawn presets: e.g. `claude`, `claude --resume`, or a custom
    /// line). Unlike `shell` (which REPLACES the pane's program and so dies with
    /// it), this is run *inside* an interactive login shell that the pane then
    /// `exec`s back into, so exiting the command (e.g. quitting Claude) drops to
    /// a live shell instead of closing the tile. `None`/empty => plain login
    /// shell, byte-for-byte today's "Shell" behavior (no regression).
    pub startup_command: Option<String>,
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

/// App-wide registry of live PTY/tmux-backed terminals, keyed by T-Hub id.
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
    // On Windows the tmux pane runs inside WSL, so a Windows path would be
    // meaningless as `-c`; default to empty and let the pane inherit wsl.exe's
    // working directory. On Unix, prefer $HOME, then the process cwd.
    #[cfg(windows)]
    {
        String::new()
    }
    #[cfg(unix)]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return home.to_string_lossy().to_string();
        }
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "/".to_string())
    }
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

/// Single-quote `s` for safe embedding inside a POSIX `sh -c '...'` string: wrap
/// in single quotes and replace every embedded `'` with the `'\''` idiom (close
/// quote, escaped literal quote, reopen quote). Lets an arbitrary user "Custom…"
/// command (which may itself contain quotes) ride safely into the pane program.
fn sh_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Resolve the tmux pane program for this spawn.
///
/// Precedence:
///   1. An explicit `shell` preset becomes the pane's program verbatim (legacy
///      behavior; it REPLACES the shell and the pane dies when it exits).
///   2. A `startup_command` ("+" presets / Custom…) runs *inside* an interactive
///      login shell that the pane then `exec`s back into, so quitting the command
///      (e.g. exiting Claude) drops to a live shell rather than closing the tile.
///   3. Otherwise `None` — tmux launches the user's plain login shell, exactly
///      today's "Shell" behavior (no regression).
fn resolve_pane_command(opts: &SpawnOptions) -> Option<String> {
    if let Some(shell) = opts.shell.as_deref().filter(|s| !s.trim().is_empty()) {
        return Some(shell.to_string());
    }
    let startup = opts
        .startup_command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    // Run the startup command in an INTERACTIVE LOGIN shell, then exec back into
    // an interactive login shell so the tile survives the command exiting.
    //
    // `-ilc` (interactive + login + command), NOT `-lc`, is the load-bearing
    // detail: tools like `claude` live on a PATH set in the user's shell rc
    // (e.g. `~/.npm-global/bin` exported in ~/.zshrc). zsh sources ~/.zshrc only
    // for INTERACTIVE shells -- a login-but-non-interactive `zsh -lc` skips it, so
    // `claude --resume ...` failed with "command not found: claude" and dropped
    // straight to the fallback shell (the "Resume opens a plain terminal, not
    // Claude" bug). `-i` forces ~/.zshrc to load, so the command resolves exactly
    // as when typed by hand. Verified in a clean env (no inherited PATH).
    Some(format!(
        "exec \"${{SHELL:-/bin/sh}}\" -ilc {}",
        sh_single_quote(&format!(
            "{startup}; exec \"${{SHELL:-/bin/sh}}\" -l"
        ))
    ))
}

#[tauri::command]
pub async fn spawn_terminal(
    app: tauri::AppHandle,
    state: tauri::State<'_, TerminalManager>,
    opts: SpawnOptions,
) -> Result<TerminalInfo, String> {
    // The terminal id IS the tmux session's own suffix, so the id is stable and
    // identical no matter who produces it: `spawn_terminal` here, `list_terminals`
    // after a reload (which strips `th_` off the session name), and the
    // `attach_terminal`/`kill_terminal` reconstructions. If id and session name
    // disagree, a reloaded tile renders under an id that has no record in the
    // store and its dot falls back to the amber "starting" placeholder forever
    // (bug #16). Using the session suffix as the id keeps them in lockstep.
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let id = suffix[..8].to_string();
    let tmux_session = format!("th_{id}");
    let cwd = resolve_cwd(&opts);
    let title = resolve_title(&opts);

    // Create the detached tmux session that backs this terminal. With
    // `command == None` tmux launches the user's login shell (the "Shell" preset
    // / today's default). A `shell` preset becomes the pane's program verbatim; a
    // `startupCommand` ("+" presets: Claude / Resume Claude / Custom…) is run
    // inside a login shell the pane execs back into (see `resolve_pane_command`).
    let command = resolve_pane_command(&opts);
    tmux::new_session(&tmux_session, &cwd, command.as_deref())
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
    let has_live = state.sessions.lock().contains_key(&id);
    {
        if !has_live {
            // Best-effort cwd for the Windows attach path; the pane already has
            // its own cwd, so an empty string is acceptable on Unix.
            let cwd = std::env::var("HOME").unwrap_or_default();
            let session =
                pty::spawn_attach_client(&app, &id, &tmux_session, &cwd, cols, rows)?;
            state.sessions.lock().insert(id.clone(), session);
        } else {
            // Already streaming — make sure the geometry matches the freshly
            // mounted xterm so the pane isn't stale.
            if let Some(session) = state.sessions.lock().get_mut(&id) {
                let _ = session.resize(cols, rows);
            }
        }
    }

    // A successful attach means a PTY client is now bound to this session, so the
    // terminal is unambiguously Live. Emit on BOTH paths (fresh client AND an
    // already-streaming reattach): after a reload the frontend may have seeded
    // this terminal from `list_terminals` as Detached (no in-memory client at
    // list time) or never seeded it at all, and without this transition the tile
    // would stay stuck on its initial dot (bug #16). Idempotent for a tile that
    // was already Live.
    let _ = app.emit(
        events::STATE,
        &StateEvent {
            id: id.clone(),
            state: TerminalState::Live,
        },
    );

    // Seed xterm. A true reattach replays full scrollback history. A FRESH spawn
    // is NOT seeded (empty): the pane's prompt may not be drawn yet (zsh startup
    // races attach) and any visible reflow from the 80x24 -> real-size resize
    // would replay as a "cascade" of duplicate prompts. The frontend instead
    // forces a single clean redraw (Ctrl-L) once it has subscribed.
    let scrollback: Vec<u8> = if has_live {
        Vec::new()
    } else {
        tmux::capture_pane(&tmux_session)
            .map_err(|e| format!("failed to capture scrollback: {e}"))?
    };
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

/// Re-capture a DEEP scrollback window for `id` and return it base64-encoded, so
/// the ⟳ refresh can re-seed xterm with far more history (scroll-up) at the pane's
/// current width. Read-only on tmux; resolves the session name from the live entry
/// if present, else `th_<id>` (mirrors attach/resize/kill). The lock is released
/// before the (blocking) tmux capture.
#[tauri::command]
pub async fn recapture_scrollback(
    state: tauri::State<'_, TerminalManager>,
    id: String,
) -> Result<String, String> {
    let tmux_session = state
        .sessions
        .lock()
        .get(&id)
        .map(|s| s.tmux_session.clone())
        .unwrap_or_else(|| format!("th_{}", &id[..id.len().min(8)]));
    let bytes = tmux::capture_pane_deep(&tmux_session)
        .map_err(|e| format!("failed to capture scrollback: {e}"))?;
    Ok(STANDARD.encode(bytes))
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
    // Source of truth for liveness is the tmux server on the `t-hub` socket;
    // the in-memory map only tells us which terminals this UI currently has a
    // PTY client for (Live) vs. ones running detached (Detached).
    let live_sessions = tmux::list_sessions()
        .map_err(|e| format!("failed to list tmux sessions: {e}"))?;

    // Per-session foreground command + live cwd (best-effort), so each tile is
    // labeled by what's actually running (`claude`, `zsh`, ...) and where, rather
    // than a raw id. A failure here just leaves the old id-based label.
    let pane_map: std::collections::HashMap<String, (String, String)> = tmux::pane_info()
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.session, (p.command, p.cwd)))
        .collect();

    let sessions = state.sessions.lock();

    // Reconcile: every tmux session named `th_*` is a T-Hub terminal. Reverse
    // any leftover in-memory entries whose tmux session vanished by NOT
    // reporting them (their reader thread will have emitted `exit`).
    let mut infos: Vec<TerminalInfo> = Vec::with_capacity(live_sessions.len());
    for tmux_session in &live_sessions {
        // Only surface sessions that belong to T-Hub (the `th_` prefix), not
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

        // Foreground command + live cwd from tmux (pane_info), so the UI labels
        // this tile by what's running and where instead of the raw id.
        let (cmd, cwd) = pane_map.get(tmux_session).cloned().unwrap_or_default();
        infos.push(TerminalInfo {
            id,
            tmux_session: tmux_session.clone(),
            cwd,
            title: if cmd.is_empty() {
                tmux_session.clone()
            } else {
                cmd
            },
            state: state_val,
        });
    }

    Ok(infos)
}
