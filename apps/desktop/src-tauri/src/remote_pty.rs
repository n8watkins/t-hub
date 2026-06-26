//! Client-side **remote-PTY transport** (server-split M2a).
//!
//! ## Why this exists
//! Today a terminal tile is backed by an in-process `portable-pty` master running
//! a `tmux attach` client ([`crate::pty::PtySession`]), wired straight into the
//! webview by `commands.rs`. M2a routes that same byte stream through the loopback
//! **control socket** instead: the server half (already live in
//! [`crate::control`] — `ATTACH_PTY_COMMAND` + `serve_pty_attach`) owns the PTY,
//! captures scrollback, spawns the `tmux attach`, and streams `{"out"}` / `{"exit"}`
//! frames down while reading `{"write"}` / `{"resize"}` frames back up. This module
//! is the **client** for that protocol: it opens the TCP connection, performs the
//! `attach_pty` handshake, and re-emits the socket's frames into the webview on the
//! exact same Tauri channels (`terminal://output|state|exit`) the in-process PTY
//! reader thread used — so the frontend is byte-for-byte unchanged.
//!
//! On localhost this loops back through the OS TCP stack — the SAME wire M2
//! stretches to a remote host; only the endpoint addr changes then.
//!
//! ## Wire protocol (mirrors [`crate::control::serve_pty_attach`])
//! After connecting we send ONE request line:
//! ```text
//! {"token":TOK,"command":"attach_pty","args":{"sessionId":ID,"cols":C,"rows":R}}
//! ```
//! Then the server streams newline-delimited JSON frames:
//!   - `{"scrollback":"<b64>"}` once (the opening frame; we decode + return it),
//!   - `{"out":"<b64>"}` per output chunk,
//!   - `{"exit":<code|null>}` once on the attach client's exit.
//! And we send back:
//!   - `{"write":"<b64>"}` for keystrokes,
//!   - `{"resize":{"cols":C,"rows":R}}` for geometry.
//! Disconnecting (we `shutdown` the socket on detach/Drop) makes the server detach;
//! the tmux SESSION survives, exactly like `close_terminal`.
//!
//! ## Concurrency
//! The reader thread reads frames off its own clone of the `TcpStream` and emits
//! into the webview; `write`/`resize` run on the command thread and write to a
//! SEPARATE clone of the stream (`writer`). Two clones of one TCP connection are
//! independently usable for the two directions, so the two never interleave a
//! partial frame. On detach we `shutdown(Both)` the stream, which unblocks the
//! reader's blocking `read_line` (it returns EOF), then we `join` the thread — no
//! leak, no hang. The manager `Mutex` is never held across the UNBOUNDED socket
//! ops (`connect`/`shutdown`/`join`): `commands.rs` `connect`s before inserting and
//! `remove`s the conn (releasing the lock) before `detach`. The one op that DOES
//! run under the lock is the `write`/`resize` frame write — bounded by
//! [`WRITE_TIMEOUT`] so a stalled remote peer errors out rather than deadlocking
//! the terminal commands.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::thread::JoinHandle;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::commands::TerminalState;
use crate::control::ATTACH_PTY_COMMAND;
use crate::events::{self, ExitEvent, OutputEvent, StateEvent};

/// How long to wait for the loopback connect before giving up. Generous for a
/// same-host round-trip; M2 may widen this for a remote server. We do NOT set a
/// read timeout on the stream: the reader thread blocks indefinitely on the live
/// stream and is unblocked by a `shutdown`, not a timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Write timeout on the per-conn writer. `write`/`resize` send their frame while
/// the command holds the manager `Mutex`, so without a bound a stalled peer (its
/// kernel recv buffer full) would block the write under the lock and deadlock ALL
/// terminal commands. Harmless on loopback (the server drains promptly); it matters
/// once M2 binds this to a remote/Tailscale host. On timeout the write errors and
/// the command returns a clear error instead of hanging. (Symmetric to the event
/// fanout's subscriber write timeout in `control::EventFanout::register`.)
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// A live remote-PTY connection for one terminal tile. Holds the WRITE half of the
/// socket (a `TcpStream` clone) for `write`/`resize`, plus the reader thread handle
/// (joined on detach/Drop). The reader thread owns the read half and emits the
/// socket's output/exit frames into the webview via a captured [`AppHandle`].
pub struct RemotePty {
    /// The T-Hub terminal id this connection streams.
    id: String,
    /// Write half: a clone of the connection, used to send `{"write"}`/`{"resize"}`
    /// frames. Distinct from the reader thread's clone so the two directions never
    /// interleave a partial frame on the wire.
    writer: TcpStream,
    /// The reader thread, joined on detach/Drop so it never outlives us.
    reader: Option<JoinHandle<()>>,
    /// Last known geometry, so `resize` can no-op an unchanged size (matching
    /// [`crate::pty::PtySession::resize`]): xterm's `fit` addon fires resize
    /// liberally and a redundant resize raises a spurious SIGWINCH some TUIs
    /// repaint on.
    cols: u16,
    rows: u16,
}

impl RemotePty {
    /// Open a connection to the control endpoint, perform the `attach_pty`
    /// handshake, read the opening `{"scrollback"}` frame, and spawn the reader
    /// thread. Returns the assembled [`RemotePty`] AND the decoded scrollback
    /// string (the command hands it straight back to the frontend, exactly as the
    /// old in-process path returned `tmux::capture_pane` as base64 — except here
    /// the server already base64-encoded it, so we return the raw base64 string).
    ///
    /// A `{"error":...}` opening frame (e.g. the tmux session vanished server-side)
    /// is surfaced as an `Err` and no thread is spawned.
    pub fn connect(
        app: &AppHandle,
        addr: &str,
        token: &str,
        id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(Self, String), String> {
        let socket: SocketAddr = addr
            .parse()
            .map_err(|e| format!("remote_pty: bad control addr {addr:?}: {e}"))?;
        let stream = TcpStream::connect_timeout(&socket, CONNECT_TIMEOUT)
            .map_err(|e| format!("remote_pty: connect to {addr} failed: {e}"))?;
        // No read timeout: the reader thread blocks on the live stream and is
        // unblocked by `shutdown`, not a timeout.

        // The write half used by this struct for write/resize. A WRITE timeout
        // bounds the frame write (which runs under the manager lock) so a stalled
        // remote peer can't deadlock the terminal commands — see WRITE_TIMEOUT.
        let writer = stream
            .try_clone()
            .map_err(|e| format!("remote_pty: clone stream failed: {e}"))?;
        let _ = writer.set_write_timeout(Some(WRITE_TIMEOUT));

        // Send the attach_pty handshake on the (soon-to-be) read half.
        let mut handshake = stream
            .try_clone()
            .map_err(|e| format!("remote_pty: clone stream failed: {e}"))?;
        let mut frame = serde_json::to_vec(&json!({
            "token": token,
            "command": ATTACH_PTY_COMMAND,
            "args": { "sessionId": id, "cols": cols, "rows": rows },
            "v": crate::control::PROTOCOL_VERSION,
        }))
        .map_err(|e| format!("remote_pty: serialize attach_pty failed: {e}"))?;
        frame.push(b'\n');
        handshake
            .write_all(&frame)
            .and_then(|()| handshake.flush())
            .map_err(|e| format!("remote_pty: write attach_pty failed: {e}"))?;

        // Read the opening frame: either {"scrollback":...} (success), {"error":...}
        // (server refused — e.g. tmux session gone), or an {"ok":false,...} control
        // response (bad token — same socket, normal response framing).
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("remote_pty: read scrollback frame failed: {e}"))?;
        if n == 0 {
            return Err("remote_pty: connection closed before the scrollback frame".to_string());
        }
        let opening: Value = serde_json::from_str(line.trim())
            .map_err(|e| format!("remote_pty: malformed opening frame: {e}"))?;
        // A bad token comes back as a normal control response, not a frame.
        if opening.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            return Err(opening
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("remote_pty: attach_pty rejected")
                .to_string());
        }
        if let Some(err) = opening.get("error").and_then(|v| v.as_str()) {
            return Err(err.to_string());
        }
        let scrollback_b64 = opening
            .get("scrollback")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                format!("remote_pty: expected a scrollback frame, got: {}", line.trim())
            })?
            .to_string();

        // Spawn the reader thread: it owns `reader` (the read half) and re-emits
        // each {"out"}/{"exit"} frame into the webview via a cheap AppHandle clone.
        let app_for_thread = app.clone();
        let id_for_thread = id.to_string();
        let handle = std::thread::Builder::new()
            .name(format!("t-hub-remote-pty-{id}"))
            .spawn(move || reader_loop(app_for_thread, id_for_thread, reader))
            .map_err(|e| format!("remote_pty: spawn reader thread failed: {e}"))?;

        Ok((
            Self {
                id: id.to_string(),
                writer,
                reader: Some(handle),
                cols,
                rows,
            },
            scrollback_b64,
        ))
    }

    /// Send keystrokes to the remote PTY: `{"write":"<b64>"}`.
    pub fn write(&mut self, data: &[u8]) -> Result<(), String> {
        let mut frame = serde_json::to_vec(&json!({ "write": STANDARD.encode(data) }))
            .map_err(|e| format!("remote_pty: serialize write frame failed: {e}"))?;
        frame.push(b'\n');
        self.writer
            .write_all(&frame)
            .and_then(|()| self.writer.flush())
            .map_err(|e| format!("remote_pty: write to terminal {} failed: {e}", self.id))
    }

    /// Resize the remote PTY: `{"resize":{"cols":C,"rows":R}}`. No-ops when the
    /// geometry is unchanged (matching [`crate::pty::PtySession::resize`]).
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), String> {
        if self.cols == cols && self.rows == rows {
            return Ok(());
        }
        let mut frame = serde_json::to_vec(&json!({ "resize": { "cols": cols, "rows": rows } }))
            .map_err(|e| format!("remote_pty: serialize resize frame failed: {e}"))?;
        frame.push(b'\n');
        self.writer
            .write_all(&frame)
            .and_then(|()| self.writer.flush())
            .map_err(|e| format!("remote_pty: resize terminal {} failed: {e}", self.id))?;
        self.cols = cols;
        self.rows = rows;
        Ok(())
    }

    /// Detach: shut down the socket so the server detaches (the tmux SESSION
    /// survives, like `close_terminal`), then join the reader thread. Shutting down
    /// `Both` makes the reader's blocking `read_line` return EOF, so the thread
    /// exits and the join can't hang. Mirrors [`crate::pty::PtySession::detach`].
    pub fn detach(mut self) {
        self.shutdown_and_join();
    }

    /// Shared teardown for `detach` + `Drop`: best-effort shutdown the connection
    /// (unblocking the reader) and join the thread. Idempotent — a second call sees
    /// `reader == None` and is a no-op.
    fn shutdown_and_join(&mut self) {
        // Best-effort: the peer may already be gone (the attach client exited and
        // the server closed the connection), in which case shutdown errors harmlessly.
        let _ = self.writer.shutdown(Shutdown::Both);
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for RemotePty {
    fn drop(&mut self) {
        // Safety net: if a RemotePty is dropped without `detach()` (e.g. via the
        // self-reap in `list_terminals`, or `kill_terminal` removing it), make sure
        // the connection is shut down and the reader thread joined so we never leak
        // a socket or a detached thread.
        self.shutdown_and_join();
    }
}

/// Drain the socket's frames, re-emitting into the webview EXACTLY like
/// [`crate::pty::reader_loop`]:
///   - `{"out":"<b64>"}`  → `app.emit(OUTPUT, OutputEvent { id, base64 })`,
///   - `{"exit":<code>}`  → `app.emit(EXIT, ExitEvent { id, code })` then
///                          `app.emit(STATE, StateEvent { id, Exited })`,
///   - EOF (connection closed without an `{"exit"}`) → same Exited transition with
///     `code: None`, so a server/connection drop still tears the tile down cleanly
///     rather than leaving it stuck "live".
///
/// NOTE: the server already base64-encodes each `out` chunk, so we forward the
/// base64 string straight through (the old in-process loop encoded raw PTY bytes;
/// here the encode happened on the server). The frontend decodes it identically.
fn reader_loop(app: AppHandle, id: String, mut reader: BufReader<TcpStream>) {
    let mut line = String::new();
    let mut saw_exit = false;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF: the server detached / the connection dropped.
            Ok(_) => {}
            Err(_) => break, // a torn-down connection (shutdown) surfaces here.
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let frame: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue, // skip a malformed frame rather than tearing down
        };
        if let Some(b64) = frame.get("out").and_then(|v| v.as_str()) {
            // The server already base64-encoded the chunk; forward it as-is.
            let payload = OutputEvent {
                id: id.clone(),
                base64: b64.to_string(),
            };
            // If emit fails the window is gone; keep draining so the socket buffer
            // doesn't back up against the server.
            let _ = app.emit(events::OUTPUT, &payload);
        } else if let Some(exit) = frame.get("exit") {
            let code = exit.as_i64().and_then(|c| i32::try_from(c).ok());
            let _ = app.emit(events::EXIT, &ExitEvent { id: id.clone(), code });
            let _ = app.emit(
                events::STATE,
                &StateEvent {
                    id: id.clone(),
                    state: TerminalState::Exited,
                },
            );
            saw_exit = true;
            break;
        }
        // Any other frame (e.g. a late {"scrollback"} — shouldn't happen) is ignored.
    }

    // If the stream ended WITHOUT an explicit {"exit"} (a server/connection drop
    // mid-stream), still emit a terminal Exited transition so the tile doesn't hang
    // "live". On a clean `detach()` the user already removed the tile, so this is a
    // harmless idempotent state event into a webview that no longer renders it.
    if !saw_exit {
        let _ = app.emit(events::EXIT, &ExitEvent { id: id.clone(), code: None });
        let _ = app.emit(
            events::STATE,
            &StateEvent {
                id,
                state: TerminalState::Exited,
            },
        );
    }
}

/// App-wide registry of live remote-PTY connections, keyed by T-Hub id. Mirrors
/// [`crate::commands::TerminalManager`] but holds socket-backed [`RemotePty`]s
/// instead of in-process `PtySession`s. Managed in Tauri state; `commands.rs`
/// pulls a [`RemotePty`] OUT of the map (releasing the lock) before any blocking
/// socket op, so the `Mutex` is never held across I/O.
#[derive(Default)]
pub struct RemotePtyManager {
    pub conns: Mutex<HashMap<String, RemotePty>>,
    /// Ids that were just `spawn_terminal`'d but not yet attached. A FRESH spawn's
    /// `attach_terminal` returns EMPTY scrollback (the frontend `Terminal.tsx` then
    /// reads `seed.length === 0` as "fresh" and draws one clean prompt via Ctrl-L)
    /// instead of replaying the reflow-prone pane capture; a reattach (id NOT in
    /// this set) returns the real scrollback to restore history. This preserves the
    /// exact fresh-vs-reattach signal the in-process path encoded via `has_live`.
    pub fresh: Mutex<HashSet<String>>,
}
