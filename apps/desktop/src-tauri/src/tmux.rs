//! tmux control on the isolated `t-hub` socket — the process-orchestration
//! layer beneath the PTY.
//!
//! Every call uses `tmux -L t-hub ...` so T-Hub never touches the user's
//! default tmux server (PRD §9.4). This module is pure `std::process::Command`
//! orchestration and is directly testable in WSL2 (tmux is installed),
//! independent of Tauri.
//!
//! Surface:
//!   - `new_session_with_env(name, cwd, command, env)` — detached session, one
//!     window/pane, with `window-size latest` so a stale hidden client can't shrink
//!     the pane (REVIEW.md risk #4), and optional per-session `-e KEY=VALUE` env.
//!   - `has_session(name) -> bool`
//!   - `kill_session(name)`
//!   - `list_sessions() -> Vec<String>`  (tolerates "no server running")

use std::process::Command;
use std::sync::LazyLock;
use std::time::Duration;

use crate::bounded_exec::output_with_timeout;

/// The isolated tmux socket name; always passed as `tmux -L <socket>`.
///
/// Resolved ONCE at startup from `$T_HUB_TMUX_SOCKET`, defaulting to
/// `"t-hub"`. The env hook exists so a second, side-by-side **DEV** instance
/// can run alongside the production app on its OWN tmux socket (e.g.
/// `T_HUB_TMUX_SOCKET=t-hub-dev`) without ever sharing sessions with — or
/// killing — production's terminals. With NO env var set the value is exactly
/// `"t-hub"`, so default behavior is byte-for-byte unchanged.
///
/// TEST ISOLATION: in a `cargo test` build of THIS crate the default flips to
/// `"t-hub-test"` so a unit test that spins up REAL tmux sessions (the attach-churn
/// suite) can NEVER create or reap sessions on the live `-L t-hub` socket a running
/// app is driving - the exact hazard behind the leaked `th_s27churn*` ghosts that
/// broke the app's post-restart adopt path. An explicit `$T_HUB_TMUX_SOCKET` still
/// wins (so a test can pin its own unique socket); only the *default* changes, and
/// only under `cfg(test)`, so the shipped binary is byte-for-byte unchanged.
///
/// SCOPE: this `cfg(test)` default covers only THIS crate's unit tests. Sibling
/// isolation lives with each producer: the `t-hub-agent` crate mirrors this
/// `cfg(test)` default in its own `registry::socket()`, and the `tests/mcp_e2e.rs`
/// integration test (a separate binary that shells out to `tmux -L` directly, so
/// this const never governs it) pins `$T_HUB_TMUX_SOCKET` to a per-process name.
/// So no test across the workspace touches the live socket - but that guarantee is
/// the sum of those three mechanisms, not this const alone.
static SOCKET_NAME: LazyLock<String> = LazyLock::new(|| {
    std::env::var("T_HUB_TMUX_SOCKET").unwrap_or_else(|_| default_socket_name().into())
});

/// The compiled-in default socket name: `"t-hub"` in a normal build, but
/// `"t-hub-test"` under `cfg(test)` so the test binary is isolated from the live
/// app's socket (see [`SOCKET_NAME`]).
const fn default_socket_name() -> &'static str {
    if cfg!(test) {
        "t-hub-test"
    } else {
        "t-hub"
    }
}

/// The resolved tmux socket name (`$T_HUB_TMUX_SOCKET` or `"t-hub"`),
/// always passed as `tmux -L <socket>`. Read once; cheap to call repeatedly.
pub fn socket() -> &'static str {
    &SOCKET_NAME
}

/// tmux per-window scrollback cap for NEW sessions. The default 2000 is why you
/// couldn't scroll up far. `history-limit` is per-window and FIXED at window
/// creation, so we set it GLOBALLY (`-g`) before `new-session` — new terminals keep
/// deep history; already-created windows keep their old limit. ~50k lines is cheap.
const HISTORY_LIMIT: i64 = 50000;

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

/// The identity of exactly one tmux pane at one instant.
///
/// This is crate-private launch evidence, not a public control-plane value.
/// A respawn must retain the session, window, and pane identities while replacing
/// the pane process generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneGeneration {
    pub(crate) session_id: u64,
    pub(crate) session_created: u64,
    pub(crate) window_id: u64,
    pub(crate) pane_id: u64,
    pub(crate) pane_pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessCgroupGeneration {
    pub(crate) path: String,
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

#[cfg(target_os = "linux")]
pub(crate) struct PrivateProcessCgroup {
    generation: ProcessCgroupGeneration,
}

#[cfg(target_os = "linux")]
pub(crate) struct SystemdProcessScopeGuardian {
    writer: Option<std::process::ChildStdin>,
    child: Option<std::process::Child>,
}

#[cfg(target_os = "linux")]
pub(crate) struct ProcessCgroupConfinementError {
    pub(crate) error: TmuxError,
    pub(crate) process_cgroup: Option<PrivateProcessCgroup>,
}

#[cfg(target_os = "linux")]
impl PrivateProcessCgroup {
    pub(crate) fn generation(&self) -> &ProcessCgroupGeneration {
        &self.generation
    }

    pub(crate) fn retain(self) -> ProcessCgroupGeneration {
        self.generation.clone()
    }
}

#[cfg(target_os = "linux")]
impl SystemdProcessScopeGuardian {
    fn create(unit: &str, pid: u32, path: &std::path::Path) -> Result<Self, TmuxError> {
        use std::io::BufRead;
        use std::process::Stdio;

        const GUARDIAN: &str = r#"
[ ! -e "$3" ] || exit 72
/usr/bin/busctl --user call org.freedesktop.systemd1 /org/freedesktop/systemd1 org.freedesktop.systemd1.Manager StartTransientUnit 'ssa(sv)a(sa(sv))' "$1" fail 2 PIDs au 1 "$2" Delegate b true 0 >/dev/null || exit 73
printf 'owned\n'
if IFS= read -r state; then
  [ "$state" = persisted ] && exit 0
fi
/usr/bin/systemctl --user stop "$1" >/dev/null 2>&1 || true
/usr/bin/systemctl --user reset-failed "$1" >/dev/null 2>&1 || true
exit 0
"#;
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                GUARDIAN,
                "t-hub-systemd-scope-guardian",
                unit,
                &pid.to_string(),
                path.to_string_lossy().as_ref(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| TmuxError {
                op: "create-systemd-scope",
                code: error.raw_os_error(),
                message: "systemd scope guardian could not start".into(),
            })?;
        let writer = child.stdin.take().ok_or_else(|| TmuxError {
            op: "create-systemd-scope",
            code: None,
            message: "systemd scope guardian has no ownership channel".into(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| TmuxError {
            op: "create-systemd-scope",
            code: None,
            message: "systemd scope guardian has no receipt channel".into(),
        })?;
        let mut reader = std::io::BufReader::new(stdout);
        let mut receipt = String::new();
        reader.read_line(&mut receipt).map_err(|error| TmuxError {
            op: "create-systemd-scope",
            code: error.raw_os_error(),
            message: "systemd scope guardian receipt is unreadable".into(),
        })?;
        if receipt.trim() != "owned" {
            return Err(TmuxError {
                op: "create-systemd-scope",
                code: None,
                message: "systemd scope guardian did not prove creation".into(),
            });
        }
        Ok(Self {
            writer: Some(writer),
            child: Some(child),
        })
    }

    pub(crate) fn disarm(&mut self) -> Result<(), TmuxError> {
        use std::io::Write;

        let mut writer = self.writer.take().ok_or_else(|| TmuxError {
            op: "disarm-systemd-scope-guardian",
            code: None,
            message: "systemd scope guardian was already disarmed".into(),
        })?;
        writer
            .write_all(b"persisted\n")
            .map_err(|error| TmuxError {
                op: "disarm-systemd-scope-guardian",
                code: error.raw_os_error(),
                message: "systemd scope guardian could not be disarmed".into(),
            })?;
        drop(writer);
        let status = self
            .child
            .take()
            .ok_or_else(|| TmuxError {
                op: "disarm-systemd-scope-guardian",
                code: None,
                message: "systemd scope guardian process is missing".into(),
            })?
            .wait()
            .map_err(|error| TmuxError {
                op: "disarm-systemd-scope-guardian",
                code: error.raw_os_error(),
                message: "systemd scope guardian result is unreadable".into(),
            })?;
        if !status.success() {
            return Err(TmuxError {
                op: "disarm-systemd-scope-guardian",
                code: status.code(),
                message: "systemd scope guardian rejected persistence".into(),
            });
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for SystemdProcessScopeGuardian {
    fn drop(&mut self) {
        drop(self.writer.take());
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

#[cfg(all(target_os = "linux", test))]
fn remove_exact_empty_cgroup_generation(
    expected: &ProcessCgroupGeneration,
) -> Result<bool, TmuxError> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let path = std::path::Path::new(&expected.path);
    let parent_path = path.parent().ok_or_else(|| TmuxError {
        op: "remove-process-cgroup",
        code: None,
        message: "private cgroup parent is missing".into(),
    })?;
    let name = path.file_name().ok_or_else(|| TmuxError {
        op: "remove-process-cgroup",
        code: None,
        message: "private cgroup name is missing".into(),
    })?;
    let name = CString::new(name.as_bytes()).map_err(|_| TmuxError {
        op: "remove-process-cgroup",
        code: None,
        message: "private cgroup name is malformed".into(),
    })?;
    let parent = std::fs::File::open(parent_path).map_err(|error| TmuxError {
        op: "remove-process-cgroup",
        code: error.raw_os_error(),
        message: "private cgroup parent could not be pinned".into(),
    })?;
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(true);
        }
        return Err(TmuxError {
            op: "remove-process-cgroup",
            code: error.raw_os_error(),
            message: "private cgroup could not be pinned for removal".into(),
        });
    }
    let pinned = unsafe { std::fs::File::from_raw_fd(descriptor) };
    let metadata = pinned.metadata().map_err(|error| TmuxError {
        op: "remove-process-cgroup",
        code: error.raw_os_error(),
        message: "private cgroup removal identity is unreadable".into(),
    })?;
    if metadata.dev() != expected.device || metadata.ino() != expected.inode {
        return Err(TmuxError {
            op: "remove-process-cgroup",
            code: None,
            message: "private cgroup identity changed at removal boundary".into(),
        });
    }
    #[cfg(test)]
    run_cgroup_cleanup_test_hook(CgroupCleanupTestPoint::BeforeRemove)?;
    let boundary_descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if boundary_descriptor < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(true);
        }
        return Err(TmuxError {
            op: "remove-process-cgroup",
            code: error.raw_os_error(),
            message: "private cgroup removal boundary is unreadable".into(),
        });
    }
    let boundary = unsafe { std::fs::File::from_raw_fd(boundary_descriptor) };
    let boundary_metadata = boundary.metadata().map_err(|error| TmuxError {
        op: "remove-process-cgroup",
        code: error.raw_os_error(),
        message: "private cgroup boundary identity is unreadable".into(),
    })?;
    if boundary_metadata.dev() != expected.device || boundary_metadata.ino() != expected.inode {
        return Err(TmuxError {
            op: "remove-process-cgroup",
            code: None,
            message: "private cgroup identity changed at the deletion boundary".into(),
        });
    }
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(libc::EBUSY) | Some(libc::ENOTEMPTY)
    ) {
        return Ok(false);
    }
    if error.raw_os_error() == Some(libc::ENOENT) {
        return Ok(true);
    }
    Err(TmuxError {
        op: "remove-process-cgroup",
        code: error.raw_os_error(),
        message: "private cgroup could not be removed".into(),
    })
}

pub(crate) fn is_canonical_private_process_cgroup_path(path: &str) -> bool {
    let path = std::path::Path::new(path);
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(identity) = name.strip_prefix("t-hub-cortana-") else {
        return false;
    };
    let managed_scope = identity.strip_suffix(".scope");
    let identity = managed_scope.unwrap_or(identity);
    identity.len() == 32
        && identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        && path
            .parent()
            .and_then(std::path::Path::file_name)
            .and_then(|parent| parent.to_str())
            .is_some_and(|parent| {
                parent == "app.slice"
                    || managed_scope.is_none()
                        && parent.starts_with("tmux-spawn-")
                        && parent.ends_with(".scope")
            })
        && path.starts_with("/sys/fs/cgroup/user.slice/")
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
}

fn is_systemd_owned_process_scope_path(path: &str) -> bool {
    let path = std::path::Path::new(path);
    is_canonical_private_process_cgroup_path(path.to_string_lossy().as_ref())
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".scope"))
}

#[cfg(test)]
fn frozen_process_identity_matches(
    before: (u32, u64),
    after: (u32, u64),
    exact_cgroup_membership: bool,
) -> bool {
    before == after && exact_cgroup_membership
}

/// Evidence for a private dormant-pane to provider-pane transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RespawnPaneTransition {
    pub(crate) before: PaneGeneration,
    pub(crate) after: PaneGeneration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactGenerationKill {
    Killed,
    Gone,
    GenerationMismatch,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CgroupCleanupTestPoint {
    BeforeFreeze,
    AfterFreeze,
    AfterFirstSnapshot,
    AfterIdentityValidation,
    BeforeKill,
    BeforePinnedSignals,
    AfterKill,
    BeforeThaw,
    BeforeRemove,
    BeforeOwnedScopeRemovalWait,
}

#[cfg(all(target_os = "linux", test))]
fn cgroup_confinement_crash_boundary(stage: &str, path: &std::path::Path) {
    if std::env::var("T_HUB_CGROUP_CONFINEMENT_CRASH_STAGE").as_deref() != Ok(stage) {
        return;
    }
    if let Some(receipt) = std::env::var_os("T_HUB_CGROUP_CONFINEMENT_PATH_RECEIPT") {
        let _ = std::fs::write(receipt, path.to_string_lossy().as_bytes());
    }
    std::process::exit(93);
}

#[cfg(test)]
thread_local! {
    static CGROUP_CLEANUP_TEST_HOOK: std::cell::RefCell<
        Option<Box<dyn FnMut(CgroupCleanupTestPoint) -> Result<(), TmuxError>>>
    > = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn run_cgroup_cleanup_test_hook(point: CgroupCleanupTestPoint) -> Result<(), TmuxError> {
    CGROUP_CLEANUP_TEST_HOOK.with(|hook| {
        let mut hook = hook.borrow_mut();
        hook.as_mut().map_or(Ok(()), |hook| hook(point))
    })
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

/// Build a `tmux -L t-hub` command with the given args.
///
/// tmux lives inside WSL, so on Windows every control command is routed through
/// `wsl.exe -e tmux …`; on Unix (including the WSL dev build) tmux is invoked
/// directly. Both then carry `-L t-hub` plus the caller's args.
fn tmux(args: &[&str]) -> Command {
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        // `--cd ~` roots the tmux server (and each new session's pane) at the WSL
        // home, so new terminals open in ~ (native ext4) instead of the app's
        // /mnt/c launch dir -- matching the user's normal `~` terminal view.
        // `-e` (exec) runs tmux DIRECTLY. A bare `--` re-joins the tail and routes
        // it through the user's DEFAULT shell (zsh), which re-expands `$`/backticks
        // in caller args -- e.g. a send-keys payload containing `$HOME` arrived
        // pre-expanded (see the note on `pane_info_command`).
        let mut c = Command::new("wsl.exe");
        c.arg("--cd").arg("~").arg("-e").arg("tmux");
        // CREATE_NO_WINDOW: every tmux control command routes through `wsl.exe`,
        // and each `wsl.exe` spawn would otherwise flash a console (CMD) window
        // for a split second. Suppress it so terminal spawns stay invisible.
        c.creation_flags(0x0800_0000);
        c
    };
    #[cfg(unix)]
    let mut cmd = Command::new("tmux");
    cmd.arg("-L").arg(socket());
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
/// tmux 3.4 phrasings observed on the `t-hub` socket:
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

/// Default per-command timeout for a tmux/wsl subprocess invocation (residual
/// control-flap fix).
///
/// The `-L t-hub` tmux server is SINGLE-THREADED: while it services one slow
/// operation - a large `capture-pane`, a `new-session` blocked on slow (e.g.
/// OneDrive-backed) filesystem I/O, a kill-tree sweep - every OTHER client command
/// QUEUES behind it inside the server. A control handler thread that ran a bare
/// `.output()` with no bound then PARKS for the full stall. Because the control
/// server caps live connections ([`crate::control::MAX_CONNS`]), enough parked
/// handlers make `serve` reject every NEW connection - which is exactly the residual
/// flap: `list_terminals` round-trips time out for minutes (bare TCP connect still
/// completes via the kernel backlog) while the app UI stays alive, and freshly
/// created sessions never get adopted. #45 bounded the socket read/write legs; it
/// did NOT bound the tmux SUBPROCESS the read handlers block on. This does.
///
/// Bounding the subprocess turns an indefinite park into a fast, recoverable error
/// that frees the handler thread and its connection slot, so a transient server
/// stall can no longer escalate into a channel-wide wedge. The Windows-to-WSL hop
/// can take more than four seconds on an otherwise healthy, already-running distro,
/// so the bound must also leave room for process startup and tmux itself. A call that
/// exceeds ten seconds is still treated as stalled so the caller can recover.
const TMUX_CMD_TIMEOUT_DEFAULT: Duration = Duration::from_secs(10);

/// Effective per-command tmux timeout: `$T_HUB_TMUX_CMD_TIMEOUT_SECS` (seconds) if
/// set to a positive integer, else [`TMUX_CMD_TIMEOUT_DEFAULT`]. Unset / 0 / junk ⇒
/// the default (NEVER unbounded - the whole point is that no tmux call may park a
/// control handler forever). The env hook lets an operator widen it on a slow host.
fn tmux_cmd_timeout() -> Duration {
    std::env::var("T_HUB_TMUX_CMD_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map(Duration::from_secs)
        .unwrap_or(TMUX_CMD_TIMEOUT_DEFAULT)
}

/// Run a tmux command and capture its output, mapping non-zero exits and io
/// failures into a structured [`TmuxError`]. Bounded by [`tmux_cmd_timeout`] so a
/// wedged server surfaces as an error instead of parking the caller forever.
fn run(op: &'static str, args: &[&str]) -> Result<std::process::Output, TmuxError> {
    let output = output_with_timeout(tmux(args), tmux_cmd_timeout()).map_err(|e| TmuxError {
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

fn parse_pane_generation(line: &str) -> Option<PaneGeneration> {
    let mut fields = line.trim().split('|');
    let parse_prefixed = |value: Option<&str>, prefix: char| {
        value
            .and_then(|value| value.strip_prefix(prefix))
            .and_then(|value| value.parse::<u64>().ok())
    };
    let parse_positive_number = |value: Option<&str>| {
        value
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
    };
    let session_id = parse_prefixed(fields.next(), '$')?;
    let session_created = parse_positive_number(fields.next())?;
    let window_id = parse_prefixed(fields.next(), '@')?;
    let pane_id = parse_prefixed(fields.next(), '%')?;
    let pane_pid = u32::try_from(parse_positive_number(fields.next())?).ok()?;
    if fields.next().is_some() {
        return None;
    }
    Some(PaneGeneration {
        session_id,
        session_created,
        window_id,
        pane_id,
        pane_pid,
    })
}

fn pane_generation(target: &str) -> Result<PaneGeneration, TmuxError> {
    let output = run(
        "list-panes",
        &[
            "list-panes",
            "-t",
            target,
            "-F",
            "#{session_id}|#{session_created}|#{window_id}|#{pane_id}|#{pane_pid}",
        ],
    )?;
    let value = String::from_utf8_lossy(&output.stdout);
    let mut lines = value.lines().filter(|line| !line.trim().is_empty());
    let line = lines.next().ok_or_else(|| TmuxError {
        op: "list-panes",
        code: None,
        message: "target pane generation is unavailable".into(),
    })?;
    if lines.next().is_some() {
        return Err(TmuxError {
            op: "list-panes",
            code: None,
            message: "target resolved to more than one pane".into(),
        });
    }
    parse_pane_generation(line).ok_or_else(|| TmuxError {
        op: "list-panes",
        code: None,
        message: "target pane generation is malformed".into(),
    })
}

/// Observe one pane only through tmux's exact-session target syntax.
/// A similarly prefixed foreign session must never satisfy this lookup.
pub(crate) fn pane_generation_exact(name: &str) -> Result<PaneGeneration, TmuxError> {
    // list-panes accepts a target-pane, so include the trailing window separator;
    // without it, `=name` may be interpreted as a pane/window target and still
    // resolve a uniquely prefixed session.
    pane_generation(&format!("={name}:"))
}

/// Replace one exact dormant pane with `command` without injecting keys into a
/// shell line editor.
///
/// `target`, `cwd`, and `command` remain argv values to tmux.  The command is
/// intentionally not returned or logged.  The pre/post evidence proves the
/// same session, window, and pane survived while the pane process generation
/// changed.
pub(crate) fn respawn_pane_exact(
    target: &str,
    cwd: &str,
    command: &str,
) -> Result<RespawnPaneTransition, TmuxError> {
    let exact_pane = format!("={target}:");
    let before = pane_generation(&exact_pane)?;
    let mut args = vec!["respawn-pane", "-k", "-t", &exact_pane];
    if !cwd.is_empty() {
        args.extend(["-c", cwd]);
    }
    args.push(command);
    args.extend([
        ";",
        "display-message",
        "-p",
        "-t",
        &exact_pane,
        "#{session_id}|#{session_created}|#{window_id}|#{pane_id}|#{pane_pid}",
    ]);
    let output = run("respawn-pane", &args)?;
    let value = String::from_utf8_lossy(&output.stdout);
    let mut lines = value.lines().filter(|line| !line.trim().is_empty());
    let line = lines.next().ok_or_else(|| TmuxError {
        op: "respawn-pane",
        code: None,
        message: "respawned pane generation is unavailable".into(),
    })?;
    if lines.next().is_some() {
        return Err(TmuxError {
            op: "respawn-pane",
            code: None,
            message: "respawned pane returned multiple generations".into(),
        });
    }
    let after = parse_pane_generation(line).ok_or_else(|| TmuxError {
        op: "respawn-pane",
        code: None,
        message: "respawned pane generation is malformed".into(),
    })?;
    if before.session_id != after.session_id
        || before.session_created != after.session_created
        || before.window_id != after.window_id
        || before.pane_id != after.pane_id
        || before.pane_pid == after.pane_pid
    {
        return Err(TmuxError {
            op: "respawn-pane",
            code: None,
            message: "target pane generation changed unexpectedly during respawn".into(),
        });
    }
    Ok(RespawnPaneTransition { before, after })
}

/// Read one value from a tmux session's private environment without exposing it
/// through process arguments or logs. Missing variables return `None`.
pub fn session_environment(name: &str, key: &str) -> Result<Option<String>, TmuxError> {
    let output = run("show-environment", &["show-environment", "-t", name, key]);
    match output {
        Ok(output) => {
            let line = String::from_utf8_lossy(&output.stdout);
            Ok(line
                .trim()
                .split_once('=')
                .map(|(_, value)| value.to_string()))
        }
        Err(error) if error.message.contains("unknown variable") => Ok(None),
        Err(error) => Err(error),
    }
}

/// Read a session environment value through tmux's exact-session target syntax.
pub(crate) fn session_environment_exact(
    name: &str,
    key: &str,
) -> Result<Option<String>, TmuxError> {
    session_environment(&format!("={name}"), key)
}

/// Set private environment values on one exact existing session.
///
/// This is used by the provider launch handoff only after its executable has
/// been opened and provenance-bound. Values are argv to the local tmux client,
/// are never returned or logged, and become visible only to subsequently
/// respawned children in this exact session.
#[cfg(test)]
pub(crate) fn set_session_environment_exact(
    name: &str,
    env: &[(String, String)],
) -> Result<(), TmuxError> {
    let target = format!("={name}");
    for (key, value) in env {
        if key.is_empty()
            || key.contains('=')
            || key.chars().any(char::is_whitespace)
            || key.chars().any(char::is_control)
        {
            return Err(TmuxError {
                op: "set-environment",
                code: None,
                message: "session environment key is malformed".into(),
            });
        }
        run(
            "set-environment",
            &["set-environment", "-t", &target, key, value],
        )?;
    }
    Ok(())
}

/// Remove private environment values from one exact existing session.
///
/// Every requested key is attempted even if an earlier removal fails, so one
/// malformed or transiently unreadable entry cannot leave the remaining bearer
/// values behind without an error.
#[cfg(test)]
pub(crate) fn unset_session_environment_exact(
    name: &str,
    keys: &[String],
) -> Result<(), TmuxError> {
    let target = format!("={name}");
    let mut first_error = None;
    for key in keys {
        let result = if key.is_empty()
            || key.contains('=')
            || key.chars().any(char::is_whitespace)
            || key.chars().any(char::is_control)
        {
            Err(TmuxError {
                op: "set-environment",
                code: None,
                message: "session environment key is malformed".into(),
            })
        } else {
            run(
                "set-environment",
                &["set-environment", "-u", "-t", &target, key],
            )
            .map(|_| ())
        };
        if first_error.is_none() {
            first_error = result.err();
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Create a new detached tmux session named `name`, rooted at `cwd`, with optional
/// per-session environment variables via tmux `-e` (socket-gate Phase 2b).
///
/// `new-session -d` starts the session detached with a single window/pane. When
/// `command` is `None` tmux runs the user's login shell (the default for the
/// nucleus). We then pin `window-size latest`: with multiple potential clients
/// (a freshly attached visible tile and a stale hidden one) this makes the pane
/// track the most recently active client instead of shrinking to the smallest,
/// which would otherwise corrupt the visible layout (REVIEW.md risk #4).
///
/// tmux applies `-e KEY=VALUE` to the session BEFORE the first pane execs, so an
/// in-session process (e.g. the MCP server the pane later launches) inherits them -
/// and, unlike prefixing the pane command, the values never appear in
/// `ps`/`pane_start_command`. Because the session env OVERRIDES the tmux server's
/// inherited global env, this is also how item-3's UI spawn path SCRUBS an inherited
/// `T_HUB_CONTROL_TOKEN` (it sets its own value at the session level). `env` empty ⇒
/// a plain login-shell session with no injected capability env.
pub fn new_session_with_env(
    name: &str,
    cwd: &str,
    command: Option<&str>,
    env: &[(String, String)],
) -> Result<(), TmuxError> {
    new_session_with_env_generation(name, cwd, command, env).map(|_| ())
}

/// Create one session and return the canonical tmux generation emitted by that
/// same `new-session` operation. A caller never has to rediscover a created
/// candidate by name before it can journal generation-bound cleanup evidence.
pub(crate) fn new_session_with_env_generation(
    name: &str,
    cwd: &str,
    command: Option<&str>,
    env: &[(String, String)],
) -> Result<PaneGeneration, TmuxError> {
    // `-c CWD` only when we actually have a (WSL-side) directory; on Windows the
    // default is empty so the pane starts in wsl.exe's launch dir rather than an
    // invalid Windows path.
    //
    // `-x 80 -y 24`: DETERMINISTIC spawn geometry. Without it, tmux ≥3.4 sizes a
    // detached session from the server's latest client — and a wedged/dead attach
    // client (the task-27 churn bug) can report 2x24, so every fresh session (and
    // any `startupCommand` booting in it, e.g. `claude --resume`) started life in
    // a 2-column pane until the first attach resized it. 80x24 is the classic
    // fallback; the first real attach reflows to the tile's true geometry anyway.
    let generation_format =
        "#{session_name}|#{session_id}|#{session_created}|#{window_id}|#{pane_id}|#{pane_pid}";
    let mut args: Vec<&str> = vec![
        "new-session",
        "-d",
        "-P",
        "-F",
        generation_format,
        "-x",
        "80",
        "-y",
        "24",
        "-s",
        name,
    ];
    // Session environment (`-e KEY=VALUE`, socket-gate Phase 2b). Pre-format so the
    // backing strings outlive `args`. tmux ≥3.2 supports `-e`; this codebase already
    // targets ≥3.4 (see the geometry note above).
    let env_pairs: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    for pair in &env_pairs {
        args.push("-e");
        args.push(pair);
    }
    if !cwd.is_empty() {
        args.push("-c");
        args.push(cwd);
    }
    if let Some(cmd) = command {
        // The command (and any embedded args) is the trailing program for the
        // session's first pane; tmux runs it via the shell.
        args.push(cmd);
    }

    // Raise the scrollback cap for the window we're about to create. history-limit
    // is per-window and fixed at creation, so it must be the GLOBAL default BEFORE
    // new-session. start-server first so `set -g` has a server to set it on (fresh
    // boot).
    //
    // All three run as ONE tmux command sequence (`;`-separated argv, no shell
    // involved) so the whole critical path costs a single process launch — on
    // Windows each tmux call is a full `wsl.exe` spawn (hundreds of ms), and this
    // path used to make ~13 of them per Ctrl+T.
    let limit = HISTORY_LIMIT.to_string();
    let mut seq: Vec<&str> = vec![
        "start-server",
        ";",
        "set-option",
        "-g",
        "history-limit",
        &limit,
        ";",
    ];
    seq.extend_from_slice(&args);
    let created = run("new-session", &seq)?;
    let value = String::from_utf8_lossy(&created.stdout);
    let mut lines = value.lines().filter(|line| !line.trim().is_empty());
    let line = lines.next().ok_or_else(|| TmuxError {
        op: "new-session",
        code: None,
        message: "created session generation is unavailable".into(),
    })?;
    if lines.next().is_some() {
        return Err(TmuxError {
            op: "new-session",
            code: None,
            message: "created session returned multiple generations".into(),
        });
    }
    let (canonical_name, generation) = line.split_once('|').ok_or_else(|| TmuxError {
        op: "new-session",
        code: None,
        message: "created session generation is malformed".into(),
    })?;
    if canonical_name != name {
        return Err(TmuxError {
            op: "new-session",
            code: None,
            message: "created session canonical name does not match its request".into(),
        });
    }
    let generation = parse_pane_generation(generation).ok_or_else(|| TmuxError {
        op: "new-session",
        code: None,
        message: "created session generation is malformed".into(),
    })?;

    // Post-create tuning, batched into one more tmux call. All best-effort — the
    // session already exists, so never fail the spawn over these:
    //   - window-size latest: pin the pane to the latest active client.
    //   - status off: T-Hub draws its own tile chrome, suppress tmux's status bar.
    //   - mouse on (global): full-screen apps that request mouse mode receive the
    //     wheel and scroll their OWN content; applies to existing sessions too.
    //     Trade-off: mouse text-selection needs Shift+drag.
    //   - global keybinds: prefix-disable + right-click unbinds. `ensure_mouse_on()`
    //     applies these at startup but may fire before any tmux server exists;
    //     re-running here — once a server is guaranteed to exist — makes them stick.
    let mut post: Vec<&str> = vec![
        "set-option",
        "-t",
        name,
        "window-size",
        "latest",
        ";",
        "set-option",
        "-t",
        name,
        "status",
        "off",
        ";",
        "set-option",
        "-g",
        "mouse",
        "on",
        ";",
    ];
    post.extend_from_slice(GLOBAL_KEYBIND_ARGS);
    let _ = run("set-option", &post);
    Ok(generation)
}

/// Test-only: pin a window to a deterministic geometry. `resize-window` flips
/// the window to MANUAL sizing, which production must never do (the attach path
/// relies on `window-size latest` tracking the visible client) — but the live
/// round-trip tests attach no client at all, and on a server whose latest
/// client is a wedged 2x24 attach (the task-27 churn artifact) a fresh session
/// otherwise wraps every line at 2 columns and the capture assertions fail.
#[cfg(test)]
pub(crate) fn resize_window_for_tests(name: &str, cols: u16, rows: u16) -> Result<(), TmuxError> {
    let (c, r) = (cols.to_string(), rows.to_string());
    run(
        "resize-window",
        &["resize-window", "-t", name, "-x", &c, "-y", &r],
    )
    .map(|_| ())
}

/// Disable tmux's Ctrl+B prefix and its right-click context menus, server-global.
///
/// Both are root-table (`-n`) / GLOBAL (`-g`) operations, so applying them once
/// covers existing AND future sessions — re-running per-session is harmless.
///
/// 1. Prefix OFF: the user uses NO tmux keybindings and wants `C-b` to reach the
///    app (it becomes an app-level shortcut). `prefix None` takes a VALUE (so it's
///    a `set-option`, not an `unbind`); we also unbind `C-b` in both the root
///    table (`-n`, what actually fires it) and the prefix table for good measure.
/// 2. Right-click menus OFF: with `mouse on`, a right-click pops tmux's own pane /
///    status menus (Split/Kill/Respawn/Zoom...) — confusing inside T-Hub, which
///    has its own tile chrome. Unbind the four root-table MouseDown3 events.
///
/// Best-effort: every error is swallowed (no server yet, etc.).
///
/// The whole set is one `;`-separated tmux command sequence so it costs a single
/// process launch (one `wsl.exe` spawn on Windows) instead of seven.
const GLOBAL_KEYBIND_ARGS: &[&str] = &[
    "set-option",
    "-g",
    "prefix",
    "None",
    ";",
    "unbind",
    "-n",
    "C-b",
    ";",
    "unbind",
    "C-b",
    ";",
    "unbind",
    "-n",
    "MouseDown3Pane",
    ";",
    "unbind",
    "-n",
    "MouseDown3Status",
    ";",
    "unbind",
    "-n",
    "MouseDown3StatusLeft",
    ";",
    "unbind",
    "-n",
    "MouseDown3StatusRight",
];

fn apply_global_keybinds() {
    let _ = run("set-option", GLOBAL_KEYBIND_ARGS);
}

/// Force `mouse on` for the whole server AND every existing session.
///
/// `new_session` sets `-g mouse on`, but a GLOBAL option is overridden by any
/// SESSION-LOCAL `mouse` value. Sessions created by older T-Hub builds (before
/// the mouse-on change) carry a session-local `mouse off`, and the tmux server is
/// preserved across deploys — so the later global flip never reached them and the
/// wheel still sent Up/Down arrow keys in those panes (e.g. zsh history) instead
/// of scrolling. Here we (1) set the global default and (2) explicitly set
/// `mouse on` on each LIVE session so a stale per-session `off` can't win.
///
/// Best-effort and side-effect-free on failure: every error is swallowed so this
/// can run at startup (off-thread) without ever aborting the app or disturbing a
/// running session (toggling the option does not perturb the pane's process).
pub fn ensure_mouse_on() {
    let _ = run("set-option", &["set-option", "-g", "mouse", "on"]);
    if let Ok(sessions) = list_sessions() {
        for s in &sessions {
            let _ = run(
                "set-option",
                &["set-option", "-t", s.as_str(), "mouse", "on"],
            );
        }
    }
    // Disable the C-b prefix and tmux's built-in mouse context menus,
    // server-global (covers existing + future sessions). See the helper for why.
    // Note: at fresh boot this may run before any tmux server exists, so the
    // unbinds silently no-op here; `new_session()` re-applies them once a server
    // is guaranteed to exist.
    apply_global_keybinds();
}

/// Resolve a T-Hub terminal id to its tmux session name. The id IS the session's
/// `th_`-prefixed suffix capped at 8 chars (see `commands::spawn_terminal`). This
/// is the SINGLE source of that mapping — shared by the in-process commands AND the
/// control channel (`remote_pty`/`serve_pty_attach`) so the client and server can
/// never derive a different name for the same id (a real footgun if the id scheme
/// ever changes). An id already prefixed `th_` (a full session name) passes through;
/// a bare id becomes `th_<id[..8]>` (the cap is a no-op for today's 8-char ids but
/// defends a future longer-id scheme).
pub fn target_for_id(id: &str) -> String {
    if id.starts_with("th_") {
        id.to_string()
    } else {
        format!("th_{}", &id[..id.len().min(8)])
    }
}

/// The outcome of a session-liveness probe, distinguishing a DEFINITIVE answer
/// (the `has-session` subprocess ran and reported) from an INDETERMINATE one (the
/// probe timed out or the subprocess failed to spawn).
///
/// Collapsing `Unknown` into `Gone` is the conflation behind the 0.3.62 spawn
/// wedge: under a degraded subprocess-spawn path a bounded `has-session` probe
/// TIMES OUT, and a LIVE session then read as absent — so `send_text`/`close`
/// reported "no such session" and the captain-transfer/prune paths could seize or
/// retire a live ship. Callers that must not act on an ambiguous probe match on
/// `Unknown` explicitly (or go through [`is_definitively_gone`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLiveness {
    /// `has-session` exited 0 — the session exists on the `t-hub` socket.
    Alive,
    /// `has-session` ran and exited non-zero — no such session (or no server
    /// running). A DEFINITIVE negative: the probe completed and reported absence.
    Gone,
    /// The probe did NOT complete: it exceeded [`tmux_cmd_timeout`] or the
    /// subprocess failed to spawn. Liveness is INDETERMINATE — never treat as
    /// `Gone`. This is the residual-wedge signal a healthy control plane never
    /// emits.
    Unknown,
}

/// Probe whether `name` exists on the `t-hub` socket, DISTINGUISHING a definitive
/// answer from an indeterminate one (see [`SessionLiveness`]).
///
/// `has-session` exits 0 when the session exists and non-zero otherwise (including
/// when no server is running at all), so a COMPLETED run maps cleanly to
/// `Alive`/`Gone` with no stderr parsing. A timeout or spawn failure is `Unknown`
/// — the whole point of the three-state split: a stalled control plane must never
/// make a live session read as gone.
pub fn session_liveness(name: &str) -> SessionLiveness {
    match output_with_timeout(tmux(&["has-session", "-t", name]), tmux_cmd_timeout()) {
        Ok(o) if o.status.success() => SessionLiveness::Alive,
        Ok(_) => SessionLiveness::Gone,
        Err(_) => SessionLiveness::Unknown,
    }
}

/// Verify that a tmux session exists and its foreground process is the expected
/// coding harness. A surviving fallback shell is a definitive `Gone` harness,
/// even though the terminal session itself remains alive.
pub fn harness_liveness(name: &str, harness: &str) -> SessionLiveness {
    match session_liveness(name) {
        SessionLiveness::Gone => return SessionLiveness::Gone,
        SessionLiveness::Unknown => return SessionLiveness::Unknown,
        SessionLiveness::Alive => {}
    }
    let expected = harness.trim().to_ascii_lowercase();
    if !matches!(expected.as_str(), "codex" | "claude") {
        return SessionLiveness::Unknown;
    }
    match pane_info() {
        Ok(panes) => panes.into_iter().find(|pane| pane.session == name).map_or(
            SessionLiveness::Unknown,
            |pane| {
                if pane.command.eq_ignore_ascii_case(&expected) {
                    SessionLiveness::Alive
                } else if is_fallback_shell(&pane.command) {
                    SessionLiveness::Gone
                } else {
                    SessionLiveness::Unknown
                }
            },
        ),
        Err(_) => SessionLiveness::Unknown,
    }
}

fn is_fallback_shell(command: &str) -> bool {
    matches!(
        command.trim().to_ascii_lowercase().as_str(),
        "bash" | "cmd" | "fish" | "nu" | "powershell" | "pwsh" | "sh" | "zsh"
    )
}

/// The transfer-grade / reap-grade death signal (R1): `true` ONLY when a probe
/// DEFINITIVELY reported the session absent. `Alive` (obviously) and `Unknown` (a
/// timed-out/failed probe) are BOTH not-gone, so a degraded control plane can
/// never seize a live captain's ship, retire a live identity, or emit a spurious
/// EXIT. This encodes the item-2 two-tier liveness invariant: ambiguous is never
/// seized.
pub fn is_definitively_gone(liveness: SessionLiveness) -> bool {
    matches!(liveness, SessionLiveness::Gone)
}

/// Convenience boolean over [`session_liveness`]: `true` IFF the session is
/// definitively `Alive`.
///
/// An `Unknown` (timed-out / failed) probe maps to `false`, so this is ONLY safe
/// for callers whose "not alive" branch is itself the safe action for an
/// indeterminate probe — a post-spawn verify that reaps + fails-retryable, or a
/// test. Callers that must not conflate `Unknown` with `Gone` (the send/close/
/// attach gates, the captain-transfer signal, the identity prune, the stream-end
/// EXIT decision) call [`session_liveness`] / [`is_definitively_gone`] directly.
pub fn has_session(name: &str) -> bool {
    matches!(session_liveness(name), SessionLiveness::Alive)
}

/// Reassert `window-size latest` on `name` (best-effort; never fails a caller).
///
/// The spawn path pins `window-size latest` ONCE at session creation
/// ([`new_session_with_env`]), after which nothing reasserts it — so a session
/// flipped to `window-size manual` out of band (historically, a captain forcing
/// `manual 220x50` to work around the width-2 background-client bug) stays manual
/// for the LIFE of the tmux session, across every attach/detach and app restart.
/// A stale manual override overshoots the real tile: content clips off the right
/// edge and tmux paints its `fill-character` dot field in the unused area.
///
/// Reasserting `latest` on every ATTACH is the belt-and-braces half of the
/// tile-attach fix (the frontend measurement guard in `Terminal.tsx` is the
/// primary cause): once the front end can no longer push a degenerate ~2-col
/// size, `window-size latest` alone keeps each window tracking its focused
/// client's real width, so a stale manual/degenerate size can never outlive its
/// purpose. Best-effort — the session already exists and is streaming, so we
/// never fail an attach over this.
pub fn reassert_window_size_latest(name: &str) {
    let _ = output_with_timeout(
        tmux(&["set-option", "-t", name, "window-size", "latest"]),
        tmux_cmd_timeout(),
    );
}

/// Kill the tmux session named `name` via plain `kill-session` (SIGHUP).
///
/// Treated as success if the session (or the whole server) is already gone, so
/// killing an already-dead terminal is idempotent.
///
/// Production callers use [`kill_session_tree`] (SIGHUP-ignoring processes like
/// `claude` survive a bare kill-session and leak); this lighter primitive is
/// kept for tests, which spawn plain shells and don't need the tree sweep.
#[cfg_attr(not(test), allow(dead_code))]
pub fn kill_session(name: &str) -> Result<(), TmuxError> {
    let output = output_with_timeout(tmux(&["kill-session", "-t", name]), tmux_cmd_timeout())
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

/// Like [`kill_session`] but GUARANTEES the pane process tree dies. `tmux
/// kill-session` only SIGHUPs the pane process group, so a `claude` that
/// ignores/handles SIGHUP survives and leaks (the orphan growth behind the
/// ~4.5 GB). So we first enumerate THIS session's pane pids and SIGKILL each pid
/// plus FOUR levels of descendants — the pane is a login shell (`zsh -ilc 'claude
/// …'`), so the depths are L0=shell, L1=claude, L2=claude's node/MCP children,
/// L3=their children — STRICTLY scoped to the named session's pid subtree (never a
/// `pkill`-by-name, and never a process-GROUP kill, either of which could reach
/// another workspace), then kill the session. Runs through the same `bash -lc`
/// helper as [`pane_info`] (on Windows `wsl.exe -e bash`) so the single-quoted tmux
/// `#{...}` format survives the round-trip (a bare `#` is eaten as a shell comment
/// under wsl.exe). Best-effort: a daemonized escapee (its own setsid) survives any
/// signal-based reap, as it would under tmux too. Idempotent — an already-gone /
/// no-server session is success; but a REAL kill-session failure now propagates
/// (no blanket `exit 0`) so a genuine reap failure surfaces instead of silently
/// leaking.
pub fn kill_session_tree(name: &str) -> Result<(), TmuxError> {
    // Each kill in the loop is `2>/dev/null` (a dead/raced pid is fine), but the
    // FINAL `kill-session` is NOT suppressed and is the LAST command, so the
    // script's exit status == kill-session's: 0 on success, non-zero+stderr on a
    // real failure (which the caller surfaces), and the already-gone case is
    // absorbed below.
    let script = format!(
        "for pid in $(tmux -L {sock} list-panes -t '{name}' -F '#{{pane_pid}}' 2>/dev/null); do \
l1=$(pgrep -P \"$pid\" 2>/dev/null); \
l2=$(for k in $l1; do pgrep -P \"$k\" 2>/dev/null; done); \
l3=$(for k in $l2; do pgrep -P \"$k\" 2>/dev/null; done); \
kill -9 $pid $l1 $l2 $l3 2>/dev/null; \
done; \
tmux -L {sock} kill-session -t '{name}'",
        sock = socket(),
        name = name,
    );
    let output =
        output_with_timeout(pane_info_command(&script), tmux_cmd_timeout()).map_err(|e| {
            TmuxError {
                op: "kill-session-tree",
                code: None,
                message: format!("failed to spawn tmux: {e}"),
            }
        })?;
    if output.status.success() {
        return Ok(());
    }
    // Idempotent success when the session is simply already gone (is_already_gone
    // already subsumes the no-server case). A genuine failure falls through to Err.
    let stderr = String::from_utf8_lossy(&output.stderr);
    if is_already_gone(&stderr) {
        return Ok(());
    }
    Err(TmuxError {
        op: "kill-session-tree",
        code: output.status.code(),
        message: stderr.trim().to_string(),
    })
}

#[cfg(all(target_os = "linux", test))]
pub(crate) fn create_private_process_cgroup() -> Result<PrivateProcessCgroup, TmuxError> {
    let membership = std::fs::read_to_string("/proc/self/cgroup").map_err(|error| TmuxError {
        op: "create-process-cgroup",
        code: error.raw_os_error(),
        message: "current cgroup membership is unreadable".into(),
    })?;
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| TmuxError {
            op: "create-process-cgroup",
            code: None,
            message: "a unified cgroup v2 membership is unavailable".into(),
        })?;
    let app_slice = relative
        .find("/app.slice")
        .map(|end| &relative[..end + "/app.slice".len()])
        .ok_or_else(|| TmuxError {
            op: "create-process-cgroup",
            code: None,
            message: "the application has no delegated cgroup app.slice".into(),
        })?;
    let parent = std::path::Path::new("/sys/fs/cgroup").join(app_slice.trim_start_matches('/'));
    create_private_process_cgroup_under(&parent)
}

#[cfg(all(target_os = "linux", test))]
fn create_private_process_cgroup_under(
    parent: &std::path::Path,
) -> Result<PrivateProcessCgroup, TmuxError> {
    let path = parent.join(format!("t-hub-cortana-{}", uuid::Uuid::new_v4().simple()));
    create_private_process_cgroup_at(&path)
}

#[cfg(all(target_os = "linux", test))]
fn create_private_process_cgroup_at(
    path: &std::path::Path,
) -> Result<PrivateProcessCgroup, TmuxError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};

    struct Cleanup(Option<std::path::PathBuf>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(path) = self.0.take() {
                let _ = std::fs::remove_dir(path);
            }
        }
    }

    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path).map_err(|error| TmuxError {
        op: "create-process-cgroup",
        code: error.raw_os_error(),
        message: "private Cortana cgroup could not be created".into(),
    })?;
    let mut cleanup = Cleanup(Some(path.to_path_buf()));
    for control in ["cgroup.procs", "cgroup.events", "cgroup.freeze"] {
        if !path.join(control).exists() {
            return Err(TmuxError {
                op: "create-process-cgroup",
                code: None,
                message: "private Cortana cgroup lacks required v2 controls".into(),
            });
        }
    }
    let metadata = std::fs::metadata(path).map_err(|error| TmuxError {
        op: "create-process-cgroup",
        code: error.raw_os_error(),
        message: "private Cortana cgroup identity is unreadable".into(),
    })?;
    if metadata.mode() & 0o077 != 0 {
        return Err(TmuxError {
            op: "create-process-cgroup",
            code: None,
            message: "private Cortana cgroup permissions are not private".into(),
        });
    }
    cleanup.0 = None;
    Ok(PrivateProcessCgroup {
        generation: ProcessCgroupGeneration {
            path: path.to_string_lossy().into_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
        },
    })
}

/// Move an exact credential-free gate-blocked pane into a systemd-owned
/// transient scope. The caller must prove its anonymous launch gate is engaged
/// and must not release credentials until this returns.
#[cfg(target_os = "linux")]
pub(crate) fn confine_blocked_pane_to_private_cgroup(
    name: &str,
    expected: PaneGeneration,
) -> Result<(PrivateProcessCgroup, SystemdProcessScopeGuardian), ProcessCgroupConfinementError> {
    use std::os::fd::{FromRawFd, OwnedFd};

    fn error(message: &str, code: Option<i32>) -> TmuxError {
        TmuxError {
            op: "confine-stopped-pane",
            code,
            message: message.into(),
        }
    }

    fn process_state_start(pid: u32) -> Result<(char, u64), TmuxError> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|failure| {
            error(
                "stopped candidate process identity is unreadable",
                failure.raw_os_error(),
            )
        })?;
        let suffix = stat
            .rsplit_once(')')
            .map(|(_, suffix)| suffix.trim())
            .ok_or_else(|| error("stopped candidate process identity is malformed", None))?;
        let fields = suffix.split_whitespace().collect::<Vec<_>>();
        let state = fields
            .first()
            .and_then(|value| value.chars().next())
            .ok_or_else(|| error("stopped candidate process state is malformed", None))?;
        let start_ticks = fields
            .get(19)
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| error("stopped candidate process generation is malformed", None))?;
        Ok((state, start_ticks))
    }

    fn process_membership(pid: u32) -> Result<String, TmuxError> {
        let membership =
            std::fs::read_to_string(format!("/proc/{pid}/cgroup")).map_err(|failure| {
                error(
                    "stopped candidate cgroup membership is unreadable",
                    failure.raw_os_error(),
                )
            })?;
        membership
            .lines()
            .find_map(|line| line.strip_prefix("0::").map(str::to_string))
            .ok_or_else(|| error("stopped candidate has no unified cgroup membership", None))
    }

    fn open_pidfd(pid: u32) -> Result<OwnedFd, TmuxError> {
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if descriptor < 0 {
            let failure = std::io::Error::last_os_error();
            return Err(error(
                "stopped candidate pidfd could not be opened",
                failure.raw_os_error(),
            ));
        }
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor as i32) })
    }

    let fail = |error, process_cgroup| ProcessCgroupConfinementError {
        error,
        process_cgroup,
    };
    match pane_generation_exact(name) {
        Ok(actual) if actual == expected => {}
        Ok(_) => {
            return Err(fail(
                error("stopped candidate tmux generation changed", None),
                None,
            ));
        }
        Err(failure) => return Err(fail(failure, None)),
    }
    let _pidfd = open_pidfd(expected.pane_pid).map_err(|error| fail(error, None))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let (settled_membership, blocked_start_ticks) = loop {
        let first = process_state_start(expected.pane_pid).map_err(|error| fail(error, None))?;
        let first_membership =
            process_membership(expected.pane_pid).map_err(|error| fail(error, None))?;
        std::thread::sleep(Duration::from_millis(10));
        let second = process_state_start(expected.pane_pid).map_err(|error| fail(error, None))?;
        let second_membership =
            process_membership(expected.pane_pid).map_err(|error| fail(error, None))?;
        if !matches!(first.0, 'Z' | 'X' | 'x')
            && first == second
            && first_membership == second_membership
            && pane_generation_exact(name).ok() == Some(expected)
        {
            break (first_membership, first.1);
        }
        if std::time::Instant::now() >= deadline {
            return Err(fail(
                error(
                    "credential-free candidate did not block in a stable tmux scope",
                    None,
                ),
                None,
            ));
        }
    };
    let parent =
        std::path::Path::new("/sys/fs/cgroup").join(settled_membership.trim_start_matches('/'));
    let parent_name = parent
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !parent.starts_with("/sys/fs/cgroup/user.slice/")
        || !settled_membership.contains("/app.slice/app-tmux.slice/")
        || !parent_name.starts_with("tmux-spawn-")
        || !parent_name.ends_with(".scope")
    {
        return Err(fail(
            error(
                "candidate tmux scope is not a delegated private launch scope",
                None,
            ),
            None,
        ));
    }
    let app_slice = settled_membership
        .find("/app.slice")
        .map(|end| &settled_membership[..end + "/app.slice".len()])
        .ok_or_else(|| fail(error("candidate has no systemd app slice", None), None))?;
    let unit = format!("t-hub-cortana-{}.scope", uuid::Uuid::new_v4().simple());
    let process_cgroup_path = std::path::Path::new("/sys/fs/cgroup")
        .join(app_slice.trim_start_matches('/'))
        .join(&unit);
    #[cfg(test)]
    cgroup_confinement_crash_boundary("before-create", &process_cgroup_path);
    if process_cgroup_path.exists() {
        return Err(fail(
            error("candidate scope path existed before systemd creation", None),
            None,
        ));
    }
    #[cfg(test)]
    if let Some(receipt) = std::env::var_os("T_HUB_CGROUP_PRECREATE_PATH_RECEIPT") {
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700).create(&process_cgroup_path).unwrap();
        std::fs::write(receipt, process_cgroup_path.to_string_lossy().as_bytes()).unwrap();
    }
    #[cfg(test)]
    if std::env::var_os("T_HUB_CGROUP_GUARDIAN_ARM_FAILURE").is_some() {
        return Err(fail(
            error("injected systemd scope creation failure", None),
            None,
        ));
    }
    let guardian =
        SystemdProcessScopeGuardian::create(&unit, expected.pane_pid, &process_cgroup_path)
            .map_err(|error| fail(error, None))?;
    #[cfg(test)]
    cgroup_confinement_crash_boundary("after-create", &process_cgroup_path);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let process_cgroup = loop {
        let metadata = std::fs::metadata(&process_cgroup_path);
        let membership = process_membership(expected.pane_pid);
        if let (Ok(metadata), Ok(membership)) = (metadata, membership) {
            use std::os::unix::fs::MetadataExt;

            let expected_membership = process_cgroup_path
                .strip_prefix("/sys/fs/cgroup")
                .ok()
                .and_then(std::path::Path::to_str)
                .map(|relative| format!("/{relative}"));
            let tasks = std::fs::read_to_string(process_cgroup_path.join("cgroup.procs"));
            let exact_task = tasks.is_ok_and(|tasks| {
                let tasks = tasks
                    .lines()
                    .filter_map(|task| task.trim().parse::<u32>().ok())
                    .collect::<Vec<_>>();
                tasks == [expected.pane_pid]
            });
            if expected_membership.as_deref() == Some(membership.as_str())
                && exact_task
                && metadata.uid() == unsafe { libc::geteuid() }
            {
                break PrivateProcessCgroup {
                    generation: ProcessCgroupGeneration {
                        path: process_cgroup_path.to_string_lossy().into_owned(),
                        device: metadata.dev(),
                        inode: metadata.ino(),
                    },
                };
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(fail(
                error("systemd scope ownership did not become exact", None),
                None,
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    #[cfg(test)]
    cgroup_confinement_crash_boundary("after-guardian-confirm", &process_cgroup_path);
    let still_exact = pane_generation_exact(name).ok() == Some(expected)
        && process_state_start(expected.pane_pid).is_ok_and(|identity| {
            identity.1 == blocked_start_ticks && !matches!(identity.0, 'Z' | 'X' | 'x')
        })
        && process_is_in_exact_cgroup(expected.pane_pid, process_cgroup.generation())
            .unwrap_or(false);
    if !still_exact {
        return Err(fail(
            error(
                "candidate generation changed during private cgroup confinement",
                None,
            ),
            Some(process_cgroup),
        ));
    }
    #[cfg(test)]
    cgroup_confinement_crash_boundary("after-move", &process_cgroup_path);
    if pane_generation_exact(name).ok() != Some(expected)
        || !process_is_in_exact_cgroup(expected.pane_pid, process_cgroup.generation())
            .unwrap_or(false)
    {
        return Err(fail(
            error(
                "candidate generation changed after private cgroup resume",
                None,
            ),
            Some(process_cgroup),
        ));
    }
    Ok((process_cgroup, guardian))
}

#[cfg(target_os = "linux")]
pub(crate) fn process_is_in_exact_cgroup(
    pid: u32,
    expected: &ProcessCgroupGeneration,
) -> Result<bool, TmuxError> {
    use std::os::unix::fs::MetadataExt;

    if !is_canonical_private_process_cgroup_path(&expected.path) {
        return Err(TmuxError {
            op: "validate-process-cgroup",
            code: None,
            message: "private Cortana cgroup path is malformed".into(),
        });
    }
    let metadata = std::fs::metadata(&expected.path).map_err(|error| TmuxError {
        op: "validate-process-cgroup",
        code: error.raw_os_error(),
        message: "private Cortana cgroup identity is unreadable".into(),
    })?;
    if metadata.dev() != expected.device || metadata.ino() != expected.inode {
        return Ok(false);
    }
    let procs = std::fs::read_to_string(std::path::Path::new(&expected.path).join("cgroup.procs"))
        .map_err(|error| TmuxError {
            op: "validate-process-cgroup",
            code: error.raw_os_error(),
            message: "private Cortana cgroup membership is unreadable".into(),
        })?;
    if !procs.lines().any(|value| value.trim() == pid.to_string()) {
        return Ok(false);
    }
    let relative = expected
        .path
        .strip_prefix("/sys/fs/cgroup")
        .ok_or_else(|| TmuxError {
            op: "validate-process-cgroup",
            code: None,
            message: "private Cortana cgroup path is outside cgroup v2".into(),
        })?;
    let membership =
        std::fs::read_to_string(format!("/proc/{pid}/cgroup")).map_err(|error| TmuxError {
            op: "validate-process-cgroup",
            code: error.raw_os_error(),
            message: "candidate process cgroup membership is unreadable".into(),
        })?;
    Ok(membership
        .lines()
        .any(|line| line.strip_prefix("0::") == Some(relative)))
}

/// Kill only the exact tmux generation previously attested for `name`.
///
/// A provider-bearing Linux generation must carry its launch-time private
/// cgroup identity. Cleanup pins the cgroup and pane, freezes the complete
/// bounded subtree, validates stable PID, start-tick, membership, and ancestry
/// evidence, then signals only the validated pinned pidfds and proves the
/// subtree empty. A dormant,
/// credential-free pane uses only its exact pidfd. No destructive operation
/// targets the tmux name after the generation can disappear or rebind.
pub(crate) fn kill_session_tree_exact_generation(
    name: &str,
    expected: PaneGeneration,
    process_cgroup: Option<&ProcessCgroupGeneration>,
) -> Result<ExactGenerationKill, TmuxError> {
    kill_session_tree_exact_generation_with_post_reap(name, expected, process_cgroup, || {})
}

fn kill_session_tree_exact_generation_with_post_reap<F>(
    name: &str,
    expected: PaneGeneration,
    process_cgroup: Option<&ProcessCgroupGeneration>,
    post_reap: F,
) -> Result<ExactGenerationKill, TmuxError>
where
    F: FnOnce(),
{
    if name.is_empty()
        || name.len() > 128
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(TmuxError {
            op: "kill-session-tree-generation",
            code: None,
            message: "exact tmux session name is malformed".into(),
        });
    }
    #[cfg(target_os = "linux")]
    {
        kill_linux_process_generation(name, expected, process_cgroup, post_reap)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (name, expected, process_cgroup, post_reap);
        Err(TmuxError {
            op: "kill-session-tree-generation",
            code: None,
            message: "exact generation cleanup is unsupported on this platform".into(),
        })
    }
}

#[cfg(target_os = "linux")]
fn kill_linux_process_generation<F>(
    name: &str,
    expected: PaneGeneration,
    process_cgroup: Option<&ProcessCgroupGeneration>,
    post_reap: F,
) -> Result<ExactGenerationKill, TmuxError>
where
    F: FnOnce(),
{
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    fn unavailable(name: &str) -> Result<ExactGenerationKill, TmuxError> {
        match session_liveness(name) {
            SessionLiveness::Gone => Ok(ExactGenerationKill::Gone),
            SessionLiveness::Alive => Ok(ExactGenerationKill::GenerationMismatch),
            SessionLiveness::Unknown => Err(TmuxError {
                op: "kill-session-tree-generation",
                code: None,
                message: "exact tmux generation became unreadable".into(),
            }),
        }
    }

    fn open_pidfd(pid: u32) -> std::io::Result<OwnedFd> {
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor as i32) })
    }

    fn signal_pidfd(descriptor: &OwnedFd) -> std::io::Result<()> {
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                descriptor.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }

    fn kill_frozen_cgroup_generation<F>(
        name: &str,
        expected: PaneGeneration,
        process_cgroup: &ProcessCgroupGeneration,
        root: Option<OwnedFd>,
        post_reap: F,
    ) -> Result<ExactGenerationKill, TmuxError>
    where
        F: FnOnce(),
    {
        use std::os::unix::fs::MetadataExt;

        const MAX_CGROUPS: usize = 64;
        const MAX_TASKS: usize = 256;
        const POLL_LIMIT: usize = 200;

        #[derive(Debug, Clone, PartialEq, Eq)]
        struct FrozenCgroupSnapshot {
            groups: Vec<FrozenCgroupNode>,
            tasks: Vec<(u32, String)>,
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        struct FrozenCgroupNode {
            relative_path: String,
            device: u64,
            inode: u64,
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct FrozenProcessIdentity {
            parent: u32,
            process_group: u32,
            session: u32,
            start_ticks: u64,
        }

        fn process_identity(pid: u32) -> Result<FrozenProcessIdentity, TmuxError> {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|error| {
                TmuxError {
                    op: "kill-session-tree-generation",
                    code: error.raw_os_error(),
                    message: "frozen candidate process identity is unreadable".into(),
                }
            })?;
            let suffix = stat
                .rsplit_once(')')
                .map(|(_, suffix)| suffix.trim())
                .ok_or_else(|| TmuxError {
                    op: "kill-session-tree-generation",
                    code: None,
                    message: "frozen candidate process identity is malformed".into(),
                })?;
            let fields = suffix.split_whitespace().collect::<Vec<_>>();
            let parent = fields
                .get(1)
                .and_then(|value| value.parse::<u32>().ok())
                .ok_or_else(|| TmuxError {
                    op: "kill-session-tree-generation",
                    code: None,
                    message: "frozen candidate parent identity is malformed".into(),
                })?;
            let process_group = fields
                .get(2)
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| TmuxError {
                    op: "kill-session-tree-generation",
                    code: None,
                    message: "frozen candidate process-group identity is malformed".into(),
                })?;
            let session = fields
                .get(3)
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| TmuxError {
                    op: "kill-session-tree-generation",
                    code: None,
                    message: "frozen candidate process-session identity is malformed".into(),
                })?;
            let start_ticks = fields
                .get(19)
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| TmuxError {
                    op: "kill-session-tree-generation",
                    code: None,
                    message: "frozen candidate start identity is malformed".into(),
                })?;
            Ok(FrozenProcessIdentity {
                parent,
                process_group,
                session,
                start_ticks,
            })
        }

        fn read_cgroup_snapshot(root: &std::path::Path) -> Result<FrozenCgroupSnapshot, TmuxError> {
            let mut pending = vec![(root.to_path_buf(), String::new())];
            let mut groups = Vec::new();
            let mut tasks = Vec::new();
            while let Some((path, relative_path)) = pending.pop() {
                if groups.len() >= MAX_CGROUPS {
                    return Err(TmuxError {
                        op: "kill-session-tree-generation",
                        code: None,
                        message: "frozen candidate cgroup exceeds the bounded subtree limit".into(),
                    });
                }
                let metadata = std::fs::metadata(&path).map_err(|error| TmuxError {
                    op: "kill-session-tree-generation",
                    code: error.raw_os_error(),
                    message: "frozen candidate cgroup subtree identity is unreadable".into(),
                })?;
                if !metadata.is_dir() {
                    return Err(TmuxError {
                        op: "kill-session-tree-generation",
                        code: None,
                        message: "frozen candidate cgroup subtree contains a non-directory node"
                            .into(),
                    });
                }
                groups.push(FrozenCgroupNode {
                    relative_path: relative_path.clone(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                });
                let values =
                    std::fs::read_to_string(path.join("cgroup.procs")).map_err(|error| {
                        TmuxError {
                            op: "kill-session-tree-generation",
                            code: error.raw_os_error(),
                            message: "frozen candidate cgroup tasks are unreadable".into(),
                        }
                    })?;
                for value in values
                    .lines()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    let pid = value.parse::<u32>().map_err(|_| TmuxError {
                        op: "kill-session-tree-generation",
                        code: None,
                        message: "frozen candidate cgroup contains a malformed task".into(),
                    })?;
                    tasks.push((pid, relative_path.clone()));
                    if tasks.len() > MAX_TASKS {
                        return Err(TmuxError {
                            op: "kill-session-tree-generation",
                            code: None,
                            message: "frozen candidate cgroup exceeds the bounded task limit"
                                .into(),
                        });
                    }
                }
                let entries = std::fs::read_dir(&path).map_err(|error| TmuxError {
                    op: "kill-session-tree-generation",
                    code: error.raw_os_error(),
                    message: "frozen candidate cgroup subtree is unreadable".into(),
                })?;
                for entry in entries {
                    let entry = entry.map_err(|error| TmuxError {
                        op: "kill-session-tree-generation",
                        code: error.raw_os_error(),
                        message: "frozen candidate cgroup subtree entry is unreadable".into(),
                    })?;
                    let file_type = entry.file_type().map_err(|error| TmuxError {
                        op: "kill-session-tree-generation",
                        code: error.raw_os_error(),
                        message: "frozen candidate cgroup subtree entry type is unreadable".into(),
                    })?;
                    if !file_type.is_dir() {
                        continue;
                    }
                    let name = entry.file_name();
                    let name = name.to_str().filter(|name| {
                        !name.is_empty()
                            && name.len() <= 128
                            && name.bytes().all(|byte| {
                                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
                            })
                    });
                    let Some(name) = name else {
                        return Err(TmuxError {
                            op: "kill-session-tree-generation",
                            code: None,
                            message: "frozen candidate cgroup has an unsafe child name".into(),
                        });
                    };
                    let child_relative = if relative_path.is_empty() {
                        name.to_string()
                    } else {
                        format!("{relative_path}/{name}")
                    };
                    pending.push((entry.path(), child_relative));
                }
            }
            groups.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
            tasks.sort_unstable();
            if tasks.windows(2).any(|pair| pair[0].0 == pair[1].0) {
                return Err(TmuxError {
                    op: "kill-session-tree-generation",
                    code: None,
                    message: "frozen candidate task appears in multiple cgroups".into(),
                });
            }
            Ok(FrozenCgroupSnapshot { groups, tasks })
        }

        fn process_has_exact_cgroup_membership(
            pid: u32,
            root_relative: &str,
            child_relative: &str,
        ) -> Result<bool, TmuxError> {
            let expected = if child_relative.is_empty() {
                root_relative.to_string()
            } else {
                format!("{root_relative}/{child_relative}")
            };
            let membership =
                std::fs::read_to_string(format!("/proc/{pid}/cgroup")).map_err(|error| {
                    TmuxError {
                        op: "kill-session-tree-generation",
                        code: error.raw_os_error(),
                        message: "frozen candidate task cgroup membership is unreadable".into(),
                    }
                })?;
            Ok(membership
                .lines()
                .any(|line| line.strip_prefix("0::") == Some(expected.as_str())))
        }

        fn thaw(path: &std::path::Path) -> Result<(), TmuxError> {
            #[cfg(test)]
            run_cgroup_cleanup_test_hook(CgroupCleanupTestPoint::BeforeThaw)?;
            std::fs::write(path.join("cgroup.freeze"), b"0").map_err(|error| TmuxError {
                op: "kill-session-tree-generation",
                code: error.raw_os_error(),
                message: "failed candidate cgroup could not be thawed".into(),
            })
        }

        fn pidfd_has_exited(descriptor: &std::os::fd::OwnedFd) -> bool {
            use std::os::fd::AsRawFd;

            let mut poll = libc::pollfd {
                fd: descriptor.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            (unsafe { libc::poll(&mut poll, 1, 0) }) == 1 && poll.revents & libc::POLLIN != 0
        }

        #[cfg(test)]
        fn remove_empty_exact_cgroup(
            path: &std::path::Path,
            control_path: &std::path::Path,
            expected: &ProcessCgroupGeneration,
        ) -> Result<(), TmuxError> {
            use std::os::unix::fs::MetadataExt;

            for _ in 0..POLL_LIMIT {
                let root_metadata = match std::fs::metadata(path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => {
                        return Err(TmuxError {
                            op: "kill-session-tree-generation",
                            code: error.raw_os_error(),
                            message: "empty candidate cgroup identity is unreadable".into(),
                        });
                    }
                };
                if root_metadata.dev() != expected.device || root_metadata.ino() != expected.inode {
                    return Err(TmuxError {
                        op: "kill-session-tree-generation",
                        code: None,
                        message: "empty candidate cgroup identity changed before removal".into(),
                    });
                }
                let snapshot = read_cgroup_snapshot(control_path)?;
                if !snapshot.tasks.is_empty() {
                    return Err(TmuxError {
                        op: "kill-session-tree-generation",
                        code: None,
                        message: "candidate cgroup repopulated before exact removal".into(),
                    });
                }
                let mut children = snapshot
                    .groups
                    .iter()
                    .filter(|group| !group.relative_path.is_empty())
                    .collect::<Vec<_>>();
                children.sort_by_key(|group| {
                    std::cmp::Reverse(group.relative_path.matches('/').count())
                });
                for child in children {
                    let child_path = path.join(&child.relative_path);
                    let metadata = std::fs::metadata(&child_path).map_err(|error| TmuxError {
                        op: "kill-session-tree-generation",
                        code: error.raw_os_error(),
                        message: "empty candidate child cgroup identity is unreadable".into(),
                    })?;
                    if metadata.dev() != child.device || metadata.ino() != child.inode {
                        return Err(TmuxError {
                            op: "kill-session-tree-generation",
                            code: None,
                            message: "empty candidate child cgroup identity changed before removal"
                                .into(),
                        });
                    }
                    let child_generation = ProcessCgroupGeneration {
                        path: child_path.to_string_lossy().into_owned(),
                        device: child.device,
                        inode: child.inode,
                    };
                    let _ = remove_exact_empty_cgroup_generation(&child_generation)?;
                }
                if remove_exact_empty_cgroup_generation(expected)? {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(TmuxError {
                op: "kill-session-tree-generation",
                code: None,
                message: "empty candidate cgroup removal did not converge".into(),
            })
        }

        fn wait_for_owned_scope_removal(
            path: &std::path::Path,
            expected: &ProcessCgroupGeneration,
        ) -> Result<(), TmuxError> {
            use std::os::unix::fs::MetadataExt;

            for _ in 0..POLL_LIMIT {
                match std::fs::metadata(path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    Ok(metadata)
                        if metadata.dev() == expected.device
                            && metadata.ino() == expected.inode =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(_) => {
                        return Err(TmuxError {
                            op: "kill-session-tree-generation",
                            code: None,
                            message: "systemd-owned candidate scope identity was substituted"
                                .into(),
                        });
                    }
                    Err(error) => {
                        return Err(TmuxError {
                            op: "kill-session-tree-generation",
                            code: error.raw_os_error(),
                            message: "systemd-owned candidate scope state is unreadable".into(),
                        });
                    }
                }
            }
            Err(TmuxError {
                op: "kill-session-tree-generation",
                code: None,
                message: "systemd-owned candidate scope removal did not converge".into(),
            })
        }

        if !is_canonical_private_process_cgroup_path(&process_cgroup.path) {
            return Err(TmuxError {
                op: "kill-session-tree-generation",
                code: None,
                message: "exact candidate cgroup path is not canonical".into(),
            });
        }
        let root_was_alive = root.is_some();
        let path = std::path::Path::new(&process_cgroup.path);
        let directory = match std::fs::File::open(path) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !root_was_alive => {
                return Ok(ExactGenerationKill::Gone);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ExactGenerationKill::GenerationMismatch);
            }
            Err(error) => {
                return Err(TmuxError {
                    op: "kill-session-tree-generation",
                    code: error.raw_os_error(),
                    message: "exact candidate cgroup could not be opened".into(),
                });
            }
        };
        let metadata = directory.metadata().map_err(|error| TmuxError {
            op: "kill-session-tree-generation",
            code: error.raw_os_error(),
            message: "exact candidate cgroup identity is unreadable".into(),
        })?;
        if metadata.dev() != process_cgroup.device || metadata.ino() != process_cgroup.inode {
            return Ok(ExactGenerationKill::GenerationMismatch);
        }
        let managed_scope = is_systemd_owned_process_scope_path(&process_cgroup.path);
        if metadata.uid() != unsafe { libc::geteuid() }
            || !managed_scope && metadata.mode() & 0o077 != 0
        {
            return Err(TmuxError {
                op: "kill-session-tree-generation",
                code: None,
                message: "exact candidate cgroup ownership or permissions are unsafe".into(),
            });
        }
        let expected_owner = metadata.uid();
        let control_path = std::path::PathBuf::from(format!(
            "/proc/self/fd/{}",
            std::os::fd::AsRawFd::as_raw_fd(&directory)
        ));
        if root_was_alive {
            if !process_is_in_exact_cgroup(expected.pane_pid, process_cgroup)? {
                return Ok(ExactGenerationKill::GenerationMismatch);
            }
            match pane_generation_exact(name) {
                Ok(actual) if actual == expected => {}
                Ok(_) => return Ok(ExactGenerationKill::GenerationMismatch),
                Err(_) => return unavailable(name),
            }
        } else {
            match session_liveness(name) {
                SessionLiveness::Gone => {}
                SessionLiveness::Alive => return Ok(ExactGenerationKill::GenerationMismatch),
                SessionLiveness::Unknown => {
                    return Err(TmuxError {
                        op: "kill-session-tree-generation",
                        code: None,
                        message: "gone candidate liveness became ambiguous before cgroup cleanup"
                            .into(),
                    });
                }
            }
        }

        #[cfg(test)]
        run_cgroup_cleanup_test_hook(CgroupCleanupTestPoint::BeforeFreeze)?;
        std::fs::write(control_path.join("cgroup.freeze"), b"1").map_err(|error| TmuxError {
            op: "kill-session-tree-generation",
            code: error.raw_os_error(),
            message: "exact candidate cgroup could not be frozen".into(),
        })?;
        let frozen = (0..POLL_LIMIT).any(|_| {
            let observed = std::fs::read_to_string(control_path.join("cgroup.events"))
                .is_ok_and(|events| events.lines().any(|line| line == "frozen 1"));
            if !observed {
                std::thread::sleep(Duration::from_millis(10));
            }
            observed
        });
        if !frozen {
            let _ = thaw(&control_path);
            return Err(TmuxError {
                op: "kill-session-tree-generation",
                code: None,
                message: "exact candidate cgroup freeze did not converge".into(),
            });
        }
        #[cfg(test)]
        if let Err(error) = run_cgroup_cleanup_test_hook(CgroupCleanupTestPoint::AfterFreeze) {
            let thaw_error = thaw(&control_path).err();
            return Err(thaw_error.unwrap_or(error));
        }

        let result = (|| {
            let first = read_cgroup_snapshot(&control_path)?;
            #[cfg(test)]
            run_cgroup_cleanup_test_hook(CgroupCleanupTestPoint::AfterFirstSnapshot)?;
            std::thread::yield_now();
            let second = read_cgroup_snapshot(&control_path)?;
            let root_presence_is_valid =
                !root_was_alive || first.tasks.iter().any(|task| task.0 == expected.pane_pid);
            if first != second || !root_presence_is_valid {
                return Err(TmuxError {
                    op: "kill-session-tree-generation",
                    code: None,
                    message: "frozen candidate cgroup did not reach a stable complete generation"
                        .into(),
                });
            }
            let mut identities = std::collections::HashMap::new();
            let mut pinned = Vec::with_capacity(first.tasks.len());
            let root_was_alive = root.is_some();
            let mut root = root;
            let root_relative = process_cgroup
                .path
                .strip_prefix("/sys/fs/cgroup")
                .ok_or_else(|| TmuxError {
                    op: "kill-session-tree-generation",
                    code: None,
                    message: "exact candidate cgroup path is outside cgroup v2".into(),
                })?;
            for (pid, relative_cgroup) in &first.tasks {
                let process_metadata =
                    std::fs::metadata(format!("/proc/{pid}")).map_err(|error| TmuxError {
                        op: "kill-session-tree-generation",
                        code: error.raw_os_error(),
                        message: "frozen candidate task ownership is unreadable".into(),
                    })?;
                if process_metadata.uid() != expected_owner {
                    return Err(TmuxError {
                        op: "kill-session-tree-generation",
                        code: None,
                        message: "frozen candidate cgroup contains a foreign-owned task".into(),
                    });
                }
                let before = process_identity(*pid)?;
                let descriptor = if root_was_alive && *pid == expected.pane_pid {
                    root.take().ok_or_else(|| TmuxError {
                        op: "kill-session-tree-generation",
                        code: None,
                        message: "frozen candidate root identity was duplicated".into(),
                    })?
                } else {
                    open_pidfd(*pid).map_err(|error| TmuxError {
                        op: "kill-session-tree-generation",
                        code: error.raw_os_error(),
                        message: "frozen candidate pidfd could not be opened".into(),
                    })?
                };
                let after = process_identity(*pid)?;
                let exact_membership =
                    process_has_exact_cgroup_membership(*pid, root_relative, relative_cgroup)?;
                if before != after || !exact_membership || before.session != expected.pane_pid {
                    return Err(TmuxError {
                        op: "kill-session-tree-generation",
                        code: None,
                        message: "frozen candidate PID identity or cgroup membership changed"
                            .into(),
                    });
                }
                identities.insert(*pid, before);
                pinned.push((*pid, descriptor));
            }
            if root_was_alive && root.is_some() {
                return Err(TmuxError {
                    op: "kill-session-tree-generation",
                    code: None,
                    message: "frozen candidate root identity was absent".into(),
                });
            }
            #[cfg(test)]
            run_cgroup_cleanup_test_hook(CgroupCleanupTestPoint::AfterIdentityValidation)?;
            if root_was_alive {
                for (pid, _) in &first.tasks {
                    if *pid == expected.pane_pid {
                        continue;
                    }
                    let mut current = *pid;
                    let mut depth = 0;
                    loop {
                        let parent = identities
                            .get(&current)
                            .map(|identity| identity.parent)
                            .ok_or_else(|| TmuxError {
                                op: "kill-session-tree-generation",
                                code: None,
                                message: "frozen candidate ancestry is incomplete".into(),
                            })?;
                        if parent == expected.pane_pid {
                            break;
                        }
                        if !identities.contains_key(&parent) || depth >= MAX_TASKS {
                            return Err(TmuxError {
                                op: "kill-session-tree-generation",
                                code: None,
                                message:
                                    "frozen candidate contains a foreign or unbounded ancestry"
                                        .into(),
                            });
                        }
                        current = parent;
                        depth += 1;
                    }
                }
            }
            if read_cgroup_snapshot(&control_path)? != first {
                return Err(TmuxError {
                    op: "kill-session-tree-generation",
                    code: None,
                    message: "frozen candidate cgroup changed after PID identity validation".into(),
                });
            }
            #[cfg(test)]
            run_cgroup_cleanup_test_hook(CgroupCleanupTestPoint::BeforeKill)?;
            if read_cgroup_snapshot(&control_path)? != first {
                return Err(TmuxError {
                    op: "kill-session-tree-generation",
                    code: None,
                    message: "frozen candidate cgroup changed at its final kill boundary".into(),
                });
            }
            for (pid, relative_cgroup) in &first.tasks {
                if identities[pid] != process_identity(*pid)?
                    || identities[pid].session != expected.pane_pid
                    || !process_has_exact_cgroup_membership(*pid, root_relative, relative_cgroup)?
                {
                    return Err(TmuxError {
                        op: "kill-session-tree-generation",
                        code: None,
                        message: "frozen candidate identity changed at its final kill boundary"
                            .into(),
                    });
                }
            }
            #[cfg(test)]
            run_cgroup_cleanup_test_hook(CgroupCleanupTestPoint::BeforePinnedSignals)?;
            for (_, descriptor) in pinned.iter().rev() {
                signal_pidfd(descriptor).map_err(|error| TmuxError {
                    op: "kill-session-tree-generation",
                    code: error.raw_os_error(),
                    message: "validated candidate pidfd signal failed".into(),
                })?;
            }
            #[cfg(test)]
            run_cgroup_cleanup_test_hook(CgroupCleanupTestPoint::AfterKill)?;
            let empty = (0..POLL_LIMIT).any(|_| {
                let explicit_empty = std::fs::read_to_string(control_path.join("cgroup.events"))
                    .is_ok_and(|events| events.lines().any(|line| line == "populated 0"))
                    && read_cgroup_snapshot(&control_path)
                        .is_ok_and(|snapshot| snapshot.tasks.is_empty());
                let removed_after_empty = !path.exists()
                    && pinned
                        .iter()
                        .all(|(_, descriptor)| pidfd_has_exited(descriptor))
                    && session_liveness(name) == SessionLiveness::Gone;
                let observed = explicit_empty || removed_after_empty;
                if !observed {
                    std::thread::sleep(Duration::from_millis(10));
                }
                observed
            });
            if !empty {
                return Err(TmuxError {
                    op: "kill-session-tree-generation",
                    code: None,
                    message: "exact candidate cgroup did not become empty after kill".into(),
                });
            }
            drop(pinned);
            if managed_scope {
                #[cfg(test)]
                run_cgroup_cleanup_test_hook(CgroupCleanupTestPoint::BeforeOwnedScopeRemovalWait)?;
                wait_for_owned_scope_removal(path, process_cgroup)?;
            } else if path.exists() {
                thaw(&control_path)?;
                #[cfg(test)]
                remove_empty_exact_cgroup(path, &control_path, process_cgroup)?;
                #[cfg(not(test))]
                return Err(TmuxError {
                    op: "kill-session-tree-generation",
                    code: None,
                    message: "legacy candidate cgroup requires incompatible recovery".into(),
                });
            }
            Ok(())
        })();
        if let Err(error) = result {
            let should_thaw = !managed_scope
                || std::fs::metadata(path).is_ok_and(|metadata| {
                    metadata.dev() == process_cgroup.device
                        && metadata.ino() == process_cgroup.inode
                });
            let thaw_error = should_thaw.then(|| thaw(&control_path).err()).flatten();
            return Err(thaw_error.unwrap_or(error));
        }
        post_reap();
        Ok(ExactGenerationKill::Killed)
    }

    match pane_generation_exact(name) {
        Ok(actual) if actual == expected => {}
        Ok(_) => return Ok(ExactGenerationKill::GenerationMismatch),
        Err(_) => {
            if let Some(process_cgroup) = process_cgroup {
                if session_liveness(name) == SessionLiveness::Gone {
                    return kill_frozen_cgroup_generation(
                        name,
                        expected,
                        process_cgroup,
                        None,
                        post_reap,
                    );
                }
            }
            return unavailable(name);
        }
    }
    let root = match open_pidfd(expected.pane_pid) {
        Ok(root) => root,
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            if let Some(process_cgroup) = process_cgroup {
                if session_liveness(name) == SessionLiveness::Gone {
                    return kill_frozen_cgroup_generation(
                        name,
                        expected,
                        process_cgroup,
                        None,
                        post_reap,
                    );
                }
            }
            return unavailable(name);
        }
        Err(error) => {
            return Err(TmuxError {
                op: "kill-session-tree-generation",
                code: error.raw_os_error(),
                message: format!("exact pane pidfd could not be opened: {error}"),
            });
        }
    };
    match pane_generation_exact(name) {
        Ok(actual) if actual == expected => {}
        Ok(_) => return Ok(ExactGenerationKill::GenerationMismatch),
        Err(_) => return unavailable(name),
    }

    if let Some(process_cgroup) = process_cgroup {
        return kill_frozen_cgroup_generation(
            name,
            expected,
            process_cgroup,
            Some(root),
            post_reap,
        );
    }
    signal_pidfd(&root).map_err(|error| TmuxError {
        op: "kill-session-tree-generation",
        code: error.raw_os_error(),
        message: format!("exact credential-free pane pidfd signal failed: {error}"),
    })?;
    post_reap();
    Ok(ExactGenerationKill::Killed)
}

/// List all session names on the `t-hub` socket.
///
/// Tolerates the "no server running" case (no sessions have ever been created,
/// or the last one was killed and the server exited) by returning an empty Vec
/// rather than an error.
pub fn list_sessions() -> Result<Vec<String>, TmuxError> {
    // NB: we deliberately do NOT use `-F '#{session_name}'`. On Windows every
    // tmux command was historically routed through `wsl.exe --` (the default
    // shell), where the leading `#` of a tmux format string was swallowed as a
    // shell comment — leaving `list-sessions -F` with no argument ("-F expects
    // an argument") and breaking the whole live terminal list (cwd/labels/
    // status). `tmux()` now uses `-e` (no shell hop), but the format-free form
    // is kept: it's simpler and proven. The default `list-sessions` output is
    // `<name>: <window/size info>`; tmux forbids `:` in session names, so the
    // name is everything before the first colon. This needs no format argument
    // and survives the wsl.exe round-trip intact.
    let output =
        output_with_timeout(tmux(&["list-sessions"]), tmux_cmd_timeout()).map_err(|e| {
            TmuxError {
                op: "list-sessions",
                code: None,
                message: format!("failed to spawn tmux: {e}"),
            }
        })?;

    if output.status.success() {
        let names = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|l| {
                let l = l.trim();
                if l.is_empty() {
                    return None;
                }
                let name = l.split_once(':').map(|(n, _)| n).unwrap_or(l).trim();
                if name.is_empty() {
                    None
                } else {
                    Some(name.to_string())
                }
            })
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

/// Per-session foreground command + current working directory, so the UI can
/// label a tile by what's actually running (`claude`, `zsh`, ...) and where,
/// instead of a raw session id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneInfo {
    pub session: String,
    pub command: String,
    pub cwd: String,
}

/// List every pane's `session_name|pane_current_command|pane_current_path`.
///
/// Unlike [`list_sessions`], this needs a tmux FORMAT (`#{...}`). A bare
/// `#{...}` argv word was swallowed as a shell comment over the old `wsl.exe --`
/// round-trip (see the note in `list_sessions`), so we run the whole tmux call
/// inside a `bash -lc` script where the format is SINGLE-QUOTED — inside single
/// quotes `#` is literal, so it survives intact (still correct, and shell-proof,
/// now that the hop uses `-e`). Best-effort: a missing server
/// (no sessions) returns an empty Vec rather than erroring.
pub fn pane_info() -> Result<Vec<PaneInfo>, TmuxError> {
    // Built from the resolved socket name (not a hardcoded `t-hub`) so a DEV
    // instance with `$T_HUB_TMUX_SOCKET` set reads ITS panes; with no env var
    // the socket is `t-hub`, reproducing the previous literal exactly.
    // We emit `session|command|cwd`, BUT `pane_current_command` only reports the
    // foreground process's comm — which is the RUNTIME (e.g. `node`) for agents
    // shipped as scripts: the Codex CLI is `node …/codex`, so it'd read as "node"
    // and never be detected as Codex (Claude runs as `claude`, so it's fine). So
    // when the foreground is a runtime, we resolve the real agent from the pane's
    // foreground process group and substitute `codex`/`claude` as the command.
    // `pane_pid` is normally the long-lived shell, not the foreground runtime, so
    // inspecting only its immediate children misses launchers such as
    // shell -> node -> native codex. Best-effort: no foreground pid / no match
    // leaves the original command intact.
    let script = format!(
        "tmux -L {sock} list-panes -a -F \
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
        sock = socket()
    );
    let output =
        output_with_timeout(pane_info_command(&script), tmux_cmd_timeout()).map_err(|e| {
            TmuxError {
                op: "list-panes",
                code: None,
                message: format!("failed to spawn tmux: {e}"),
            }
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_no_server(&stderr) || stderr.contains("error connecting to") {
            return Ok(Vec::new());
        }
        return Err(TmuxError {
            op: "list-panes",
            code: output.status.code(),
            message: stderr.trim().to_string(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '|');
        let session = parts.next().unwrap_or("").trim().to_string();
        let command = parts.next().unwrap_or("").trim().to_string();
        let cwd = parts.next().unwrap_or("").trim().to_string();
        if !session.is_empty() {
            out.push(PaneInfo {
                session,
                command,
                cwd,
            });
        }
    }
    Ok(out)
}

/// Build the `bash -lc <script>` command used by [`pane_info`]. On Windows this
/// goes through `wsl.exe` (CREATE_NO_WINDOW so no console flashes); on unix it
/// runs `sh -c` directly. The single-quoted tmux format inside `script` is what
/// protects `#{...}` from being eaten as a shell comment.
///
/// CRITICAL: pass `-e` (alias `--exec`) so wsl.exe runs `bash` DIRECTLY. Without
/// it, `wsl.exe -- bash -lc <script>` does NOT run bash — wsl routes the command
/// through the user's DEFAULT login shell (here `/usr/bin/zsh`). The script then
/// runs under zsh, where `$path` is a special array tied to `$PATH`: the loop's
/// `read -r s cmd path pid` clobbers PATH and `"$path"` expands to the entire
/// PATH, so every pane came back as `||<PATH>` with an EMPTY session/command/cwd.
/// That empty cwd/title is exactly what made the sidebar fall back to the raw 8-char
/// id and the tile header go blank. `-e` makes the real bash run; the data is clean.
#[cfg(windows)]
fn pane_info_command(script: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("wsl.exe");
    c.arg("--cd")
        .arg("~")
        .arg("-e")
        .arg("bash")
        .arg("-lc")
        .arg(script);
    c.creation_flags(0x0800_0000);
    c
}

#[cfg(unix)]
fn pane_info_command(script: &str) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(script);
    c
}

/// Capture the visible pane of `name` as **plain text** (no ANSI escapes),
/// optionally including the last `history_lines` of scrollback above the screen.
///
/// This is the MCP/control-channel read path (`capture_pane`/`read_terminal`):
/// an external Claude wants to *read* what a session currently shows, so we omit
/// `-e` (no escape sequences — clean readable text) unlike [`capture_pane`],
/// which preserves ANSI to seed xterm. `tmux -L t-hub capture-pane -p [-S -N] -t <name>`.
///
/// `history_lines == 0` ⇒ visible screen only; `Some(n)` ⇒ start `n` lines into
/// the scrollback (`-S -n`). Returns the captured text as a `String`.
pub fn capture_pane_text(name: &str, history_lines: u32) -> Result<String, TmuxError> {
    let start; // owns the `-N` string for the borrow below
    let output = if history_lines > 0 {
        start = format!("-{history_lines}");
        run(
            "capture-pane",
            &["capture-pane", "-p", "-S", &start, "-t", name],
        )?
    } else {
        run("capture-pane", &["capture-pane", "-p", "-t", name])?
    };
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Send literal `text` to session `name` via `tmux -L t-hub send-keys -l`, then
/// (when `enter` is true) a trailing `Enter` keystroke to submit it.
///
/// `-l` makes tmux treat the payload literally (no key-name interpretation), so
/// arbitrary text — including characters that would otherwise be parsed as key
/// names — is typed verbatim. The `Enter` is sent as a *separate* `send-keys`
/// without `-l` so tmux interprets it as the Enter key. This is the write path
/// for the process-changing `send_text` MCP tool.
pub fn send_text(name: &str, text: &str, enter: bool) -> Result<(), TmuxError> {
    // Type the literal text. `--` guards against a payload that begins with `-`.
    run("send-keys", &["send-keys", "-t", name, "-l", "--", text])?;
    if enter {
        run("send-keys", &["send-keys", "-t", name, "Enter"])?;
    }
    Ok(())
}

/// Send one or more **named keys** (e.g. `C-c`, `Enter`, `Up`, `Escape`) to
/// session `name` via `tmux -L t-hub send-keys -t <name> <key>...`.
///
/// Unlike [`send_text`], keys are *not* literal: tmux interprets each token as a
/// key name, so this drives control sequences (Ctrl-C to interrupt, arrows to
/// navigate, etc.). Backs the `keys` mode of the process-changing `send_keys` tool.
pub fn send_keys(name: &str, keys: &[&str]) -> Result<(), TmuxError> {
    let mut args: Vec<&str> = vec!["send-keys", "-t", name];
    args.extend_from_slice(keys);
    run("send-keys", &args)?;
    Ok(())
}

/// Read a pane format (e.g. `#{pane_in_mode}`, `#{scroll_position}`) for session
/// `name`'s active pane. Returns the trimmed value, or None on any failure.
fn pane_format(name: &str, fmt: &str) -> Option<String> {
    let out = run(
        "display-message",
        &["display-message", "-p", "-t", name, fmt],
    )
    .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Page session `name`'s scrollback history up/down by driving tmux **copy-mode**.
///
/// This is the only way to scroll a pane's history when an alternate-screen app
/// (claude / vim) owns it — xterm's local scrollback only holds what streamed, not
/// tmux's history, and the `C-b` prefix is disabled. We enter copy-mode (only when
/// the pane isn't already in a mode, so a repeated page-up keeps climbing instead
/// of snapping to the bottom) and send a copy-mode page command. `down == true`
/// pages toward the live prompt; once it reaches the bottom we EXIT copy-mode so
/// the pane resumes showing live output (copy-mode freezes it). Best-effort.
pub fn scroll_history(name: &str, down: bool) -> Result<(), TmuxError> {
    let in_mode = pane_format(name, "#{pane_in_mode}").as_deref() == Some("1");
    if !in_mode {
        run("copy-mode", &["copy-mode", "-t", name])?;
    }
    let cmd = if down { "page-down" } else { "page-up" };
    run("send-keys", &["send-keys", "-X", "-t", name, cmd])?;
    // Paging back to the bottom returns to the LIVE pane; copy-mode otherwise
    // freezes output, so leave it once we're at scroll_position 0.
    if down && pane_format(name, "#{scroll_position}").as_deref() == Some("0") {
        let _ = exit_copy_mode(name);
    }
    Ok(())
}

/// Exit copy-mode for `name` (back to the live prompt). Best-effort — a harmless
/// no-op when the pane isn't in a mode. Used to return to typing the instant the
/// user types after scrolling.
pub fn exit_copy_mode(name: &str) -> Result<(), TmuxError> {
    run("send-keys", &["send-keys", "-X", "-t", name, "cancel"])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CgroupCleanupHookGuard;

    impl CgroupCleanupHookGuard {
        fn install(
            hook: impl FnMut(CgroupCleanupTestPoint) -> Result<(), TmuxError> + 'static,
        ) -> Self {
            CGROUP_CLEANUP_TEST_HOOK.with(|slot| {
                assert!(
                    slot.borrow().is_none(),
                    "cgroup cleanup hook was already set"
                );
                *slot.borrow_mut() = Some(Box::new(hook));
            });
            Self
        }
    }

    impl Drop for CgroupCleanupHookGuard {
        fn drop(&mut self) {
            CGROUP_CLEANUP_TEST_HOOK.with(|slot| *slot.borrow_mut() = None);
        }
    }

    fn injected_cgroup_cleanup_error(message: &str) -> TmuxError {
        TmuxError {
            op: "kill-session-tree-generation",
            code: None,
            message: message.to_string(),
        }
    }

    #[cfg(target_os = "linux")]
    fn wait_for_exact_cgroup_member(pid: u32, cgroup: &ProcessCgroupGeneration) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let exact_membership = process_is_in_exact_cgroup(pid, cgroup);
            if exact_membership.as_ref().is_ok_and(|exact| *exact) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "process did not join its private cgroup: expected={cgroup:?} exact_membership={exact_membership:?} process_membership={:?} cgroup_tasks={:?} cmdline={:?}",
                std::fs::read_to_string(format!("/proc/{pid}/cgroup")),
                std::fs::read_to_string(
                    std::path::Path::new(&cgroup.path).join("cgroup.procs")
                ),
                std::fs::read(format!("/proc/{pid}/cmdline")),
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// True when a real `tmux` binary is reachable for the tests below. These
    /// tests drive a live tmux server on the isolated socket, so they can only
    /// pass where tmux is installed (the WSL2 dev shell) — NOT on the Windows CI
    /// target, where tmux isn't on PATH. We probe with `tmux -V` (cheap, touches
    /// no socket/session) and SKIP gracefully when it can't be spawned, mirroring
    /// the missing-binary gate `agent/connection.rs` uses for its agent tests
    /// (`bin_path.exists()` → eprintln + early `return`) so CI never hard-fails on
    /// a platform without tmux.
    ///
    /// On Windows tmux lives inside WSL, so — like every other tmux call here — we
    /// probe through `wsl.exe -- tmux -V` rather than a bare `tmux` (which doesn't
    /// exist on the Windows host at all).
    fn tmux_available() -> bool {
        #[cfg(windows)]
        let mut cmd = {
            use std::os::windows::process::CommandExt;
            let mut c = Command::new("wsl.exe");
            // Bare `--` is safe here ONLY because the argv is constant single-word
            // tokens (`tmux -V`) — nothing for the default shell to re-expand (see
            // the note on `pane_info_command`).
            c.arg("--").arg("tmux").arg("-V");
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            c
        };
        #[cfg(unix)]
        let mut cmd = {
            let mut c = Command::new("tmux");
            c.arg("-V");
            c
        };
        cmd.output().map(|o| o.status.success()).unwrap_or(false)
    }

    /// Generate a unique throwaway session name so concurrent test runs (or a
    /// crashed prior run) don't collide.
    fn unique_name() -> String {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("th_test_{ts}")
    }

    fn create_shell_session_after_final_teardown(name: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match new_session_with_env(name, "/tmp", None, &[]) {
                Ok(()) => return,
                Err(error) if error.message == "server exited unexpectedly" => {
                    match session_liveness(name) {
                        SessionLiveness::Alive => return,
                        SessionLiveness::Gone if std::time::Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        liveness => panic!(
                            "new_session did not recover after final-server teardown: {liveness:?}: {error}"
                        ),
                    }
                }
                Err(error) => panic!("new_session should succeed: {error}"),
            }
        }
    }

    struct TestSession {
        name: String,
        closed: bool,
    }

    fn test_session_cleanup_confirmed(
        result: &Result<(), TmuxError>,
        liveness_after_error: Option<SessionLiveness>,
    ) -> bool {
        result.is_ok() || liveness_after_error == Some(SessionLiveness::Gone)
    }

    impl TestSession {
        fn new() -> Self {
            let name = unique_name();
            let _ = kill_session(&name);
            Self {
                name,
                closed: false,
            }
        }

        fn close(&mut self) -> Result<(), TmuxError> {
            let result = kill_session(&self.name);
            let liveness_after_error = result.as_ref().err().map(|_| session_liveness(&self.name));
            self.closed = test_session_cleanup_confirmed(&result, liveness_after_error);
            result
        }
    }

    impl Drop for TestSession {
        fn drop(&mut self) {
            if !self.closed {
                let _ = kill_session(&self.name);
            }
        }
    }

    struct TmuxTestsAnchorGuard {
        session: TestSession,
    }

    impl TmuxTestsAnchorGuard {
        fn new() -> Self {
            let session = TestSession::new();
            create_shell_session_after_final_teardown(&session.name);
            Self { session }
        }
    }

    impl Drop for TmuxTestsAnchorGuard {
        fn drop(&mut self) {
            if let Err(error) = self.session.close() {
                assert!(
                    error.message == "server exited unexpectedly"
                        && session_liveness(&self.session.name) == SessionLiveness::Gone,
                    "anchor cleanup failed before confirmed final-session teardown: {error}"
                );
            }
        }
    }

    #[test]
    fn test_session_cleanup_retries_unexpected_live_or_ambiguous_failures() {
        let error = || TmuxError {
            op: "kill-session",
            code: Some(1),
            message: "unexpected cleanup failure".to_string(),
        };
        assert!(test_session_cleanup_confirmed(&Ok(()), None));
        assert!(test_session_cleanup_confirmed(
            &Err(error()),
            Some(SessionLiveness::Gone)
        ));
        assert!(!test_session_cleanup_confirmed(
            &Err(error()),
            Some(SessionLiveness::Alive)
        ));
        assert!(!test_session_cleanup_confirmed(
            &Err(error()),
            Some(SessionLiveness::Unknown)
        ));
    }

    #[test]
    fn pane_generation_parser_accepts_fresh_tmux_zero_ids() {
        assert_eq!(
            parse_pane_generation("$0|123|@0|%0|456"),
            Some(PaneGeneration {
                session_id: 0,
                session_created: 123,
                window_id: 0,
                pane_id: 0,
                pane_pid: 456,
            })
        );
        assert!(parse_pane_generation("$0|0|@0|%0|456").is_none());
        assert!(parse_pane_generation("$0|123|@0|%0|0").is_none());
    }

    // NB: the generic `output_with_timeout` bound (kill-a-hung-child, fast
    // pass-through, no-serialization, large dual-pipe drain) is exercised in
    // `bounded_exec.rs`, which now OWNS that shared helper. The tests below cover
    // the tmux-specific surface that routes through it.

    /// Full lifecycle on the isolated socket: create → list contains it →
    /// has_session true → capture returns bytes → kill → has_session false.
    ///
    /// NOTE: requires a real `tmux` on PATH. It compiles everywhere but only
    /// passes where tmux is installed (it is in this WSL2 dev shell; it is not
    /// expected to run on the Windows CI target).
    #[test]
    fn lifecycle_create_list_kill() {
        if !tmux_available() {
            eprintln!("tmux::tests::lifecycle_create_list_kill: tmux not on PATH - skipping");
            return;
        }
        let name = unique_name();

        // Clean slate in case a previous run leaked this name (it shouldn't).
        let _ = kill_session(&name);

        let created = new_session_with_env_generation(&name, "/tmp", None, &[])
            .expect("new_session should succeed");

        assert!(has_session(&name), "session should exist after creation");
        assert_eq!(pane_generation_exact(&name).unwrap(), created);

        let sessions = list_sessions().expect("list_sessions should succeed");
        assert!(
            sessions.iter().any(|s| s == &name),
            "list_sessions {sessions:?} should contain {name}"
        );

        kill_session(&name).expect("kill_session should succeed");
        assert!(
            !has_session(&name),
            "session should be gone after kill_session"
        );
    }

    #[test]
    fn exact_generation_cleanup_never_kills_a_same_named_replacement() {
        if !tmux_available() {
            return;
        }
        let _anchor = TmuxTestsAnchorGuard::new();
        let target = TestSession::new();
        create_shell_session_after_final_teardown(&target.name);
        let incumbent = pane_generation_exact(&target.name).unwrap();
        let replacement = std::cell::Cell::new(None);
        assert_eq!(
            kill_session_tree_exact_generation_with_post_reap(
                &target.name,
                incumbent,
                None,
                || {
                    let deadline = std::time::Instant::now() + Duration::from_secs(2);
                    while session_liveness(&target.name) != SessionLiveness::Gone {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "old exact generation did not disappear before replacement barrier"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    create_shell_session_after_final_teardown(&target.name);
                    replacement.set(Some(pane_generation_exact(&target.name).unwrap()));
                },
            )
            .unwrap(),
            ExactGenerationKill::Killed
        );
        let replacement = replacement.get().unwrap();
        assert_ne!(replacement, incumbent);
        assert_eq!(pane_generation_exact(&target.name).unwrap(), replacement);
        assert_eq!(
            kill_session_tree_exact_generation(&target.name, incumbent, None).unwrap(),
            ExactGenerationKill::GenerationMismatch
        );
        assert_eq!(session_liveness(&target.name), SessionLiveness::Alive);
    }

    #[test]
    fn frozen_process_identity_rejects_exit_reuse_and_membership_substitution() {
        assert!(frozen_process_identity_matches((41, 900), (41, 900), true));
        assert!(!frozen_process_identity_matches((41, 900), (41, 901), true));
        assert!(!frozen_process_identity_matches((41, 900), (42, 900), true));
        assert!(!frozen_process_identity_matches(
            (41, 900),
            (41, 900),
            false
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn frozen_private_cgroup_cleanup_contains_late_nested_forks_and_bearer_descendants() {
        if !tmux_available() {
            return;
        }
        let _anchor = TmuxTestsAnchorGuard::new();
        let target = TestSession::new();
        create_shell_session_after_final_teardown(&target.name);
        let cgroup = create_private_process_cgroup().unwrap();
        let nested = std::path::Path::new(&cgroup.generation().path).join("nested");
        std::fs::create_dir(&nested).unwrap();
        let trigger = tempfile::NamedTempFile::new().unwrap();
        let trigger_path = trigger.path().to_path_buf();
        drop(trigger);
        let ready_path = trigger_path.with_extension("ready");
        let script_dir = tempfile::tempdir().unwrap();
        let script = script_dir.path().join("nested-provider.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             if [ \"${1:-}\" = nested ]; then\n\
               printf '%s\\n' \"$$\" > \"$CGROUP_ROOT/nested/cgroup.procs\" || exit 125\n\
               sh -c 'sleep 30' &\n\
               while [ ! -e \"$FORK_TRIGGER\" ]; do sleep 0.01; done\n\
               sh -c 'sleep 30' &\n\
               wait\n\
             fi\n\
             export T_HUB_SESSION_TOKEN=escaped-bearer-marker\n\
             while [ ! -e \"$CGROUP_READY\" ]; do sleep 0.01; done\n\
             \"$0\" nested &\n\
             wait\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let command = format!(
            "CGROUP_ROOT='{root}' CGROUP_READY='{ready}' FORK_TRIGGER='{trigger}' exec '{script}'",
            root = cgroup.generation().path,
            ready = ready_path.display(),
            trigger = trigger_path.display(),
            script = script.display(),
        );
        let transition = respawn_pane_exact(&target.name, "/tmp", &command).unwrap();
        let expected = transition.after;
        std::fs::write(
            std::path::Path::new(&cgroup.generation().path).join("cgroup.procs"),
            format!("{}\n", expected.pane_pid),
        )
        .unwrap();
        wait_for_exact_cgroup_member(expected.pane_pid, cgroup.generation());
        std::fs::write(&ready_path, b"ready").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::fs::read_to_string(nested.join("cgroup.procs"))
            .unwrap_or_default()
            .lines()
            .count()
            < 2
        {
            assert!(
                std::time::Instant::now() < deadline,
                "nested provider descendants were not created"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut observed_pids = std::fs::read_to_string(
            std::path::Path::new(&cgroup.generation().path).join("cgroup.procs"),
        )
        .unwrap()
        .lines()
        .chain(
            std::fs::read_to_string(nested.join("cgroup.procs"))
                .unwrap()
                .lines(),
        )
        .filter_map(|pid| pid.parse::<u32>().ok())
        .collect::<Vec<_>>();
        observed_pids.sort_unstable();
        observed_pids.dedup();
        let replacement = std::rc::Rc::new(std::cell::Cell::new(None));
        let replacement_from_hook = replacement.clone();
        let trigger_from_hook = trigger_path.clone();
        let _hook = CgroupCleanupHookGuard::install(move |point| {
            if point == CgroupCleanupTestPoint::AfterFirstSnapshot {
                std::fs::write(&trigger_from_hook, b"fork-now").unwrap();
            }
            Ok(())
        });
        assert_eq!(
            kill_session_tree_exact_generation_with_post_reap(
                &target.name,
                expected,
                Some(cgroup.generation()),
                || {
                    let deadline = std::time::Instant::now() + Duration::from_secs(2);
                    while session_liveness(&target.name) != SessionLiveness::Gone {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "killed cgroup generation did not leave tmux"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    create_shell_session_after_final_teardown(&target.name);
                    replacement_from_hook.set(Some(pane_generation_exact(&target.name).unwrap()));
                },
            )
            .unwrap(),
            ExactGenerationKill::Killed
        );
        assert_ne!(replacement.get(), Some(expected));
        assert_eq!(session_liveness(&target.name), SessionLiveness::Alive);
        for pid in observed_pids {
            assert!(
                !std::path::Path::new(&format!("/proc/{pid}")).exists(),
                "bearer-bearing descendant {pid} escaped private cgroup cleanup"
            );
        }
        assert!(!std::path::Path::new(&cgroup.generation().path).exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn private_cgroup_cleanup_fails_closed_on_foreign_task_or_freeze_thaw_failure() {
        if !tmux_available() {
            return;
        }
        let _anchor = TmuxTestsAnchorGuard::new();

        let run_fixture = || {
            let target = TestSession::new();
            create_shell_session_after_final_teardown(&target.name);
            let cgroup = create_private_process_cgroup().unwrap();
            let expected = pane_generation_exact(&target.name).unwrap();
            std::fs::write(
                std::path::Path::new(&cgroup.generation().path).join("cgroup.procs"),
                format!("{}\n", expected.pane_pid),
            )
            .unwrap();
            wait_for_exact_cgroup_member(expected.pane_pid, cgroup.generation());
            (target, cgroup, expected)
        };

        let (foreign_target, foreign_cgroup, foreign_expected) = run_fixture();
        let foreign_child = std::rc::Rc::new(std::cell::RefCell::new(None));
        let foreign_child_from_hook = foreign_child.clone();
        let foreign_path = foreign_cgroup.generation().path.clone();
        let hook = CgroupCleanupHookGuard::install(move |point| {
            if point == CgroupCleanupTestPoint::BeforeKill {
                let child = Command::new("sleep").arg("30").spawn().unwrap();
                let pid = child.id();
                std::fs::write(
                    std::path::Path::new(&foreign_path).join("cgroup.procs"),
                    format!("{pid}\n"),
                )
                .unwrap();
                *foreign_child_from_hook.borrow_mut() = Some(child);
            }
            Ok(())
        });
        assert!(kill_session_tree_exact_generation(
            &foreign_target.name,
            foreign_expected,
            Some(foreign_cgroup.generation())
        )
        .is_err());
        drop(hook);
        assert_eq!(
            session_liveness(&foreign_target.name),
            SessionLiveness::Alive
        );
        let mut foreign_child = foreign_child.borrow_mut().take().unwrap();
        assert!(foreign_child.try_wait().unwrap().is_none());
        foreign_child.kill().unwrap();
        let _ = foreign_child.wait();
        assert_eq!(
            kill_session_tree_exact_generation(
                &foreign_target.name,
                foreign_expected,
                Some(foreign_cgroup.generation())
            )
            .unwrap(),
            ExactGenerationKill::Killed
        );
        assert!(!std::path::Path::new(&foreign_cgroup.generation().path).exists());
        let foreign_name = foreign_target.name.clone();
        drop(foreign_target);
        drop(foreign_cgroup);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while session_liveness(&foreign_name) != SessionLiveness::Gone {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }

        let (boundary_target, boundary_cgroup, boundary_expected) = run_fixture();
        let boundary_foreign = std::rc::Rc::new(std::cell::RefCell::new(None));
        let boundary_foreign_from_hook = boundary_foreign.clone();
        let boundary_path = boundary_cgroup.generation().path.clone();
        let hook = CgroupCleanupHookGuard::install(move |point| {
            if point == CgroupCleanupTestPoint::BeforePinnedSignals {
                let child = Command::new("sleep").arg("30").spawn().unwrap();
                let pid = child.id();
                let membership = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
                    .unwrap()
                    .lines()
                    .find_map(|line| line.strip_prefix("0::").map(str::to_string))
                    .unwrap();
                std::fs::write(
                    std::path::Path::new(&boundary_path).join("cgroup.procs"),
                    format!("{pid}\n"),
                )
                .unwrap();
                *boundary_foreign_from_hook.borrow_mut() = Some((child, membership));
            }
            Ok(())
        });
        let error = kill_session_tree_exact_generation(
            &boundary_target.name,
            boundary_expected,
            Some(boundary_cgroup.generation()),
        )
        .unwrap_err();
        drop(hook);
        assert!(error.message.contains("did not become empty"));
        assert_eq!(
            session_liveness(&boundary_target.name),
            SessionLiveness::Gone
        );
        let (mut boundary_foreign, original_membership) =
            boundary_foreign.borrow_mut().take().unwrap();
        assert!(boundary_foreign.try_wait().unwrap().is_none());
        let original_procs = std::path::Path::new("/sys/fs/cgroup")
            .join(original_membership.trim_start_matches('/'))
            .join("cgroup.procs");
        std::fs::write(original_procs, format!("{}\n", boundary_foreign.id())).unwrap();
        boundary_foreign.kill().unwrap();
        boundary_foreign.wait().unwrap();
        assert_eq!(
            kill_session_tree_exact_generation(
                &boundary_target.name,
                boundary_expected,
                Some(boundary_cgroup.generation())
            )
            .unwrap(),
            ExactGenerationKill::Killed
        );
        assert!(!std::path::Path::new(&boundary_cgroup.generation().path).exists());

        let (freeze_target, freeze_cgroup, freeze_expected) = run_fixture();
        let hook = CgroupCleanupHookGuard::install(|point| {
            if point == CgroupCleanupTestPoint::BeforeFreeze {
                return Err(injected_cgroup_cleanup_error("injected freeze failure"));
            }
            Ok(())
        });
        assert!(kill_session_tree_exact_generation(
            &freeze_target.name,
            freeze_expected,
            Some(freeze_cgroup.generation())
        )
        .is_err());
        drop(hook);
        assert_eq!(
            session_liveness(&freeze_target.name),
            SessionLiveness::Alive
        );
        assert_eq!(
            kill_session_tree_exact_generation(
                &freeze_target.name,
                freeze_expected,
                Some(freeze_cgroup.generation())
            )
            .unwrap(),
            ExactGenerationKill::Killed
        );
        assert!(!std::path::Path::new(&freeze_cgroup.generation().path).exists());
        let freeze_name = freeze_target.name.clone();
        drop(freeze_target);
        drop(freeze_cgroup);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while session_liveness(&freeze_name) != SessionLiveness::Gone {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }

        let (thaw_target, thaw_cgroup, thaw_expected) = run_fixture();
        let hook = CgroupCleanupHookGuard::install(|point| match point {
            CgroupCleanupTestPoint::AfterFreeze => {
                Err(injected_cgroup_cleanup_error("injected frozen failure"))
            }
            CgroupCleanupTestPoint::BeforeThaw => {
                Err(injected_cgroup_cleanup_error("injected thaw failure"))
            }
            _ => Ok(()),
        });
        let error = kill_session_tree_exact_generation(
            &thaw_target.name,
            thaw_expected,
            Some(thaw_cgroup.generation()),
        )
        .unwrap_err();
        assert_eq!(error.message, "injected thaw failure");
        drop(hook);
        assert_eq!(
            kill_session_tree_exact_generation(
                &thaw_target.name,
                thaw_expected,
                Some(thaw_cgroup.generation())
            )
            .unwrap(),
            ExactGenerationKill::Killed
        );
        assert!(!std::path::Path::new(&thaw_cgroup.generation().path).exists());

        let (remove_target, remove_cgroup, remove_expected) = run_fixture();
        let hook = CgroupCleanupHookGuard::install(|point| {
            if point == CgroupCleanupTestPoint::BeforeRemove {
                return Err(injected_cgroup_cleanup_error("injected removal failure"));
            }
            Ok(())
        });
        let error = kill_session_tree_exact_generation(
            &remove_target.name,
            remove_expected,
            Some(remove_cgroup.generation()),
        )
        .unwrap_err();
        assert_eq!(error.message, "injected removal failure");
        assert!(std::path::Path::new(&remove_cgroup.generation().path).exists());
        assert_eq!(session_liveness(&remove_target.name), SessionLiveness::Gone);
        drop(hook);
        assert_eq!(
            kill_session_tree_exact_generation(
                &remove_target.name,
                remove_expected,
                Some(remove_cgroup.generation())
            )
            .unwrap(),
            ExactGenerationKill::Killed
        );
        assert!(!std::path::Path::new(&remove_cgroup.generation().path).exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn private_cgroup_cleanup_rejects_bounded_subtree_overflow_without_killing() {
        if !tmux_available() {
            return;
        }
        let _anchor = TmuxTestsAnchorGuard::new();
        let target = TestSession::new();
        create_shell_session_after_final_teardown(&target.name);
        let cgroup = create_private_process_cgroup().unwrap();
        for index in 0..64 {
            std::fs::create_dir(
                std::path::Path::new(&cgroup.generation().path).join(format!("child-{index}")),
            )
            .unwrap();
        }
        let expected = pane_generation_exact(&target.name).unwrap();
        std::fs::write(
            std::path::Path::new(&cgroup.generation().path).join("cgroup.procs"),
            format!("{}\n", expected.pane_pid),
        )
        .unwrap();
        wait_for_exact_cgroup_member(expected.pane_pid, cgroup.generation());
        let error =
            kill_session_tree_exact_generation(&target.name, expected, Some(cgroup.generation()))
                .unwrap_err();
        assert!(error.message.contains("bounded subtree limit"));
        assert_eq!(session_liveness(&target.name), SessionLiveness::Alive);
        for index in 0..64 {
            std::fs::remove_dir(
                std::path::Path::new(&cgroup.generation().path).join(format!("child-{index}")),
            )
            .unwrap();
        }
        assert_eq!(
            kill_session_tree_exact_generation(&target.name, expected, Some(cgroup.generation()))
                .unwrap(),
            ExactGenerationKill::Killed
        );
        assert!(!std::path::Path::new(&cgroup.generation().path).exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn persisted_private_cgroup_generation_replays_exact_cleanup_after_restart() {
        if !tmux_available() {
            return;
        }
        let _anchor = TmuxTestsAnchorGuard::new();
        let target = TestSession::new();
        create_shell_session_after_final_teardown(&target.name);
        let cgroup = create_private_process_cgroup().unwrap();
        let expected = pane_generation_exact(&target.name).unwrap();
        std::fs::write(
            std::path::Path::new(&cgroup.generation().path).join("cgroup.procs"),
            format!("{}\n", expected.pane_pid),
        )
        .unwrap();
        wait_for_exact_cgroup_member(expected.pane_pid, cgroup.generation());
        let receipt = crate::harness::HarnessTerminalGenerationReceipt {
            tmux_target: target.name.clone(),
            session_id: expected.session_id,
            session_created: expected.session_created,
            window_id: expected.window_id,
            pane_id: expected.pane_id,
            pane_pid: expected.pane_pid,
            process_cgroup: Some(crate::harness::HarnessProcessCgroupReceipt {
                path: cgroup.generation().path.clone(),
                device: cgroup.generation().device,
                inode: cgroup.generation().inode,
            }),
        };
        let persisted = serde_json::to_vec(&receipt).unwrap();
        let retained = cgroup.retain();
        let reloaded: crate::harness::HarnessTerminalGenerationReceipt =
            serde_json::from_slice(&persisted).unwrap();
        let reloaded_cgroup = reloaded.process_cgroup.unwrap();
        let reloaded_cgroup = ProcessCgroupGeneration {
            path: reloaded_cgroup.path,
            device: reloaded_cgroup.device,
            inode: reloaded_cgroup.inode,
        };
        assert_eq!(reloaded_cgroup, retained);
        assert_eq!(
            kill_session_tree_exact_generation(&target.name, expected, Some(&reloaded_cgroup))
                .unwrap(),
            ExactGenerationKill::Killed
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while session_liveness(&target.name) != SessionLiveness::Gone {
            assert!(
                std::time::Instant::now() < deadline,
                "replayed exact cgroup cleanup did not converge"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!std::path::Path::new(&reloaded_cgroup.path).exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn persisted_empty_cgroup_is_removed_after_exact_root_is_already_gone() {
        if !tmux_available() {
            return;
        }
        let _anchor = TmuxTestsAnchorGuard::new();
        let target = TestSession::new();
        create_shell_session_after_final_teardown(&target.name);
        let cgroup = create_private_process_cgroup().unwrap();
        let expected = pane_generation_exact(&target.name).unwrap();
        std::fs::write(
            std::path::Path::new(&cgroup.generation().path).join("cgroup.procs"),
            format!("{}\n", expected.pane_pid),
        )
        .unwrap();
        wait_for_exact_cgroup_member(expected.pane_pid, cgroup.generation());
        let status = Command::new("kill")
            .args(["-KILL", &expected.pane_pid.to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while session_liveness(&target.name) != SessionLiveness::Gone {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(std::path::Path::new(&cgroup.generation().path).exists());
        assert_eq!(
            kill_session_tree_exact_generation(&target.name, expected, Some(cgroup.generation()))
                .unwrap(),
            ExactGenerationKill::Killed
        );
        assert!(!std::path::Path::new(&cgroup.generation().path).exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn persisted_cgroup_kills_exact_live_descendants_after_root_is_gone() {
        let cgroup = create_private_process_cgroup().unwrap();
        let gate = std::env::temp_dir().join(format!(
            "t-hub-root-gone-cgroup-gate-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let script = format!(
            "while [ ! -e '{}' ]; do sleep 0.01; done; trap '' HUP; sh -c 'trap \"\" HUP; sleep 2147483647' & wait",
            gate.display()
        );
        let mut root = Command::new("setsid")
            .args(["sh", "-c", &script])
            .env("T_HUB_TEST_CORTANA_BEARER", "fixture-only")
            .spawn()
            .unwrap();
        let expected = PaneGeneration {
            session_id: 1,
            session_created: 1,
            window_id: 1,
            pane_id: 1,
            pane_pid: root.id(),
        };
        std::fs::write(
            std::path::Path::new(&cgroup.generation().path).join("cgroup.procs"),
            format!("{}\n", expected.pane_pid),
        )
        .unwrap();
        wait_for_exact_cgroup_member(expected.pane_pid, cgroup.generation());
        std::fs::write(&gate, b"release").unwrap();
        let cgroup_procs = std::path::Path::new(&cgroup.generation().path).join("cgroup.procs");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let descendants = loop {
            let tasks = std::fs::read_to_string(&cgroup_procs)
                .unwrap()
                .lines()
                .filter_map(|value| value.parse::<u32>().ok())
                .filter(|pid| *pid != expected.pane_pid)
                .collect::<Vec<_>>();
            if tasks.len() >= 2 {
                break tasks;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "credential-bearing descendant fixture did not start"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let status = Command::new("kill")
            .args(["-KILL", &expected.pane_pid.to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        root.wait().unwrap();
        assert!(descendants.iter().any(|pid| {
            std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|environment| {
                environment
                    .split(|byte| *byte == 0)
                    .any(|entry| entry.starts_with(b"T_HUB_TEST_CORTANA_BEARER="))
            })
        }));
        let target = format!("th_test_root_gone_{}", uuid::Uuid::new_v4().simple());
        assert_eq!(session_liveness(&target), SessionLiveness::Gone);
        assert_eq!(
            kill_session_tree_exact_generation(&target, expected, Some(cgroup.generation()))
                .unwrap(),
            ExactGenerationKill::Killed
        );
        for pid in descendants {
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        }
        assert!(!std::path::Path::new(&cgroup.generation().path).exists());
        std::fs::remove_file(gate).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn exact_cgroup_replacement_at_anchored_removal_boundary_survives() {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};

        if !tmux_available() {
            return;
        }
        let _anchor = TmuxTestsAnchorGuard::new();
        let target = TestSession::new();
        create_shell_session_after_final_teardown(&target.name);
        let expected = pane_generation_exact(&target.name).unwrap();
        let (cgroup, mut guardian) = confine_blocked_pane_to_private_cgroup(&target.name, expected)
            .unwrap_or_else(|failure| {
                panic!("scope confinement failed: {}", failure.error.message)
            });
        guardian.disarm().unwrap();
        let original = cgroup.generation().clone();
        let replacement_identity = std::rc::Rc::new(std::cell::RefCell::new(None));
        let replacement_from_hook = replacement_identity.clone();
        let replacement_path = original.path.clone();
        let hook = CgroupCleanupHookGuard::install(move |point| {
            if point == CgroupCleanupTestPoint::BeforeOwnedScopeRemovalWait {
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while std::path::Path::new(&replacement_path).exists() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "systemd did not remove its exact owned scope"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                let mut builder = std::fs::DirBuilder::new();
                builder.mode(0o700).create(&replacement_path).unwrap();
                let metadata = std::fs::metadata(&replacement_path).unwrap();
                *replacement_from_hook.borrow_mut() = Some((metadata.dev(), metadata.ino()));
            }
            Ok(())
        });
        let error =
            kill_session_tree_exact_generation(&target.name, expected, Some(cgroup.generation()))
                .unwrap_err();
        drop(hook);
        assert!(
            error.message.contains("substituted"),
            "unexpected cleanup error: {}",
            error.message
        );
        let metadata = std::fs::metadata(&original.path).unwrap();
        let replacement = replacement_identity.borrow().unwrap();
        assert_eq!((metadata.dev(), metadata.ino()), replacement);
        assert_ne!(
            (metadata.dev(), metadata.ino()),
            (original.device, original.inode)
        );
        drop(cgroup);
        assert!(std::path::Path::new(&original.path).exists());
        std::fs::remove_dir(&original.path).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn root_gone_cleanup_rejects_foreign_task_and_replays_after_exact_removal() {
        let cgroup = create_private_process_cgroup().unwrap();
        let mut foreign = Command::new("sleep").arg("30").spawn().unwrap();
        let foreign_membership = std::fs::read_to_string(format!("/proc/{}/cgroup", foreign.id()))
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("0::").map(str::to_string))
            .unwrap();
        std::fs::write(
            std::path::Path::new(&cgroup.generation().path).join("cgroup.procs"),
            format!("{}\n", foreign.id()),
        )
        .unwrap();
        wait_for_exact_cgroup_member(foreign.id(), cgroup.generation());
        let expected = PaneGeneration {
            session_id: 1,
            session_created: 1,
            window_id: 1,
            pane_id: 1,
            pane_pid: u32::MAX - 1,
        };
        let target = format!("th_test_root_foreign_{}", uuid::Uuid::new_v4().simple());
        let error =
            kill_session_tree_exact_generation(&target, expected, Some(cgroup.generation()))
                .unwrap_err();
        assert!(error.message.contains("identity") || error.message.contains("session"));
        assert!(foreign.try_wait().unwrap().is_none());
        let original_cgroup = std::path::Path::new("/sys/fs/cgroup")
            .join(foreign_membership.trim_start_matches('/'))
            .join("cgroup.procs");
        std::fs::write(original_cgroup, format!("{}\n", foreign.id())).unwrap();
        foreign.kill().unwrap();
        foreign.wait().unwrap();
        assert_eq!(
            kill_session_tree_exact_generation(&target, expected, Some(cgroup.generation()))
                .unwrap(),
            ExactGenerationKill::Killed
        );
        assert!(!std::path::Path::new(&cgroup.generation().path).exists());
    }

    #[test]
    fn exact_session_environment_scrub_removes_every_credential_key() {
        if !tmux_available() {
            return;
        }
        let _anchor = TmuxTestsAnchorGuard::new();
        let target = TestSession::new();
        create_shell_session_after_final_teardown(&target.name);
        let env = vec![
            ("T_HUB_CONTROL_ADDR".to_string(), "127.0.0.1:1".to_string()),
            (
                "T_HUB_CONTROL_TOKEN".to_string(),
                "control-secret".to_string(),
            ),
            (
                "T_HUB_SESSION_TOKEN".to_string(),
                "session-secret".to_string(),
            ),
        ];
        set_session_environment_exact(&target.name, &env).unwrap();
        let keys = env.into_iter().map(|(key, _)| key).collect::<Vec<_>>();
        unset_session_environment_exact(&target.name, &keys).unwrap();
        for key in keys {
            assert_eq!(session_environment_exact(&target.name, &key).unwrap(), None);
        }
    }

    #[test]
    fn dormant_pane_respawns_in_place_with_a_new_generation() {
        if !tmux_available() {
            eprintln!(
                "tmux::tests::dormant_pane_respawns_in_place_with_a_new_generation: tmux not on PATH - skipping"
            );
            return;
        }
        let session = TestSession::new();
        new_session_with_env(
            &session.name,
            "/tmp",
            Some("exec sleep 2147483647"),
            &[("T_HUB_RESPAWN_ENV".into(), "preserved".into())],
        )
        .unwrap();
        let command = crate::commands::pane_command(
            None,
            Some("printf 'T_HUB_RESPAWN_OK\\n'; exec sleep 2147483647"),
        )
        .unwrap();
        let transition = respawn_pane_exact(&session.name, "/tmp", &command).unwrap();
        assert_eq!(transition.before.session_id, transition.after.session_id);
        assert_eq!(
            transition.before.session_created,
            transition.after.session_created
        );
        assert_eq!(transition.before.window_id, transition.after.window_id);
        assert_eq!(transition.before.pane_id, transition.after.pane_id);
        assert_ne!(transition.before.pane_pid, transition.after.pane_pid);
        assert_eq!(
            session_environment(&session.name, "T_HUB_RESPAWN_ENV").unwrap(),
            Some("preserved".into())
        );
        assert!(has_session(&session.name));
    }

    #[test]
    fn respawn_executes_when_a_hostile_pane_discards_injected_keys() {
        if !tmux_available() {
            eprintln!(
                "tmux::tests::respawn_executes_when_a_hostile_pane_discards_injected_keys: tmux not on PATH - skipping"
            );
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let ready = fixture.path().join("ready");
        let injected = fixture.path().join("injected");
        let respawned = fixture.path().join("respawned");
        let hostile = fixture.path().join("hostile-shell");
        std::fs::write(
            &hostile,
            format!(
                "#!/bin/sh\n: > {}\nwhile IFS= read -r ignored; do :; done\n",
                ready.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hostile, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let session = TestSession::new();
        new_session_with_env(&session.name, "/tmp", Some(hostile.to_str().unwrap()), &[]).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !ready.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "hostile pane did not become ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        send_text(
            &session.name,
            &format!("printf injected > {}", injected.display()),
            true,
        )
        .unwrap();
        assert!(
            !injected.exists(),
            "the hostile pane must discard the injected line rather than execute it"
        );
        let command = crate::commands::pane_command(
            None,
            Some(&format!(
                "printf respawned > {}; exec sleep 2147483647",
                respawned.display()
            )),
        )
        .unwrap();
        respawn_pane_exact(&session.name, "/tmp", &command).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !respawned.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "respawn command did not execute"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The MCP read/write helpers round-trip through a real session: send a
    /// literal line, then read it back as plain text from the captured pane.
    ///
    /// Like `lifecycle_create_list_capture_kill`, this needs a real `tmux` on
    /// PATH (present in the WSL2 dev shell, not on the Windows CI target).
    #[test]
    fn send_text_then_capture_plain_text_roundtrips() {
        if !tmux_available() {
            eprintln!(
                "tmux::tests::send_text_then_capture_plain_text_roundtrips: \
                 tmux not on PATH — skipping"
            );
            return;
        }
        let name = unique_name();
        let _ = kill_session(&name);
        new_session_with_env(&name, "/tmp", None, &[]).expect("new_session should succeed");
        // Deterministic geometry regardless of what the server's latest client
        // reports (see `resize_window_for_tests` — the wedged-2x24 gotcha).
        resize_window_for_tests(&name, 80, 24).expect("resize should succeed");

        // Echo a sentinel so it lands in the visible pane, then submit it.
        send_text(&name, "echo T_HUB_MCP_SENTINEL_42", true).expect("send_text should succeed");
        // Give the shell a beat to execute + render the echo output.
        std::thread::sleep(std::time::Duration::from_millis(300));

        let text = capture_pane_text(&name, 0).expect("capture_pane_text should succeed");
        assert!(
            text.contains("T_HUB_MCP_SENTINEL_42"),
            "captured plain text should echo the sentinel; got: {text:?}"
        );
        // Plain capture must not carry raw ANSI escape bytes.
        assert!(
            !text.contains('\u{1b}'),
            "plain capture should be free of ANSI escapes"
        );

        kill_session(&name).expect("kill_session should succeed");
    }

    /// `send_keys` interprets named keys: a `C-c` then `Enter` should not error
    /// on a live session (it interrupts whatever is running / clears the line).
    #[test]
    fn send_keys_named_keys_succeed_on_live_session() {
        if !tmux_available() {
            eprintln!(
                "tmux::tests::send_keys_named_keys_succeed_on_live_session: \
                 tmux not on PATH — skipping"
            );
            return;
        }
        let _anchor = TmuxTestsAnchorGuard::new();
        let mut target = TestSession::new();
        create_shell_session_after_final_teardown(&target.name);

        send_keys(&target.name, &["C-c"]).expect("send_keys C-c should succeed");
        send_keys(&target.name, &["Enter"]).expect("send_keys Enter should succeed");

        target.close().expect("target cleanup should succeed");
    }

    /// The attach belt-and-braces: [`reassert_window_size_latest`] restores
    /// `window-size latest` on a session that was flipped to `manual` (the retired
    /// captain 220x50 workaround). Proves a stale manual override can't outlive its
    /// purpose once the attach path reasserts on every attach.
    ///
    /// Needs a real `tmux` on PATH (the WSL2 dev shell, not the Windows CI target).
    #[test]
    fn reassert_window_size_latest_restores_from_manual() {
        if !tmux_available() {
            eprintln!(
                "tmux::tests::reassert_window_size_latest_restores_from_manual: \
                 tmux not on PATH — skipping"
            );
            return;
        }
        // Reads the window-size option's current mode ("latest" / "manual") off a
        // live session. `show-options -w -t <name> window-size` prints
        // `window-size <mode>`; we return just the mode token.
        fn window_size_mode(name: &str) -> String {
            let out = output_with_timeout(
                tmux(&["show-options", "-w", "-t", name, "window-size"]),
                tmux_cmd_timeout(),
            )
            .expect("show-options should run");
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string()
        }

        let name = unique_name();
        let _ = kill_session(&name);
        new_session_with_env(&name, "/tmp", None, &[]).expect("new_session should succeed");
        // Fresh sessions are pinned `latest` at creation.
        assert_eq!(
            window_size_mode(&name),
            "latest",
            "a freshly created session pins window-size latest"
        );

        // `resize-window` flips the window to `manual` — exactly the degenerate
        // state the retired captain 220x50 workaround left behind.
        resize_window_for_tests(&name, 220, 50).expect("resize should succeed");
        assert_eq!(
            window_size_mode(&name),
            "manual",
            "resize-window must flip the window to manual sizing"
        );

        // The attach reassert restores `latest`, so the window tracks its client
        // again instead of clipping at a stale manual size.
        reassert_window_size_latest(&name);
        assert_eq!(
            window_size_mode(&name),
            "latest",
            "reassert_window_size_latest must restore window-size latest"
        );

        kill_session(&name).expect("kill_session should succeed");
    }

    /// kill_session on a missing session is idempotent (success), and
    /// has_session reports false for a name that was never created.
    #[test]
    fn kill_missing_is_idempotent() {
        if !tmux_available() {
            eprintln!("tmux::tests::kill_missing_is_idempotent: tmux not on PATH — skipping");
            return;
        }
        let name = format!("{}_never", unique_name());
        assert!(!has_session(&name));
        kill_session(&name).expect("killing a missing session should be Ok");
    }

    /// list_sessions tolerates the no-server / empty case by returning Ok
    /// (possibly empty) rather than erroring.
    #[test]
    fn list_sessions_tolerates_empty() {
        if !tmux_available() {
            eprintln!("tmux::tests::list_sessions_tolerates_empty: tmux not on PATH — skipping");
            return;
        }
        // Whether or not a server is running, this must not error.
        let _ = list_sessions().expect("list_sessions must tolerate no-server");
    }

    /// De-conflation guard (spawn-wedge): the transfer-/reap-grade death signal is
    /// TRUE only for a DEFINITIVE `Gone`. `Alive` and `Unknown` (a timed-out /
    /// failed probe) are BOTH not-gone, so a degraded control plane can never seize
    /// a live captain's ship, retire a live identity, or emit a spurious EXIT. This
    /// is pure (no tmux) so it runs everywhere, including Windows CI. Reverting the
    /// `Unknown => false` arm to the old `unwrap_or(false)` conflation trips it.
    #[test]
    fn definitively_gone_is_only_the_completed_absent_probe() {
        assert!(
            is_definitively_gone(SessionLiveness::Gone),
            "a completed absent probe is transfer/reap-grade (R1)"
        );
        assert!(
            !is_definitively_gone(SessionLiveness::Alive),
            "a live session is never gone"
        );
        assert!(
            !is_definitively_gone(SessionLiveness::Unknown),
            "an indeterminate (timed-out) probe must NOT read as gone — ambiguous is never seized"
        );
    }

    /// A live tmux session probes `Alive`, and a never-created name probes `Gone`
    /// (a COMPLETED negative, not `Unknown`) — the definitive arms of the split.
    /// Needs a real tmux (WSL dev shell), skipped on the Windows CI target.
    #[test]
    fn session_liveness_reports_alive_and_definitive_gone() {
        if !tmux_available() {
            eprintln!(
                "tmux::tests::session_liveness_reports_alive_and_definitive_gone: \
                 tmux not on PATH — skipping"
            );
            return;
        }
        let name = unique_name();
        let _ = kill_session(&name);
        assert_eq!(
            session_liveness(&name),
            SessionLiveness::Gone,
            "a never-created session is a definitive Gone (probe completed, exited non-zero)"
        );
        new_session_with_env(&name, "/tmp", None, &[]).expect("new_session should succeed");
        assert_eq!(
            session_liveness(&name),
            SessionLiveness::Alive,
            "a created session is Alive"
        );
        kill_session(&name).expect("kill_session should succeed");
        assert_eq!(
            session_liveness(&name),
            SessionLiveness::Gone,
            "a killed session is a definitive Gone"
        );
    }

    #[test]
    fn harness_liveness_rejects_the_fallback_shell_after_agent_exit() {
        if !tmux_available() {
            eprintln!(
                "tmux::tests::harness_liveness_rejects_the_fallback_shell_after_agent_exit: \
                 tmux not on PATH - skipping"
            );
            return;
        }
        let name = format!("th_harness-exit-{}", uuid::Uuid::new_v4().simple());
        let pane = crate::commands::pane_command(None, Some("true")).unwrap();
        new_session_with_env(&name, "/tmp", Some(&pane), &[]).unwrap();
        std::thread::sleep(Duration::from_millis(250));

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut terminal = SessionLiveness::Unknown;
        let mut harness = SessionLiveness::Unknown;
        while std::time::Instant::now() < deadline {
            terminal = session_liveness(&name);
            harness = harness_liveness(&name, "codex");
            if terminal == SessionLiveness::Alive && harness == SessionLiveness::Gone {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(terminal, SessionLiveness::Alive);
        assert_eq!(harness, SessionLiveness::Gone);
        let _ = kill_session(&name);
    }

    #[test]
    fn harness_liveness_accepts_the_expected_foreground_process() {
        if !tmux_available() {
            eprintln!(
                "tmux::tests::harness_liveness_accepts_the_expected_foreground_process: \
                 tmux not on PATH - skipping"
            );
            return;
        }
        let name = format!("th_harness-live-{}", uuid::Uuid::new_v4().simple());
        let bin_dir = std::env::temp_dir().join(format!("{name}-bin"));
        std::fs::create_dir_all(&bin_dir).unwrap();
        let executable = bin_dir.join("codex");
        std::fs::copy("/bin/sleep", &executable).unwrap();
        let command = format!("{} 60", executable.display());
        new_session_with_env(&name, "/tmp", Some(&command), &[]).unwrap();

        assert_eq!(harness_liveness(&name, "codex"), SessionLiveness::Alive);
        let _ = kill_session(&name);
        std::fs::remove_dir_all(bin_dir).unwrap();
    }

    #[test]
    fn harness_liveness_accepts_a_node_wrapped_codex_process() {
        if !tmux_available()
            || !Command::new("node")
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        {
            eprintln!(
                "tmux::tests::harness_liveness_accepts_a_node_wrapped_codex_process: \
                 tmux or node not on PATH - skipping"
            );
            return;
        }
        let name = format!("th_harness-node-{}", uuid::Uuid::new_v4().simple());
        let fixture_dir = std::env::temp_dir().join(format!("{name}-fixture"));
        std::fs::create_dir_all(&fixture_dir).unwrap();
        let launcher = fixture_dir.join("codex.js");
        std::fs::write(&launcher, "setInterval(() => {}, 1000);\n").unwrap();
        let command = format!("node {}", launcher.display());
        new_session_with_env(&name, "/tmp", Some(&command), &[]).unwrap();

        assert_eq!(harness_liveness(&name, "codex"), SessionLiveness::Alive);
        let _ = kill_session(&name);
        std::fs::remove_dir_all(fixture_dir).unwrap();
    }
}
