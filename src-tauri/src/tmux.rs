//! tmux control on the isolated `termhub` socket — the process-orchestration
//! layer beneath the PTY.
//!
//! Every call uses `tmux -L termhub ...` so TermHub never touches the user's
//! default tmux server (PRD §9.4). This module is pure `std::process::Command`
//! orchestration and is directly testable in WSL2 (tmux is installed),
//! independent of Tauri.
//!
//! Surface:
//!   - `new_session(name, cwd, command)` — detached session, one window/pane,
//!     with `window-size latest` so a stale hidden client can't shrink the pane
//!     (REVIEW.md risk #4).
//!   - `has_session(name) -> bool`
//!   - `kill_session(name)`
//!   - `list_sessions() -> Vec<String>`  (tolerates "no server running")
//!   - `capture_pane(name) -> Vec<u8>`   (scrollback to seed xterm on attach)

use std::process::Command;

/// The isolated tmux socket name; always passed as `tmux -L termhub`.
pub const SOCKET: &str = "termhub";

/// How many lines of scrollback history we capture to seed xterm on attach.
const SCROLLBACK_LINES: i64 = 2000;

/// A structured error from a tmux invocation.
#[derive(Debug, Clone)]
pub struct TmuxError {
    /// The tmux subcommand we attempted (e.g. `"new-session"`).
    pub op: &'static str,
    /// Process exit code, if the process ran and exited with a code.
    pub code: Option<i32>,
    /// Trimmed stderr from tmux (its diagnostic message), or the io error text.
    pub message: String,
}

impl std::fmt::Display for TmuxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code {
            Some(c) => write!(f, "tmux {} failed (exit {}): {}", self.op, c, self.message),
            None => write!(f, "tmux {} failed: {}", self.op, self.message),
        }
    }
}

impl std::error::Error for TmuxError {}

// Allow `?` to bubble a TmuxError up through a `Result<_, String>` command body.
impl From<TmuxError> for String {
    fn from(e: TmuxError) -> Self {
        e.to_string()
    }
}

/// Build a `tmux -L termhub` command with the given args.
fn tmux(args: &[&str]) -> Command {
    let mut cmd = Command::new("tmux");
    cmd.arg("-L").arg(SOCKET);
    cmd.args(args);
    cmd
}

/// True when stderr indicates the server simply isn't running yet. This is the
/// benign "no sessions exist" case for read operations, not a real failure.
fn is_no_server(stderr: &str) -> bool {
    stderr.contains("no server running")
}

/// True when stderr indicates the target session/pane is already gone — either
/// the server isn't running, the named session doesn't exist, or the server is
/// mid-teardown and can't resolve a target. Used to make kill/lookup idempotent.
///
/// tmux 3.4 phrasings observed on the `termhub` socket:
///   - `no server running on <socket>`                    (server down)
///   - `can't find session: <name>`                       (session absent)
///   - `can't find pane: <name>`                          (capture target absent)
///   - `no current target`                                (server tearing down)
///   - `error connecting to <socket> (No such file ...)`  (socket unlinked mid-race)
fn is_already_gone(stderr: &str) -> bool {
    is_no_server(stderr)
        || stderr.contains("can't find session")
        || stderr.contains("can't find pane")
        || stderr.contains("no current target")
        || stderr.contains("error connecting to")
        || stderr.contains("No such file or directory")
}

/// Run a tmux command and capture its output, mapping non-zero exits and io
/// failures into a structured [`TmuxError`].
fn run(op: &'static str, args: &[&str]) -> Result<std::process::Output, TmuxError> {
    let output = tmux(args).output().map_err(|e| TmuxError {
        op,
        code: None,
        message: format!("failed to spawn tmux: {e}"),
    })?;

    if output.status.success() {
        Ok(output)
    } else {
        Err(TmuxError {
            op,
            code: output.status.code(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// Create a new detached tmux session named `name`, rooted at `cwd`.
///
/// `new-session -d` starts the session detached with a single window/pane. When
/// `command` is `None` tmux runs the user's login shell (the default for the
/// nucleus). We then pin `window-size latest`: with multiple potential clients
/// (a freshly attached visible tile and a stale hidden one) this makes the pane
/// track the most recently active client instead of shrinking to the smallest,
/// which would otherwise corrupt the visible layout (REVIEW.md risk #4).
pub fn new_session(name: &str, cwd: &str, command: Option<&str>) -> Result<(), TmuxError> {
    let mut args: Vec<&str> = vec!["new-session", "-d", "-s", name, "-c", cwd];
    if let Some(cmd) = command {
        // The command (and any embedded args) is the trailing program for the
        // session's first pane; tmux runs it via the shell.
        args.push(cmd);
    }
    run("new-session", &args)?;

    // Pin the pane to the latest active client. Best-effort: if this fails the
    // session still exists, so we surface it as an error only if tmux reports
    // one (the session create above already succeeded).
    run("set-option", &["set-option", "-t", name, "window-size", "latest"])?;
    Ok(())
}

/// Returns true if a session named `name` exists on the `termhub` socket.
///
/// `has-session` exits 0 when the session exists and non-zero otherwise
/// (including when no server is running at all), so the exit status is the
/// single source of truth — no stderr parsing required.
pub fn has_session(name: &str) -> bool {
    tmux(&["has-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Kill the tmux session named `name` (terminating its process tree).
///
/// Treated as success if the session (or the whole server) is already gone, so
/// killing an already-dead terminal is idempotent.
pub fn kill_session(name: &str) -> Result<(), TmuxError> {
    let output = tmux(&["kill-session", "-t", name])
        .output()
        .map_err(|e| TmuxError {
            op: "kill-session",
            code: None,
            message: format!("failed to spawn tmux: {e}"),
        })?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if is_already_gone(&stderr) {
        // Nothing to kill — already gone. Idempotent success.
        return Ok(());
    }

    Err(TmuxError {
        op: "kill-session",
        code: output.status.code(),
        message: stderr.trim().to_string(),
    })
}

/// List all session names on the `termhub` socket.
///
/// Tolerates the "no server running" case (no sessions have ever been created,
/// or the last one was killed and the server exited) by returning an empty Vec
/// rather than an error.
pub fn list_sessions() -> Result<Vec<String>, TmuxError> {
    let output = tmux(&["list-sessions", "-F", "#{session_name}"])
        .output()
        .map_err(|e| TmuxError {
            op: "list-sessions",
            code: None,
            message: format!("failed to spawn tmux: {e}"),
        })?;

    if output.status.success() {
        let names = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        return Ok(names);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    // No server / socket already torn down ⇒ there are simply no sessions.
    if is_no_server(&stderr) || stderr.contains("error connecting to") {
        return Ok(Vec::new());
    }

    Err(TmuxError {
        op: "list-sessions",
        code: output.status.code(),
        message: stderr.trim().to_string(),
    })
}

/// Capture the visible pane plus `SCROLLBACK_LINES` of scrollback for `name`,
/// preserving ANSI escape sequences (`-e`), so the frontend can seed xterm with
/// the existing screen state on (re)attach.
///
/// Returns the raw bytes (which include escape sequences); the caller is
/// responsible for base64-encoding before sending over IPC.
pub fn capture_pane(name: &str) -> Result<Vec<u8>, TmuxError> {
    let start = format!("-{SCROLLBACK_LINES}");
    let output = run(
        "capture-pane",
        &["capture-pane", "-p", "-e", "-S", &start, "-t", name],
    )?;
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a unique throwaway session name so concurrent test runs (or a
    /// crashed prior run) don't collide.
    fn unique_name() -> String {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("th_test_{ts}")
    }

    /// Full lifecycle on the isolated socket: create → list contains it →
    /// has_session true → capture returns bytes → kill → has_session false.
    ///
    /// NOTE: requires a real `tmux` on PATH. It compiles everywhere but only
    /// passes where tmux is installed (it is in this WSL2 dev shell; it is not
    /// expected to run on the Windows CI target).
    #[test]
    fn lifecycle_create_list_capture_kill() {
        let name = unique_name();

        // Clean slate in case a previous run leaked this name (it shouldn't).
        let _ = kill_session(&name);

        new_session(&name, "/tmp", None).expect("new_session should succeed");

        assert!(has_session(&name), "session should exist after creation");

        let sessions = list_sessions().expect("list_sessions should succeed");
        assert!(
            sessions.iter().any(|s| s == &name),
            "list_sessions {sessions:?} should contain {name}"
        );

        // capture-pane should succeed for a live session and return some bytes
        // (at minimum the shell prompt / blank pane).
        let captured = capture_pane(&name).expect("capture_pane should succeed");
        // The pane may legitimately be empty bytes if rendering hasn't settled,
        // but the call itself must succeed; we only assert it returned Ok above.
        let _ = captured;

        kill_session(&name).expect("kill_session should succeed");
        assert!(
            !has_session(&name),
            "session should be gone after kill_session"
        );
    }

    /// kill_session on a missing session is idempotent (success), and
    /// has_session reports false for a name that was never created.
    #[test]
    fn kill_missing_is_idempotent() {
        let name = format!("{}_never", unique_name());
        assert!(!has_session(&name));
        kill_session(&name).expect("killing a missing session should be Ok");
    }

    /// list_sessions tolerates the no-server / empty case by returning Ok
    /// (possibly empty) rather than erroring.
    #[test]
    fn list_sessions_tolerates_empty() {
        // Whether or not a server is running, this must not error.
        let _ = list_sessions().expect("list_sessions must tolerate no-server");
    }
}
