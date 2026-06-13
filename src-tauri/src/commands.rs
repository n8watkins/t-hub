//! Tauri command surface — the IPC contract, mirrored from `src/ipc/types.ts`.
//!
//! Command names and (camelCase) payload fields MUST stay in lockstep with the
//! frontend contract. The bodies below are stubs implemented by the Rust backend
//! subagent (task #9); they compile and return errors until then.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::pty::PtySession;

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

#[tauri::command]
pub async fn spawn_terminal(
    _app: tauri::AppHandle,
    _state: tauri::State<'_, TerminalManager>,
    _opts: SpawnOptions,
) -> Result<TerminalInfo, String> {
    // TODO(subagent #9): create a tmux session on the `termhub` socket, spawn a
    // PTY client attached to it, and stream output on `terminal://output`.
    Err("spawn_terminal not yet implemented".into())
}

#[tauri::command]
pub async fn attach_terminal(
    _app: tauri::AppHandle,
    _state: tauri::State<'_, TerminalManager>,
    _id: String,
    _cols: u16,
    _rows: u16,
) -> Result<String, String> {
    // TODO(subagent #9): (re)attach a PTY client and return base64 scrollback
    // (tmux capture-pane) to seed xterm.
    Err("attach_terminal not yet implemented".into())
}

#[tauri::command]
pub async fn write_terminal(
    _state: tauri::State<'_, TerminalManager>,
    _id: String,
    _data: String,
) -> Result<(), String> {
    // TODO(subagent #9): write utf-8 input bytes to the PTY master.
    Err("write_terminal not yet implemented".into())
}

#[tauri::command]
pub async fn resize_terminal(
    _state: tauri::State<'_, TerminalManager>,
    _id: String,
    _cols: u16,
    _rows: u16,
) -> Result<(), String> {
    // TODO(subagent #9): resize the PTY (and rely on tmux window-size latest).
    Err("resize_terminal not yet implemented".into())
}

#[tauri::command]
pub async fn close_terminal(
    _state: tauri::State<'_, TerminalManager>,
    _id: String,
) -> Result<(), String> {
    // TODO(subagent #9): detach the PTY client; leave the tmux session alive.
    Err("close_terminal not yet implemented".into())
}

#[tauri::command]
pub async fn kill_terminal(
    _state: tauri::State<'_, TerminalManager>,
    _id: String,
) -> Result<(), String> {
    // TODO(subagent #9): kill the tmux session (stop the process).
    Err("kill_terminal not yet implemented".into())
}

#[tauri::command]
pub async fn list_terminals(
    _state: tauri::State<'_, TerminalManager>,
) -> Result<Vec<TerminalInfo>, String> {
    // TODO(subagent #9): reconcile against `tmux -L termhub list-sessions`.
    Err("list_terminals not yet implemented".into())
}
