//! tmux control on the isolated `termhub` socket — implemented by the Rust
//! backend subagent (task #9).
//!
//! Every call uses `tmux -L termhub ...` so TermHub never touches the user's
//! default tmux server (PRD §9.4). This module is pure process orchestration and
//! is directly testable in WSL2 (tmux is installed), independent of Tauri.
//!
//! Planned surface:
//!   - `new_session(name, cwd, command)` — detached session, one window/pane.
//!   - `has_session(name) -> bool`
//!   - `kill_session(name)`
//!   - `list_sessions() -> Vec<String>`
//!   - `capture_pane(name) -> Vec<u8>`  (scrollback to seed xterm on attach)
//!   - set `window-size latest` so a stale hidden client can't shrink the pane
//!     (REVIEW.md risk #4).

/// The isolated tmux socket name; pass as `tmux -L termhub`.
pub const SOCKET: &str = "termhub";

// TODO(subagent #9): implement the functions above over `std::process::Command`,
// returning structured errors. Add unit/integration tests that create, list,
// capture, and kill a throwaway session on the `termhub` socket.
