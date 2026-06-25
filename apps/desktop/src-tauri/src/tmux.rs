//! tmux control on the isolated `t-hub` socket — the process-orchestration
//! layer beneath the PTY.
//!
//! Every call uses `tmux -L t-hub ...` so T-Hub never touches the user's
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
use std::sync::LazyLock;

/// The isolated tmux socket name; always passed as `tmux -L <socket>`.
///
/// Resolved ONCE at startup from `$T_HUB_TMUX_SOCKET`, defaulting to
/// `"t-hub"`. The env hook exists so a second, side-by-side **DEV** instance
/// can run alongside the production app on its OWN tmux socket (e.g.
/// `T_HUB_TMUX_SOCKET=t-hub-dev`) without ever sharing sessions with — or
/// killing — production's terminals. With NO env var set the value is exactly
/// `"t-hub"`, so default behavior is byte-for-byte unchanged.
static SOCKET_NAME: LazyLock<String> =
    LazyLock::new(|| std::env::var("T_HUB_TMUX_SOCKET").unwrap_or_else(|_| "t-hub".into()));

/// The resolved tmux socket name (`$T_HUB_TMUX_SOCKET` or `"t-hub"`),
/// always passed as `tmux -L <socket>`. Read once; cheap to call repeatedly.
pub fn socket() -> &'static str {
    &SOCKET_NAME
}

/// How many lines of scrollback history we capture to seed xterm on attach.
const SCROLLBACK_LINES: i64 = 2000;

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
/// `wsl.exe -- tmux …`; on Unix (including the WSL dev build) tmux is invoked
/// directly. Both then carry `-L t-hub` plus the caller's args.
fn tmux(args: &[&str]) -> Command {
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        // `--cd ~` roots the tmux server (and each new session's pane) at the WSL
        // home, so new terminals open in ~ (native ext4) instead of the app's
        // /mnt/c launch dir -- matching the user's normal `~` terminal view.
        let mut c = Command::new("wsl.exe");
        c.arg("--cd").arg("~").arg("--").arg("tmux");
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
    // `-c CWD` only when we actually have a (WSL-side) directory; on Windows the
    // default is empty so the pane starts in wsl.exe's launch dir rather than an
    // invalid Windows path.
    let mut args: Vec<&str> = vec!["new-session", "-d", "-s", name];
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
    // boot). Best-effort: a failure just falls back to tmux's 2000 default.
    let limit = HISTORY_LIMIT.to_string();
    let _ = run("start-server", &["start-server"]);
    let _ = run(
        "set-option",
        &["set-option", "-g", "history-limit", &limit],
    );

    run("new-session", &args)?;

    // Pin the pane to the latest active client. Best-effort: if this fails the
    // session still exists, so we surface it as an error only if tmux reports
    // one (the session create above already succeeded).
    run("set-option", &["set-option", "-t", name, "window-size", "latest"])?;

    // T-Hub draws its own tile chrome, so suppress tmux's status bar (the green
    // "0:zsh" line). Best-effort -- purely cosmetic, never fail the spawn.
    let _ = run("set-option", &["set-option", "-t", name, "status", "off"]);
    // Mouse ON (global): full-screen apps that request mouse mode (Claude Code,
    // vim, less, ...) receive the wheel and scroll their OWN content instead of
    // the terminal translating the wheel into Up/Down arrow keys; a plain shell's
    // wheel enters tmux copy-mode scrollback. Set globally (`-g`) so it also
    // applies to already-running sessions, not just this new one. Trade-off:
    // mouse text-selection now needs Shift+drag (which bypasses tmux's capture).
    let _ = run("set-option", &["set-option", "-g", "mouse", "on"]);
    // Re-apply the server-global keybinds (prefix-disable + right-click unbinds).
    // `ensure_mouse_on()` already does this at startup, but it runs ONCE and may
    // fire before any tmux server exists (fresh boot), so its unbinds silently
    // no-op. Re-running here — once a server is guaranteed to exist — makes them
    // stick. Root-table (`-n`) bindings are server-global, so this is idempotent.
    apply_global_keybinds();
    Ok(())
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
fn apply_global_keybinds() {
    let _ = run("set-option", &["set-option", "-g", "prefix", "None"]);
    let _ = run("unbind", &["unbind", "-n", "C-b"]);
    let _ = run("unbind", &["unbind", "C-b"]);
    for ev in [
        "MouseDown3Pane",
        "MouseDown3Status",
        "MouseDown3StatusLeft",
        "MouseDown3StatusRight",
    ] {
        let _ = run("unbind", &["unbind", "-n", ev]);
    }
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
            let _ = run("set-option", &["set-option", "-t", s.as_str(), "mouse", "on"]);
        }
    }
    // Disable the C-b prefix and tmux's built-in mouse context menus,
    // server-global (covers existing + future sessions). See the helper for why.
    // Note: at fresh boot this may run before any tmux server exists, so the
    // unbinds silently no-op here; `new_session()` re-applies them once a server
    // is guaranteed to exist.
    apply_global_keybinds();
}

/// Returns true if a session named `name` exists on the `t-hub` socket.
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

/// List all session names on the `t-hub` socket.
///
/// Tolerates the "no server running" case (no sessions have ever been created,
/// or the last one was killed and the server exited) by returning an empty Vec
/// rather than an error.
pub fn list_sessions() -> Result<Vec<String>, TmuxError> {
    // NB: we deliberately do NOT use `-F '#{session_name}'`. On Windows every
    // tmux command is routed through `wsl.exe`, where the leading `#` of a tmux
    // format string is swallowed as a shell comment — leaving `list-sessions -F`
    // with no argument ("-F expects an argument") and breaking the whole live
    // terminal list (cwd/labels/status). The default `list-sessions` output is
    // `<name>: <window/size info>`; tmux forbids `:` in session names, so the
    // name is everything before the first colon. This needs no format argument
    // and survives the wsl.exe round-trip intact.
    let output = tmux(&["list-sessions"])
        .output()
        .map_err(|e| TmuxError {
            op: "list-sessions",
            code: None,
            message: format!("failed to spawn tmux: {e}"),
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
pub struct PaneInfo {
    pub session: String,
    pub command: String,
    pub cwd: String,
}

/// List every pane's `session_name|pane_current_command|pane_current_path`.
///
/// Unlike [`list_sessions`], this needs a tmux FORMAT (`#{...}`). A bare
/// `#{...}` argv word is swallowed as a shell comment over the `wsl.exe`
/// round-trip (see the note in `list_sessions`), so we run the whole tmux call
/// inside a `bash -lc` script where the format is SINGLE-QUOTED — inside single
/// quotes `#` is literal, so it survives intact. Best-effort: a missing server
/// (no sessions) returns an empty Vec rather than erroring.
pub fn pane_info() -> Result<Vec<PaneInfo>, TmuxError> {
    // Built from the resolved socket name (not a hardcoded `t-hub`) so a DEV
    // instance with `$T_HUB_TMUX_SOCKET` set reads ITS panes; with no env var
    // the socket is `t-hub`, reproducing the previous literal exactly.
    // We emit `session|command|cwd`, BUT `pane_current_command` only reports the
    // foreground process's comm — which is the RUNTIME (e.g. `node`) for agents
    // shipped as scripts: the Codex CLI is `node …/codex`, so it'd read as "node"
    // and never be detected as Codex (Claude runs as `claude`, so it's fine). So
    // when the foreground is a runtime, we resolve the real agent from the pane
    // pid's child argv (`/proc/<kid>/cmdline`) and substitute `codex`/`claude` as
    // the command. Output shape is unchanged, so the parser/callers don't change.
    // Best-effort: no pgrep / no match leaves the original command intact.
    let script = format!(
        "tmux -L {sock} list-panes -a -F \
'#{{session_name}}|#{{pane_current_command}}|#{{pane_current_path}}|#{{pane_pid}}' \
| while IFS='|' read -r s cmd path pid; do eff=\"$cmd\"; \
case \"$cmd\" in node|bun|deno|python|python3) \
for kid in $(pgrep -P \"$pid\" 2>/dev/null); do \
line=$(tr '\\0' ' ' < /proc/$kid/cmdline 2>/dev/null); \
case \"$line\" in *codex*) eff=codex; break;; *claude*) eff=claude; break;; esac; \
done;; esac; printf '%s|%s|%s\\n' \"$s\" \"$eff\" \"$path\"; done",
        sock = socket()
    );
    let output = pane_info_command(&script).output().map_err(|e| TmuxError {
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
    c.arg("--cd").arg("~").arg("-e").arg("bash").arg("-lc").arg(script);
    c.creation_flags(0x0800_0000);
    c
}

#[cfg(unix)]
fn pane_info_command(script: &str) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(script);
    c
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

/// Capture only the visible screen of `name` (no scrollback history), ANSI
/// preserved. Used to seed a freshly spawned tile with a single clean prompt
/// rather than the 80x24-then-resize redraw trail that full-history capture
/// would replay.
#[allow(dead_code)] // retained for potential visible-only reattach seeding
pub fn capture_visible(name: &str) -> Result<Vec<u8>, TmuxError> {
    let output = run("capture-pane", &["capture-pane", "-p", "-e", "-t", name])?;
    Ok(output.stdout)
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
    let out = run("display-message", &["display-message", "-p", "-t", name, fmt]).ok()?;
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

    /// Full lifecycle on the isolated socket: create → list contains it →
    /// has_session true → capture returns bytes → kill → has_session false.
    ///
    /// NOTE: requires a real `tmux` on PATH. It compiles everywhere but only
    /// passes where tmux is installed (it is in this WSL2 dev shell; it is not
    /// expected to run on the Windows CI target).
    #[test]
    fn lifecycle_create_list_capture_kill() {
        if !tmux_available() {
            eprintln!(
                "tmux::tests::lifecycle_create_list_capture_kill: tmux not on PATH — skipping"
            );
            return;
        }
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
        new_session(&name, "/tmp", None).expect("new_session should succeed");

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
        new_session(&name, "/tmp", None).expect("new_session should succeed");

        send_keys(&name, &["C-c"]).expect("send_keys C-c should succeed");
        send_keys(&name, &["Enter"]).expect("send_keys Enter should succeed");

        kill_session(&name).expect("kill_session should succeed");
    }

    /// kill_session on a missing session is idempotent (success), and
    /// has_session reports false for a name that was never created.
    #[test]
    fn kill_missing_is_idempotent() {
        if !tmux_available() {
            eprintln!(
                "tmux::tests::kill_missing_is_idempotent: tmux not on PATH — skipping"
            );
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
            eprintln!(
                "tmux::tests::list_sessions_tolerates_empty: tmux not on PATH — skipping"
            );
            return;
        }
        // Whether or not a server is running, this must not error.
        let _ = list_sessions().expect("list_sessions must tolerate no-server");
    }
}
