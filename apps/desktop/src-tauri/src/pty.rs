//! PTY management — bridges a `portable-pty` master to a tmux session on the
//! isolated `t-hub` socket, with one PTY client per visible tile.
//!
//! Each terminal tile is a PTY whose child process is a `tmux attach` client
//! pointed at that terminal's tmux session. Output from the attach client is
//! read on a dedicated thread and emitted to the frontend as base64
//! `terminal://output` events; on EOF we emit `terminal://exit` and a
//! `terminal://state = Exited`.
//!
//! Platform model (the key abstraction that lets the nucleus run in WSL now):
//!   - `#[cfg(unix)]`:    spawn `tmux -L t-hub attach -t NAME` directly (this
//!     runs inside WSL2 and is testable now).
//!   - `#[cfg(windows)]`: spawn `wsl.exe -e tmux -L t-hub attach -t NAME`
//!     (ConPTY fronting the WSL distro).
//!
//! `PtySession` holds only `Send` handles so it can live inside the
//! Tauri-managed `parking_lot::Mutex<HashMap<..>>`: the boxed PTY writer, the
//! `Box<dyn MasterPty + Send>` (for resize), a `ChildKiller` (so we can detach
//! the attach client without owning the `Child`, which the reader thread waits
//! on), and the reader-thread `JoinHandle`. Output is routed through an
//! `AppHandle` clone captured by the reader thread, never through shared state.

use std::io::{Read, Write};
use std::thread::JoinHandle;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use portable_pty::{
    native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize,
};
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::commands::TerminalState;
use crate::events::{self, ExitEvent, OutputEvent, StateEvent};
use crate::tmux;

/// Size of the read buffer for draining the PTY (8 KiB).
const READ_BUF: usize = 8 * 1024;

/// A live terminal tile: its T-Hub id, the backing tmux session name, and the
/// `Send` handles for the PTY attach client.
///
/// All fields are `Send`, so `PtySession` is `Send` and can be stored in the
/// Tauri-managed `Mutex<HashMap<String, PtySession>>`.
///
/// Server-split M2a: this in-process PTY path no longer backs the terminal
/// commands — they stream over the control socket via `crate::remote_pty` now.
/// It is retained (not deleted) so reverting the streaming path to in-process is a
/// one-line swap; `#[allow(dead_code)]` keeps that intentional retention from
/// warning. (The socket-streaming `PtyStreamHandle`/`stream_attach_to_sink` below
/// are still LIVE — used by the server half in `control::serve_pty_attach`.)
#[allow(dead_code)]
pub struct PtySession {
    pub id: String,
    pub tmux_session: String,
    /// Input sink: bytes written here reach the attach client's stdin.
    writer: Box<dyn std::io::Write + Send>,
    /// The PTY master, retained for `resize`.
    master: Box<dyn MasterPty + Send>,
    /// Detaches the attach client (kills the `tmux attach` process). The owned
    /// `Child` lives in the reader thread (so it can `wait()` for the exit code
    /// on EOF); this killer lets us signal it from here without that ownership.
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// The output-draining thread; joined on drop so it can't outlive us.
    reader: Option<JoinHandle<()>>,
    /// Last known size, kept for reference/debugging.
    size: PtySize,
}

#[allow(dead_code)] // retained for the M2a revert path; see PtySession docs.
impl PtySession {
    /// Write raw bytes to the PTY (the attach client's stdin → the shell).
    pub fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()
    }

    /// Resize the PTY. tmux (`window-size latest`) makes the pane follow the
    /// most recently active client, so this visible tile drives the geometry.
    ///
    /// No-ops when the geometry is unchanged: xterm's `fit` addon fires resize
    /// liberally (e.g. on every layout tick), and a redundant `TIOCSWINSZ`
    /// raises a spurious `SIGWINCH` that some full-screen TUIs repaint on.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), String> {
        if self.size.cols == cols && self.size.rows == rows {
            return Ok(());
        }
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        self.master
            .resize(size)
            .map_err(|e| format!("failed to resize pty: {e}"))?;
        self.size = size;
        Ok(())
    }

    /// Detach this tile: kill the `tmux attach` client and tear down the PTY
    /// (writer, master, reader thread). The tmux *session* is intentionally
    /// left running — this is the "survive UI close" guarantee. Killing the
    /// attach client closes the slave, so the reader hits EOF and the thread
    /// exits (then we join it).
    pub fn detach(mut self) {
        // Best-effort: the client may already be gone (process exited).
        let _ = self.killer.kill();
        // Dropping the writer sends EOF to the slave; dropping the master frees
        // the fd. These happen on drop of `self` after this method returns, but
        // we explicitly join the reader so it doesn't linger.
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Safety net: if a PtySession is dropped without `detach()` (e.g. via
        // `kill_terminal`, which kills the tmux session out from under us), make
        // sure the attach client and reader thread are cleaned up. Killing the
        // tmux session already closes the slave → EOF → thread exits; this just
        // guarantees we don't leak the client process or a detached thread.
        let _ = self.killer.kill();
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

/// Build the argv that attaches a PTY client to tmux session `name`.
///
/// On Windows this fronts `wsl.exe` so the ConPTY child runs `tmux attach` inside
/// the distro; on Unix tmux is invoked directly. `cwd` is unused: attaching binds
/// to an existing session whose pane already has its own working directory.
pub fn attach_argv(name: &str, _cwd: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        // `-e` (exec) runs tmux DIRECTLY. A bare `--` would re-join the tail
        // through the user's default shell (zsh), re-expanding `$`/backticks in
        // the session name arg (see the note on tmux.rs::pane_info_command).
        vec![
            "wsl.exe".to_string(),
            "-e".to_string(),
            "tmux".to_string(),
            "-L".to_string(),
            tmux::socket().to_string(),
            "attach".to_string(),
            "-t".to_string(),
            name.to_string(),
        ]
    }
    #[cfg(unix)]
    {
        vec![
            "tmux".to_string(),
            "-L".to_string(),
            tmux::socket().to_string(),
            "attach".to_string(),
            "-t".to_string(),
            name.to_string(),
        ]
    }
}

/// Spawn a PTY whose child is a `tmux attach` client for `tmux_session`, wire up
/// the output-draining reader thread, and return the assembled [`PtySession`].
///
/// Output chunks are base64-encoded and emitted on `terminal://output`; on EOF
/// the reader emits `terminal://exit` (with the client's exit code) and
/// `terminal://state = Exited`.
#[allow(dead_code)] // retained for the M2a revert path; see PtySession docs.
pub fn spawn_attach_client(
    app: &AppHandle,
    id: &str,
    tmux_session: &str,
    cwd: &str,
    cols: u16,
    rows: u16,
) -> Result<PtySession, String> {
    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(size)
        .map_err(|e| format!("failed to open pty: {e}"))?;

    // Assemble the per-platform attach command.
    let argv = attach_argv(tmux_session, cwd);
    let mut builder = CommandBuilder::new(&argv[0]);
    for arg in &argv[1..] {
        builder.arg(arg);
    }
    // Advertise a capable terminal so tmux/programs emit colour + rich keys.
    builder.env("TERM", "xterm-256color");

    let child = pair
        .slave
        .spawn_command(builder)
        .map_err(|e| format!("failed to spawn tmux attach client: {e}"))?;

    // Drop the slave promptly: the master owns the surviving end of the PTY and
    // holding the slave open would prevent EOF from ever being delivered.
    drop(pair.slave);

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("failed to take pty writer: {e}"))?;
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("failed to clone pty reader: {e}"))?;

    // A killer we can use from `PtySession` to detach without owning the Child.
    let killer = child.clone_killer();

    // The reader thread owns the `Child` so it can `wait()` for the exit code
    // after EOF. It captures a cheap `AppHandle` clone and the terminal id.
    let app_for_thread = app.clone();
    let id_for_thread = id.to_string();
    let handle = std::thread::Builder::new()
        .name(format!("t-hub-pty-reader-{id}"))
        .spawn(move || {
            reader_loop(app_for_thread, id_for_thread, reader, child);
        })
        .map_err(|e| format!("failed to spawn pty reader thread: {e}"))?;

    Ok(PtySession {
        id: id.to_string(),
        tmux_session: tmux_session.to_string(),
        writer,
        master: pair.master,
        killer,
        reader: Some(handle),
        size,
    })
}

/// Drain the PTY reader, emitting base64 output chunks, until EOF; then report
/// the child's exit code and an `Exited` state transition.
#[allow(dead_code)] // retained for the M2a revert path; see PtySession docs.
fn reader_loop(
    app: AppHandle,
    id: String,
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
) {
    let mut buf = [0u8; READ_BUF];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break, // EOF: the slave (attach client) closed.
            Ok(n) => {
                let payload = OutputEvent {
                    id: id.clone(),
                    base64: STANDARD.encode(&buf[..n]),
                };
                // If emit fails the frontend/window is gone; nothing useful to
                // do but keep draining so the child isn't blocked on a full pty.
                let _ = app.emit(events::OUTPUT, &payload);
            }
            Err(e) => {
                // On Unix a vanished pty surfaces as EIO at EOF; treat any read
                // error as end-of-stream rather than spinning.
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
        }
    }

    // The stream is closed; reap the child to learn its exit code. `wait()` is
    // safe here because this thread owns `child`; any detach/kill from elsewhere
    // goes through the cloned `ChildKiller`, not through this `child`.
    let code = child
        .wait()
        .ok()
        .and_then(|status| i32::try_from(status.exit_code()).ok());

    let _ = app.emit(events::EXIT, &ExitEvent { id: id.clone(), code });
    let _ = app.emit(
        events::STATE,
        &StateEvent {
            id,
            state: TerminalState::Exited,
        },
    );
}

// ---------------------------------------------------------------------------
// Socket-streaming variant (server-split M2a): the SAME PTY-runs-`tmux attach`
// client, but its output is streamed to an arbitrary byte `sink` (a control-
// channel socket connection) as newline-delimited JSON frames, instead of being
// `app.emit`'d in-process. This is the server half of "tiles over the wire": the
// daemon owns the PTY; the client just renders the frames it reads off the socket.
//
// Frames written to the sink:  {"out":"<base64 chunk>"}  per output chunk, then
// {"exit":<code|null>} once on EOF. (Scrollback is sent by the caller before the
// stream starts, so it isn't duplicated here.)
//
// NOTE: this duplicates [`spawn_attach_client`]/[`reader_loop`] rather than
// generalizing their sink, to keep the in-process terminal nucleus untouched
// while the split is proven; folding the two onto one sink abstraction is a
// follow-up once the socket path is the default.
// ---------------------------------------------------------------------------

/// Handles for driving a socket-streamed PTY: write stdin, resize, detach. The
/// output reader thread streams to the sink on its own; these let the owning
/// connection feed keystrokes / resizes in and tear the client down on disconnect.
pub struct PtyStreamHandle {
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    reader: Option<JoinHandle<()>>,
    size: PtySize,
}

impl PtyStreamHandle {
    /// Write raw bytes to the PTY (the attach client's stdin → the shell).
    pub fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()
    }

    /// Resize the PTY (no-op when unchanged, matching [`PtySession::resize`]).
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), String> {
        if self.size.cols == cols && self.size.rows == rows {
            return Ok(());
        }
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        self.master
            .resize(size)
            .map_err(|e| format!("failed to resize pty: {e}"))?;
        self.size = size;
        Ok(())
    }

    /// Detach: kill the `tmux attach` client (the tmux SESSION survives) and join
    /// the reader thread. Mirrors [`PtySession::detach`].
    pub fn detach(mut self) {
        let _ = self.killer.kill();
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for PtyStreamHandle {
    fn drop(&mut self) {
        let _ = self.killer.kill();
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn a PTY whose child is a `tmux attach` client for `tmux_session`, streaming
/// its output to `sink` as `{"out":"<b64>"}` frames (then `{"exit":code}` on EOF).
/// Returns a [`PtyStreamHandle`] so the owning control connection can write/resize/
/// detach. A sink write failure (the client disconnected) ends the stream — the
/// owning connection then detaches the handle.
pub fn stream_attach_to_sink(
    tmux_session: &str,
    cwd: &str,
    cols: u16,
    rows: u16,
    sink: Box<dyn Write + Send>,
) -> Result<PtyStreamHandle, String> {
    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(size)
        .map_err(|e| format!("failed to open pty: {e}"))?;

    let argv = attach_argv(tmux_session, cwd);
    let mut builder = CommandBuilder::new(&argv[0]);
    for arg in &argv[1..] {
        builder.arg(arg);
    }
    builder.env("TERM", "xterm-256color");

    let child = pair
        .slave
        .spawn_command(builder)
        .map_err(|e| format!("failed to spawn tmux attach client: {e}"))?;
    drop(pair.slave);

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("failed to take pty writer: {e}"))?;
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("failed to clone pty reader: {e}"))?;
    let killer = child.clone_killer();

    let handle = std::thread::Builder::new()
        .name(format!("t-hub-pty-stream-{tmux_session}"))
        .spawn(move || stream_reader_loop(reader, child, sink))
        .map_err(|e| format!("failed to spawn pty stream thread: {e}"))?;

    Ok(PtyStreamHandle {
        writer,
        master: pair.master,
        killer,
        reader: Some(handle),
        size,
    })
}

/// Drain the PTY reader, writing base64 `{"out":...}` frames to `sink` until EOF
/// or a sink write error (the client disconnected); then write one `{"exit":code}`.
fn stream_reader_loop(
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    mut sink: Box<dyn Write + Send>,
) {
    let mut buf = [0u8; READ_BUF];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break, // EOF: the attach client closed.
            Ok(n) => {
                if write_frame(&mut sink, &json!({ "out": STANDARD.encode(&buf[..n]) })).is_err() {
                    // The client (socket) is gone; stop draining + tear down.
                    break;
                }
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
        }
    }

    let code = child
        .wait()
        .ok()
        .and_then(|status| i32::try_from(status.exit_code()).ok());
    let _ = write_frame(&mut sink, &json!({ "exit": code }));
}

/// Write one newline-delimited JSON frame to a byte sink (best-effort flush).
fn write_frame(sink: &mut Box<dyn Write + Send>, frame: &serde_json::Value) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(frame).unwrap_or_default();
    line.push(b'\n');
    sink.write_all(&line)?;
    sink.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn attach_argv_unix_shape() {
        let argv = attach_argv("th_abc123", "/home/user");
        assert_eq!(
            argv,
            vec!["tmux", "-L", "t-hub", "attach", "-t", "th_abc123"]
        );
    }

    #[cfg(windows)]
    #[test]
    fn attach_argv_windows_shape() {
        let argv = attach_argv("th_abc123", "/home/user");
        assert_eq!(
            argv,
            vec![
                "wsl.exe", "-e", "tmux", "-L", "t-hub", "attach", "-t", "th_abc123"
            ]
        );
    }
}
