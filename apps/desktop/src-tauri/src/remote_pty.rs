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
//!   - `{"exit":<code|null>}` once on the attach client's exit,
//!   - `{"keepalive":"..."}` on an idle stream (ignorable padding the server writes
//!     to reap a gone/stalled client; [`parse_pty_frame`] drops it like any frame
//!     without `out`/`exit`).
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
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
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

/// How long the reader keeps gathering more `{"out"}` frames before flushing a
/// pending batch as ONE `terminal://output` emit. A redraw-heavy TUI (Claude's
/// spinner, streaming tokens, full-screen repaints) produces many small chunks;
/// without coalescing that was one webview event per chunk — a sustained
/// hundreds/sec IPC stream per terminal that backed up against the main thread
/// (notably while a window drag parks it in the OS modal loop). The window is
/// only applied WHILE a batch is pending, so an idle terminal still does a plain
/// blocking read (no busy-poll), and it is well under the frontend's ~16 ms rAF
/// flush so the added echo latency is imperceptible.
const COALESCE_WINDOW: Duration = Duration::from_millis(8);
/// Flush a pending batch the moment it reaches this many DECODED bytes, so a
/// firehose stays responsive (and memory bounded) even within one window.
const MAX_BATCH_BYTES: usize = 256 * 1024;
/// Raw socket read size. A single read can carry several NDJSON frames, which the
/// loop parses out of its own accumulation buffer.
const RECV_BUF: usize = 16 * 1024;

/// One parsed PTY wire frame. Factored out of [`reader_loop`] so the framing —
/// `{"out":"<b64>"}` / `{"exit":<code>}` / everything-else — has a single
/// definition that is unit-testable without a socket or an `AppHandle`.
#[derive(Debug, PartialEq)]
enum PtyFrame {
    /// Decoded output bytes (the server's base64 already undone).
    Output(Vec<u8>),
    /// The process exited; `Option<i32>` is the exit code when known.
    Exit(Option<i32>),
    /// A blank line, a malformed frame, or any other shape (e.g. a late
    /// `{"scrollback"}` or the server's idle `{"keepalive"}`) — skipped without
    /// tearing the stream down.
    Ignore,
}

/// Parse one NDJSON line (without the trailing newline) into a [`PtyFrame`]. A
/// blank line, non-JSON, or un-decodable base64 yields [`PtyFrame::Ignore`] so a
/// single bad frame can never tear down the terminal.
fn parse_pty_frame(line: &[u8]) -> PtyFrame {
    if line.iter().all(|b| b.is_ascii_whitespace()) {
        return PtyFrame::Ignore;
    }
    let frame: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(_) => return PtyFrame::Ignore,
    };
    if let Some(b64) = frame.get("out").and_then(|v| v.as_str()) {
        match STANDARD.decode(b64) {
            Ok(bytes) => PtyFrame::Output(bytes),
            Err(_) => PtyFrame::Ignore,
        }
    } else if let Some(exit) = frame.get("exit") {
        PtyFrame::Exit(exit.as_i64().and_then(|c| i32::try_from(c).ok()))
    } else {
        PtyFrame::Ignore
    }
}

/// Emit the accumulated output `batch` as a single base64 `terminal://output`
/// event, then clear it. A no-op when empty. (We re-encode the COMBINED bytes
/// once rather than per source frame, so N coalesced chunks cost one emit.)
fn emit_batch(app: &AppHandle, id: &str, batch: &mut Vec<u8>) {
    if batch.is_empty() {
        return;
    }
    let payload = OutputEvent {
        id: id.to_string(),
        base64: STANDARD.encode(&batch),
    };
    // If emit fails the window is gone; we still clear so the buffer doesn't grow.
    crate::hangwatch::note_emit(); // count toward the main-thread emit-rate watchdog
    let _ = app.emit(events::OUTPUT, &payload);
    batch.clear();
}

/// The attach stream ended — an explicit `{"exit"}` frame, or EOF/error on the
/// socket. Neither PROVES the pane's process exited: the server-side attach
/// client also exits on a detach (`tmux detach-client`), and the connection also
/// drops when the control server churns/restarts — in both cases the tmux
/// session (and the user's process) is alive and well. Emitting `Exited` there
/// is the false-dead-tile bug: the tile renders "[process exited]" over a live
/// session.
///
/// So verify against tmux — the source of truth for liveness — before declaring
/// death:
///   - session DEFINITIVELY gone → the process really ended: emit `EXIT` + `STATE
///     Exited`, exactly the old behavior;
///   - session alive, OR liveness INDETERMINATE → treat as an ATTACH loss: emit
///     `STATE Detached` (no `EXIT`), which the frontend's auto-reattach picks up. A
///     clean local `detach()`/`close_terminal` also lands here — `Detached` is the
///     truthful state for that too (the tile is gone, the event is a harmless no-op).
///
/// De-conflation (spawn-wedge): a probe that TIMES OUT (`Unknown`) must NOT be read
/// as death - emitting a spurious `EXIT` would tear a live tile out of the UI on a
/// transient control-plane stall. Only a DEFINITIVE `Gone` emits `EXIT`; `Unknown`
/// falls through to `Detached`, which auto-reattach retries (and a real exit is
/// confirmed by the next probe).
///
/// The liveness probe shells out to tmux; this runs on the (terminating) reader
/// thread, so the cost is off every hot path. NOTE: the check runs on the CLIENT
/// host — correct while the control endpoint is loopback (M2a); when M2 points this
/// at a remote host, liveness must be asked of the remote server instead.
fn emit_stream_end(app: &AppHandle, id: &str, code: Option<i32>) {
    let gone = crate::tmux::is_definitively_gone(crate::tmux::session_liveness(
        &crate::tmux::target_for_id(id),
    ));
    if !gone {
        let _ = app.emit(
            events::STATE,
            &StateEvent {
                id: id.to_string(),
                state: TerminalState::Detached,
            },
        );
        return;
    }
    let _ = app.emit(
        events::EXIT,
        &ExitEvent {
            id: id.to_string(),
            code,
        },
    );
    let _ = app.emit(
        events::STATE,
        &StateEvent {
            id: id.to_string(),
            state: TerminalState::Exited,
        },
    );
}

/// Drain the socket's frames, re-emitting into the webview like
/// [`crate::pty::reader_loop`], but COALESCING bursts of `{"out"}` frames into one
/// `terminal://output` emit per [`COALESCE_WINDOW`] (or per [`MAX_BATCH_BYTES`]):
///   - `{"out":"<b64>"}`  → decoded + appended to the pending batch,
///   - `{"exit":<code>}`  → flush the batch, then [`emit_stream_end`] (verified
///     against tmux: Exited only when the session is really gone, Detached else),
///   - EOF (connection closed without an `{"exit"}`) → flush, then the same
///     verified transition with `code: None`, so a server/connection drop over a
///     LIVE session reads as an attach loss (Detached), not a false exit.
fn reader_loop(app: AppHandle, id: String, reader: BufReader<TcpStream>) {
    // The handshake's BufReader may already hold bytes past the scrollback frame
    // (the first `out` frames can ride the same TCP segment). Drain that buffered
    // tail into our accumulator BEFORE switching to raw, timeout-toggled reads, so
    // no output is lost or reordered.
    let mut acc: Vec<u8> = reader.buffer().to_vec();
    let stream = reader.into_inner();

    let mut batch: Vec<u8> = Vec::new();
    let mut buf = [0u8; RECV_BUF];
    let mut saw_exit = false;

    'read: loop {
        // Parse every COMPLETE line currently in `acc` before blocking again. A
        // partial trailing line stays in `acc` for the next read.
        while let Some(pos) = acc.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = acc.drain(..=pos).collect();
            match parse_pty_frame(&line[..line.len() - 1]) {
                PtyFrame::Output(bytes) => {
                    batch.extend_from_slice(&bytes);
                    if batch.len() >= MAX_BATCH_BYTES {
                        emit_batch(&app, &id, &mut batch);
                    }
                }
                PtyFrame::Exit(code) => {
                    // Flush any output that preceded the exit so order is preserved.
                    emit_batch(&app, &id, &mut batch);
                    emit_stream_end(&app, &id, code);
                    saw_exit = true;
                    break 'read;
                }
                PtyFrame::Ignore => {}
            }
        }

        // Coalesce: block indefinitely when nothing is pending (no busy-poll), but
        // cap the wait at COALESCE_WINDOW once a batch is building so it flushes
        // promptly. A raw `read` into our own buffer means a timeout consumes
        // nothing (no partial-line corruption) — unlike a timed `read_line`.
        let pending = !batch.is_empty();
        let _ = stream.set_read_timeout(if pending {
            Some(COALESCE_WINDOW)
        } else {
            None
        });
        match (&stream).read(&mut buf) {
            Ok(0) => break, // EOF: server detached / connection dropped.
            Ok(n) => acc.extend_from_slice(&buf[..n]),
            // The coalesce window elapsed with a batch pending → flush it.
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                emit_batch(&app, &id, &mut batch);
            }
            Err(_) => break, // a torn-down connection (shutdown) surfaces here.
        }
    }

    // Flush whatever output was still pending when the stream ended.
    emit_batch(&app, &id, &mut batch);

    // If the stream ended WITHOUT an explicit {"exit"} (a server/connection drop
    // mid-stream), still emit a verified terminal transition so the tile doesn't
    // hang "live": Exited when the tmux session is really gone, Detached when the
    // session survived the drop (attach churn — the frontend auto-reattaches). On
    // a clean `detach()` the user already removed the tile, so the (Detached)
    // state event lands in a webview that no longer renders it.
    if !saw_exit {
        emit_stream_end(&app, &id, None);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn out_frame(bytes: &[u8]) -> Vec<u8> {
        format!("{{\"out\":\"{}\"}}", STANDARD.encode(bytes)).into_bytes()
    }

    #[test]
    fn parses_out_frame_decoding_base64() {
        assert_eq!(
            parse_pty_frame(&out_frame(b"hello\x1b[0m")),
            PtyFrame::Output(b"hello\x1b[0m".to_vec())
        );
    }

    #[test]
    fn parses_exit_frame_with_and_without_code() {
        assert_eq!(parse_pty_frame(br#"{"exit":0}"#), PtyFrame::Exit(Some(0)));
        assert_eq!(parse_pty_frame(br#"{"exit":137}"#), PtyFrame::Exit(Some(137)));
        // A null/absent exit code → Exit(None) (signalled / unknown).
        assert_eq!(parse_pty_frame(br#"{"exit":null}"#), PtyFrame::Exit(None));
    }

    #[test]
    fn ignores_blank_malformed_undecodable_and_other_frames() {
        assert_eq!(parse_pty_frame(b""), PtyFrame::Ignore);
        assert_eq!(parse_pty_frame(b"   \t"), PtyFrame::Ignore);
        assert_eq!(parse_pty_frame(b"not json"), PtyFrame::Ignore);
        // Well-formed JSON but `out` isn't valid base64 → skipped, not a panic.
        assert_eq!(parse_pty_frame(br#"{"out":"!!!not base64!!!"}"#), PtyFrame::Ignore);
        // A late/unknown frame shape (e.g. scrollback) is ignored.
        assert_eq!(parse_pty_frame(br#"{"scrollback":"x"}"#), PtyFrame::Ignore);
        // The server's idle keepalive is a no-op here: it carries no `out`/`exit`,
        // so it must drop silently (the s27 idle-leak fix relies on this contract).
        assert_eq!(parse_pty_frame(br#"{"keepalive":"...."}"#), PtyFrame::Ignore);
    }

    #[test]
    fn coalescing_two_out_frames_concatenates_their_decoded_bytes() {
        // The reader appends each Output frame's bytes to one batch; the emitted
        // base64 is the COMBINED bytes (re-encoded once), so the frontend sees the
        // same stream it would have from two separate emits.
        let mut batch = Vec::new();
        for chunk in [b"foo".as_slice(), b"bar".as_slice()] {
            if let PtyFrame::Output(b) = parse_pty_frame(&out_frame(chunk)) {
                batch.extend_from_slice(&b);
            }
        }
        assert_eq!(batch, b"foobar");
        assert_eq!(STANDARD.encode(&batch), STANDARD.encode(b"foobar"));
    }
}
