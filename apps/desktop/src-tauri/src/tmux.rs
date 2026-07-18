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
    let mut fields = line.trim().split('|');
    let parse_prefixed = |value: Option<&str>, prefix: char| {
        value
            .and_then(|value| value.strip_prefix(prefix))
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
    };
    let parse_number = |value: Option<&str>| {
        value
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
    };
    let session_id = parse_prefixed(fields.next(), '$');
    let session_created = parse_number(fields.next());
    let window_id = parse_prefixed(fields.next(), '@');
    let pane_id = parse_prefixed(fields.next(), '%');
    let pane_pid = parse_number(fields.next()).and_then(|value| u32::try_from(value).ok());
    if fields.next().is_some()
        || session_id.is_none()
        || session_created.is_none()
        || window_id.is_none()
        || pane_id.is_none()
        || pane_pid.is_none()
    {
        return Err(TmuxError {
            op: "list-panes",
            code: None,
            message: "target pane generation is malformed".into(),
        });
    }
    Ok(PaneGeneration {
        session_id: session_id.unwrap(),
        session_created: session_created.unwrap(),
        window_id: window_id.unwrap(),
        pane_id: pane_id.unwrap(),
        pane_pid: pane_pid.unwrap(),
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
    fn dormant_pane_respawns_in_place_with_a_new_generation() {
        if !tmux_available() {
            eprintln!(
                "tmux::tests::dormant_pane_respawns_in_place_with_a_new_generation: tmux not on PATH - skipping"
            );
            return;
        }
        let name = unique_name();
        let _ = kill_session(&name);
        new_session_with_env(
            &name,
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
        let transition = respawn_pane_exact(&name, "/tmp", &command).unwrap();
        assert_eq!(transition.before.session_id, transition.after.session_id);
        assert_eq!(
            transition.before.session_created,
            transition.after.session_created
        );
        assert_eq!(transition.before.window_id, transition.after.window_id);
        assert_eq!(transition.before.pane_id, transition.after.pane_id);
        assert_ne!(transition.before.pane_pid, transition.after.pane_pid);
        assert_eq!(
            session_environment(&name, "T_HUB_RESPAWN_ENV").unwrap(),
            Some("preserved".into())
        );
        assert!(has_session(&name));
        kill_session(&name).unwrap();
    }

    #[test]
    fn respawn_executes_when_a_hostile_pane_discards_injected_keys() {
        if !tmux_available() {
            eprintln!(
                "tmux::tests::respawn_executes_when_a_hostile_pane_discards_injected_keys: tmux not on PATH - skipping"
            );
            return;
        }
        let fixture = std::env::temp_dir().join(format!(
            "t-hub-respawn-hostile-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&fixture).unwrap();
        let ready = fixture.join("ready");
        let injected = fixture.join("injected");
        let respawned = fixture.join("respawned");
        let hostile = fixture.join("hostile-shell");
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
        let name = unique_name();
        let _ = kill_session(&name);
        new_session_with_env(&name, "/tmp", Some(hostile.to_str().unwrap()), &[]).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !ready.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "hostile pane did not become ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        send_text(
            &name,
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
        respawn_pane_exact(&name, "/tmp", &command).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !respawned.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "respawn command did not execute"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        kill_session(&name).unwrap();
        let _ = std::fs::remove_dir_all(fixture);
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
