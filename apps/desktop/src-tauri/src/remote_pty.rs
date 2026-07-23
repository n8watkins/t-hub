//! Client-side **remote-PTY transport** (server-split M2a).
//!
//! ## Why this exists
//! Today a terminal tile is backed by an in-process `portable-pty` master running
//! a `tmux attach` client ([`crate::pty::PtySession`]), wired straight into the
//! webview by `commands.rs`. M2a routes that same byte stream through the loopback
//! **control socket** instead: the server half (already live in
//! [`crate::control`] — `ATTACH_PTY_COMMAND` + `serve_pty_attach`) owns the PTY,
//! sends an empty compatibility seed, spawns the `tmux attach`, and streams
//! `{"out"}` / `{"exit"}`
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

use std::collections::HashMap;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::{Arc, Condvar, Mutex as StdMutex};
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
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
struct ProbeProgress {
    acknowledged: u64,
    closed: bool,
}

#[derive(Default)]
struct ProbeChannel {
    progress: StdMutex<ProbeProgress>,
    changed: Condvar,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GenerationState {
    Pending,
    Current,
    Retired,
}

struct GenerationProgress {
    state: GenerationState,
    closed: bool,
    pending: Vec<ReaderEvent>,
    pending_bytes: usize,
}

struct GenerationAuthority {
    progress: StdMutex<GenerationProgress>,
    changed: Condvar,
    sink: Arc<dyn Fn(ReaderEvent) + Send + Sync>,
}

const MAX_PENDING_AUTHORITY_BYTES: usize = MAX_BATCH_BYTES * 4;

impl GenerationAuthority {
    fn new(sink: Arc<dyn Fn(ReaderEvent) + Send + Sync>) -> Self {
        Self {
            progress: StdMutex::new(GenerationProgress {
                state: GenerationState::Pending,
                closed: false,
                pending: Vec::new(),
                pending_bytes: 0,
            }),
            changed: Condvar::new(),
            sink,
        }
    }

    fn activate_if_open(&self, on_activated: impl FnOnce()) -> bool {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        if progress.closed || progress.state != GenerationState::Pending {
            progress.state = GenerationState::Retired;
            progress.pending.clear();
            progress.pending_bytes = 0;
            self.changed.notify_all();
            return false;
        }
        progress.state = GenerationState::Current;
        on_activated();
        for event in progress.pending.drain(..) {
            (self.sink)(event);
        }
        progress.pending_bytes = 0;
        self.changed.notify_all();
        true
    }

    fn retire(&self) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        progress.state = GenerationState::Retired;
        progress.pending.clear();
        progress.pending_bytes = 0;
        self.changed.notify_all();
    }

    fn mark_closed(&self) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        progress.closed = true;
        self.changed.notify_all();
    }

    fn emit_if_current(&self, emit: impl FnOnce()) -> bool {
        let progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        if progress.state == GenerationState::Current && !progress.closed {
            emit();
            true
        } else {
            false
        }
    }

    fn dispatch(&self, event: ReaderEvent) {
        let event_bytes = event.byte_len();
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        while progress.state == GenerationState::Pending
            && progress.pending_bytes.saturating_add(event_bytes) > MAX_PENDING_AUTHORITY_BYTES
        {
            progress = self
                .changed
                .wait(progress)
                .unwrap_or_else(|e| e.into_inner());
        }
        match progress.state {
            GenerationState::Pending => {
                progress.pending_bytes = progress.pending_bytes.saturating_add(event_bytes);
                progress.pending.push(event);
                self.changed.notify_all();
            }
            GenerationState::Current => (self.sink)(event),
            GenerationState::Retired => {}
        }
    }

    #[cfg(test)]
    fn wait_closed(&self, timeout: Duration) -> bool {
        let progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        let (progress, _) = self
            .changed
            .wait_timeout_while(progress, timeout, |progress| !progress.closed)
            .unwrap_or_else(|e| e.into_inner());
        progress.closed
    }

    #[cfg(test)]
    fn wait_pending_events(&self, count: usize, timeout: Duration) -> bool {
        let progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        let (progress, _) = self
            .changed
            .wait_timeout_while(progress, timeout, |progress| progress.pending.len() < count)
            .unwrap_or_else(|e| e.into_inner());
        progress.pending.len() >= count
    }
}

impl ProbeChannel {
    fn acknowledge(&self, nonce: u64) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        progress.acknowledged = progress.acknowledged.max(nonce);
        self.changed.notify_all();
    }

    fn close(&self) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        progress.closed = true;
        self.changed.notify_all();
    }
}

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
    probe_channel: Arc<ProbeChannel>,
    generation: Arc<GenerationAuthority>,
    #[cfg(test)]
    after_probe_write: Option<Arc<dyn Fn() + Send + Sync>>,
    next_probe: u64,
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
    /// thread. Returns the assembled [`RemotePty`] and the compatibility seed.
    /// The seed is intentionally empty because the attached tmux client supplies
    /// the one authoritative redraw of the current screen.
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
                format!(
                    "remote_pty: expected a scrollback frame, got: {}",
                    line.trim()
                )
            })?
            .to_string();

        // Spawn the reader thread: it owns `reader` (the read half) and re-emits
        // each {"out"}/{"exit"} frame into the webview via a cheap AppHandle clone.
        let id_for_thread = id.to_string();
        let probe_channel = Arc::new(ProbeChannel::default());
        let probe_for_thread = probe_channel.clone();
        let event_app = app.clone();
        let event_id = id.to_string();
        let event_sink: Arc<dyn Fn(ReaderEvent) + Send + Sync> =
            Arc::new(move |event| emit_reader_event(&event_app, &event_id, event));
        let generation = Arc::new(GenerationAuthority::new(event_sink));
        let generation_for_thread = generation.clone();
        let handle = std::thread::Builder::new()
            .name(format!("t-hub-remote-pty-{id}"))
            .spawn(move || {
                reader_loop(
                    id_for_thread,
                    reader,
                    probe_for_thread,
                    generation_for_thread,
                )
            })
            .map_err(|e| format!("remote_pty: spawn reader thread failed: {e}"))?;

        Ok((
            Self {
                id: id.to_string(),
                writer,
                reader: Some(handle),
                probe_channel,
                generation,
                #[cfg(test)]
                after_probe_write: None,
                next_probe: 0,
                cols,
                rows,
            },
            scrollback_b64,
        ))
    }

    /// Whether the reader thread is still running. It blocks on the live socket for
    /// the whole connection and returns only on EOF/error/`shutdown` (a dropped or
    /// rebound control server, a server-side detach, or our own teardown). Once it
    /// has returned, this connection's `writer` points at a dead socket: a `write`/
    /// `resize` would fail (or silently no-op if geometry is unchanged) and no output
    /// would ever arrive again. `attach_terminal` uses this to PURGE a stale cached
    /// connection instead of reporting a frozen tile as `Live` (the disconnect-needs-
    /// restart bug: a control rebind rotated the port, the reader hit EOF and exited,
    /// but the dead `RemotePty` lingered in the manager map and got reused).
    pub fn is_alive(&self) -> bool {
        self.reader
            .as_ref()
            .map(|handle| !handle.is_finished())
            .unwrap_or(false)
    }

    pub(crate) fn retire_generation(&self) {
        self.generation.retire();
    }

    /// Perform an actual transport write before a cached connection is reused.
    ///
    /// A reader-thread check alone has a check-to-use window, and `resize` cannot
    /// serve as the probe because unchanged geometry deliberately performs no I/O.
    /// The server echoes the nonce only after its attach input loop receives the
    /// frame. A local write/flush is not sufficient evidence because TCP can accept
    /// bytes briefly after the peer has closed.
    pub fn probe(&mut self) -> Result<(), String> {
        if !self.is_alive() {
            return Err(format!(
                "remote_pty: probe terminal {} failed: reader already stopped",
                self.id
            ));
        }
        self.next_probe = self.next_probe.wrapping_add(1).max(1);
        let nonce = self.next_probe;
        let mut frame = serde_json::to_vec(&json!({ "probe": nonce }))
            .map_err(|e| format!("remote_pty: serialize probe failed: {e}"))?;
        frame.push(b'\n');
        self.writer
            .write_all(&frame)
            .and_then(|()| self.writer.flush())
            .map_err(|e| format!("remote_pty: probe terminal {} failed: {e}", self.id))?;

        #[cfg(test)]
        if let Some(after_probe_write) = &self.after_probe_write {
            after_probe_write();
        }

        let progress = self
            .probe_channel
            .progress
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (progress, timeout) = self
            .probe_channel
            .changed
            .wait_timeout_while(progress, PROBE_TIMEOUT, |progress| {
                progress.acknowledged < nonce && !progress.closed
            })
            .unwrap_or_else(|e| e.into_inner());
        if progress.closed || progress.acknowledged < nonce {
            let reason = if progress.closed {
                "reader stopped during probe"
            } else if timeout.timed_out() {
                "acknowledgement timed out"
            } else {
                "acknowledgement missing"
            };
            return Err(format!(
                "remote_pty: probe terminal {} failed: {reason}",
                self.id
            ));
        }
        Ok(())
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
    pub fn detach(&mut self) {
        self.shutdown_and_join();
    }

    /// Shared teardown for `detach` + `Drop`: best-effort shutdown the connection
    /// (unblocking the reader) and join the thread. Idempotent — a second call sees
    /// `reader == None` and is a no-op.
    fn shutdown_and_join(&mut self) {
        // Wake a Pending reader that is backpressured on the bounded pre-install
        // event buffer, and suppress any later event from this generation.
        self.generation.retire();
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
    ProbeAck(u64),
    /// A blank line, a malformed frame, or any other shape (e.g. a late
    /// `{"scrollback"}` or the server's idle `{"keepalive"}`) — skipped without
    /// tearing the stream down.
    Ignore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PreparedStreamEnd {
    Detached,
    Exited(Option<i32>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ReaderEvent {
    Output(Vec<u8>),
    StreamEnd(PreparedStreamEnd),
}

impl ReaderEvent {
    fn byte_len(&self) -> usize {
        match self {
            Self::Output(bytes) => bytes.len(),
            Self::StreamEnd(_) => 0,
        }
    }
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
    } else if let Some(nonce) = frame.get("probeAck").and_then(|value| value.as_u64()) {
        PtyFrame::ProbeAck(nonce)
    } else {
        PtyFrame::Ignore
    }
}

/// Emit the accumulated output `batch` as a single base64 `terminal://output`
/// event, then clear it. A no-op when empty. (We re-encode the COMBINED bytes
/// once rather than per source frame, so N coalesced chunks cost one emit.)
fn emit_batch(authority: &GenerationAuthority, batch: &mut Vec<u8>) {
    if batch.is_empty() {
        return;
    }
    authority.dispatch(ReaderEvent::Output(std::mem::take(batch)));
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
fn prepare_stream_end(id: &str, code: Option<i32>) -> PreparedStreamEnd {
    let gone = crate::tmux::is_definitively_gone(crate::tmux::session_liveness(
        &crate::tmux::target_for_id(id),
    ));
    if !gone {
        return PreparedStreamEnd::Detached;
    }
    PreparedStreamEnd::Exited(code)
}

fn emit_reader_event(app: &AppHandle, id: &str, event: ReaderEvent) {
    match event {
        ReaderEvent::Output(bytes) => {
            let payload = OutputEvent {
                id: id.to_string(),
                base64: STANDARD.encode(bytes),
            };
            crate::hangwatch::note_emit();
            let _ = app.emit(events::OUTPUT, &payload);
        }
        ReaderEvent::StreamEnd(PreparedStreamEnd::Detached) => {
            let _ = app.emit(
                events::STATE,
                &StateEvent {
                    id: id.to_string(),
                    state: TerminalState::Detached,
                },
            );
        }
        ReaderEvent::StreamEnd(PreparedStreamEnd::Exited(code)) => {
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
    }
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
fn reader_loop(
    id: String,
    reader: BufReader<TcpStream>,
    probe_channel: Arc<ProbeChannel>,
    generation: Arc<GenerationAuthority>,
) {
    // The handshake's BufReader may already hold bytes past the scrollback frame
    // (the first `out` frames can ride the same TCP segment). Drain that buffered
    // tail into our accumulator BEFORE switching to raw, timeout-toggled reads, so
    // no output is lost or reordered.
    let mut acc: Vec<u8> = reader.buffer().to_vec();
    let stream = reader.into_inner();

    let mut batch: Vec<u8> = Vec::new();
    let mut buf = [0u8; RECV_BUF];
    let mut exit_code = None;

    'read: loop {
        // Parse every COMPLETE line currently in `acc` before blocking again. A
        // partial trailing line stays in `acc` for the next read.
        while let Some(pos) = acc.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = acc.drain(..=pos).collect();
            match parse_pty_frame(&line[..line.len() - 1]) {
                PtyFrame::Output(bytes) => {
                    batch.extend_from_slice(&bytes);
                    if batch.len() >= MAX_BATCH_BYTES {
                        emit_batch(&generation, &mut batch);
                    }
                }
                PtyFrame::Exit(code) => {
                    // Flush any output that preceded the exit so order is preserved.
                    emit_batch(&generation, &mut batch);
                    exit_code = Some(code);
                    break 'read;
                }
                PtyFrame::ProbeAck(nonce) => probe_channel.acknowledge(nonce),
                PtyFrame::Ignore => {}
            }
        }

        // Coalesce: block indefinitely when nothing is pending (no busy-poll), but
        // cap the wait at COALESCE_WINDOW once a batch is building so it flushes
        // promptly. A raw `read` into our own buffer means a timeout consumes
        // nothing (no partial-line corruption) — unlike a timed `read_line`.
        let pending = !batch.is_empty();
        let _ = stream.set_read_timeout(if pending { Some(COALESCE_WINDOW) } else { None });
        match (&stream).read(&mut buf) {
            Ok(0) => break, // EOF: server detached / connection dropped.
            Ok(n) => acc.extend_from_slice(&buf[..n]),
            // The coalesce window elapsed with a batch pending → flush it.
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                emit_batch(&generation, &mut batch);
            }
            Err(_) => break, // a torn-down connection (shutdown) surfaces here.
        }
    }

    // Flush whatever output was still pending when the stream ended.
    emit_batch(&generation, &mut batch);

    // Close health authority before any potentially slow tmux classification or
    // stream-end event. A probe waiter can therefore never accept an ack and emit
    // Live after this reader has already published Detached/Exited.
    probe_channel.close();
    generation.mark_closed();
    let stream_end = prepare_stream_end(&id, exit_code.unwrap_or(None));
    generation.dispatch(ReaderEvent::StreamEnd(stream_end));
}

/// App-wide registry of live remote-PTY connections, keyed by T-Hub id. Mirrors
/// [`crate::commands::TerminalManager`] but holds socket-backed [`RemotePty`]s
/// instead of in-process `PtySession`s. Managed in Tauri state; `commands.rs`
/// pulls a [`RemotePty`] OUT of the map (releasing the lock) before any blocking
/// socket op, so the `Mutex` is never held across I/O.
#[derive(Default)]
pub struct RemotePtyManager {
    pub conns: Mutex<HashMap<String, Arc<Mutex<RemotePty>>>>,
}

impl RemotePtyManager {
    pub fn cached(&self, id: &str) -> Option<Arc<Mutex<RemotePty>>> {
        self.conns.lock().get(id).cloned()
    }

    pub fn with_current(
        &self,
        id: &str,
        candidate: &Arc<Mutex<RemotePty>>,
        on_current: impl FnOnce(),
    ) -> bool {
        let conns = self.conns.lock();
        if conns
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, candidate))
        {
            candidate.lock().generation.emit_if_current(on_current)
        } else {
            false
        }
    }

    pub fn remove_if_current(
        &self,
        id: &str,
        candidate: &Arc<Mutex<RemotePty>>,
    ) -> Option<Arc<Mutex<RemotePty>>> {
        let mut conns = self.conns.lock();
        if conns
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, candidate))
        {
            candidate.lock().generation.retire();
            conns.remove(id)
        } else {
            None
        }
    }

    pub fn install_if_absent(
        &self,
        id: String,
        connection: RemotePty,
        on_installed: impl FnOnce(),
    ) -> Result<Arc<Mutex<RemotePty>>, RemotePty> {
        use std::collections::hash_map::Entry;

        let mut conns = self.conns.lock();
        match conns.entry(id) {
            Entry::Vacant(entry) => {
                let key = entry.key().clone();
                let connection = Arc::new(Mutex::new(connection));
                entry.insert(connection.clone());
                if connection.lock().generation.activate_if_open(on_installed) {
                    Ok(connection)
                } else {
                    let removed = conns.remove(&key).expect("just-inserted generation");
                    drop(connection);
                    let mutex = Arc::try_unwrap(removed)
                        .unwrap_or_else(|_| panic!("unpublished generation unexpectedly shared"));
                    Err(mutex.into_inner())
                }
            }
            Entry::Occupied(_) => Err(connection),
        }
    }

    pub fn remove(&self, id: &str) -> Option<Arc<Mutex<RemotePty>>> {
        let mut conns = self.conns.lock();
        let connection = conns.get(id)?.clone();
        connection.lock().generation.retire();
        conns.remove(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn test_remote(
        writer: TcpStream,
        reader: JoinHandle<()>,
        probe_channel: Arc<ProbeChannel>,
    ) -> RemotePty {
        test_remote_with_sink(writer, reader, probe_channel, Arc::new(|_| {}))
    }

    fn test_remote_with_sink(
        writer: TcpStream,
        reader: JoinHandle<()>,
        probe_channel: Arc<ProbeChannel>,
        sink: Arc<dyn Fn(ReaderEvent) + Send + Sync>,
    ) -> RemotePty {
        RemotePty {
            id: "test".into(),
            writer,
            reader: Some(reader),
            probe_channel,
            generation: Arc::new(GenerationAuthority::new(sink)),
            after_probe_write: None,
            next_probe: 0,
            cols: 80,
            rows: 24,
        }
    }

    fn socket_backed_remote(
        id: &str,
        writer: TcpStream,
        reader_stream: TcpStream,
        sink: Arc<dyn Fn(ReaderEvent) + Send + Sync>,
    ) -> (RemotePty, Arc<GenerationAuthority>) {
        let probe_channel = Arc::new(ProbeChannel::default());
        let generation = Arc::new(GenerationAuthority::new(sink));
        let reader_probe_channel = probe_channel.clone();
        let reader_generation = generation.clone();
        let reader_id = id.to_string();
        let reader = std::thread::spawn(move || {
            reader_loop(
                reader_id,
                BufReader::new(reader_stream),
                reader_probe_channel,
                reader_generation,
            )
        });
        (
            RemotePty {
                id: id.into(),
                writer,
                reader: Some(reader),
                probe_channel,
                generation: generation.clone(),
                after_probe_write: None,
                next_probe: 0,
                cols: 80,
                rows: 24,
            },
            generation,
        )
    }

    fn installed(result: Result<Arc<Mutex<RemotePty>>, RemotePty>) -> Arc<Mutex<RemotePty>> {
        match result {
            Ok(connection) => connection,
            Err(_) => panic!("test connection unexpectedly lost compare-and-insert"),
        }
    }

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
        assert_eq!(
            parse_pty_frame(br#"{"exit":137}"#),
            PtyFrame::Exit(Some(137))
        );
        // A null/absent exit code → Exit(None) (signalled / unknown).
        assert_eq!(parse_pty_frame(br#"{"exit":null}"#), PtyFrame::Exit(None));
        assert_eq!(
            parse_pty_frame(br#"{"probeAck":17}"#),
            PtyFrame::ProbeAck(17)
        );
    }

    #[test]
    fn ignores_blank_malformed_undecodable_and_other_frames() {
        assert_eq!(parse_pty_frame(b""), PtyFrame::Ignore);
        assert_eq!(parse_pty_frame(b"   \t"), PtyFrame::Ignore);
        assert_eq!(parse_pty_frame(b"not json"), PtyFrame::Ignore);
        // Well-formed JSON but `out` isn't valid base64 → skipped, not a panic.
        assert_eq!(
            parse_pty_frame(br#"{"out":"!!!not base64!!!"}"#),
            PtyFrame::Ignore
        );
        // A late/unknown frame shape (e.g. scrollback) is ignored.
        assert_eq!(parse_pty_frame(br#"{"scrollback":"x"}"#), PtyFrame::Ignore);
        // The server's idle keepalive is a no-op here: it carries no `out`/`exit`,
        // so it must drop silently (the s27 idle-leak fix relies on this contract).
        assert_eq!(
            parse_pty_frame(br#"{"keepalive":"...."}"#),
            PtyFrame::Ignore
        );
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

    #[test]
    fn probe_rejects_a_server_closed_transport() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        server.shutdown(Shutdown::Both).unwrap();

        // Observe the peer's FIN before probing so this exercises the broken
        // transport deterministically rather than racing TCP close propagation.
        let mut observer = writer.try_clone().unwrap();
        observer
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let reader = std::thread::spawn(move || {
            let mut byte = [0u8; 1];
            assert_eq!(observer.read(&mut byte).unwrap(), 0);
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !reader.is_finished() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(reader.is_finished(), "reader did not observe server close");

        let mut remote = test_remote(writer, reader, Arc::new(ProbeChannel::default()));
        assert!(remote.probe().is_err());
    }

    #[test]
    fn acknowledged_probe_roundtrip_succeeds() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let reader_stream = writer.try_clone().unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let server_thread = std::thread::spawn(move || {
            let mut input = BufReader::new(server.try_clone().unwrap());
            let mut line = String::new();
            input.read_line(&mut line).unwrap();
            let nonce = serde_json::from_str::<Value>(&line).unwrap()["probe"]
                .as_u64()
                .unwrap();
            write!(server, "{{\"probeAck\":{nonce}}}\n").unwrap();
            server.flush().unwrap();
            let mut tail = Vec::new();
            input.read_to_end(&mut tail).unwrap();
        });
        let probe_channel = Arc::new(ProbeChannel::default());
        let probe_for_reader = probe_channel.clone();
        let reader = std::thread::spawn(move || {
            let mut input = BufReader::new(reader_stream);
            let mut line = String::new();
            while input.read_line(&mut line).unwrap() > 0 {
                if let PtyFrame::ProbeAck(nonce) = parse_pty_frame(line.trim_end().as_bytes()) {
                    probe_for_reader.acknowledge(nonce);
                }
                line.clear();
            }
            probe_for_reader.close();
        });
        let mut remote = test_remote(writer, reader, probe_channel);

        remote.probe().unwrap();
        assert!(remote.is_alive());
        remote.detach();
        server_thread.join().unwrap();
    }

    #[test]
    fn stalled_probe_does_not_block_another_terminal_write() {
        use std::sync::mpsc;

        let stalled_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stalled_writer = TcpStream::connect(stalled_listener.local_addr().unwrap()).unwrap();
        let stalled_reader_stream = stalled_writer.try_clone().unwrap();
        let (mut stalled_server, _) = stalled_listener.accept().unwrap();
        let (probe_seen_tx, probe_seen_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let stalled_server_thread = std::thread::spawn(move || {
            let mut input = BufReader::new(stalled_server.try_clone().unwrap());
            let mut line = String::new();
            input.read_line(&mut line).unwrap();
            let nonce = serde_json::from_str::<Value>(&line).unwrap()["probe"]
                .as_u64()
                .unwrap();
            probe_seen_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            write!(stalled_server, "{{\"probeAck\":{nonce}}}\n").unwrap();
            stalled_server.flush().unwrap();
            let mut tail = Vec::new();
            input.read_to_end(&mut tail).unwrap();
        });
        let stalled_channel = Arc::new(ProbeChannel::default());
        let stalled_channel_reader = stalled_channel.clone();
        let stalled_reader = std::thread::spawn(move || {
            let mut input = BufReader::new(stalled_reader_stream);
            let mut line = String::new();
            while input.read_line(&mut line).unwrap() > 0 {
                if let PtyFrame::ProbeAck(nonce) = parse_pty_frame(line.trim_end().as_bytes()) {
                    stalled_channel_reader.acknowledge(nonce);
                }
                line.clear();
            }
            stalled_channel_reader.close();
        });

        let other_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let other_writer = TcpStream::connect(other_listener.local_addr().unwrap()).unwrap();
        let other_reader_stream = other_writer.try_clone().unwrap();
        let (other_server, _) = other_listener.accept().unwrap();
        let other_reader = std::thread::spawn(move || {
            let mut reader = other_reader_stream;
            let mut tail = Vec::new();
            let _ = reader.read_to_end(&mut tail);
        });

        let manager = Arc::new(RemotePtyManager::default());
        installed(manager.install_if_absent(
            "stalled".into(),
            test_remote(stalled_writer, stalled_reader, stalled_channel),
            || {},
        ));
        installed(manager.install_if_absent(
            "other".into(),
            test_remote(
                other_writer,
                other_reader,
                Arc::new(ProbeChannel::default()),
            ),
            || {},
        ));

        let probe_manager = manager.clone();
        let probe =
            std::thread::spawn(move || probe_manager.cached("stalled").unwrap().lock().probe());
        probe_seen_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let started = std::time::Instant::now();
        manager
            .cached("other")
            .unwrap()
            .lock()
            .write(b"responsive")
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "another terminal write waited behind the stalled probe"
        );

        release_tx.send(()).unwrap();
        probe.join().unwrap().unwrap();
        for id in ["stalled", "other"] {
            manager.conns.lock().remove(id).unwrap().lock().detach();
        }
        drop(other_server);
        stalled_server_thread.join().unwrap();
    }

    #[test]
    fn ack_followed_by_reader_loss_is_evicted_before_replacement_is_installed() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let reader_stream = writer.try_clone().unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let server_thread = std::thread::spawn(move || {
            let mut input = BufReader::new(server.try_clone().unwrap());
            let mut line = String::new();
            input.read_line(&mut line).unwrap();
            let nonce = serde_json::from_str::<Value>(&line).unwrap()["probe"]
                .as_u64()
                .unwrap();
            write!(server, "{{\"probeAck\":{nonce}}}\n").unwrap();
            server.flush().unwrap();
            server.shutdown(Shutdown::Both).unwrap();
        });
        let probe_channel = Arc::new(ProbeChannel::default());
        let probe_for_reader = probe_channel.clone();
        let reader = std::thread::spawn(move || {
            let mut input = BufReader::new(reader_stream);
            let mut line = String::new();
            input.read_line(&mut line).unwrap();
            let nonce = match parse_pty_frame(line.trim_end().as_bytes()) {
                PtyFrame::ProbeAck(nonce) => nonce,
                frame => panic!("expected probe ack, got {frame:?}"),
            };
            let mut tail = Vec::new();
            input.read_to_end(&mut tail).unwrap();
            // Coordinate the race: loss is recorded before the waiter may accept
            // the acknowledgement.
            probe_for_reader.close();
            probe_for_reader.acknowledge(nonce);
        });
        let manager = RemotePtyManager::default();
        let cached = installed(manager.install_if_absent(
            "term".into(),
            test_remote(writer, reader, probe_channel),
            || {},
        ));

        assert!(cached.lock().probe().is_err());
        let stale = manager
            .remove_if_current("term", &cached)
            .expect("failed generation must be evicted");
        assert!(manager.cached("term").is_none());
        stale.lock().detach();
        server_thread.join().unwrap();

        let replacement_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let replacement_writer =
            TcpStream::connect(replacement_listener.local_addr().unwrap()).unwrap();
        let (replacement_server, _) = replacement_listener.accept().unwrap();
        let replacement_reader_stream = replacement_writer.try_clone().unwrap();
        let replacement_reader = std::thread::spawn(move || {
            let mut reader = replacement_reader_stream;
            let mut tail = Vec::new();
            let _ = reader.read_to_end(&mut tail);
        });
        let installed_live = std::sync::atomic::AtomicBool::new(false);
        let replacement = installed(manager.install_if_absent(
            "term".into(),
            test_remote(
                replacement_writer,
                replacement_reader,
                Arc::new(ProbeChannel::default()),
            ),
            || installed_live.store(true, std::sync::atomic::Ordering::SeqCst),
        ));
        assert!(installed_live.load(std::sync::atomic::Ordering::SeqCst));
        let current = std::sync::atomic::AtomicBool::new(false);
        assert!(manager.with_current("term", &replacement, || {
            current.store(true, std::sync::atomic::Ordering::SeqCst)
        }));
        assert!(current.load(std::sync::atomic::Ordering::SeqCst));
        replacement.lock().detach();
        drop(replacement_server);
    }

    #[test]
    fn close_cannot_order_detached_before_current_live_commit() {
        use std::sync::mpsc;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let reader_stream = writer.try_clone().unwrap();
        let (server, _) = listener.accept().unwrap();
        let reader = std::thread::spawn(move || {
            let mut reader = reader_stream;
            let mut tail = Vec::new();
            let _ = reader.read_to_end(&mut tail);
        });
        let manager = Arc::new(RemotePtyManager::default());
        let candidate = installed(manager.install_if_absent(
            "term".into(),
            test_remote(writer, reader, Arc::new(ProbeChannel::default())),
            || {},
        ));
        let events = Arc::new(StdMutex::new(Vec::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let live_manager = manager.clone();
        let live_candidate = candidate.clone();
        let live_events = events.clone();
        let live = std::thread::spawn(move || {
            assert!(live_manager.with_current("term", &live_candidate, || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                live_events.lock().unwrap().push("Live");
            }));
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let close_manager = manager.clone();
        let close_candidate = candidate.clone();
        let close_events = events.clone();
        let close = std::thread::spawn(move || {
            let removed = close_manager
                .remove_if_current("term", &close_candidate)
                .unwrap();
            close_events.lock().unwrap().push("Detached");
            removed
        });
        std::thread::sleep(Duration::from_millis(20));
        assert!(events.lock().unwrap().is_empty());
        release_tx.send(()).unwrap();
        live.join().unwrap();
        let removed = close.join().unwrap();
        assert_eq!(*events.lock().unwrap(), ["Live", "Detached"]);
        removed.lock().detach();
        drop(server);
    }

    #[test]
    fn compare_and_insert_never_overwrites_a_concurrent_winner() {
        fn connection() -> (RemotePty, TcpStream) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let reader_stream = writer.try_clone().unwrap();
            let (server, _) = listener.accept().unwrap();
            let reader = std::thread::spawn(move || {
                let mut reader = reader_stream;
                let mut tail = Vec::new();
                let _ = reader.read_to_end(&mut tail);
            });
            (
                test_remote(writer, reader, Arc::new(ProbeChannel::default())),
                server,
            )
        }

        let manager = RemotePtyManager::default();
        let (winner_connection, winner_server) = connection();
        let winner = installed(manager.install_if_absent("term".into(), winner_connection, || {}));
        let (loser_connection, loser_server) = connection();
        let loser_live = std::sync::atomic::AtomicBool::new(false);
        let mut loser = match manager.install_if_absent("term".into(), loser_connection, || {
            loser_live.store(true, std::sync::atomic::Ordering::SeqCst)
        }) {
            Ok(_) => panic!("loser overwrote the installed winner"),
            Err(connection) => connection,
        };
        assert!(!loser_live.load(std::sync::atomic::Ordering::SeqCst));
        let current = std::sync::atomic::AtomicBool::new(false);
        assert!(manager.with_current("term", &winner, || {
            current.store(true, std::sync::atomic::Ordering::SeqCst)
        }));
        assert!(current.load(std::sync::atomic::Ordering::SeqCst));

        loser.detach();
        manager.conns.lock().remove("term").unwrap().lock().detach();
        drop(loser_server);
        drop(winner_server);
    }

    #[test]
    fn pending_winner_flushes_output_once_and_loser_never_emits() {
        let winner_events = Arc::new(StdMutex::new(Vec::new()));
        let winner_sink = winner_events.clone();
        let winner = GenerationAuthority::new(Arc::new(move |event| {
            winner_sink.lock().unwrap().push(event)
        }));
        winner.dispatch(ReaderEvent::Output(b"pre-install".to_vec()));
        assert!(winner_events.lock().unwrap().is_empty());
        assert!(winner.activate_if_open(|| {}));
        assert_eq!(
            *winner_events.lock().unwrap(),
            [ReaderEvent::Output(b"pre-install".to_vec())]
        );
        assert!(!winner.activate_if_open(|| {}));

        let loser_events = Arc::new(StdMutex::new(Vec::new()));
        let loser_sink = loser_events.clone();
        let loser = GenerationAuthority::new(Arc::new(move |event| {
            loser_sink.lock().unwrap().push(event)
        }));
        loser.dispatch(ReaderEvent::Output(b"loser".to_vec()));
        loser.retire();
        assert!(loser_events.lock().unwrap().is_empty());
    }

    #[test]
    fn socket_reader_ack_then_eof_rejects_probe_before_live() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let reader_stream = writer.try_clone().unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let server_thread = std::thread::spawn(move || {
            let mut input = BufReader::new(server.try_clone().unwrap());
            let mut line = String::new();
            input.read_line(&mut line).unwrap();
            let nonce = serde_json::from_str::<Value>(&line).unwrap()["probe"]
                .as_u64()
                .unwrap();
            writeln!(server, "{{\"probeAck\":{nonce}}}").unwrap();
            server.flush().unwrap();
            server.shutdown(Shutdown::Both).unwrap();
        });

        let events = Arc::new(StdMutex::new(Vec::new()));
        let sink_events = events.clone();
        let (mut remote, generation) = socket_backed_remote(
            "ack-eof",
            writer,
            reader_stream,
            Arc::new(move |event| {
                sink_events.lock().unwrap().push(format!("{event:?}"));
            }),
        );
        assert!(
            generation.activate_if_open(|| { events.lock().unwrap().push("initial:Live".into()) })
        );

        let probe_written = Arc::new(std::sync::Barrier::new(2));
        let release_probe = Arc::new(std::sync::Barrier::new(2));
        let hook_written = probe_written.clone();
        let hook_release = release_probe.clone();
        remote.after_probe_write = Some(Arc::new(move || {
            hook_written.wait();
            hook_release.wait();
        }));
        let remote = Arc::new(Mutex::new(remote));
        let probing_remote = remote.clone();
        let probing_generation = generation.clone();
        let probe_events = events.clone();
        let probe = std::thread::spawn(move || {
            let result = probing_remote.lock().probe();
            if result.is_ok() {
                probing_generation
                    .emit_if_current(|| probe_events.lock().unwrap().push("probe:Live".into()));
            }
            result
        });

        probe_written.wait();
        assert!(
            generation.wait_closed(Duration::from_secs(1)),
            "actual reader_loop did not record EOF"
        );
        release_probe.wait();
        assert!(probe.join().unwrap().is_err());
        remote
            .lock()
            .reader
            .take()
            .expect("reader handle")
            .join()
            .unwrap();
        server_thread.join().unwrap();

        let events = events.lock().unwrap();
        assert_eq!(events.first().map(String::as_str), Some("initial:Live"));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("StreamEnd("))
                .count(),
            1
        );
        assert!(!events.iter().any(|event| event == "probe:Live"));
    }

    #[test]
    fn retired_socket_reader_eof_is_suppressed_after_replacement_live() {
        let old_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let old_writer = TcpStream::connect(old_listener.local_addr().unwrap()).unwrap();
        let old_reader_stream = old_writer.try_clone().unwrap();
        let (old_server, _) = old_listener.accept().unwrap();
        let events = Arc::new(StdMutex::new(Vec::new()));
        let old_events = events.clone();
        let (old_remote, _) = socket_backed_remote(
            "old-generation",
            old_writer,
            old_reader_stream,
            Arc::new(move |event| {
                old_events.lock().unwrap().push(format!("old:{event:?}"));
            }),
        );
        let manager = RemotePtyManager::default();
        let installed_events = events.clone();
        let old = installed(
            manager.install_if_absent("term".into(), old_remote, move || {
                installed_events.lock().unwrap().push("old:Live".into())
            }),
        );
        let retired = manager.remove_if_current("term", &old).unwrap();

        let replacement_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let replacement_writer =
            TcpStream::connect(replacement_listener.local_addr().unwrap()).unwrap();
        let replacement_reader_stream = replacement_writer.try_clone().unwrap();
        let (replacement_server, _) = replacement_listener.accept().unwrap();
        let replacement_reader = std::thread::spawn(move || {
            let mut reader = replacement_reader_stream;
            let mut tail = Vec::new();
            let _ = reader.read_to_end(&mut tail);
        });
        let replacement_events = events.clone();
        let replacement = installed(manager.install_if_absent(
            "term".into(),
            test_remote(
                replacement_writer,
                replacement_reader,
                Arc::new(ProbeChannel::default()),
            ),
            move || {
                replacement_events
                    .lock()
                    .unwrap()
                    .push("replacement:Live".into())
            },
        ));

        old_server.shutdown(Shutdown::Both).unwrap();
        retired
            .lock()
            .reader
            .take()
            .expect("old reader handle")
            .join()
            .unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            ["old:Live".to_string(), "replacement:Live".to_string()]
        );

        manager.remove_if_current("term", &replacement).unwrap();
        replacement.lock().detach();
        drop(replacement_server);
    }

    #[test]
    fn socket_reader_flushes_preinstall_output_once_after_live() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let reader_stream = writer.try_clone().unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let events = Arc::new(StdMutex::new(Vec::new()));
        let sink_events = events.clone();
        let (remote, generation) = socket_backed_remote(
            "preinstall",
            writer,
            reader_stream,
            Arc::new(move |event| {
                sink_events.lock().unwrap().push(format!("{event:?}"));
            }),
        );

        writeln!(
            server,
            "{{\"out\":\"{}\"}}",
            STANDARD.encode(b"pre-install")
        )
        .unwrap();
        server.flush().unwrap();
        assert!(
            generation.wait_pending_events(1, Duration::from_secs(1)),
            "actual reader_loop did not buffer pre-install output"
        );
        assert!(events.lock().unwrap().is_empty());

        let manager = RemotePtyManager::default();
        let live_events = events.clone();
        let installed_remote =
            installed(manager.install_if_absent("term".into(), remote, move || {
                live_events.lock().unwrap().push("Live".into())
            }));
        assert_eq!(
            *events.lock().unwrap(),
            [
                "Live".to_string(),
                "Output([112, 114, 101, 45, 105, 110, 115, 116, 97, 108, 108])".to_string()
            ]
        );

        manager
            .remove_if_current("term", &installed_remote)
            .unwrap();
        installed_remote.lock().detach();
        drop(server);
    }

    #[test]
    fn ack_then_eof_cannot_emit_stale_live_after_detached() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let sink_events = events.clone();
        let authority = GenerationAuthority::new(Arc::new(move |event| {
            sink_events.lock().unwrap().push(format!("{event:?}"))
        }));
        assert!(authority.activate_if_open(|| {}));

        authority.mark_closed();
        authority.dispatch(ReaderEvent::StreamEnd(PreparedStreamEnd::Detached));
        assert!(!authority.emit_if_current(|| { events.lock().unwrap().push("Live".into()) }));
        assert_eq!(*events.lock().unwrap(), ["StreamEnd(Detached)".to_string()]);
    }

    #[test]
    fn retired_old_reader_cannot_emit_after_replacement_live() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let old_events = events.clone();
        let old = GenerationAuthority::new(Arc::new(move |event| {
            old_events.lock().unwrap().push(format!("old:{event:?}"))
        }));
        assert!(old.activate_if_open(|| {}));
        old.retire();

        let new_events = events.clone();
        let new = GenerationAuthority::new(Arc::new(move |event| {
            new_events.lock().unwrap().push(format!("new:{event:?}"))
        }));
        assert!(new.activate_if_open(|| { events.lock().unwrap().push("new:Live".into()) }));
        old.dispatch(ReaderEvent::StreamEnd(PreparedStreamEnd::Detached));
        assert_eq!(*events.lock().unwrap(), ["new:Live".to_string()]);
    }

    #[test]
    fn detach_wakes_reader_blocked_at_pending_authority_cap() {
        let authority = Arc::new(GenerationAuthority::new(Arc::new(|_| {})));
        for _ in 0..4 {
            authority.dispatch(ReaderEvent::Output(vec![0; MAX_BATCH_BYTES]));
        }
        let blocked_authority = authority.clone();
        let blocked_reader = std::thread::spawn(move || {
            blocked_authority.dispatch(ReaderEvent::Output(vec![0; MAX_BATCH_BYTES]));
        });

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let mut remote = RemotePty {
            id: "pending".into(),
            writer,
            reader: Some(blocked_reader),
            probe_channel: Arc::new(ProbeChannel::default()),
            generation: authority,
            after_probe_write: None,
            next_probe: 0,
            cols: 80,
            rows: 24,
        };
        let started = std::time::Instant::now();
        remote.detach();
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "detach did not wake the pending reader"
        );
        drop(server);
    }
}
