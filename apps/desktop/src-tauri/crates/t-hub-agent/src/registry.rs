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

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::{mpsc, LazyLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use t_hub_protocol::{TerminalPane, TerminalSnapshot};

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

/// Keep registry collection below the core bridge's 10 second RPC deadline.
const SNAPSHOT_COMMAND_TIMEOUT: Duration = Duration::from_secs(4);

/// Build a `tmux -L <socket>` command with the given args.
fn tmux(args: &[&str]) -> Command {
    let mut cmd = Command::new("tmux");
    cmd.arg("-L").arg(socket());
    cmd.args(args);
    cmd
}

/// Run one snapshot collector in its own process group with a hard deadline.
///
/// A timeout kills and reaps the whole group before returning, so one stalled
/// tmux query cannot wedge the agent's synchronous request dispatcher.
fn bounded_output(mut command: Command, label: &str, timeout: Duration) -> Result<Output> {
    command
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| anyhow!("spawn {label}: {e}"))?;
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let (stdout_tx, stdout_rx) = mpsc::sync_channel(1);
    let (stderr_tx, stderr_rx) = mpsc::sync_channel(1);
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout_pipe.read_to_end(&mut bytes).map(|_| bytes);
        let _ = stdout_tx.send(result);
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stderr_pipe.read_to_end(&mut bytes).map(|_| bytes);
        let _ = stderr_tx.send(result);
    });
    let deadline = Instant::now() + timeout;
    let mut status = None;
    let mut stdout = None;
    let mut stderr = None;

    loop {
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(|e| anyhow!("wait for {label}: {e}"))?;
        }
        if stdout.is_none() {
            if let Ok(result) = stdout_rx.try_recv() {
                stdout = Some(result.map_err(|e| anyhow!("read {label} stdout: {e}"))?);
            }
        }
        if stderr.is_none() {
            if let Ok(result) = stderr_rx.try_recv() {
                stderr = Some(result.map_err(|e| anyhow!("read {label} stderr: {e}"))?);
            }
        }
        match (status.take(), stdout.take(), stderr.take()) {
            (Some(status_value), Some(stdout_value), Some(stderr_value)) => {
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Ok(Output {
                    status: status_value,
                    stdout: stdout_value,
                    stderr: stderr_value,
                });
            }
            (status_value, stdout_value, stderr_value) => {
                status = status_value;
                stdout = stdout_value;
                stderr = stderr_value;
            }
        }
        if Instant::now() >= deadline {
            let pid = child.id() as i32;
            // SAFETY: the child was placed in a new process group whose id
            // equals its pid. A negative pid targets only that group, including
            // descendants that inherited the collector's output pipes.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(anyhow!(
                "{label} timed out after {} ms",
                timeout.as_millis()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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
    let out = bounded_output(
        tmux(&["list-sessions", "-F", "#{session_name}"]),
        "tmux list-sessions",
        SNAPSHOT_COMMAND_TIMEOUT,
    )?;
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

/// Collect live session names and pane metadata inside the persistent WSL
/// agent, so the Windows core does not need recurring `wsl.exe` subprocesses.
pub fn terminal_snapshot() -> Result<TerminalSnapshot> {
    let sessions = list_sessions()?;
    let panes = list_panes()?;
    Ok(TerminalSnapshot { sessions, panes })
}

/// List pane command/cwd metadata while resolving script-hosted Codex and Claude
/// processes to their harness names instead of the generic runtime executable.
fn list_panes() -> Result<Vec<TerminalPane>> {
    let script = format!(
        "tmux -L {socket} list-panes -a -F \
'#{{session_name}}|#{{pane_current_command}}|#{{pane_current_path}}|#{{pane_pid}}' \
| while IFS='|' read -r s cmd path pid; do eff=\"$cmd\"; \
case \"$cmd\" in node|bun|deno|python|python3) \
for kid in $(pgrep -P \"$pid\" 2>/dev/null); do \
line=$(tr '\\0' ' ' < /proc/$kid/cmdline 2>/dev/null); \
case \"$line\" in *codex*) eff=codex; break;; *claude*) eff=claude; break;; esac; \
done;; esac; case \"$eff\" in node|bun|deno|python|python3) \
fgpid=$(ps -o tpgid= -p \"$pid\" 2>/dev/null | tr -d ' '); \
case \"$fgpid\" in ''|*[!0-9]*|0) fgpid=\"$pid\";; esac; \
line=$(tr '\\0' ' ' < /proc/$fgpid/cmdline 2>/dev/null); \
case \"$line\" in *codex*) eff=codex;; *claude*) eff=claude;; esac;; esac; \
printf '%s|%s|%s\\n' \"$s\" \"$eff\" \"$path\"; done",
        socket = socket()
    );
    let mut command = Command::new("bash");
    command.args(["-lc", &script]);
    let out = bounded_output(command, "tmux list-panes", SNAPSHOT_COMMAND_TIMEOUT)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if is_already_gone(&stderr) {
            return Ok(Vec::new());
        }
        return Err(anyhow!("tmux list-panes failed: {}", stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.trim().splitn(3, '|');
            let session = parts.next()?.trim();
            if session.is_empty() {
                return None;
            }
            Some(TerminalPane {
                session: session.to_string(),
                command: parts.next().unwrap_or("").trim().to_string(),
                cwd: parts.next().unwrap_or("").trim().to_string(),
            })
        })
        .collect())
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

    #[test]
    fn bounded_output_reaps_timed_out_process_group_and_recovers() {
        let pid_file = std::env::temp_dir().join(format!("{}.pid", unique()));
        let script = format!(
            "sleep 60 & child=$!; printf '%s' \"$child\" > '{}'; wait",
            pid_file.display()
        );
        let mut hanging = Command::new("bash");
        hanging.args(["-lc", &script]);

        let started = Instant::now();
        let error = bounded_output(hanging, "timeout-test", Duration::from_millis(100))
            .expect_err("collector must time out");
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));

        let descendant_pid: i32 = std::fs::read_to_string(&pid_file)
            .expect("descendant pid file")
            .parse()
            .expect("descendant pid");
        let reap_deadline = Instant::now() + Duration::from_secs(1);
        loop {
            // SAFETY: signal 0 only probes whether the recorded process exists.
            let probe = unsafe { libc::kill(descendant_pid, 0) };
            if probe == -1 {
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH)
                );
                break;
            }
            assert!(
                Instant::now() < reap_deadline,
                "timed-out descendant must be reaped"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_file(pid_file);

        let mut healthy = Command::new("printf");
        healthy.arg("recovered");
        let output = bounded_output(healthy, "recovery-test", Duration::from_secs(1))
            .expect("subsequent command must succeed");
        assert_eq!(output.stdout, b"recovered");
    }

    #[test]
    fn bounded_output_times_out_when_descendant_holds_inherited_pipes() {
        let mut command = Command::new("bash");
        command.args(["-lc", "sleep 60 & exit 0"]);
        let started = Instant::now();
        let error = bounded_output(command, "inherited-pipe-test", Duration::from_millis(100))
            .expect_err("inherited pipe must remain under the deadline");
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn bounded_output_drains_large_stdout_and_stderr() {
        let mut command = Command::new("bash");
        command.args([
            "-lc",
            "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2",
        ]);
        let output = bounded_output(command, "large-output-test", Duration::from_secs(2))
            .expect("large dual output must not fill either pipe");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 1_048_576);
        assert_eq!(output.stderr.len(), 1_048_576);
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
        let snapshot = terminal_snapshot().expect("terminal_snapshot");
        assert!(snapshot.sessions.iter().any(|session| session == &name));
        assert!(snapshot.panes.iter().any(|pane| pane.session == name));
        let _ = capture_pane(&name).expect("capture_pane");
        kill_session(&name).expect("kill_session");
        assert!(!has_session(&name));
    }
}
