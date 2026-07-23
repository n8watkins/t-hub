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

use crate::bounded_exec::{output_with_timeout, output_with_timeout_and_limit};

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

#[cfg(test)]
static FAIL_NEXT_SESSION_ENVIRONMENT_TARGET: LazyLock<std::sync::Mutex<Option<String>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

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

fn validate_socket_name(value: &str) -> Result<&str, TmuxError> {
    if value.is_empty()
        || value.len() > 64
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(TmuxError {
            op: "validate-socket",
            code: None,
            message: "configured tmux socket name is outside the safe socket-name contract".into(),
        });
    }
    Ok(value)
}

pub(crate) fn validated_socket_name() -> Result<&'static str, TmuxError> {
    validate_socket_name(socket())
}

/// Serialize tests that exercise real tmux process ownership and keep an
/// independent anchor alive while the shared isolated server is in use.
#[cfg(test)]
pub(crate) struct TestLifecycleGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    anchor: String,
}

#[cfg(test)]
impl TestLifecycleGuard {
    pub(crate) fn acquire() -> Self {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let lock = LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let anchor = format!(
            "th_test_anchor_{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match new_session_with_env(&anchor, "/tmp", None, &[]) {
                Ok(()) => return Self { _lock: lock, anchor },
                Err(error) if error.message == "server exited unexpectedly" => {
                    match session_liveness(&anchor) {
                        SessionLiveness::Alive => return Self { _lock: lock, anchor },
                        SessionLiveness::Gone if std::time::Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        liveness => panic!(
                            "tmux test anchor could not start after server teardown ({liveness:?}): {error}"
                        ),
                    }
                }
                Err(error) => panic!("tmux test anchor could not start: {error}"),
            }
        }
    }
}

#[cfg(test)]
impl Drop for TestLifecycleGuard {
    fn drop(&mut self) {
        let _ = kill_session(&self.anchor);
    }
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

/// Exact Linux process and tmux generation for one retirement effect.
///
/// Both process start tokens and the process-group/session ownership are needed:
/// numeric PIDs and a tmux session name can be reused independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SessionEffectIdentity {
    pub(crate) tmux_session_id: u64,
    pub(crate) tmux_session_created: u64,
    pub(crate) tmux_window_id: u64,
    pub(crate) tmux_pane_id: u64,
    pub(crate) pane_pid: u32,
    pub(crate) pane_start_ticks: u64,
    pub(crate) pane_process_group_id: u32,
    pub(crate) pane_process_session_id: u32,
    pub(crate) foreground_pid: u32,
    pub(crate) foreground_start_ticks: u64,
    pub(crate) foreground_process_group_id: u32,
    pub(crate) foreground_process_session_id: u32,
}

pub(crate) const MANAGED_RUNTIME_OWNER_VERSION: u32 = 2;

const MANAGED_HELPER_OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ManagedExecutableIdentity {
    pub(crate) path: String,
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ManagedSystemTools {
    pub(crate) python: ManagedExecutableIdentity,
    pub(crate) systemctl: ManagedExecutableIdentity,
    pub(crate) systemd_run: ManagedExecutableIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ManagedRuntimeLaunchSpec {
    pub(crate) unit_name: String,
    pub(crate) launch_nonce: String,
    pub(crate) tools: ManagedSystemTools,
}

/// Stable proof that one tmux pane generation is owned by one transient user
/// systemd scope and its exact cgroup-v2 directory.
///
/// This is a stale-effect and accidental-interposition boundary, not a hostile
/// same-UID security boundary.
/// A malicious process running as the same user can modify user-owned state.
/// Legacy migration therefore treats ambiguous effects as unowned, revokes every
/// candidate bearer identity, and does not signal any candidate runtime.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ManagedRuntimeOwnerToken {
    pub(crate) version: u32,
    pub(crate) unit_name: String,
    pub(crate) invocation_id: String,
    pub(crate) cgroup_path: String,
    pub(crate) cgroup_inode: u64,
    pub(crate) launcher_pid: u32,
    pub(crate) launcher_start_ticks: u64,
    pub(crate) launch_nonce: String,
    pub(crate) tools: ManagedSystemTools,
    pub(crate) tmux: SessionEffectIdentity,
}

/// Evidence for a private dormant-pane to provider-pane transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RespawnPaneTransition {
    pub(crate) before: PaneGeneration,
    pub(crate) after: PaneGeneration,
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
fn tmux(args: &[&str]) -> Result<Command, TmuxError> {
    let socket = validated_socket_name()?;
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
    cmd.arg("-L").arg(socket);
    cmd.args(args);
    Ok(cmd)
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
    let output = output_with_timeout(tmux(args)?, tmux_cmd_timeout()).map_err(|e| TmuxError {
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
    let before = pane_generation(target)?;
    let mut args = vec!["respawn-pane", "-k", "-t", target];
    if !cwd.is_empty() {
        args.extend(["-c", cwd]);
    }
    args.push(command);
    run("respawn-pane", &args)?;
    let after = pane_generation(target)?;
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

/// Set one value in a live tmux session's private environment.
///
/// The command uses argv values rather than a shell and is bounded by the same
/// timeout as every other tmux control operation. Environment names are
/// validated here so callers cannot accidentally ask tmux to interpret an
/// option or malformed assignment as the variable name.
pub fn set_session_environment(name: &str, key: &str, value: &str) -> Result<(), TmuxError> {
    let valid_key = !key.is_empty()
        && key.len() <= 128
        && key.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphanumeric() && (index > 0 || !byte.is_ascii_digit())
        });
    if !valid_key {
        return Err(TmuxError {
            op: "set-environment",
            code: None,
            message: "environment name must be a valid identifier no longer than 128 bytes".into(),
        });
    }
    if name.is_empty() || name.len() > 128 || value.len() > 4096 || value.contains('\0') {
        return Err(TmuxError {
            op: "set-environment",
            code: None,
            message: "session target or environment value is outside the bounded input contract"
                .into(),
        });
    }
    #[cfg(test)]
    {
        let mut failure = FAIL_NEXT_SESSION_ENVIRONMENT_TARGET
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if failure.as_deref() == Some(name) {
            *failure = None;
            return Err(TmuxError {
                op: "set-environment",
                code: None,
                message: "injected session environment update failure".into(),
            });
        }
    }
    run(
        "set-environment",
        &["set-environment", "-t", name, key, value],
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn fail_next_session_environment_set_for(name: &str) {
    *FAIL_NEXT_SESSION_ENVIRONMENT_TARGET
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(name.to_string());
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
fn session_env_with_agent_journal(
    env: &[(String, String)],
    configured_journal: Option<String>,
) -> Vec<(String, String)> {
    let mut effective = env.to_vec();
    if !effective
        .iter()
        .any(|(key, _)| key == "T_HUB_AGENT_JOURNAL_DIR")
    {
        if let Some(journal) = configured_journal.filter(|value| !value.trim().is_empty()) {
            effective.push(("T_HUB_AGENT_JOURNAL_DIR".to_string(), journal));
        }
    }
    effective
}

pub fn new_session_with_env(
    name: &str,
    cwd: &str,
    command: Option<&str>,
    env: &[(String, String)],
) -> Result<(), TmuxError> {
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
    let mut args: Vec<&str> = vec!["new-session", "-d", "-x", "80", "-y", "24", "-s", name];
    // Session environment (`-e KEY=VALUE`, socket-gate Phase 2b). Pre-format so the
    // backing strings outlive `args`. tmux ≥3.2 supports `-e`; this codebase already
    // targets ≥3.4 (see the geometry note above).
    let effective_env =
        session_env_with_agent_journal(env, std::env::var("T_HUB_AGENT_JOURNAL_DIR").ok());
    let env_pairs: Vec<String> = effective_env
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
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
    run("new-session", &seq)?;

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
    Ok(())
}

/// Create a detached pane whose first user process is already inside a unique
/// transient user-systemd scope, then publish ownership only after every kernel,
/// systemd, nonce, process, and tmux identity agrees.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn new_managed_session_with_env(
    name: &str,
    cwd: &str,
    command: Option<&str>,
    env: &[(String, String)],
) -> Result<ManagedRuntimeOwnerToken, TmuxError> {
    let launch = prepare_managed_runtime_launch()?;
    new_prepared_managed_session_with_env(name, cwd, command, env, &launch)
}

pub(crate) fn prepare_managed_runtime_launch() -> Result<ManagedRuntimeLaunchSpec, TmuxError> {
    let tools = resolve_managed_system_tools()?;
    managed_runtime_preflight_with_tools(&tools)?;
    let launch_nonce = uuid::Uuid::new_v4().simple().to_string();
    Ok(ManagedRuntimeLaunchSpec {
        unit_name: format!("t-hub-{launch_nonce}.scope"),
        launch_nonce,
        tools,
    })
}

pub(crate) fn new_prepared_managed_session_with_env(
    name: &str,
    cwd: &str,
    command: Option<&str>,
    env: &[(String, String)],
    launch: &ManagedRuntimeLaunchSpec,
) -> Result<ManagedRuntimeOwnerToken, TmuxError> {
    managed_runtime_preflight_with_tools(&launch.tools)?;
    if launch.unit_name != format!("t-hub-{}.scope", launch.launch_nonce)
        || launch.launch_nonce.len() != 32
        || !launch
            .launch_nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(TmuxError {
            op: "new-managed-session",
            code: None,
            message: "prepared managed runtime identity is malformed".into(),
        });
    }
    if env
        .iter()
        .any(|(key, _)| matches!(key.as_str(), "T_HUB_LAUNCH_NONCE" | "T_HUB_MANAGED_STARTUP"))
    {
        return Err(TmuxError {
            op: "new-managed-session",
            code: None,
            message: "managed runtime ownership environment is reserved".into(),
        });
    }
    let nonce = &launch.launch_nonce;
    let unit_name = &launch.unit_name;
    let startup = command
        .map(str::to_string)
        .unwrap_or_else(|| "exec \"${SHELL:-/bin/sh}\" -l".into());
    let mut managed_env = env.to_vec();
    managed_env.push(("T_HUB_LAUNCH_NONCE".into(), nonce.clone()));
    managed_env.push(("T_HUB_MANAGED_STARTUP".into(), startup));
    let wrapper = format!(
        "exec {} --user --scope --unit={unit_name} --collect --quiet -- \
         /bin/sh -lc 'exec /bin/sh -lc \"$T_HUB_MANAGED_STARTUP\"'",
        launch.tools.systemd_run.path
    );
    new_session_with_env(name, cwd, Some(&wrapper), &managed_env)?;

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last_error = None;
    while std::time::Instant::now() < deadline {
        match observe_session_effect_identity(name)
            .and_then(|tmux| observe_managed_runtime_owner(&launch.tools, unit_name, nonce, tmux))
        {
            Ok(owner) => return Ok(owner),
            Err(error) => last_error = Some(error),
        }
        if session_liveness(name) == SessionLiveness::Gone {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let unit_cleanup = retire_prepared_managed_runtime(launch);
    let session_cleanup = kill_session(name);
    let mut error = last_error.unwrap_or(TmuxError {
        op: "new-managed-session",
        code: None,
        message: "managed runtime ownership was not established before publication".into(),
    });
    if let Err(cleanup) = unit_cleanup {
        error.message = format!(
            "{}; exact prepared cleanup failed: {cleanup}",
            error.message
        );
    }
    if let Err(cleanup) = session_cleanup {
        error.message = format!("{}; exact tmux cleanup failed: {cleanup}", error.message);
    }
    Err(error)
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
    let Ok(command) = tmux(&["has-session", "-t", name]) else {
        return SessionLiveness::Unknown;
    };
    match output_with_timeout(command, tmux_cmd_timeout()) {
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
    if let Ok(command) = tmux(&["set-option", "-t", name, "window-size", "latest"]) {
        let _ = output_with_timeout(command, tmux_cmd_timeout());
    }
}

/// Perform bounded, non-destructive maintenance on one exact live session.
///
/// Unlike [`reassert_window_size_latest`], this administrative path reports an
/// error when the exact mutation cannot be confirmed. Callers can therefore
/// distinguish a maintained session from a missing or indeterminate target and
/// record an honest operation outcome.
pub fn maintain_session(name: &str) -> Result<(), TmuxError> {
    if name.is_empty() || name.len() > 128 || !name.starts_with("th_") {
        return Err(TmuxError {
            op: "maintain-session",
            code: None,
            message: "session target must be one exact T-Hub tmux session".into(),
        });
    }
    match session_liveness(name) {
        SessionLiveness::Alive => {}
        SessionLiveness::Gone => {
            return Err(TmuxError {
                op: "maintain-session",
                code: None,
                message: "session is definitively gone".into(),
            });
        }
        SessionLiveness::Unknown => {
            return Err(TmuxError {
                op: "maintain-session",
                code: None,
                message: "session liveness is indeterminate; retry without mutating it".into(),
            });
        }
    }
    run(
        "maintain-session",
        &["set-option", "-t", name, "window-size", "latest"],
    )?;
    Ok(())
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
    let output = output_with_timeout(tmux(&["kill-session", "-t", name])?, tmux_cmd_timeout())
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

fn exact_effect_target(name: &str) -> Result<(), TmuxError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'='))
    {
        return Err(TmuxError {
            op: "exact-session-effect",
            code: None,
            message: "target is outside the exact session effect contract".into(),
        });
    }
    Ok(())
}

const EXACT_SESSION_EFFECT_PY: &str = r##"
import json, os, re, signal, stat, subprocess, sys, time

KNOWN_PYTHON = ("/usr/bin/python3", "/bin/python3")

def refuse(code):
    raise SystemExit(code)

def executable_identity(path, candidates):
    if not isinstance(path, str) or not path.startswith("/") or path.startswith("//"):
        refuse(68)
    canonical_candidates = {os.path.realpath(candidate) for candidate in candidates}
    canonical = os.path.realpath(path)
    if canonical not in canonical_candidates or path != canonical:
        refuse(69)
    try:
        details = os.stat(canonical, follow_symlinks=False)
    except OSError:
        refuse(70)
    if (not stat.S_ISREG(details.st_mode) or details.st_uid != 0 or
        details.st_mode & (stat.S_IWGRP | stat.S_IWOTH) or
        not details.st_mode & stat.S_IXUSR):
        refuse(71)
    return {"path": canonical, "device": details.st_dev, "inode": details.st_ino}

try:
    expected_python = json.loads(sys.argv[1])
except (IndexError, TypeError, ValueError, json.JSONDecodeError):
    refuse(72)
actual_python = executable_identity(os.path.realpath(sys.executable), KNOWN_PYTHON)
if actual_python != expected_python:
    refuse(73)
sys.argv = [sys.argv[0], *sys.argv[2:]]

def proc_stat(pid):
    try:
        raw = open(f"/proc/{pid}/stat", "r", encoding="ascii").read()
        fields = raw.rsplit(") ", 1)[1].split()
        if len(fields) < 20:
            refuse(40)
        return {
            "pid": int(pid), "ppid": int(fields[1]), "pgrp": int(fields[2]),
            "sid": int(fields[3]), "tpgid": int(fields[5]), "start": int(fields[19]),
        }
    except (FileNotFoundError, ProcessLookupError, PermissionError, ValueError, IndexError):
        refuse(41)

def prefixed(value, prefix):
    if not value.startswith(prefix):
        refuse(42)
    return int(value[1:])

def observe(socket_name, target):
    try:
        result = subprocess.run(
            ["tmux", "-L", socket_name, "list-panes", "-t", target, "-F",
             "#{session_id}|#{session_created}|#{window_id}|#{pane_id}|#{pane_pid}"],
            capture_output=True, text=True, timeout=5, check=False)
    except (OSError, subprocess.TimeoutExpired):
        refuse(43)
    lines = [line for line in result.stdout.splitlines() if line]
    if result.returncode != 0 or result.stderr or len(lines) != 1:
        refuse(44)
    parts = lines[0].split("|")
    if len(parts) != 5:
        refuse(45)
    try:
        pane_pid = int(parts[4])
        pane = proc_stat(pane_pid)
        foreground_pid = pane["tpgid"]
        foreground = proc_stat(foreground_pid)
        identity = {
            "tmux_session_id": prefixed(parts[0], "$"),
            "tmux_session_created": int(parts[1]),
            "tmux_window_id": prefixed(parts[2], "@"),
            "tmux_pane_id": prefixed(parts[3], "%"),
            "pane_pid": pane_pid,
            "pane_start_ticks": pane["start"],
            "pane_process_group_id": pane["pgrp"],
            "pane_process_session_id": pane["sid"],
            "foreground_pid": foreground_pid,
            "foreground_start_ticks": foreground["start"],
            "foreground_process_group_id": foreground["pgrp"],
            "foreground_process_session_id": foreground["sid"],
        }
    except (ValueError, IndexError):
        refuse(46)
    if (identity["tmux_session_created"] <= 0 or pane_pid <= 0 or
        foreground_pid <= 0 or foreground["pgrp"] != foreground_pid or
        foreground["sid"] != pane["sid"]):
        refuse(47)
    return identity

def scan_owned_tree(root_pid, required_sid):
    owned, depths, stats, pending = [], {}, {}, [(root_pid, 0)]
    while pending:
        pid, depth = pending.pop()
        if pid in depths or len(owned) >= 512:
            refuse(48)
        item = proc_stat(pid)
        if item["sid"] != required_sid:
            refuse(49)
        depths[pid] = depth
        stats[pid] = item
        owned.append(pid)
        try:
            raw_children = open(
                f"/proc/{pid}/task/{pid}/children", "r", encoding="ascii").read()
            children = [] if not raw_children.strip() else [
                int(value) for value in raw_children.split()]
        except (FileNotFoundError, ProcessLookupError, PermissionError, ValueError):
            refuse(66)
        pending.extend((child, depth + 1) for child in children)
    return owned, depths, stats

def same_stat(left, right):
    return all(left[key] == right[key] for key in ("pid", "ppid", "pgrp", "sid", "start"))

mode, socket_name, target = sys.argv[1:4]
if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,63}", socket_name):
    refuse(50)
if not re.fullmatch(r"[A-Za-z0-9_=-]{1,128}", target):
    refuse(51)
first = observe(socket_name, target)
if mode == "observe":
    print(json.dumps(first, separators=(",", ":")))
    raise SystemExit(0)
if mode != "kill" or len(sys.argv) not in (5, 6):
    refuse(52)
try:
    expected = json.loads(sys.argv[4])
except (TypeError, ValueError):
    refuse(53)
if first != expected:
    refuse(54)

owned, depths, stats = scan_owned_tree(first["pane_pid"], first["pane_process_session_id"])
if first["foreground_pid"] not in stats:
    refuse(55)
pidfds = {}
try:
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        refuse(56)
    for pid in owned:
        try:
            pidfds[pid] = os.pidfd_open(pid, 0)
        except (OSError, PermissionError, ProcessLookupError):
            refuse(57)
    for pid in owned:
        if not same_stat(stats[pid], proc_stat(pid)):
            refuse(58)
    second_owned, _, second_stats = scan_owned_tree(
        first["pane_pid"], first["pane_process_session_id"])
    if set(second_owned) != set(owned):
        refuse(59)
    for pid in owned:
        if not same_stat(stats[pid], second_stats[pid]):
            refuse(60)
    if observe(socket_name, target) != expected:
        refuse(61)
    for pid in owned:
        try:
            signal.pidfd_send_signal(pidfds[pid], 0, None, 0)
        except (OSError, PermissionError, ProcessLookupError):
            refuse(62)
    final_owned, _, final_stats = scan_owned_tree(
        first["pane_pid"], first["pane_process_session_id"])
    if set(final_owned) != set(owned):
        refuse(63)
    for pid in owned:
        if not same_stat(stats[pid], final_stats[pid]):
            refuse(64)
    if observe(socket_name, target) != expected:
        refuse(65)
    if len(sys.argv) == 6:
        seam = sys.argv[5]
        open(seam + ".ready", "x", encoding="ascii").close()
        deadline = time.monotonic() + 5
        while not os.path.exists(seam + ".ack"):
            if time.monotonic() >= deadline:
                refuse(67)
            time.sleep(0.005)
    for pid in sorted(owned, key=lambda value: (depths[value], value), reverse=True):
        signal.pidfd_send_signal(pidfds[pid], signal.SIGKILL, None, 0)
finally:
    for descriptor in pidfds.values():
        os.close(descriptor)
"##;

const KNOWN_PYTHON_CANDIDATES: [&str; 2] = ["/usr/bin/python3", "/bin/python3"];

#[cfg(unix)]
fn trusted_python_identity() -> Result<ManagedExecutableIdentity, TmuxError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    for candidate in KNOWN_PYTHON_CANDIDATES {
        let Ok(path) = std::fs::canonicalize(candidate) else {
            continue;
        };
        let allowed = KNOWN_PYTHON_CANDIDATES
            .iter()
            .any(|candidate| std::fs::canonicalize(candidate).is_ok_and(|known| known == path));
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if allowed
            && path.is_absolute()
            && metadata.is_file()
            && metadata.uid() == 0
            && metadata.permissions().mode() & 0o022 == 0
            && metadata.permissions().mode() & 0o100 != 0
        {
            return Ok(ManagedExecutableIdentity {
                path: path.to_string_lossy().into_owned(),
                device: metadata.dev(),
                inode: metadata.ino(),
            });
        }
    }
    Err(TmuxError {
        op: "trusted-python",
        code: None,
        message: "trusted absolute Python interpreter is unavailable".into(),
    })
}

#[cfg(unix)]
fn revalidate_python_identity(expected: &ManagedExecutableIdentity) -> Result<(), TmuxError> {
    let actual = trusted_python_identity()?;
    if &actual == expected {
        Ok(())
    } else {
        Err(TmuxError {
            op: "trusted-python",
            code: None,
            message: "trusted Python interpreter identity changed".into(),
        })
    }
}

#[cfg(windows)]
fn trusted_wsl_path() -> Result<std::path::PathBuf, TmuxError> {
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = vec![0u16; 32_768];
    let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(TmuxError {
            op: "trusted-wsl",
            code: None,
            message: "Windows system directory is unavailable".into(),
        });
    }
    let system_directory = std::path::PathBuf::from(String::from_utf16_lossy(&buffer[..length]));
    let canonical_system = std::fs::canonicalize(&system_directory).map_err(|error| TmuxError {
        op: "trusted-wsl",
        code: None,
        message: format!("Windows system directory could not be validated: {error}"),
    })?;
    let wsl =
        std::fs::canonicalize(system_directory.join("wsl.exe")).map_err(|error| TmuxError {
            op: "trusted-wsl",
            code: None,
            message: format!("Windows WSL host executable could not be validated: {error}"),
        })?;
    let metadata = std::fs::metadata(&wsl).map_err(|error| TmuxError {
        op: "trusted-wsl",
        code: None,
        message: format!("Windows WSL host executable metadata is unavailable: {error}"),
    })?;
    let parent_matches = wsl.parent().is_some_and(|parent| {
        parent
            .to_string_lossy()
            .eq_ignore_ascii_case(&canonical_system.to_string_lossy())
    });
    let name_matches = wsl
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("wsl.exe"));
    if !metadata.is_file() || !parent_matches || !name_matches {
        return Err(TmuxError {
            op: "trusted-wsl",
            code: None,
            message: "Windows WSL host executable is outside the system directory".into(),
        });
    }
    Ok(wsl)
}

#[cfg(windows)]
fn trusted_python_identity() -> Result<ManagedExecutableIdentity, TmuxError> {
    let wsl = trusted_wsl_path()?;
    let script = r#"import json, os, stat, sys
for candidate in ('/usr/bin/python3', '/bin/python3'):
    canonical = os.path.realpath(candidate)
    try:
        details = os.stat(canonical, follow_symlinks=False)
    except OSError:
        continue
    if (canonical.startswith('/') and stat.S_ISREG(details.st_mode) and details.st_uid == 0
        and not details.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
        and details.st_mode & stat.S_IXUSR):
        print(json.dumps({'path': canonical, 'device': details.st_dev, 'inode': details.st_ino}, separators=(',', ':')))
        raise SystemExit(0)
raise SystemExit(1)"#;
    let command = windows_helper_command(&wsl, "/usr/bin/python3", script, None);
    let output =
        output_with_timeout_and_limit(command, tmux_cmd_timeout(), MANAGED_HELPER_OUTPUT_LIMIT)
            .map_err(|error| TmuxError {
                op: "trusted-python",
                code: None,
                message: format!("trusted Python interpreter resolution failed: {error}"),
            })?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(TmuxError {
            op: "trusted-python",
            code: output.status.code(),
            message: "trusted absolute Python interpreter is unavailable".into(),
        });
    }
    serde_json::from_slice(&output.stdout).map_err(|_| TmuxError {
        op: "trusted-python",
        code: None,
        message: "trusted Python interpreter identity was malformed".into(),
    })
}

#[cfg(windows)]
fn revalidate_python_identity(expected: &ManagedExecutableIdentity) -> Result<(), TmuxError> {
    let actual = trusted_python_identity()?;
    if &actual == expected {
        Ok(())
    } else {
        Err(TmuxError {
            op: "trusted-python",
            code: None,
            message: "trusted Python interpreter identity changed".into(),
        })
    }
}

#[cfg(any(windows, test))]
const WINDOWS_HELPER_CREATION_FLAGS: u32 = 0x0800_0000; // CREATE_NO_WINDOW

#[cfg(any(windows, test))]
fn apply_windows_helper_process_policy(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(WINDOWS_HELPER_CREATION_FLAGS);
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(any(windows, test))]
fn windows_helper_command(
    wsl: &std::path::Path,
    python: &str,
    script: &str,
    identity: Option<&ManagedExecutableIdentity>,
) -> Command {
    let mut command = Command::new(wsl);
    command
        .arg("--cd")
        .arg("~")
        .arg("-e")
        .arg(python)
        .arg("-c")
        .arg(script);
    if let Some(identity) = identity {
        command.arg(serde_json::to_string(identity).expect("executable identity serializes"));
    }
    // Keep the policy in this lowest-level constructor. trusted_python_identity
    // uses it directly, before the higher-level exact/cgroup helpers exist, and
    // Cortana reconciliation runs that probe immediately and every 30 seconds.
    apply_windows_helper_process_policy(&mut command);
    command
}

fn exact_effect_command() -> Result<Command, TmuxError> {
    let python = trusted_python_identity()?;
    exact_effect_command_with_python(&python)
}

fn exact_effect_command_with_python(
    python: &ManagedExecutableIdentity,
) -> Result<Command, TmuxError> {
    revalidate_python_identity(python)?;
    #[cfg(windows)]
    {
        let wsl = trusted_wsl_path()?;
        Ok(windows_helper_command(
            &wsl,
            &python.path,
            EXACT_SESSION_EFFECT_PY,
            Some(python),
        ))
    }
    #[cfg(unix)]
    {
        let mut command = Command::new(&python.path);
        command
            .arg("-c")
            .arg(EXACT_SESSION_EFFECT_PY)
            .arg(serde_json::to_string(python).expect("executable identity serializes"));
        Ok(command)
    }
}

const MANAGED_CGROUP_EFFECT_PY: &str = r##"
import json, os, re, resource, stat, subprocess, sys, tempfile, time

def refuse(code):
    raise SystemExit(code)

def bounded_read(path):
    try:
        with open(path, "r", encoding="ascii") as source:
            value = source.read(4097)
    except (FileNotFoundError, PermissionError, OSError):
        refuse(80)
    if len(value) > 4096:
        refuse(81)
    return value

def proc_start(pid):
    try:
        raw = bounded_read(f"/proc/{pid}/stat")
        fields = raw.rsplit(") ", 1)[1].split()
        return int(fields[19])
    except (ValueError, IndexError):
        refuse(82)

KNOWN_SYSTEMCTL = ("/usr/bin/systemctl", "/bin/systemctl")
KNOWN_SYSTEMD_RUN = ("/usr/bin/systemd-run", "/bin/systemd-run")
KNOWN_PYTHON = ("/usr/bin/python3", "/bin/python3")

def executable_identity(path, candidates):
    if not isinstance(path, str) or not path.startswith("/") or path.startswith("//"):
        refuse(104)
    canonical_candidates = {os.path.realpath(candidate) for candidate in candidates}
    canonical = os.path.realpath(path)
    if canonical not in canonical_candidates or path != canonical:
        refuse(105)
    try:
        details = os.stat(canonical, follow_symlinks=False)
    except OSError:
        refuse(106)
    if (not stat.S_ISREG(details.st_mode) or details.st_uid != 0 or
        details.st_mode & (stat.S_IWGRP | stat.S_IWOTH) or
        not details.st_mode & stat.S_IXUSR):
        refuse(107)
    return {"path": canonical, "device": details.st_dev, "inode": details.st_ino}

def resolve_tool(candidates):
    for candidate in candidates:
        canonical = os.path.realpath(candidate)
        if os.path.exists(canonical):
            return executable_identity(canonical, candidates)
    refuse(108)

def validate_tools(raw):
    if not isinstance(raw, dict) or set(raw) != {"python", "systemctl", "systemdRun"}:
        refuse(109)
    python = executable_identity(raw["python"].get("path"), KNOWN_PYTHON)
    systemctl = executable_identity(raw["systemctl"].get("path"), KNOWN_SYSTEMCTL)
    systemd_run = executable_identity(raw["systemdRun"].get("path"), KNOWN_SYSTEMD_RUN)
    if (python != raw["python"] or systemctl != raw["systemctl"] or
        systemd_run != raw["systemdRun"]):
        refuse(110)
    return {"python": python, "systemctl": systemctl, "systemdRun": systemd_run}

def bounded_systemctl(systemctl, unit, include_load_state=False):
    def limit_output():
        resource.setrlimit(resource.RLIMIT_FSIZE, (65536, 65536))
    stdout = tempfile.TemporaryFile()
    stderr = tempfile.TemporaryFile()
    try:
        arguments = [systemctl, "--user", "show", "--no-pager", unit,
                     "--property=Id", "--property=InvocationID", "--property=ControlGroup",
                     "--property=ActiveState", "--property=SubState"]
        if include_load_state:
            arguments.append("--property=LoadState")
        result = subprocess.run(
            arguments,
            stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr,
            timeout=5, check=False, preexec_fn=limit_output)
    except (OSError, subprocess.TimeoutExpired):
        refuse(83)
    stdout.seek(0)
    stderr.seek(0)
    stdout_raw = stdout.read(65537)
    stderr_raw = stderr.read(65537)
    if (result.returncode != 0 or stderr_raw or len(stdout_raw) > 65536 or
        len(stderr_raw) > 65536):
        refuse(84)
    try:
        return stdout_raw.decode("utf-8")
    except UnicodeDecodeError:
        refuse(84)

def systemd_properties(systemctl, unit, include_load_state=False):
    output = bounded_systemctl(systemctl, unit, include_load_state)
    props = {}
    for line in output.splitlines():
        key, separator, value = line.partition("=")
        if not separator or key in props:
            refuse(85)
        props[key] = value
    expected = {"Id", "InvocationID", "ControlGroup", "ActiveState", "SubState"}
    if include_load_state:
        expected.add("LoadState")
    if set(props) != expected:
        refuse(86)
    return props

def prepared_converged(unit, path, props, directory_exists):
    required = {"Id", "InvocationID", "ControlGroup", "ActiveState", "SubState", "LoadState"}
    if (not isinstance(props, dict) or set(props) != required or
        props["Id"] != unit or directory_exists or
        props["ControlGroup"] not in ("", path)):
        return False
    if props["LoadState"] == "not-found":
        return (props["ActiveState"] == "inactive" and props["SubState"] == "dead" and
                props["InvocationID"] == "" and props["ControlGroup"] == "")
    if props["LoadState"] != "loaded":
        return False
    return (props["ActiveState"] == "inactive" and props["SubState"] == "dead" and
            (props["InvocationID"] == "" or
             re.fullmatch(r"[0-9a-f]{32}", props["InvocationID"]) is not None))

def exact_cgroup_path(unit, path):
    uid = os.getuid()
    expected = f"/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/{unit}"
    return path == expected and os.path.basename(path) == unit

def observe(tools, unit, nonce, pid):
    if not re.fullmatch(r"t-hub-[0-9a-f]{32}\.scope", unit):
        refuse(87)
    if not re.fullmatch(r"[0-9a-f]{32}", nonce) or pid <= 0:
        refuse(88)
    props = systemd_properties(tools["systemctl"]["path"], unit)
    path = props["ControlGroup"]
    if (props["Id"] != unit or props["ActiveState"] != "active" or
        props["SubState"] != "running" or
        not re.fullmatch(r"[0-9a-f]{32}", props["InvocationID"]) or
        not exact_cgroup_path(unit, path) or ".." in path.split("/") or "\x00" in path):
        refuse(89)
    directory = "/sys/fs/cgroup" + path
    try:
        descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        inode = os.fstat(descriptor).st_ino
        for control in ("cgroup.freeze", "cgroup.kill", "cgroup.events"):
            control_details = os.stat(control, dir_fd=descriptor, follow_symlinks=False)
            if not stat.S_ISREG(control_details.st_mode):
                refuse(90)
        os.close(descriptor)
    except (FileNotFoundError, PermissionError, OSError):
        refuse(90)
    membership = [line for line in bounded_read(f"/proc/{pid}/cgroup").splitlines() if line]
    if membership != ["0::" + path]:
        refuse(91)
    try:
        environ = open(f"/proc/{pid}/environ", "rb").read(131073)
    except (FileNotFoundError, PermissionError, OSError):
        refuse(92)
    if len(environ) > 131072 or (b"T_HUB_LAUNCH_NONCE=" + nonce.encode() + b"\0") not in environ:
        refuse(93)
    return {
        "unitName": unit,
        "invocationId": props["InvocationID"],
        "cgroupPath": path,
        "cgroupInode": inode,
        "launcherPid": pid,
        "launcherStartTicks": proc_start(pid),
        "launchNonce": nonce,
        "tools": tools,
    }

def static_owner(expected):
    tools = validate_tools(expected.get("tools"))
    unit = expected.get("unitName")
    path = expected.get("cgroupPath")
    if (not re.fullmatch(r"t-hub-[0-9a-f]{32}\.scope", unit or "") or
        expected.get("launchNonce") != unit[6:-6] or
        not exact_cgroup_path(unit, path or "") or
        not re.fullmatch(r"[0-9a-f]{32}", expected.get("invocationId") or "") or
        not isinstance(expected.get("cgroupInode"), int) or expected["cgroupInode"] <= 0):
        refuse(113)
    props = systemd_properties(tools["systemctl"]["path"], unit)
    directory_path = "/sys/fs/cgroup" + path
    if props["ActiveState"] != "active":
        if (props["Id"] == unit and props["InvocationID"] in ("", expected["invocationId"]) and
            props["ControlGroup"] in ("", path) and not os.path.exists(directory_path)):
            return tools, None
        refuse(114)
    if (props["Id"] != unit or props["SubState"] != "running" or
        props["InvocationID"] != expected["invocationId"] or
        props["ControlGroup"] != path):
        refuse(115)
    directory = None
    try:
        directory = os.open(directory_path, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        if os.fstat(directory).st_ino != expected["cgroupInode"]:
            refuse(116)
        for control in ("cgroup.freeze", "cgroup.kill", "cgroup.events"):
            if not stat.S_ISREG(os.stat(control, dir_fd=directory, follow_symlinks=False).st_mode):
                refuse(117)
    except (FileNotFoundError, PermissionError, OSError):
        refuse(118)
    return tools, directory

def event_value(directory, key, missing_is_zero=False):
    try:
        descriptor = os.open("cgroup.events", os.O_RDONLY | os.O_CLOEXEC, dir_fd=directory)
        raw = os.read(descriptor, 4097)
        os.close(descriptor)
    except OSError:
        if missing_is_zero:
            return 0
        refuse(94)
    if len(raw) > 4096:
        refuse(95)
    values = {}
    try:
        for line in raw.decode("ascii").splitlines():
            name, value = line.split()
            if name in values:
                refuse(96)
            values[name] = int(value)
    except (UnicodeDecodeError, ValueError):
        refuse(97)
    if key not in values:
        refuse(98)
    return values[key]

def write_control(directory, name, value):
    try:
        descriptor = os.open(name, os.O_WRONLY | os.O_CLOEXEC, dir_fd=directory)
        os.write(descriptor, value)
        os.close(descriptor)
    except (FileNotFoundError, PermissionError, OSError):
        refuse(99)

try:
    expected_python = json.loads(sys.argv[1])
except (IndexError, TypeError, ValueError, json.JSONDecodeError):
    refuse(126)
actual_python = executable_identity(os.path.realpath(sys.executable), KNOWN_PYTHON)
if actual_python != expected_python:
    refuse(127)
sys.argv = [sys.argv[0], *sys.argv[2:]]

mode = sys.argv[1] if len(sys.argv) > 1 else ""
if mode == "tools" and len(sys.argv) == 2:
    print(json.dumps({"python": actual_python,
                      "systemctl": resolve_tool(KNOWN_SYSTEMCTL),
                      "systemdRun": resolve_tool(KNOWN_SYSTEMD_RUN)}, separators=(",", ":")))
    raise SystemExit(0)
if mode == "validate-tools" and len(sys.argv) == 3:
    validate_tools(json.loads(sys.argv[2]))
    raise SystemExit(0)
if mode == "validate-path" and len(sys.argv) == 4:
    if not re.fullmatch(r"t-hub-[0-9a-f]{32}\.scope", sys.argv[2]):
        refuse(111)
    if not exact_cgroup_path(sys.argv[2], sys.argv[3]):
        refuse(112)
    raise SystemExit(0)
if mode == "preflight" and len(sys.argv) == 3:
    tools = validate_tools(json.loads(sys.argv[2]))
    if not os.path.exists("/sys/fs/cgroup/cgroup.controllers"):
        refuse(70)
    current = bounded_read("/proc/self/cgroup").strip()
    if not current.startswith("0::/"):
        refuse(71)
    directory = "/sys/fs/cgroup" + current[3:]
    for name in ("cgroup.freeze", "cgroup.kill", "cgroup.events"):
        if not os.path.exists(os.path.join(directory, name)):
            refuse(72)
    props = systemd_properties(tools["systemctl"]["path"], "app.slice")
    if props["ActiveState"] != "active":
        refuse(73)
    raise SystemExit(0)
if mode == "validate-prepared-state" and len(sys.argv) == 5:
    unit = sys.argv[2]
    if (not re.fullmatch(r"t-hub-[0-9a-f]{32}\.scope", unit) or
        sys.argv[4] not in ("0", "1")):
        refuse(128)
    path = f"/user.slice/user-{os.getuid()}.slice/user@{os.getuid()}.service/app.slice/{unit}"
    if prepared_converged(unit, path, json.loads(sys.argv[3]), sys.argv[4] == "1"):
        raise SystemExit(0)
    refuse(129)
if mode == "retire-prepared" and len(sys.argv) == 3:
    prepared = json.loads(sys.argv[2])
    tools = validate_tools(prepared.get("tools"))
    unit = prepared.get("unitName")
    nonce = prepared.get("launchNonce")
    if (not re.fullmatch(r"t-hub-[0-9a-f]{32}\.scope", unit or "") or
        nonce != unit[6:-6]):
        refuse(119)
    path = f"/user.slice/user-{os.getuid()}.slice/user@{os.getuid()}.service/app.slice/{unit}"
    props = systemd_properties(tools["systemctl"]["path"], unit, True)
    directory_path = "/sys/fs/cgroup" + path
    if prepared_converged(unit, path, props, os.path.exists(directory_path)):
        raise SystemExit(0)
    refuse(120)
if mode == "observe" and len(sys.argv) == 6:
    tools = validate_tools(json.loads(sys.argv[2]))
    print(json.dumps(observe(tools, sys.argv[3], sys.argv[4], int(sys.argv[5])), separators=(",", ":")))
    raise SystemExit(0)
if mode not in ("retire", "retire-gone") or len(sys.argv) not in (3, 5):
    refuse(74)
crash_stage = sys.argv[3] if len(sys.argv) == 5 else ""
crash_marker = sys.argv[4] if len(sys.argv) == 5 else ""
try:
    expected = json.loads(sys.argv[2])
    tools = validate_tools(expected["tools"])
    if mode == "retire":
        actual = observe(tools, expected["unitName"], expected["launchNonce"], expected["launcherPid"])
        directory = None
    else:
        tools, directory = static_owner(expected)
        if directory is None:
            raise SystemExit(0)
        actual = expected
except (KeyError, TypeError, ValueError):
    refuse(75)
for key in ("unitName", "invocationId", "cgroupPath", "cgroupInode",
            "launcherPid", "launcherStartTicks", "launchNonce", "tools"):
    if actual[key] != expected.get(key):
        refuse(76)
directory_path = "/sys/fs/cgroup" + actual["cgroupPath"]
if directory is None:
    try:
        directory = os.open(directory_path, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    except (FileNotFoundError, PermissionError, OSError):
        refuse(77)
try:
    if os.fstat(directory).st_ino != actual["cgroupInode"]:
        refuse(78)
    second = systemd_properties(tools["systemctl"]["path"], actual["unitName"])
    if (second["InvocationID"] != actual["invocationId"] or
        second["ControlGroup"] != actual["cgroupPath"]):
        refuse(79)
    write_control(directory, "cgroup.freeze", b"1")
    deadline = time.monotonic() + 5
    while event_value(directory, "frozen") != 1:
        if time.monotonic() >= deadline:
            refuse(100)
        time.sleep(0.005)
    if crash_stage == "freeze":
        open(crash_marker, "x", encoding="ascii").close()
        refuse(102)
    write_control(directory, "cgroup.kill", b"1")
    deadline = time.monotonic() + 5
    while event_value(directory, "populated", True) != 0:
        if time.monotonic() >= deadline:
            refuse(101)
        time.sleep(0.005)
    if crash_stage == "kill":
        open(crash_marker, "x", encoding="ascii").close()
        refuse(103)
finally:
    os.close(directory)
"##;

#[cfg(any(windows, test))]
const WINDOWS_MANAGED_CGROUP_HELPERS_PER_EFFECT: usize = 1;

#[cfg(any(windows, test))]
fn windows_managed_cgroup_effect_command(
    wsl: &std::path::Path,
    python: &ManagedExecutableIdentity,
) -> Command {
    windows_helper_command(wsl, &python.path, MANAGED_CGROUP_EFFECT_PY, Some(python))
}

fn managed_cgroup_effect_command(python: &ManagedExecutableIdentity) -> Result<Command, TmuxError> {
    #[cfg(unix)]
    revalidate_python_identity(python)?;
    #[cfg(windows)]
    {
        let wsl = trusted_wsl_path()?;
        // The helper validates sys.executable against this exact root-owned
        // path/device/inode tuple before inspecting or changing any managed
        // runtime state. Keep that validation and the cgroup operation in the
        // same WSL process so owner observation has one bounded helper.
        Ok(windows_managed_cgroup_effect_command(&wsl, python))
    }
    #[cfg(unix)]
    {
        let mut command = Command::new(&python.path);
        command
            .arg("-c")
            .arg(MANAGED_CGROUP_EFFECT_PY)
            .arg(serde_json::to_string(python).expect("executable identity serializes"));
        Ok(command)
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn managed_runtime_preflight() -> Result<(), TmuxError> {
    let tools = resolve_managed_system_tools()?;
    managed_runtime_preflight_with_tools(&tools)
}

fn resolve_managed_system_tools() -> Result<ManagedSystemTools, TmuxError> {
    let python = trusted_python_identity()?;
    let mut command = managed_cgroup_effect_command(&python)?;
    command.arg("tools");
    let output =
        output_with_timeout_and_limit(command, tmux_cmd_timeout(), MANAGED_HELPER_OUTPUT_LIMIT)
            .map_err(|error| TmuxError {
                op: "managed-runtime-tools",
                code: None,
                message: format!("managed system helper resolution failed: {error}"),
            })?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(TmuxError {
            op: "managed-runtime-tools",
            code: output.status.code(),
            message: "trusted absolute systemd helpers are unavailable".into(),
        });
    }
    serde_json::from_slice(&output.stdout).map_err(|_| TmuxError {
        op: "managed-runtime-tools",
        code: None,
        message: "managed system helper identity was malformed".into(),
    })
}

fn managed_runtime_preflight_with_tools(tools: &ManagedSystemTools) -> Result<(), TmuxError> {
    let mut command = managed_cgroup_effect_command(&tools.python)?;
    let encoded = serde_json::to_string(tools).map_err(|_| TmuxError {
        op: "managed-runtime-preflight",
        code: None,
        message: "managed system helper identity could not be encoded".into(),
    })?;
    command.args(["preflight", &encoded]);
    let output =
        output_with_timeout_and_limit(command, tmux_cmd_timeout(), MANAGED_HELPER_OUTPUT_LIMIT)
            .map_err(|error| TmuxError {
                op: "managed-runtime-preflight",
                code: None,
                message: format!("managed runtime ownership probe failed: {error}"),
            })?;
    if output.status.success() && output.stderr.is_empty() {
        Ok(())
    } else {
        Err(TmuxError {
            op: "managed-runtime-preflight",
            code: output.status.code(),
            message: "user systemd with delegated cgroup-v2 freeze and kill is unavailable".into(),
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManagedRuntimeObservation {
    unit_name: String,
    invocation_id: String,
    cgroup_path: String,
    cgroup_inode: u64,
    launcher_pid: u32,
    launcher_start_ticks: u64,
    launch_nonce: String,
    tools: ManagedSystemTools,
}

fn observe_managed_runtime_owner(
    tools: &ManagedSystemTools,
    unit_name: &str,
    launch_nonce: &str,
    tmux: SessionEffectIdentity,
) -> Result<ManagedRuntimeOwnerToken, TmuxError> {
    let mut command = managed_cgroup_effect_command(&tools.python)?;
    let encoded_tools = serde_json::to_string(tools).map_err(|_| TmuxError {
        op: "observe-managed-runtime-owner",
        code: None,
        message: "managed system helper identity could not be encoded".into(),
    })?;
    command.args([
        "observe",
        &encoded_tools,
        unit_name,
        launch_nonce,
        &tmux.pane_pid.to_string(),
    ]);
    let output =
        output_with_timeout_and_limit(command, tmux_cmd_timeout(), MANAGED_HELPER_OUTPUT_LIMIT)
            .map_err(|error| TmuxError {
                op: "observe-managed-runtime-owner",
                code: None,
                message: format!("managed runtime ownership inspection failed: {error}"),
            })?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(TmuxError {
            op: "observe-managed-runtime-owner",
            code: output.status.code(),
            message: "systemd, cgroup, process, nonce, and tmux ownership did not agree".into(),
        });
    }
    let observed: ManagedRuntimeObservation =
        serde_json::from_slice(&output.stdout).map_err(|_| TmuxError {
            op: "observe-managed-runtime-owner",
            code: None,
            message: "managed runtime ownership observation was malformed".into(),
        })?;
    Ok(ManagedRuntimeOwnerToken {
        version: MANAGED_RUNTIME_OWNER_VERSION,
        unit_name: observed.unit_name,
        invocation_id: observed.invocation_id,
        cgroup_path: observed.cgroup_path,
        cgroup_inode: observed.cgroup_inode,
        launcher_pid: observed.launcher_pid,
        launcher_start_ticks: observed.launcher_start_ticks,
        launch_nonce: observed.launch_nonce,
        tools: observed.tools,
        tmux,
    })
}

pub(crate) fn observe_prepared_managed_runtime_owner(
    name: &str,
    launch: &ManagedRuntimeLaunchSpec,
) -> Result<ManagedRuntimeOwnerToken, TmuxError> {
    let tmux = observe_session_effect_identity(name)?;
    observe_managed_runtime_owner(&launch.tools, &launch.unit_name, &launch.launch_nonce, tmux)
}

pub(crate) fn retire_prepared_managed_runtime(
    launch: &ManagedRuntimeLaunchSpec,
) -> Result<(), TmuxError> {
    let encoded = serde_json::to_string(launch).map_err(|_| TmuxError {
        op: "retire-prepared-managed-runtime",
        code: None,
        message: "prepared managed runtime identity could not be encoded".into(),
    })?;
    let mut command = managed_cgroup_effect_command(&launch.tools.python)?;
    command.args(["retire-prepared", &encoded]);
    let output =
        output_with_timeout_and_limit(command, tmux_cmd_timeout(), MANAGED_HELPER_OUTPUT_LIMIT)
            .map_err(|error| TmuxError {
                op: "retire-prepared-managed-runtime",
                code: None,
                message: format!("prepared managed runtime cleanup failed: {error}"),
            })?;
    if output.status.success() && output.stderr.is_empty() {
        Ok(())
    } else {
        Err(TmuxError {
            op: "retire-prepared-managed-runtime",
            code: output.status.code(),
            message: "prepared managed unit was populated, reused, or unverifiable".into(),
        })
    }
}

pub(crate) fn revalidate_managed_runtime_owner(
    name: &str,
    owner: &ManagedRuntimeOwnerToken,
) -> Result<(), TmuxError> {
    if owner.version != MANAGED_RUNTIME_OWNER_VERSION
        || !same_managed_tmux_generation(&observe_session_effect_identity(name)?, &owner.tmux)
    {
        return Err(TmuxError {
            op: "revalidate-managed-runtime-owner",
            code: None,
            message: "managed tmux generation changed".into(),
        });
    }
    let observed = observe_managed_runtime_owner(
        &owner.tools,
        &owner.unit_name,
        &owner.launch_nonce,
        owner.tmux,
    )?;
    if &observed == owner {
        Ok(())
    } else {
        Err(TmuxError {
            op: "revalidate-managed-runtime-owner",
            code: None,
            message: "managed systemd or cgroup owner generation changed".into(),
        })
    }
}

fn same_managed_tmux_generation(
    observed: &SessionEffectIdentity,
    expected: &SessionEffectIdentity,
) -> bool {
    observed.tmux_session_id == expected.tmux_session_id
        && observed.tmux_session_created == expected.tmux_session_created
        && observed.tmux_window_id == expected.tmux_window_id
        && observed.tmux_pane_id == expected.tmux_pane_id
        && observed.pane_pid == expected.pane_pid
        && observed.pane_start_ticks == expected.pane_start_ticks
}

pub(crate) fn observe_session_effect_identity(
    name: &str,
) -> Result<SessionEffectIdentity, TmuxError> {
    exact_effect_target(name)?;
    let socket = validated_socket_name()?;
    let mut command = exact_effect_command()?;
    command.args(["observe", socket, name]);
    let output =
        output_with_timeout_and_limit(command, tmux_cmd_timeout(), MANAGED_HELPER_OUTPUT_LIMIT)
            .map_err(|error| TmuxError {
                op: "observe-session-effect",
                code: None,
                message: format!("failed to inspect exact session effect identity: {error}"),
            })?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(TmuxError {
            op: "observe-session-effect",
            code: output.status.code(),
            message: "exact session effect identity is unavailable or ambiguous".into(),
        });
    }
    serde_json::from_slice(&output.stdout).map_err(|_| TmuxError {
        op: "observe-session-effect",
        code: None,
        message: "exact session effect identity is malformed".into(),
    })
}

#[cfg(test)]
type ExactEffectHook = Box<dyn FnOnce() + Send>;

#[cfg(test)]
static BEFORE_EXACT_EFFECT_HOOK: LazyLock<std::sync::Mutex<Option<(String, ExactEffectHook)>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

#[cfg(test)]
static AFTER_FINAL_SCAN_SEAM: LazyLock<std::sync::Mutex<Option<(String, String)>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

#[cfg(test)]
static MANAGED_RETIRE_CRASH: LazyLock<std::sync::Mutex<Option<(String, &'static str, String)>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

#[cfg(test)]
pub(crate) fn set_before_exact_effect_hook(target: &str, hook: ExactEffectHook) {
    *BEFORE_EXACT_EFFECT_HOOK.lock().unwrap() = Some((target.to_string(), hook));
}

#[cfg(test)]
fn set_after_final_scan_seam(target: &str, path: &std::path::Path) {
    *AFTER_FINAL_SCAN_SEAM.lock().unwrap() =
        Some((target.to_string(), path.to_string_lossy().into_owned()));
}

#[cfg(test)]
fn set_managed_retire_crash(target: &str, stage: &'static str, marker: &std::path::Path) {
    assert!(matches!(stage, "freeze" | "kill"));
    *MANAGED_RETIRE_CRASH.lock().unwrap() = Some((
        target.to_string(),
        stage,
        marker.to_string_lossy().into_owned(),
    ));
}

/// Retire only the exact process generation recorded before the durable prepare.
///
/// This intentionally does not call `tmux kill-session`: a reused session name or
/// pane may now own a different process. The in-effect identity check precedes
/// signals, and the caller must separately prove that the original session is gone.
pub(crate) fn kill_session_tree_exact(
    name: &str,
    expected: SessionEffectIdentity,
) -> Result<(), TmuxError> {
    let observed = observe_session_effect_identity(name)?;
    if observed != expected {
        return Err(TmuxError {
            op: "kill-session-tree-exact",
            code: None,
            message: "exact session effect identity changed before retirement".into(),
        });
    }
    #[cfg(test)]
    let hook = {
        let mut pending = BEFORE_EXACT_EFFECT_HOOK.lock().unwrap();
        (pending.as_ref().map(|(target, _)| target.as_str()) == Some(name))
            .then(|| pending.take().expect("matching exact effect hook").1)
    };
    #[cfg(test)]
    if let Some(hook) = hook {
        hook();
    }
    let socket = validated_socket_name()?;
    let encoded = serde_json::to_string(&expected).map_err(|_| TmuxError {
        op: "kill-session-tree-exact",
        code: None,
        message: "exact session effect identity could not be encoded".into(),
    })?;
    let mut command = exact_effect_command()?;
    command.args(["kill", socket, name, &encoded]);
    #[cfg(test)]
    {
        let mut pending = AFTER_FINAL_SCAN_SEAM.lock().unwrap();
        let seam = (pending.as_ref().map(|(target, _)| target.as_str()) == Some(name))
            .then(|| pending.take().expect("matching final scan seam"));
        if let Some((target, path)) = seam {
            debug_assert_eq!(target, name);
            command.arg(path);
        }
    }
    let output = output_with_timeout(command, tmux_cmd_timeout()).map_err(|error| TmuxError {
        op: "kill-session-tree-exact",
        code: None,
        message: format!("failed to retire exact session process identity: {error}"),
    })?;
    if output.status.success() && output.stderr.is_empty() {
        Ok(())
    } else {
        Err(TmuxError {
            op: "kill-session-tree-exact",
            code: output.status.code(),
            message: "exact session effect identity changed during retirement".into(),
        })
    }
}

/// Retire exactly one published managed runtime owner.
///
/// The cgroup is pinned and revalidated inside the WSL-side effect process,
/// frozen to close the fork race, killed through cgroup-v2, and observed empty.
/// A crash after kill is idempotent once the exact tmux generation is gone.
pub(crate) fn retire_managed_runtime(
    name: &str,
    owner: &ManagedRuntimeOwnerToken,
) -> Result<(), TmuxError> {
    if owner.version != MANAGED_RUNTIME_OWNER_VERSION {
        return Err(TmuxError {
            op: "retire-managed-runtime",
            code: None,
            message: "managed runtime owner token version is unsupported".into(),
        });
    }
    let tmux_liveness = session_liveness(name);
    match tmux_liveness {
        SessionLiveness::Gone => {}
        SessionLiveness::Unknown => {
            return Err(TmuxError {
                op: "retire-managed-runtime",
                code: None,
                message: "tmux generation liveness is indeterminate before retirement".into(),
            });
        }
        SessionLiveness::Alive => {}
    }
    if tmux_liveness == SessionLiveness::Alive
        && !same_managed_tmux_generation(&observe_session_effect_identity(name)?, &owner.tmux)
    {
        return Err(TmuxError {
            op: "retire-managed-runtime",
            code: None,
            message: "tmux generation changed before managed retirement".into(),
        });
    }
    let encoded = serde_json::to_string(owner).map_err(|_| TmuxError {
        op: "retire-managed-runtime",
        code: None,
        message: "managed runtime owner token could not be encoded".into(),
    })?;
    let mut command = managed_cgroup_effect_command(&owner.tools.python)?;
    command.args([
        if tmux_liveness == SessionLiveness::Gone {
            "retire-gone"
        } else {
            "retire"
        },
        &encoded,
    ]);
    #[cfg(test)]
    {
        let mut pending = MANAGED_RETIRE_CRASH.lock().unwrap();
        let crash = (pending.as_ref().map(|(target, _, _)| target.as_str()) == Some(name))
            .then(|| pending.take().expect("matching managed retirement crash"));
        if let Some((target, stage, marker)) = crash {
            debug_assert_eq!(target, name);
            command.arg(stage).arg(marker);
        }
    }
    let output =
        output_with_timeout_and_limit(command, tmux_cmd_timeout(), MANAGED_HELPER_OUTPUT_LIMIT)
            .map_err(|error| TmuxError {
                op: "retire-managed-runtime",
                code: None,
                message: format!("managed cgroup retirement failed: {error}"),
            })?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(TmuxError {
            op: "retire-managed-runtime",
            code: output.status.code(),
            message: "managed owner changed or cgroup freeze/kill did not complete".into(),
        });
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        match session_liveness(name) {
            SessionLiveness::Gone => return Ok(()),
            SessionLiveness::Unknown => break,
            SessionLiveness::Alive => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    Err(TmuxError {
        op: "retire-managed-runtime",
        code: None,
        message: "managed cgroup emptied but the exact tmux generation did not retire".into(),
    })
}

/// Best-effort retirement for a legacy pane through enumerated pidfds.
///
/// This is not a complete subtree guarantee: a process can fork after the final
/// scan. New managed runtimes must use [`retire_managed_runtime`]. An already-gone
/// legacy session remains idempotent success.
pub fn kill_session_tree(name: &str) -> Result<(), TmuxError> {
    let mut last_error = None;
    for _ in 0..5 {
        if session_liveness(name) == SessionLiveness::Gone {
            return Ok(());
        }
        let identity = observe_session_effect_identity(name)?;
        match kill_session_tree_exact(name, identity) {
            Ok(()) => {
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while std::time::Instant::now() < deadline {
                    match session_liveness(name) {
                        SessionLiveness::Gone => return Ok(()),
                        SessionLiveness::Unknown => break,
                        SessionLiveness::Alive => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
                last_error = Some(TmuxError {
                    op: "kill-session-tree",
                    code: None,
                    message: "verified pane tree remained alive after pidfd retirement".into(),
                });
                break;
            }
            Err(error) => {
                last_error = Some(error);
                if session_liveness(name) != SessionLiveness::Alive {
                    break;
                }
                std::thread::yield_now();
            }
        }
    }
    match session_liveness(name) {
        SessionLiveness::Gone => Ok(()),
        SessionLiveness::Alive => Err(last_error.unwrap_or(TmuxError {
            op: "kill-session-tree",
            code: None,
            message: "verified pane tree remained alive after pidfd retirement".into(),
        })),
        SessionLiveness::Unknown => Err(TmuxError {
            op: "kill-session-tree",
            code: None,
            message: "pane liveness is indeterminate after pidfd retirement".into(),
        }),
    }
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
        output_with_timeout(tmux(&["list-sessions"])?, tmux_cmd_timeout()).map_err(|e| {
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

/// One exact Codex rollout currently held open by a process under a T-Hub pane.
///
/// The rollout path is provider evidence, not identity by itself. History parses
/// and verifies the versioned filename before using its native conversation ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveCodexRollout {
    pub terminal_id: String,
    pub path: String,
}

/// Discover live Codex rollout files in one bounded WSL process invocation.
///
/// Codex does not currently publish its fresh interactive thread ID into tmux's
/// session environment. Its process keeps the exact rollout open, so inspecting
/// file descriptors under each pane's bounded four-level process tree provides an
/// exact runtime join without guessing from cwd, timestamps, or display text.
/// This is called only when History is requested, never from a background poll.
fn active_codex_rollouts_script() -> String {
    "set -o pipefail; socket=$1; pane_count=0; result_count=0; \
tmux -L \"$socket\" list-panes -a -F '#{session_name}|#{pane_pid}' \
| while IFS='|' read -r session root; do \
pane_count=$((pane_count + 1)); \
if [ \"$pane_count\" -gt 512 ]; then echo 'active-codex-rollouts: pane bound exceeded' >&2; exit 70; fi; \
case \"$root\" in ''|*[!0-9]*) echo 'active-codex-rollouts: invalid pane pid' >&2; exit 71;; esac; \
pids=\"$root\"; frontier=\"$root\"; process_count=1; \
for depth in 1 2 3 4; do next=\"\"; \
for parent in $frontier; do kids=$(pgrep -P \"$parent\" 2>/dev/null); rc=$?; \
if [ \"$rc\" -gt 1 ]; then echo 'active-codex-rollouts: process scan failed' >&2; exit 72; fi; \
for kid in $kids; do process_count=$((process_count + 1)); \
if [ \"$process_count\" -gt 256 ]; then echo 'active-codex-rollouts: process bound exceeded' >&2; exit 73; fi; \
next=\"$next $kid\"; done; done; pids=\"$pids $next\"; frontier=\"$next\"; done; \
for parent in $frontier; do kids=$(pgrep -P \"$parent\" 2>/dev/null); rc=$?; \
if [ \"$rc\" -gt 1 ]; then echo 'active-codex-rollouts: process depth probe failed' >&2; exit 76; fi; \
if [ \"$rc\" -eq 0 ] && [ -n \"$kids\" ]; then echo 'active-codex-rollouts: process depth bound exceeded' >&2; exit 77; fi; done; \
for pid in $pids; do [ -d \"/proc/$pid/fd\" ] || continue; fd_count=0; \
for fd in /proc/$pid/fd/*; do [ -L \"$fd\" ] || continue; fd_count=$((fd_count + 1)); \
if [ \"$fd_count\" -gt 512 ]; then echo 'active-codex-rollouts: fd bound exceeded' >&2; exit 74; fi; \
target=$(readlink \"$fd\" 2>/dev/null); rc=$?; \
if [ \"$rc\" -ne 0 ]; then [ -d \"/proc/$pid\" ] || continue; \
echo 'active-codex-rollouts: fd inspection failed' >&2; exit 75; fi; \
case \"$target\" in */.codex/sessions/*/rollout-*.jsonl) \
result_count=$((result_count + 1)); \
if [ \"$result_count\" -gt 512 ]; then echo 'active-codex-rollouts: result bound exceeded' >&2; exit 78; fi; \
printf '%s|%s\\n' \"$session\" \"$target\";; esac; done; done; done | sort -u"
        .to_string()
}

pub fn active_codex_rollouts() -> Result<Vec<ActiveCodexRollout>, TmuxError> {
    let script = active_codex_rollouts_script();
    let socket = validated_socket_name()?;
    let output = output_with_timeout(
        pane_info_command_with_args(&script, &[socket]),
        tmux_cmd_timeout(),
    )
    .map_err(|e| TmuxError {
        op: "active-codex-rollouts",
        code: None,
        message: format!("failed to inspect Codex runtime identity: {e}"),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_no_server(&stderr) || stderr.contains("error connecting to") {
            return Ok(Vec::new());
        }
        return Err(TmuxError {
            op: "active-codex-rollouts",
            code: output.status.code(),
            message: stderr.trim().to_string(),
        });
    }
    let mut rollouts = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((session, path)) = line.split_once('|') else {
            return Err(TmuxError {
                op: "active-codex-rollouts",
                code: None,
                message: "runtime scan returned a malformed identity row".into(),
            });
        };
        let terminal_id = session.trim().strip_prefix("th_").unwrap_or(session.trim());
        if terminal_id.is_empty() || path.trim().is_empty() || path.contains('|') {
            return Err(TmuxError {
                op: "active-codex-rollouts",
                code: None,
                message: "runtime scan returned an invalid identity row".into(),
            });
        }
        if rollouts.len() >= 512 {
            return Err(TmuxError {
                op: "active-codex-rollouts",
                code: None,
                message: "runtime scan result bound exceeded".into(),
            });
        }
        rollouts.push(ActiveCodexRollout {
            terminal_id: terminal_id.to_string(),
            path: path.trim().to_string(),
        });
    }
    Ok(rollouts)
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
    let socket = validated_socket_name()?;
    let script = "socket=$1; tmux -L \"$socket\" list-panes -a -F \
'#{session_name}|#{pane_current_command}|#{pane_current_path}|#{pane_pid}' \
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
printf '%s|%s|%s\\n' \"$s\" \"$eff\" \"$path\"; done";
    let output = output_with_timeout(
        pane_info_command_with_args(script, &[socket]),
        tmux_cmd_timeout(),
    )
    .map_err(|e| TmuxError {
        op: "list-panes",
        code: None,
        message: format!("failed to spawn tmux: {e}"),
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

/// Build the `bash -lc <script> <args...>` command used by [`pane_info`]. On Windows this
/// goes through `wsl.exe` (CREATE_NO_WINDOW so no console flashes); on Unix it
/// runs `bash -lc` directly. The single-quoted tmux format inside `script` is what
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
fn pane_info_command_with_args(script: &str, args: &[&str]) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("wsl.exe");
    command
        .arg("--cd")
        .arg("~")
        .arg("-e")
        .arg("bash")
        .arg("-lc")
        .arg(script)
        .arg("t-hub-script")
        .args(args);
    command.creation_flags(0x0800_0000);
    command
}

#[cfg(unix)]
fn pane_info_command_with_args(script: &str, args: &[&str]) -> Command {
    let mut command = Command::new("bash");
    command
        .arg("-lc")
        .arg(script)
        .arg("t-hub-script")
        .args(args);
    command
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

    #[cfg(unix)]
    #[test]
    fn managed_path_shims_cannot_intercept_systemd_helpers() {
        use std::os::unix::fs::PermissionsExt;

        if std::env::var_os("T_HUB_MANAGED_PATH_SHIM_HELPER").is_some() {
            let marker = std::path::PathBuf::from(
                std::env::var_os("T_HUB_MANAGED_PATH_SHIM_MARKER").unwrap(),
            );
            managed_runtime_preflight().unwrap();
            let mut exact = exact_effect_command().unwrap();
            exact.args(["observe", validated_socket_name().unwrap(), "=th_missing:"]);
            let output = output_with_timeout_and_limit(
                exact,
                Duration::from_secs(5),
                MANAGED_HELPER_OUTPUT_LIMIT,
            )
            .unwrap();
            assert!(!output.status.success());
            assert!(!marker.exists());
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let marker = fixture.path().join("intercepted");
        for helper in ["python3", "systemctl", "systemd-run"] {
            let path = fixture.path().join(helper);
            std::fs::write(
                &path,
                format!("#!/bin/sh\ntouch '{}'\nexit 91\n", marker.display()),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let original_path = std::env::var("PATH").unwrap_or_default();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tmux::tests::managed_path_shims_cannot_intercept_systemd_helpers",
                "--nocapture",
            ])
            .env("T_HUB_MANAGED_PATH_SHIM_HELPER", "1")
            .env("T_HUB_MANAGED_PATH_SHIM_MARKER", &marker)
            .env(
                "PATH",
                format!("{}:{original_path}", fixture.path().display()),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn exact_and_managed_helpers_pin_python_identity_and_reject_reuse() {
        let python = trusted_python_identity().unwrap();
        let exact = exact_effect_command_with_python(&python).unwrap();
        let managed = managed_cgroup_effect_command(&python).unwrap();
        assert_eq!(exact.get_program(), std::ffi::OsStr::new(&python.path));
        assert_eq!(managed.get_program(), std::ffi::OsStr::new(&python.path));

        let mut reused = python.clone();
        reused.inode = reused.inode.saturating_add(1);
        assert!(exact_effect_command_with_python(&reused).is_err());
        assert!(managed_cgroup_effect_command(&reused).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn windows_helper_construction_pins_tools_and_hidden_window_policy() {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(WINDOWS_HELPER_CREATION_FLAGS, 0x0800_0000);
        let fixture = tempfile::tempdir().unwrap();
        let marker = fixture.path().join("intercepted");
        for helper in ["wsl.exe", "python3"] {
            let path = fixture.path().join(helper);
            std::fs::write(
                &path,
                format!("#!/bin/sh\ntouch '{}'\nexit 91\n", marker.display()),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let python = trusted_python_identity().unwrap();
        let trusted_wsl = std::path::Path::new("/not-on-this-host/System32/wsl.exe");
        let mut command = windows_helper_command(
            trusted_wsl,
            &python.path,
            "raise SystemExit(0)",
            Some(&python),
        );
        command.env("PATH", fixture.path());
        assert_eq!(command.get_program(), trusted_wsl.as_os_str());
        assert!(command
            .get_args()
            .any(|argument| argument == std::ffi::OsStr::new(&python.path)));
        assert!(command.output().is_err());
        assert!(!marker.exists());
    }

    #[test]
    fn windows_managed_owner_observation_is_one_bounded_self_validating_helper() {
        const MEASURED_WSL_HELPER_LATENCY: Duration = Duration::from_millis(1_100);

        let python = ManagedExecutableIdentity {
            path: "/usr/bin/python3".into(),
            device: 8,
            inode: 1234,
        };
        let trusted_wsl = std::path::Path::new("C:\\Windows\\System32\\wsl.exe");
        let command = windows_managed_cgroup_effect_command(trusted_wsl, &python);
        let args = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(command.get_program(), trusted_wsl.as_os_str());
        assert_eq!(WINDOWS_MANAGED_CGROUP_HELPERS_PER_EFFECT, 1);
        assert!(MEASURED_WSL_HELPER_LATENCY < TMUX_CMD_TIMEOUT_DEFAULT);
        assert_eq!(
            &args[..6],
            [
                "--cd",
                "~",
                "-e",
                "/usr/bin/python3",
                "-c",
                MANAGED_CGROUP_EFFECT_PY
            ]
        );
        assert_eq!(
            serde_json::from_str::<ManagedExecutableIdentity>(&args[6]).unwrap(),
            python
        );
        assert!(MANAGED_CGROUP_EFFECT_PY.contains("os.path.realpath(sys.executable)"));
        assert!(MANAGED_CGROUP_EFFECT_PY.contains("actual_python != expected_python"));
        assert!(MANAGED_CGROUP_EFFECT_PY.contains("details.st_uid != 0"));
        assert!(MANAGED_CGROUP_EFFECT_PY.contains("stat.S_IWGRP | stat.S_IWOTH"));
        assert!(MANAGED_CGROUP_EFFECT_PY.contains("details.st_dev"));
        assert!(MANAGED_CGROUP_EFFECT_PY.contains("details.st_ino"));
    }

    #[cfg(unix)]
    #[test]
    fn managed_cgroup_path_validation_rejects_every_broad_group() {
        let unit = format!("t-hub-{}.scope", "a".repeat(32));
        for broad in [
            "/",
            "/user.slice",
            "/user.slice/user-1000.slice",
            "/user.slice/user-1000.slice/user@1000.service",
            "/user.slice/user-1000.slice/user@1000.service/app.slice",
        ] {
            let python = trusted_python_identity().unwrap();
            let mut command = managed_cgroup_effect_command(&python).unwrap();
            command.args(["validate-path", &unit, broad]);
            let output = output_with_timeout_and_limit(
                command,
                Duration::from_secs(5),
                MANAGED_HELPER_OUTPUT_LIMIT,
            )
            .unwrap();
            assert!(
                !output.status.success(),
                "broad cgroup was accepted: {broad}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn pane_scans_use_bash_for_pipefail_semantics() {
        let command = pane_info_command_with_args("set -o pipefail; printf '%s' \"$1\"", &["ok"]);
        assert_eq!(command.get_program(), "bash");
        assert_eq!(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec![
                "-lc",
                "set -o pipefail; printf '%s' \"$1\"",
                "t-hub-script",
                "ok"
            ]
        );
    }

    #[test]
    fn active_codex_scan_fails_closed_beyond_its_process_depth_bound() {
        let script = active_codex_rollouts_script();
        assert!(script.contains("process depth probe failed"));
        assert!(script.contains("process depth bound exceeded"));
        assert!(script.contains("for parent in $frontier"));
        assert!(script.contains("result bound exceeded"));
    }

    #[test]
    fn socket_name_validation_rejects_shell_and_option_shapes_without_effect() {
        let marker = std::env::temp_dir().join(format!(
            "t-hub-hostile-socket-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let marker_text = marker.to_string_lossy();
        let hostile = vec![
            "has space".to_string(),
            "has'quote".to_string(),
            "has\"quote".to_string(),
            "semi;colon".to_string(),
            format!("$(touch {marker_text})"),
            format!("`touch {marker_text}`"),
            "line\nbreak".to_string(),
            "-option-like".to_string(),
        ];
        for value in hostile {
            let error = validate_socket_name(&value).unwrap_err();
            assert_eq!(error.op, "validate-socket");
            assert!(!error.message.contains(&value));
        }
        assert!(!marker.exists());
        assert_eq!(
            validate_socket_name("t-hub.dev_01").unwrap(),
            "t-hub.dev_01"
        );
    }

    #[test]
    fn hostile_socket_environment_subprocess_helper() {
        if std::env::var("T_HUB_HOSTILE_SOCKET_HELPER").as_deref() != Ok("1") {
            return;
        }
        let error = list_sessions().unwrap_err();
        assert_eq!(error.op, "validate-socket");
        assert!(!error.message.contains(socket()));
    }

    #[test]
    fn hostile_socket_environment_fails_closed_before_process_effect() {
        let marker = std::env::temp_dir().join(format!(
            "t-hub-hostile-socket-env-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let marker_text = marker.to_string_lossy();
        let hostile = [
            "has space".to_string(),
            "has'quote".to_string(),
            "has\"quote".to_string(),
            "semi;colon".to_string(),
            format!("$(touch {marker_text})"),
            format!("`touch {marker_text}`"),
            "line\nbreak".to_string(),
            "-option-like".to_string(),
        ];
        let test_binary = std::env::current_exe().unwrap();
        for value in hostile {
            let output = Command::new(&test_binary)
                .args([
                    "--exact",
                    "tmux::tests::hostile_socket_environment_subprocess_helper",
                    "--nocapture",
                ])
                .env("T_HUB_HOSTILE_SOCKET_HELPER", "1")
                .env("T_HUB_TMUX_SOCKET", value)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!marker.exists());
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

    struct TestSession {
        name: String,
        _lifecycle: TestLifecycleGuard,
    }

    impl TestSession {
        fn new() -> Self {
            let lifecycle = TestLifecycleGuard::acquire();
            let name = unique_name();
            let _ = kill_session(&name);
            Self {
                name,
                _lifecycle: lifecycle,
            }
        }
    }

    impl Drop for TestSession {
        fn drop(&mut self) {
            let _ = kill_session(&self.name);
        }
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

    #[test]
    fn pidfd_effect_refuses_same_pane_replacement_at_final_seam() {
        if !tmux_available() {
            eprintln!("pidfd effect seam test skipped: tmux unavailable");
            return;
        }
        let session = TestSession::new();
        new_session_with_env(&session.name, "/tmp", Some("sleep 60"), &[]).unwrap();
        let expected = observe_session_effect_identity(&session.name).unwrap();
        let hook_target = session.name.clone();
        set_before_exact_effect_hook(
            &session.name,
            Box::new(move || {
                respawn_pane_exact(&hook_target, "/tmp", "sleep 60").unwrap();
            }),
        );

        let error = kill_session_tree_exact(&session.name, expected).unwrap_err();
        assert_eq!(error.op, "kill-session-tree-exact");
        assert_eq!(session_liveness(&session.name), SessionLiveness::Alive);
        assert_ne!(
            observe_session_effect_identity(&session.name).unwrap(),
            expected
        );
    }

    #[cfg(unix)]
    #[test]
    fn pidfd_effect_reproduction_proves_late_double_fork_can_escape() {
        if !tmux_available() {
            eprintln!("pidfd late-fork regression skipped: tmux unavailable");
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let seam = fixture.path().join("retire-seam");
        let survivor = fixture.path().join("survivor.pid");
        let workload = fixture.path().join("late-fork.py");
        std::fs::write(
            &workload,
            r#"import os, signal, sys, time
seam, survivor = sys.argv[1:3]
while not os.path.exists(seam + ".ready"):
    time.sleep(0.005)
pid = os.fork()
if pid == 0:
    os.setsid()
    pid = os.fork()
    if pid != 0:
        os._exit(0)
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    with open(survivor, "w", encoding="ascii") as output:
        output.write(str(os.getpid()))
    while True:
        time.sleep(1)
os.waitpid(pid, 0)
open(seam + ".ack", "x", encoding="ascii").close()
while True:
    time.sleep(1)
"#,
        )
        .unwrap();
        let session = TestSession::new();
        let command = format!(
            "python3 {} {} {}",
            workload.display(),
            seam.display(),
            survivor.display()
        );
        new_session_with_env(&session.name, "/tmp", Some(&command), &[]).unwrap();
        let expected = observe_session_effect_identity(&session.name).unwrap();
        set_after_final_scan_seam(&session.name, &seam);

        kill_session_tree_exact(&session.name, expected).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !survivor.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let pid = std::fs::read_to_string(&survivor)
            .unwrap()
            .parse::<u32>()
            .unwrap();
        let survived = std::path::Path::new(&format!("/proc/{pid}/stat")).exists();
        let _ = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();

        assert!(
            survived,
            "the regression setup must prove why pidfd tree scans cannot claim complete retirement"
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_cgroup_retirement_contains_continuous_double_forks_and_preserves_sibling() {
        if !tmux_available() {
            eprintln!("managed cgroup retirement test skipped: tmux unavailable");
            return;
        }
        if let Err(error) = managed_runtime_preflight() {
            eprintln!("managed cgroup retirement test skipped: {error}");
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let survivor = fixture.path().join("managed-child.pid");
        let workload = fixture.path().join("continuous-fork.py");
        std::fs::write(
            &workload,
            r#"import os, signal, sys, time
survivor = sys.argv[1]
signal.signal(signal.SIGHUP, signal.SIG_IGN)
pid = os.fork()
if pid == 0:
    os.setsid()
    pid = os.fork()
    if pid != 0:
        os._exit(0)
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    with open(survivor, "w", encoding="ascii") as output:
        output.write(str(os.getpid()))
    while True:
        child = os.fork()
        if child == 0:
            time.sleep(0.02)
            os._exit(0)
        try:
            os.waitpid(child, 0)
        except ChildProcessError:
            pass
os.waitpid(pid, 0)
while True:
    time.sleep(1)
"#,
        )
        .unwrap();
        let mut sibling = Command::new("sleep").arg("60").spawn().unwrap();
        let sibling_pid = sibling.id();
        let session = TestSession::new();
        let command = format!("python3 {} {}", workload.display(), survivor.display());
        let owner =
            new_managed_session_with_env(&session.name, "/tmp", Some(&command), &[]).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !survivor.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let child_pid = std::fs::read_to_string(&survivor)
            .unwrap()
            .parse::<u32>()
            .unwrap();
        let child_cgroup = std::fs::read_to_string(format!("/proc/{child_pid}/cgroup")).unwrap();
        assert_eq!(child_cgroup.trim(), format!("0::{}", owner.cgroup_path));

        retire_managed_runtime(&session.name, &owner).unwrap();
        assert_eq!(session_liveness(&session.name), SessionLiveness::Gone);
        assert!(!std::path::Path::new(&format!("/proc/{child_pid}")).exists());
        assert!(std::path::Path::new(&format!("/proc/{sibling_pid}")).exists());
        let _ = sibling.kill();
        let _ = sibling.wait();
    }

    #[cfg(unix)]
    #[test]
    fn unpublished_managed_cleanup_converges_when_exact_unit_is_absent() {
        if managed_runtime_preflight().is_err() {
            return;
        }
        let launch = prepare_managed_runtime_launch().unwrap();

        retire_prepared_managed_runtime(&launch).unwrap();
        retire_prepared_managed_runtime(&launch).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn prepared_cleanup_refuses_active_unit_without_signaling_it_or_a_sibling() {
        if !tmux_available() || managed_runtime_preflight().is_err() {
            return;
        }
        let session = TestSession::new();
        let launch = prepare_managed_runtime_launch().unwrap();
        let owner = new_prepared_managed_session_with_env(
            &session.name,
            "/tmp",
            Some("sleep 60"),
            &[],
            &launch,
        )
        .unwrap();
        let mut sibling = Command::new("sleep").arg("60").spawn().unwrap();
        let sibling_pid = sibling.id();

        let error = retire_prepared_managed_runtime(&launch).unwrap_err();

        assert_eq!(error.op, "retire-prepared-managed-runtime");
        assert_eq!(session_liveness(&session.name), SessionLiveness::Alive);
        revalidate_managed_runtime_owner(&session.name, &owner).unwrap();
        assert!(std::path::Path::new(&format!("/proc/{sibling_pid}")).exists());

        retire_managed_runtime(&session.name, &owner).unwrap();
        let _ = sibling.kill();
        let _ = sibling.wait();
    }

    #[cfg(unix)]
    #[test]
    fn prepared_cleanup_state_contract_rejects_every_ambiguous_shape() {
        use std::os::unix::fs::MetadataExt;

        let python = trusted_python_identity().unwrap();
        let unit = "t-hub-0123456789abcdef0123456789abcdef.scope";
        let uid = std::fs::metadata("/proc/self").unwrap().uid();
        let path = format!(
            "/user.slice/user-{}.slice/user@{}.service/app.slice/{unit}",
            uid, uid
        );
        let invoke = |properties: serde_json::Value, directory_exists: bool| {
            let mut command = managed_cgroup_effect_command(&python).unwrap();
            command.args([
                "validate-prepared-state",
                unit,
                &properties.to_string(),
                if directory_exists { "1" } else { "0" },
            ]);
            output_with_timeout_and_limit(command, tmux_cmd_timeout(), MANAGED_HELPER_OUTPUT_LIMIT)
                .unwrap()
                .status
                .success()
        };
        let inactive = serde_json::json!({
            "Id": unit,
            "InvocationID": "0123456789abcdef0123456789abcdef",
            "ControlGroup": path,
            "ActiveState": "inactive",
            "SubState": "dead",
            "LoadState": "loaded",
        });
        assert!(invoke(inactive.clone(), false));
        assert!(invoke(
            serde_json::json!({
                "Id": unit,
                "InvocationID": "",
                "ControlGroup": "",
                "ActiveState": "inactive",
                "SubState": "dead",
                "LoadState": "not-found",
            }),
            false,
        ));

        for active_state in [
            "active",
            "activating",
            "deactivating",
            "reloading",
            "failed",
            "maintenance",
            "unknown",
        ] {
            let mut ambiguous = inactive.clone();
            ambiguous["ActiveState"] = active_state.into();
            assert!(!invoke(ambiguous, false), "accepted {active_state}");
        }
        for ambiguous in [
            serde_json::json!({
                "Id": unit,
                "InvocationID": "",
                "ControlGroup": "",
                "ActiveState": "inactive",
                "SubState": "dead",
                "LoadState": "masked",
            }),
            serde_json::json!({
                "Id": unit,
                "InvocationID": "",
                "ControlGroup": "",
                "ActiveState": "inactive",
                "SubState": "failed",
                "LoadState": "loaded",
            }),
            serde_json::json!({
                "Id": unit,
                "InvocationID": "0123456789abcdef0123456789abcdef",
                "ControlGroup": "",
                "ActiveState": "inactive",
                "SubState": "dead",
                "LoadState": "not-found",
            }),
            serde_json::json!({
                "Id": unit,
                "InvocationID": "malformed",
                "ControlGroup": path,
                "ActiveState": "inactive",
                "SubState": "dead",
                "LoadState": "loaded",
            }),
            serde_json::json!({
                "Id": unit,
                "InvocationID": "",
                "ControlGroup": "/user.slice/foreign.scope",
                "ActiveState": "inactive",
                "SubState": "dead",
                "LoadState": "loaded",
            }),
            serde_json::json!({
                "Id": unit,
                "InvocationID": "",
                "ControlGroup": "",
                "ActiveState": "inactive",
                "SubState": "dead",
            }),
        ] {
            assert!(!invoke(ambiguous, false));
        }
        assert!(!invoke(inactive, true));
    }

    #[cfg(unix)]
    #[test]
    fn failed_managed_publication_converges_after_runtime_exits() {
        if !tmux_available() || managed_runtime_preflight().is_err() {
            return;
        }
        let session = TestSession::new();
        let launch = prepare_managed_runtime_launch().unwrap();

        let error = new_prepared_managed_session_with_env(
            &session.name,
            "/tmp",
            Some("exit 0"),
            &[],
            &launch,
        )
        .unwrap_err();

        assert!(
            !error.message.contains("prepared cleanup failed"),
            "{error}"
        );
        assert_eq!(session_liveness(&session.name), SessionLiveness::Gone);
        retire_prepared_managed_runtime(&launch).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn managed_retirement_refuses_invocation_and_inode_reuse() {
        if !tmux_available() || managed_runtime_preflight().is_err() {
            return;
        }
        for mutate in ["invocation", "inode"] {
            let session = TestSession::new();
            let owner =
                new_managed_session_with_env(&session.name, "/tmp", Some("sleep 60"), &[]).unwrap();
            let mut reused = owner.clone();
            if mutate == "invocation" {
                reused.invocation_id = "f".repeat(32);
            } else {
                reused.cgroup_inode = reused.cgroup_inode.saturating_add(1);
            }
            let error = retire_managed_runtime(&session.name, &reused).unwrap_err();
            assert_eq!(error.op, "retire-managed-runtime");
            assert_eq!(session_liveness(&session.name), SessionLiveness::Alive);
            retire_managed_runtime(&session.name, &owner).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn managed_retirement_recovers_after_freeze_and_kill_crashes() {
        if !tmux_available() || managed_runtime_preflight().is_err() {
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        for stage in ["freeze", "kill"] {
            let session = TestSession::new();
            let owner =
                new_managed_session_with_env(&session.name, "/tmp", Some("sleep 60"), &[]).unwrap();
            let marker = fixture.path().join(format!("crash-{stage}"));
            set_managed_retire_crash(&session.name, stage, &marker);
            let result = retire_managed_runtime(&session.name, &owner);
            assert!(
                result.is_err(),
                "simulated crash after {stage} unexpectedly completed; marker={} ",
                marker.exists()
            );
            let error = result.unwrap_err();
            assert_eq!(error.op, "retire-managed-runtime");
            assert!(marker.exists());
            retire_managed_runtime(&session.name, &owner).unwrap();
            assert_eq!(session_liveness(&session.name), SessionLiveness::Gone);
        }
    }

    #[cfg(unix)]
    #[test]
    fn managed_retirement_handles_detached_descendant_after_tmux_is_gone() {
        if !tmux_available() || managed_runtime_preflight().is_err() {
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let pid_file = fixture.path().join("detached.pid");
        let script = fixture.path().join("detached.py");
        std::fs::write(
            &script,
            format!(
                "import os,signal,time\nsignal.signal(signal.SIGHUP, signal.SIG_IGN)\nopen({:?}, 'w').write(str(os.getpid()))\nwhile True: time.sleep(1)\n",
                pid_file
            ),
        )
        .unwrap();
        let session = TestSession::new();
        let owner = new_managed_session_with_env(
            &session.name,
            "/tmp",
            Some(&format!("python3 {}", script.display())),
            &[],
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !pid_file.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .parse::<u32>()
            .unwrap();
        kill_session(&session.name).unwrap();
        assert_eq!(session_liveness(&session.name), SessionLiveness::Gone);
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
        retire_managed_runtime(&session.name, &owner).unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn managed_owner_contract_fails_closed_on_unsupported_shapes() {
        assert!(MANAGED_CGROUP_EFFECT_PY.contains("cgroup.freeze"));
        assert!(MANAGED_CGROUP_EFFECT_PY.contains("cgroup.kill"));
        assert!(MANAGED_CGROUP_EFFECT_PY.contains("systemd-run"));
        assert!(MANAGED_CGROUP_EFFECT_PY.contains("refuse(70)"));
        assert!(MANAGED_CGROUP_EFFECT_PY.contains("refuse(72)"));
    }

    #[test]
    fn session_environment_inherits_isolated_agent_journal() {
        let inherited = session_env_with_agent_journal(
            &[("EXISTING".to_string(), "value".to_string())],
            Some(".t-hub-dev/journal".to_string()),
        );
        assert!(inherited.iter().any(|(key, value)| {
            key == "T_HUB_AGENT_JOURNAL_DIR" && value == ".t-hub-dev/journal"
        }));

        let explicit = session_env_with_agent_journal(
            &[(
                "T_HUB_AGENT_JOURNAL_DIR".to_string(),
                "/explicit/journal".to_string(),
            )],
            Some(".t-hub-dev/journal".to_string()),
        );
        assert_eq!(
            explicit,
            vec![(
                "T_HUB_AGENT_JOURNAL_DIR".to_string(),
                "/explicit/journal".to_string()
            )]
        );
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

        new_session_with_env(&name, "/tmp", None, &[]).expect("new_session should succeed");

        assert!(has_session(&name), "session should exist after creation");

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
    fn session_environment_setter_roundtrips_on_exact_session() {
        if !tmux_available() {
            eprintln!(
                "tmux::tests::session_environment_setter_roundtrips_on_exact_session: tmux not on PATH - skipping"
            );
            return;
        }
        let session = TestSession::new();
        new_session_with_env(&session.name, "/tmp", None, &[]).unwrap();

        set_session_environment(&session.name, "T_HUB_TEST_GENERATION", "1").unwrap();

        assert_eq!(
            session_environment(&session.name, "T_HUB_TEST_GENERATION").unwrap(),
            Some("1".into())
        );
    }

    #[test]
    fn session_environment_setter_rejects_unbounded_or_invalid_input() {
        let invalid_name = set_session_environment("target", "-bad", "1").unwrap_err();
        assert_eq!(invalid_name.op, "set-environment");
        assert!(invalid_name.message.contains("valid identifier"));

        let oversized_value = "x".repeat(4097);
        let invalid_value =
            set_session_environment("target", "VALID_NAME", &oversized_value).unwrap_err();
        assert_eq!(invalid_value.op, "set-environment");
        assert!(invalid_value.message.contains("bounded input contract"));
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
        let name = unique_name();
        let _ = kill_session(&name);
        new_session_with_env(&name, "/tmp", None, &[]).expect("new_session should succeed");

        send_keys(&name, &["C-c"]).expect("send_keys C-c should succeed");
        send_keys(&name, &["Enter"]).expect("send_keys Enter should succeed");

        kill_session(&name).expect("kill_session should succeed");
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
        let _lifecycle = TestLifecycleGuard::acquire();
        // Reads the window-size option's current mode ("latest" / "manual") off a
        // live session. `show-options -w -t <name> window-size` prints
        // `window-size <mode>`; we return just the mode token.
        fn window_size_mode(name: &str) -> String {
            let out = output_with_timeout(
                tmux(&["show-options", "-w", "-t", name, "window-size"]).unwrap(),
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
