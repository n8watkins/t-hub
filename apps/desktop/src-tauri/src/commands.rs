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
    // Snapshot what the reconciliation needs from the in-memory map BEFORE we
    // hop to a blocking thread: a `tmux_session -> canonical id` map. The closure
    // is `'static`, so it can't borrow `&State`; this owned snapshot is all the
    // matching logic uses (recover the canonical id, and Live vs Detached based
    // on whether this UI holds a client for the session). Lock is released the
    // instant this block ends — never held across the blocking tmux walk.
    let live_clients: std::collections::HashMap<String, String> = {
        let sessions = state.sessions.lock();
        sessions
            .values()
            .map(|s| (s.tmux_session.clone(), s.id.clone()))
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

    // Self-reap genuinely-EXITED terminals: any in-memory map entry whose tmux
    // session is no longer in the live set has had its process tree end (tmux
    // tore the session down), so the reader thread already emitted `exit` +
    // `state=Exited` but the dead `PtySession` (retained master-PTY fd + joined
    // reader handle) still lingers in the map until the UI happens to call
    // `close_terminal`/`kill_terminal`. Evict it here, piggybacking on this
    // existing 5s reconcile.
    //
    // SAFETY — a DETACHED-but-running terminal is NEVER reaped: `close_terminal`
    // already removed it from the map (nothing to evict) and intentionally kept
    // its tmux session ALIVE, so its session is in `live_sessions` and the
    // predicate can't match. We only ever drop entries whose session tmux itself
    // reports as gone, so we can neither kill a live process nor double-free one:
    // we do NOT touch tmux here, only drop the already-dead in-memory handle.
    let stale_ids = stale_session_ids(&reap_candidates, &live_sessions);
    if !stale_ids.is_empty() {
        // The tmux session we judged stale for each candidate id. We re-confirm
        // UNDER THE LOCK that the entry STILL backs that same (now-dead) session
        // before dropping it, so a concurrent op that replaced the entry under this
        // id can never make us drop a LIVE PtySession (belt-and-braces: ids are
        // unique today, so a replacement can't reuse an id — this just makes the
        // invariant explicit instead of removing by id alone).
        let expected: std::collections::HashMap<&str, &str> = reap_candidates
            .iter()
            .map(|(id, t)| (id.as_str(), t.as_str()))
            .collect();
        let mut dead = Vec::new();
        {
            let mut sessions = state.sessions.lock();
            for id in &stale_ids {
                let still_backs = sessions
                    .get(id)
                    .zip(expected.get(id.as_str()).copied())
                    .is_some_and(|(entry, exp)| entry.tmux_session.as_str() == exp);
                if still_backs {
                    if let Some(s) = sessions.remove(id) {
                        dead.push(s);
                    }
                }
            }
        }
        // Drop the dead `PtySession`s OUTSIDE the lock: each `Drop` best-effort kills
        // an already-gone attach client + joins the already-exited reader thread
        // (no-ops on an ended process), so it can't block or double-free. We never
        // touch tmux here — the session is already gone.
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
