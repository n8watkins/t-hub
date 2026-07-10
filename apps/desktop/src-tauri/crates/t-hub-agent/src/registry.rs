//! tmux session registry on the isolated `t-hub` socket (agent side; a
//! `cargo test` build uses `t-hub-test` so tests never touch the live socket).
//!
//! Mirrors the core's `src-tauri/src/tmux.rs` surface, but runs *inside WSL*
//! where tmux actually lives. The agent is the future single owner of tmux
//! control (PLAN.md Workstream A: "tmux/session registry & commands"); for 0.5
//! these provide the registry RPCs ([`ListSessions`]/[`NewSession`]/
//! [`HasSession`]/[`KillSession`]/[`CapturePane`]).
//!
//! SUBAGENT(registry): flesh out scrollback line count, `window-size latest`
//! pinning, and richer error classification to match `tmux.rs`. The signatures
//! below are the contract and must not change.

use std::process::Command;

use anyhow::{anyhow, Result};
use std::sync::LazyLock;

/// The resolved tmux socket name, always passed as `tmux -L <socket>`.
///
/// Resolved ONCE from `$T_HUB_TMUX_SOCKET`, defaulting to `"t-hub"` - mirroring
/// the app-side `t_hub_lib::tmux` so the agent and the app share one socket. In a
/// `cargo test` build the default flips to `"t-hub-test"` so the lifecycle test
/// (which creates + reaps REAL tmux sessions) can NEVER touch the live `t-hub`
/// socket a running app drives. An explicit env still wins; the shipped binary is
/// byte-for-byte unchanged (the `cfg(test)` branch only compiles under test).
static SOCKET_NAME: LazyLock<String> = LazyLock::new(|| {
    std::env::var("T_HUB_TMUX_SOCKET").unwrap_or_else(|_| default_socket_name().into())
});

/// The compiled-in default socket name: `"t-hub"` normally, `"t-hub-test"` under
/// `cfg(test)` (see [`SOCKET_NAME`]).
const fn default_socket_name() -> &'static str {
    if cfg!(test) {
        "t-hub-test"
    } else {
        "t-hub"
    }
}

/// The resolved tmux socket name (`$T_HUB_TMUX_SOCKET` or the default).
pub fn socket() -> &'static str {
    &SOCKET_NAME
}

/// Lines of scrollback to capture when seeding xterm.
const SCROLLBACK_LINES: i64 = 2000;

/// Build a `tmux -L <socket>` command with the given args.
fn tmux(args: &[&str]) -> Command {
    let mut cmd = Command::new("tmux");
    cmd.arg("-L").arg(socket());
    cmd.args(args);
    cmd
}

/// True when stderr indicates the server simply isn't running / target is gone.
fn is_already_gone(stderr: &str) -> bool {
    stderr.contains("no server running")
        || stderr.contains("can't find session")
        || stderr.contains("can't find pane")
        || stderr.contains("no current target")
        || stderr.contains("error connecting to")
        || stderr.contains("No such file or directory")
}

/// List session names on the `t-hub` socket (empty when no server runs).
pub fn list_sessions() -> Result<Vec<String>> {
    let out = tmux(&["list-sessions", "-F", "#{session_name}"])
        .output()
        .map_err(|e| anyhow!("spawn tmux list-sessions: {e}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if is_already_gone(&stderr) {
        return Ok(Vec::new());
    }
    Err(anyhow!("tmux list-sessions failed: {}", stderr.trim()))
}

/// Create a detached session rooted at `cwd`, pinning `window-size latest`.
pub fn new_session(name: &str, cwd: &str, command: Option<&str>) -> Result<()> {
    let mut args: Vec<&str> = vec!["new-session", "-d", "-s", name, "-c", cwd];
    if let Some(c) = command {
        args.push(c);
    }
    let out = tmux(&args)
        .output()
        .map_err(|e| anyhow!("spawn tmux new-session: {e}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "tmux new-session failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // Best-effort: pin window size to the latest active client.
    let _ = tmux(&["set-option", "-t", name, "window-size", "latest"]).output();
    Ok(())
}

/// Whether a session exists (exit status is authoritative).
pub fn has_session(name: &str) -> bool {
    tmux(&["has-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Kill a session; idempotent (Ok if already gone).
pub fn kill_session(name: &str) -> Result<()> {
    let out = tmux(&["kill-session", "-t", name])
        .output()
        .map_err(|e| anyhow!("spawn tmux kill-session: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if is_already_gone(&stderr) {
        return Ok(());
    }
    Err(anyhow!("tmux kill-session failed: {}", stderr.trim()))
}

/// Capture the visible pane + scrollback (ANSI preserved) as raw bytes.
pub fn capture_pane(name: &str) -> Result<Vec<u8>> {
    let start = format!("-{SCROLLBACK_LINES}");
    let out = tmux(&["capture-pane", "-p", "-e", "-S", &start, "-t", name])
        .output()
        .map_err(|e| anyhow!("spawn tmux capture-pane: {e}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "tmux capture-pane failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique() -> String {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("th_agent_test_{ts}")
    }

    /// Kills its session on drop - including on a panicking assertion - so the
    /// lifecycle test can NEVER leak a session (paired with the `cfg(test)`
    /// `t-hub-test` socket isolation, a leak can neither hit the live app nor
    /// linger).
    struct SessionGuard(String);
    impl Drop for SessionGuard {
        fn drop(&mut self) {
            let _ = kill_session(&self.0);
        }
    }

    // Requires a real tmux (present in this WSL2 dev shell). Runs on the isolated
    // `t-hub-test` socket (the cfg(test) default), never the live `t-hub`.
    #[test]
    fn lifecycle() {
        let name = unique();
        let _guard = SessionGuard(name.clone());
        let _ = kill_session(&name);
        new_session(&name, "/tmp", None).expect("new_session");
        assert!(has_session(&name));
        assert!(list_sessions().unwrap().iter().any(|s| s == &name));
        let _ = capture_pane(&name).expect("capture_pane");
        kill_session(&name).expect("kill_session");
        assert!(!has_session(&name));
    }
}
