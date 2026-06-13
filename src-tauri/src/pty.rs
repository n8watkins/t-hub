//! PTY management — implemented by the Rust backend subagent (task #9).
//!
//! Bridges a portable-pty master to a tmux session on the isolated `termhub`
//! socket, with one PTY client per visible tile. Output is read on a dedicated
//! thread and emitted to the frontend as base64 `terminal://output` events.
//!
//! Platform model (the key abstraction that lets the nucleus run in WSL now):
//!   - `#[cfg(windows)]`: spawn `wsl.exe -d <distro> -- <command>` (ConPTY).
//!   - `#[cfg(unix)]`:    spawn the login shell directly (testable in WSL2).
//!
//! Keep only `Send` handles in `PtySession` (it lives inside the Tauri-managed
//! `Mutex<HashMap<..>>`): typically the boxed PTY writer plus the reader-thread
//! join handle; route output through the `AppHandle`, not through shared state.

/// A live terminal: its TermHub id, the backing tmux session name, and (once
/// implemented) the PTY writer + reader thread handles.
pub struct PtySession {
    pub id: String,
    pub tmux_session: String,
    // TODO(subagent #9): writer: Box<dyn std::io::Write + Send>, reader handle,
    //                    master (for resize), child, current (cols, rows).
}

/// Build the argv that reaches an interactive shell on this platform.
///
/// On Windows this fronts `wsl.exe`; on Unix it is the shell directly. The
/// subagent wires this into `portable_pty::CommandBuilder`.
pub fn shell_command(_shell: Option<&str>, _cwd: Option<&str>) -> Vec<String> {
    // TODO(subagent #9): implement per-platform argv.
    Vec::new()
}
