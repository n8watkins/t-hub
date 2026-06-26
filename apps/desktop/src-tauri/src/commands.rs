//! Tauri command surface — the IPC contract, mirrored from `src/ipc/types.ts`.
//!
//! Command names and (camelCase) payload fields MUST stay in lockstep with the
//! frontend contract. The bodies below are stubs implemented by the Rust backend
//! subagent (task #9); they compile and return errors until then.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::{Emitter, Manager};

use crate::control_client::ControlEndpoint;
use crate::events::{self, StateEvent};
use crate::pty::PtySession;
use crate::remote_pty::{RemotePty, RemotePtyManager};
use crate::tmux;

// Wire-contract enum mirroring the frontend `TerminalState` (ipc/types.ts). The
// backend doesn't emit every variant yet — `Starting`/`Error` are part of the
// contract but currently unconstructed Rust-side — so allow dead variants rather
// than diverge from the frontend's type.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
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
    /// The tile's working directory. At SPAWN time this is the dir the pane was
    /// rooted at (`resolve_cwd`); on every `list_terminals` reconcile it is
    /// REPLACED by the pane's *live* current path (`#{pane_current_path}` from
    /// `tmux::pane_info`), so it tracks the user `cd`-ing around. There is a
    /// single `cwd` field — the spawn value is just its initial seed, and the
    /// ~5s `list_terminals` poll keeps it current. This is the enabling primitive
    /// for the worktree "anchor to the focused tile's repo" flow (WS-9) and the
    /// relative-file-open TODO (WS-1).
    pub cwd: String,
    pub title: String,
    pub state: TerminalState,
}

/// App-wide registry of live PTY/tmux-backed terminals, keyed by T-Hub id.
///
/// As of server-split M2a the streaming path (attach/write/resize/close) is
/// backed by [`RemotePtyManager`] (terminal tiles over the control socket); this
/// in-process manager is retained but no longer drives those commands. It (and
/// [`pty::spawn_attach_client`]) stay defined so the revert is a one-liner and the
/// reaping unit tests below keep proving the tmux-vs-map invariant.
#[derive(Default)]
pub struct TerminalManager {
    // Retained for the M2a revert path; no command reads it now (the streaming
    // commands are backed by `RemotePtyManager`). `#[allow(dead_code)]` keeps the
    // intentional retention from warning.
    #[allow(dead_code)]
    pub sessions: Mutex<HashMap<String, PtySession>>,
}

/// Resolve the tmux session name that backs terminal `id`. The id IS the tmux
/// session's `th_`-prefixed suffix (see [`spawn_terminal`]), capped at 8 chars,
/// so `th_<id[..8]>` reconstructs it deterministically — identical to the
/// derivation the old in-process path used in `attach_terminal`/`kill_terminal`.
fn tmux_target(id: &str) -> String {
    format!("th_{}", &id[..id.len().min(8)])
}

/// Read the managed [`ControlEndpoint`] (addr + token) installed by `setup()`
/// after the control listener binds. Terminal commands run on user action (well
/// after setup), so it is normally present; if a command somehow runs before it
/// exists we return a clear error rather than panicking on the missing state.
fn control_endpoint(app: &tauri::AppHandle) -> Result<ControlEndpoint, String> {
    app.try_state::<ControlEndpoint>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| {
            "control endpoint is not available yet (the control listener has not \
             finished binding); retry once the app has finished starting"
                .to_string()
        })
}

/// Decide which in-memory map entries are stale and must be evicted, given the
/// set of tmux sessions tmux reports as currently alive.
///
/// This is the self-reap predicate for genuinely-EXITED terminals. An entry is
/// stale — and ONLY stale — when its backing tmux session is absent from the
/// live set: the process tree ended, tmux tore the session down, and the reader
/// thread has already emitted `exit` + `state=Exited`, yet the dead `PtySession`
/// (retained master-PTY fd + joined reader handle) still sits in the map.
///
/// CRITICAL SAFETY INVARIANT — a DETACHED-but-running terminal can never match:
///   - Detach (`close_terminal`) REMOVES the entry from the map and deliberately
///     leaves the tmux session alive. So a detached terminal is not among the
///     `candidates` at all (nothing to evict), and even if it were its session is
///     present in `live_sessions` and so would be kept.
///   - The cross-check is against tmux itself (the source of truth for
///     liveness): only a session tmux no longer knows about is reaped, so we can
///     never evict a terminal whose process is still running.
///
/// `candidates` is `(id, tmux_session)` for each map entry that existed BEFORE
/// the tmux walk; `live_sessions` is the names from `tmux::list_sessions()`.
/// Returns the ids to remove.
///
/// RACE SAFETY — only entries that predate the tmux snapshot are considered: a
/// terminal `spawn_terminal` creates AFTER `list_sessions()` ran (tmux session
/// made, then inserted into the map) is absent from `live_sessions` purely
/// because the snapshot is stale, NOT because it exited. If such a fresh entry
/// were a candidate we'd wrongly reap a live terminal. Passing only the pre-walk
/// snapshot as `candidates` excludes it; it is re-evaluated on the next reconcile
/// with a fresh tmux view.
fn stale_session_ids<S: std::hash::BuildHasher>(
    candidates: &[(String, String)],
    live_sessions: &std::collections::HashSet<String, S>,
) -> Vec<String> {
    candidates
        .iter()
        .filter(|(_, tmux_session)| !live_sessions.contains(tmux_session))
        .map(|(id, _)| id.clone())
        .collect()
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
    remote: tauri::State<'_, RemotePtyManager>,
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

    // Mark this id FRESH so its first `attach_terminal` returns empty scrollback
    // (the frontend then draws one clean prompt via Ctrl-L instead of replaying the
    // reflow-prone capture). A later reattach-after-close is NOT fresh → real
    // scrollback. Preserves the in-process path's `has_live` fresh-vs-reattach signal.
    remote.fresh.lock().insert(id.clone());

    // Server-split M2a: spawn NO local PTY here. The detached tmux session is now
    // ready; the frontend's mount flow calls `attach_terminal`, which opens a
    // `RemotePty` against the control socket and begins streaming. (Previously this
    // spawned an in-process `pty::spawn_attach_client` into the TerminalManager —
    // that step moved to `attach_terminal` over the wire.)
    //
    // The Live state event is still emitted here so the freshly-created tile flips
    // out of its "starting" placeholder immediately, exactly as before; the
    // subsequent `attach_terminal` re-emits Live idempotently once it's streaming.
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
    remote: tauri::State<'_, RemotePtyManager>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    // Reconstruct the tmux session name from the id (the `th_<id[..8]>` derivation
    // `spawn_terminal` uses). Server-split M2a: there is no in-process PtySession to
    // read it off — the RemotePtyManager keys connections by id directly.
    let tmux_session = tmux_target(&id);

    // The tmux session must still exist to (re)attach; if it's gone the terminal
    // has been killed or exited and there is nothing to attach to. (The server-side
    // `serve_pty_attach` re-checks this too, but verifying here lets us return the
    // same clear error before opening a socket.)
    if !tmux::has_session(&tmux_session) {
        return Err(format!(
            "tmux session {tmux_session} for terminal {id} no longer exists"
        ));
    }

    // If a RemotePty already streams this id (the tile is visible), keep it and
    // just resize it to the freshly-mounted xterm geometry — no re-seed (matching
    // the old `has_live` branch, which returned empty scrollback). We hold the
    // manager lock only for the in-memory resize-frame write (a non-blocking socket
    // write of a tiny frame), never across a connect.
    {
        let mut conns = remote.conns.lock();
        if let Some(conn) = conns.get_mut(&id) {
            let _ = conn.resize(cols, rows);
            // Already streaming — re-affirm Live (idempotent) and return empty
            // scrollback (no re-seed), exactly as the in-process path did.
            let _ = app.emit(
                events::STATE,
                &StateEvent {
                    id: id.clone(),
                    state: TerminalState::Live,
                },
            );
            return Ok(String::new());
        }
    }

    // First attach (or re-attach after `close_terminal` detached it): open a new
    // RemotePty over the control socket. `connect` performs the attach_pty
    // handshake and returns the server's base64 scrollback (its opening frame),
    // which we hand straight back to the frontend — the same wire shape the old
    // path returned (a base64 string of the pane scrollback).
    let endpoint = control_endpoint(&app)?;
    let (conn, scrollback_b64) =
        RemotePty::connect(&app, &endpoint.addr, &endpoint.token, &id, cols, rows)?;
    remote.conns.lock().insert(id.clone(), conn);

    // Fresh spawn (spawn_terminal marked it) → return EMPTY scrollback so the
    // frontend draws a clean prompt via Ctrl-L instead of replaying the capture's
    // reflow cascade. A reattach (not in `fresh`) → the real scrollback, to restore
    // history. Mirrors the old `has_live` ? empty : capture_pane branch.
    let was_fresh = remote.fresh.lock().remove(&id);

    // A successful attach binds a remote PTY to this session, so the terminal is
    // unambiguously Live. After a reload the frontend may have seeded this terminal
    // as Detached (no live conn at list time) or never seeded it; without this
    // transition the tile would stay stuck on its initial dot (bug #16). Idempotent
    // for a tile that was already Live.
    let _ = app.emit(
        events::STATE,
        &StateEvent {
            id: id.clone(),
            state: TerminalState::Live,
        },
    );

    Ok(if was_fresh {
        String::new()
    } else {
        scrollback_b64
    })
}

#[tauri::command]
pub async fn write_terminal(
    remote: tauri::State<'_, RemotePtyManager>,
    id: String,
    data: String,
) -> Result<(), String> {
    // Write a `{"write"}` frame to the remote PTY. The lock is held only for the
    // small frame write (a non-blocking loopback send), never across a connect.
    let mut conns = remote.conns.lock();
    let conn = conns
        .get_mut(&id)
        .ok_or_else(|| format!("no live terminal {id}"))?;
    conn.write(data.as_bytes())
        .map_err(|e| format!("failed to write to terminal {id}: {e}"))
}

#[tauri::command]
pub async fn resize_terminal(
    remote: tauri::State<'_, RemotePtyManager>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let mut conns = remote.conns.lock();
    let conn = conns
        .get_mut(&id)
        .ok_or_else(|| format!("no live terminal {id}"))?;
    conn.resize(cols, rows)
}

#[tauri::command]
pub async fn close_terminal(
    remote: tauri::State<'_, RemotePtyManager>,
    id: String,
) -> Result<(), String> {
    // Detach the remote PTY and remove it from the map, but DO NOT kill the tmux
    // session: the backing process keeps running so the terminal survives the UI
    // closing the tile. Re-attaching later via `attach_terminal` opens a fresh
    // RemotePty against the still-alive session.
    //
    // We `remove` the conn (releasing the lock) BEFORE `detach`, so the manager
    // Mutex is never held across the blocking socket shutdown + reader-thread join.
    let conn = remote.conns.lock().remove(&id);
    if let Some(conn) = conn {
        // Shuts down the socket (the server detaches; tmux survives) and joins the
        // reader thread.
        conn.detach();
    }
    Ok(())
}

#[tauri::command]
pub async fn kill_terminal(
    remote: tauri::State<'_, RemotePtyManager>,
    id: String,
) -> Result<(), String> {
    // Stop for real: detach the remote PTY AND kill the tmux session (terminating
    // its process tree). Remove the conn (releasing the lock) before any blocking
    // socket op so the Mutex is never held across I/O.
    let conn = remote.conns.lock().remove(&id);

    // The tmux session name is reconstructed from the id (the RemotePty doesn't
    // carry it); this is the same `th_<id[..8]>` derivation as everywhere else.
    let tmux_session = tmux_target(&id);

    let kill_result = tmux::kill_session(&tmux_session)
        .map_err(|e| format!("failed to kill tmux session {tmux_session}: {e}"));

    // Detaching the RemotePty shuts down the socket + joins the reader; do this
    // regardless of whether the tmux kill reported an error. (Killing the tmux
    // session also closes the server-side attach client, so the connection would
    // EOF on its own; detaching here makes the teardown prompt + deterministic.)
    if let Some(conn) = conn {
        conn.detach();
    }

    kill_result
}

#[tauri::command]
pub async fn list_terminals(
    remote: tauri::State<'_, RemotePtyManager>,
) -> Result<Vec<TerminalInfo>, String> {
    // Snapshot what the reconciliation needs from the in-memory map BEFORE we
    // hop to a blocking thread: a `tmux_session -> canonical id` map. The closure
    // is `'static`, so it can't borrow `&State`; this owned snapshot is all the
    // matching logic uses (recover the canonical id, and Live vs Detached based
    // on whether this UI holds a connection for the session). Lock is released the
    // instant this block ends — never held across the blocking tmux walk.
    //
    // Server-split M2a: liveness is "this UI holds a RemotePty conn for the id".
    // The conn map is keyed by id (it doesn't carry the tmux session name), so we
    // reconstruct each one's session via the `th_<id[..8]>` derivation, matching
    // how every other command resolves it.
    let live_clients: std::collections::HashMap<String, String> = {
        let conns = remote.conns.lock();
        conns
            .keys()
            .map(|id| (tmux_target(id), id.clone()))
            .collect()
    };

    // Candidate set for the self-reap below: `(id, tmux_session)` for exactly the
    // entries that existed BEFORE the tmux walk. Limiting reaping to pre-walk
    // entries is what makes it race-safe against a concurrent `spawn_terminal`
    // (see `stale_session_ids`). Built from `live_clients` before it's moved into
    // the closure.
    let reap_candidates: Vec<(String, String)> = live_clients
        .iter()
        .map(|(tmux_session, id)| (id.clone(), tmux_session.clone()))
        .collect();

    // The two `wsl.exe` spawns (`list_sessions` + `pane_info`) each wait on a
    // child, which would pin a Tokio worker; run the whole reconcile off the
    // executor so a saturated worker pool can't stall the UI's IPC. The blocking
    // walk returns both the rendered `infos` AND the live tmux session set, so
    // the caller (which still holds `state`) can evict dead map entries without
    // the `'static` closure needing to borrow `&State`.
    let (infos, live_sessions): (Vec<TerminalInfo>, std::collections::HashSet<String>) =
        tauri::async_runtime::spawn_blocking(move || {
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
            let (id, state_val) = match live_clients.get(tmux_session) {
                Some(id) => (id.clone(), TerminalState::Live),
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

        // Hand back the live tmux session set too, so the caller can self-reap
        // genuinely-EXITED entries from the in-memory map (see below).
        let live_set: std::collections::HashSet<String> =
            live_sessions.into_iter().collect();
        Ok::<_, String>((infos, live_set))
    })
    .await
    .map_err(|e| format!("list_terminals task failed: {e}"))??;

    // Self-reap genuinely-EXITED terminals: any in-memory conn whose tmux session
    // is no longer in the live set has had its process tree end (tmux tore the
    // session down), so the server-side attach client EOF'd, the reader thread
    // already emitted `exit` + `state=Exited`, and the connection dropped — yet the
    // dead `RemotePty` (joined reader handle) still lingers in the map until the UI
    // happens to call `close_terminal`/`kill_terminal`. Evict it here, piggybacking
    // on this existing 5s reconcile.
    //
    // SAFETY — a DETACHED-but-running terminal is NEVER reaped: `close_terminal`
    // already removed it from the map (nothing to evict) and intentionally kept
    // its tmux session ALIVE, so its session is in `live_sessions` and the
    // predicate can't match. We only ever drop entries whose session tmux itself
    // reports as gone, so we can neither kill a live process nor double-free one:
    // we do NOT touch tmux here, only drop the already-dead in-memory handle.
    let stale_ids = stale_session_ids(&reap_candidates, &live_sessions);
    if !stale_ids.is_empty() {
        // Re-confirm UNDER THE LOCK that the entry we're about to drop STILL maps to
        // the same (now-dead) tmux session before dropping it. The conn map doesn't
        // store the session name, but the id→session derivation is deterministic
        // (`tmux_target`), so re-deriving it and comparing to the candidate's
        // recorded session makes the "still the same dead entry" invariant explicit
        // (belt-and-braces: ids are unique today, so a replacement can't reuse an id).
        let expected: std::collections::HashMap<&str, &str> = reap_candidates
            .iter()
            .map(|(id, t)| (id.as_str(), t.as_str()))
            .collect();
        let mut dead = Vec::new();
        {
            let mut conns = remote.conns.lock();
            for id in &stale_ids {
                let still_backs = conns.contains_key(id)
                    && expected
                        .get(id.as_str())
                        .copied()
                        .is_some_and(|exp| tmux_target(id) == exp);
                if still_backs {
                    if let Some(c) = conns.remove(id) {
                        dead.push(c);
                    }
                }
            }
        }
        // Drop the dead `RemotePty`s OUTSIDE the lock: each `Drop` best-effort shuts
        // down an already-closed socket + joins the already-exited reader thread
        // (no-ops on a dropped connection), so it can't block or double-free. We
        // never touch tmux here — the session is already gone.
        drop(dead);
    }

    Ok(infos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn live(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn candidate(id: &str, session: &str) -> (String, String) {
        (id.to_string(), session.to_string())
    }

    /// An EXITED terminal — in the map but its tmux session has vanished from the
    /// live set (process tree ended, tmux tore the session down) — is reaped.
    #[test]
    fn reconcile_evicts_entry_whose_tmux_session_is_gone() {
        let candidates = vec![candidate("aaaa1111", "th_aaaa1111")];
        // tmux reports NO sessions (the exited terminal's was the only one).
        let live = live(&[]);

        let stale = stale_session_ids(&candidates, &live);
        assert_eq!(
            stale,
            vec!["aaaa1111".to_string()],
            "an entry whose tmux session is gone must be evicted"
        );
    }

    /// A DETACHED-but-running terminal must NEVER be reaped. After `close_terminal`
    /// it isn't even in the map; here we model the stricter case where (somehow) an
    /// entry remains AND its tmux session is still alive — the live-tmux cross-check
    /// keeps it. This is the core safety guarantee.
    #[test]
    fn reconcile_keeps_detached_but_alive_session() {
        let candidates = vec![candidate("bbbb2222", "th_bbbb2222")];
        // The session is still alive on the socket (detached, process running).
        let live = live(&["th_bbbb2222"]);

        let stale = stale_session_ids(&candidates, &live);
        assert!(
            stale.is_empty(),
            "a terminal whose tmux session is still alive must NOT be reaped, got {stale:?}"
        );
    }

    /// Mixed map: one exited (session gone) and one live (session present). Only the
    /// exited one is evicted; the live one is untouched.
    #[test]
    fn reconcile_evicts_only_the_dead_entry() {
        let candidates = vec![
            candidate("dead0001", "th_dead0001"),
            candidate("live0002", "th_live0002"),
        ];
        let live = live(&["th_live0002"]);

        let stale = stale_session_ids(&candidates, &live);
        assert_eq!(stale, vec!["dead0001".to_string()]);
    }

    /// RACE SAFETY: a terminal spawned AFTER the tmux walk is absent from the live
    /// set yet must not be reaped. Modeled by it NOT being among the pre-walk
    /// `candidates`: even though its session isn't in `live`, it's never considered.
    #[test]
    fn reconcile_ignores_entries_created_after_the_tmux_walk() {
        // Only the pre-walk entry is a candidate; the freshly-spawned `new00099`
        // is deliberately absent from `candidates`.
        let candidates = vec![candidate("old00001", "th_old00001")];
        // tmux now has the new session but not the old (which exited).
        let live = live(&["th_new00099"]);

        let stale = stale_session_ids(&candidates, &live);
        assert_eq!(
            stale,
            vec!["old00001".to_string()],
            "only the pre-walk entry is reaped; the post-walk spawn is never a candidate"
        );
    }

    /// Empty inputs are well-behaved: no candidates ⇒ nothing to reap.
    #[test]
    fn reconcile_no_candidates_is_noop() {
        let stale = stale_session_ids(&[], &live(&["th_anything"]));
        assert!(stale.is_empty());
    }
}
