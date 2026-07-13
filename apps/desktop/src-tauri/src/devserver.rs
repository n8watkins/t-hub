//! Managed `npm run dev` runner for the per-project **Dev** tab.
//!
//! Each project tile can run ONE managed dev server, scoped to that project's
//! directory. We spawn it as a child process, capture its combined stdout+stderr
//! line-by-line, and stream each line to the frontend on a per-terminal channel
//! (`devserver://<id>`). The Dev tab renders those lines in a scrolling pane and
//! sniffs each one for the localhost URL the dev server prints, handing that URL
//! to the Preview tab automatically.
//!
//! Platform model (mirrors the PTY/tmux/files layers):
//!   - `#[cfg(windows)]`:    run the command INSIDE WSL via
//!     `wsl.exe -d <distro> --cd <cwd> -e bash -lc '<command>'`, with the console
//!     window suppressed (`CREATE_NO_WINDOW`) so no CMD window flashes. The cwd
//!     is a native WSL path (`/home/...`) translated back from any UNC form, just
//!     like `files.rs` does for its native-in-WSL fast paths.
//!   - `#[cfg(not(windows))]`: run `sh -lc '<command>'` directly with the cwd set
//!     on the `Command` (this is the WSL dev build's path, and the one Linux
//!     `cargo check` actually compiles + verifies).
//!
//! State: a single process-global registry (`Mutex<HashMap<terminal_id, child>>`)
//! so the module is fully self-contained — no Tauri-managed state to register in
//! `lib.rs` beyond the two commands + `mod devserver;`. `start_dev_server`
//! replaces (kills) any existing child for the same terminal id before spawning a
//! new one, and `stop_dev_server` kills + drops the child for an id.
//!
//! Boundaries: this module owns its own registry and shares nothing with the
//! terminal/agent/files modules. It reuses the same crates already in the tree
//! (`std::process`, `parking_lot::Mutex`, `tauri::Emitter`) — no new deps.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::LazyLock;
use std::thread::JoinHandle;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// The per-terminal event channel carrying dev-server output lines. The frontend
/// subscribes to `devserver://<terminal_id>` (see `src/ipc/devserver.ts`). Built
/// here so the channel name lives in exactly one place.
pub fn channel(terminal_id: &str) -> String {
    format!("devserver://{terminal_id}")
}

/// `devserver://<id>` — one event from a managed dev server. `kind` discriminates
/// a streamed output line from the lifecycle markers ("started"/"exited") so the
/// Dev tab can update its status row without parsing the line text.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevServerEvent {
    /// The terminal/project id this event belongs to (echoed for fan-out safety).
    pub id: String,
    /// `"line"` (a captured stdout/stderr line), `"started"` (the child spawned),
    /// or `"exited"` (the child ended; `line` carries an exit summary).
    pub kind: String,
    /// The output line text for `kind == "line"`, or a human-readable summary for
    /// the lifecycle kinds. Never includes the trailing newline.
    pub line: String,
}

impl DevServerEvent {
    fn line(id: &str, line: String) -> Self {
        Self {
            id: id.to_string(),
            kind: "line".to_string(),
            line,
        }
    }
    fn started(id: &str) -> Self {
        Self {
            id: id.to_string(),
            kind: "started".to_string(),
            line: String::new(),
        }
    }
    fn exited(id: &str, summary: String) -> Self {
        Self {
            id: id.to_string(),
            kind: "exited".to_string(),
            line: summary,
        }
    }
}

/// A running managed dev server: the child process handle (so we can kill it) and
/// the reader thread draining its combined output (joined on stop so it can't
/// linger). Held in the global registry keyed by terminal id.
struct DevProcess {
    child: Child,
    reader: Option<JoinHandle<()>>,
}

impl DevProcess {
    /// Kill the child and join its reader thread. Best-effort: the child may have
    /// already exited on its own (the reader will have hit EOF and be exiting).
    fn stop(mut self) {
        // Killing the child closes its stdout pipe, so the reader thread hits EOF
        // and returns; then we join it so it never outlives the process.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

/// Process-global registry of running dev servers, keyed by terminal id. A single
/// `LazyLock<Mutex<..>>` keeps the module self-contained (no Tauri-managed state).
static REGISTRY: LazyLock<Mutex<HashMap<String, DevProcess>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Recover a POSIX/WSL path from a `\\wsl.localhost\<distro>\...` (or legacy
/// `\\wsl$\<distro>\...`) UNC path, or pass through a path that is already a bare
/// POSIX path. Returns `None` for a genuine Windows drive path (`C:\...`).
///
/// This replicates the minimal logic of `files.rs::unc_to_posix` (which is
/// private to that module) so the dev server can run natively inside WSL at the
/// project's cwd rather than over the slow UNC bridge.
#[cfg(windows)]
fn unc_to_posix(path: &str) -> Option<String> {
    // Already a bare POSIX path: pass through.
    if path.starts_with('/') {
        return Some(path.to_string());
    }
    // Peel a verbatim extended-length prefix first (`\\?\UNC\...` / `\\?\C:\...`).
    let s: std::borrow::Cow<str> = if let Some(rest) = path.strip_prefix("\\\\?\\UNC\\") {
        std::borrow::Cow::Owned(format!("\\\\{rest}"))
    } else if let Some(rest) = path.strip_prefix("\\\\?\\") {
        std::borrow::Cow::Owned(rest.to_string())
    } else {
        std::borrow::Cow::Borrowed(path)
    };
    for prefix in ["\\\\wsl.localhost\\", "\\\\wsl$\\"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            // `rest` is `<distro>\home\natkins\...`; drop the distro segment.
            let tail = match rest.split_once('\\') {
                Some((_distro, tail)) => tail,
                None => "",
            };
            let posix = format!("/{}", tail.replace('\\', "/"));
            return Some(posix);
        }
    }
    None
}

/// Wrap the user's dev command so the server binds to ALL interfaces
/// (`0.0.0.0`) rather than only the WSL loopback (`127.0.0.1`).
///
/// WHY (the core WSL2 preview bug): the dev server runs INSIDE WSL, but the
/// preview (a Windows WebView2 iframe) is a Windows process. With
/// `networkingMode=mirrored` — and, differently, with NAT's localhost relay —
/// a server bound to `127.0.0.1` listens only on WSL's loopback, which is a
/// SEPARATE loopback from Windows'. The Windows-side iframe then can't reach
/// `localhost:<port>` ("refuses to connect even on a host that exists"). A
/// server bound to `0.0.0.0` also listens on the shared/mirrored interface, so
/// the Windows iframe (pointed at the WSL interface IP, see [`preview_host`])
/// can reach it.
///
/// We do this WITHOUT mangling the command string: we `export` the bind-host env
/// vars the common frameworks read BEFORE running the user's command, so e.g.
/// `pnpm dev` runs verbatim afterwards. `HOST` is honoured by CRA, Next, Nuxt,
/// Remix, Astro, Gatsby and many custom servers; the framework-specific aliases
/// cover the rest. A tool that ignores all of them (notably Vite, which binds to
/// `127.0.0.1` unless `--host`/`server.host` is set) still works: the iframe URL
/// is rewritten to a reachable host (see [`preview_host`] + the frontend), which
/// is the safety net. Setting these vars is harmless where ignored.
fn host_binding_prefix() -> &'static str {
    "export HOST=0.0.0.0 HOSTNAME=0.0.0.0 NUXT_HOST=0.0.0.0 ASTRO_HOST=0.0.0.0; "
}

/// Build the OS command that runs `command` inside `cwd`, with stdout+stderr piped
/// so we can drain them line-by-line.
///
/// On Windows we shell into WSL: `wsl.exe -d <distro> --cd <posix-cwd> -e bash -lc
/// '<command>'`, console suppressed (`CREATE_NO_WINDOW`, copying tmux.rs). `--cd`
/// roots the dev server at the project's WSL directory. On unix we run `sh -lc
/// '<command>'` with the cwd set on the `Command` directly. The login shell
/// (`-lc`) ensures the user's PATH (nvm/volta/etc.) is loaded so `npm`/`pnpm`
/// resolve, matching how the rest of T-Hub shells in.
///
/// Both platforms prepend [`host_binding_prefix`] so the server binds to all
/// interfaces (reachable from the Windows-side preview iframe — see that fn).
fn build_command(cwd: &str, command: &str) -> Command {
    let command = format!("{}{command}", host_binding_prefix());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Translate a UNC/bare cwd back to the native WSL POSIX path so `--cd`
        // lands inside the distro's ext4 filesystem; fall back to the given cwd.
        let posix_cwd = unc_to_posix(cwd).unwrap_or_else(|| cwd.to_string());
        let mut c = Command::new("wsl.exe");
        c.arg("-d").arg(crate::files::host_distro());
        if !posix_cwd.is_empty() {
            c.arg("--cd").arg(&posix_cwd);
        }
        // `-e` (exec) runs bash DIRECTLY. A bare `--` re-joins the tail through the
        // user's DEFAULT shell (zsh), which re-splits the quoted command string and
        // re-expands `$`/backticks in it (see the note on tmux.rs::pane_info_command).
        c.arg("-e").arg("bash").arg("-lc").arg(&command);
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW (see tmux.rs / files.rs)
        c.stdout(Stdio::piped());
        c.stderr(Stdio::piped());
        c.stdin(Stdio::null());
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-lc").arg(&command);
        if !cwd.is_empty() {
            c.current_dir(cwd);
        }
        c.stdout(Stdio::piped());
        c.stderr(Stdio::piped());
        c.stdin(Stdio::null());
        c
    }
}

/// Drain a piped reader line-by-line, emitting each line on the dev-server
/// channel for `id`. Used for both stdout and stderr (each on its own thread).
/// Lines are emitted as soon as they complete a newline; partial trailing data at
/// EOF is flushed too. Reads bytes (not `String`) and lossily decodes so a stray
/// non-UTF-8 byte can't kill the stream.
fn pump<R: std::io::Read>(app: &AppHandle, id: &str, reader: R) {
    let ch = channel(id);
    let mut buf = BufReader::new(reader);
    let mut line = Vec::<u8>::new();
    loop {
        line.clear();
        // `read_until('\n')` returns 0 only at EOF; otherwise it includes the
        // newline (if any) in `line`.
        match buf.read_until(b'\n', &mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                // Strip the trailing CR/LF so the frontend gets clean lines.
                while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
                    line.pop();
                }
                let text = String::from_utf8_lossy(&line).into_owned();
                // A bare empty line (just a newline) is still meaningful spacing
                // in dev-server output, so we forward it as-is.
                let _ = app.emit(&ch, DevServerEvent::line(id, text));
            }
            Err(_) => break, // read error: treat as end-of-stream
        }
    }
}

/// Start (or restart) the managed dev server for `terminal_id`.
///
/// Spawns `command` inside `cwd` (WSL on Windows; directly on unix), wires two
/// reader threads (stdout + stderr) that stream lines on `devserver://<id>`, and
/// registers the child so [`stop_dev_server`] can kill it. Any existing dev
/// server for the same id is stopped first (a re-Run replaces it). Emits a
/// `started` lifecycle event immediately, and an `exited` event when the child
/// ends on its own.
#[tauri::command]
pub async fn start_dev_server(
    app: AppHandle,
    terminal_id: String,
    cwd: String,
    command: String,
) -> Result<(), String> {
    let command = command.trim();
    if command.is_empty() {
        return Err("dev command is empty".to_string());
    }

    // Replace any existing dev server for this id (re-Run = restart). Take it out
    // of the registry under the lock, then stop it OUTSIDE the lock (stop joins a
    // thread, which we must not do while holding the registry mutex).
    let existing = REGISTRY.lock().remove(&terminal_id);
    if let Some(proc) = existing {
        proc.stop();
    }

    let mut cmd = build_command(&cwd, command);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start dev server: {e}"))?;

    // Take the piped handles BEFORE moving `child` into the registry. Each is
    // drained on its own thread so stdout and stderr can't deadlock each other.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let app_out = app.clone();
    let id_out = terminal_id.clone();
    let out_handle = stdout.map(|s| {
        std::thread::Builder::new()
            .name(format!("t-hub-devserver-out-{terminal_id}"))
            .spawn(move || pump(&app_out, &id_out, s))
            .expect("spawn devserver stdout reader")
    });

    // stderr is pumped on the SAME channel (combined output): dev servers print
    // their banner/URL to either stream, and the Dev tab shows one merged log.
    let app_err = app.clone();
    let id_err = terminal_id.clone();
    if let Some(s) = stderr {
        std::thread::Builder::new()
            .name(format!("t-hub-devserver-err-{terminal_id}"))
            .spawn(move || pump(&app_err, &id_err, s))
            .ok();
    }

    // Announce the start so the Dev tab flips to "running" immediately.
    let _ = app.emit(
        &channel(&terminal_id),
        DevServerEvent::started(&terminal_id),
    );

    // Register the child + the stdout reader handle (joined on stop). We keep only
    // the stdout handle to join; the stderr thread exits on its own EOF and is
    // detached (joining both would require holding two handles for marginal gain).
    REGISTRY.lock().insert(
        terminal_id.clone(),
        DevProcess {
            child,
            reader: out_handle,
        },
    );

    // A waiter thread reaps the child if it exits ON ITS OWN (crash, or a dev
    // server that runs-then-quits) and emits an `exited` event so the Dev tab can
    // flip back to idle. It only acts if THIS child is still the registered one
    // (a restart/stop already removed+killed it, so we must not double-report).
    let app_wait = app.clone();
    let id_wait = terminal_id.clone();
    std::thread::Builder::new()
        .name(format!("t-hub-devserver-wait-{terminal_id}"))
        .spawn(move || {
            // Poll for natural exit without holding the registry lock across the
            // wait. We can't `child.wait()` here (the registry owns the child), so
            // we periodically try_wait on it under a short lock.
            loop {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let mut reg = REGISTRY.lock();
                let still_ours = match reg.get_mut(&id_wait) {
                    Some(proc) => match proc.child.try_wait() {
                        Ok(Some(status)) => Some(status.code()),
                        Ok(None) => None,     // still running
                        Err(_) => Some(None), // wait failed: treat as gone
                    },
                    None => {
                        // Removed by stop/restart — that path owns the teardown.
                        return;
                    }
                };
                if let Some(code) = still_ours {
                    // The child exited on its own. Drop it from the registry and
                    // emit the lifecycle event (outside any further work).
                    reg.remove(&id_wait);
                    drop(reg);
                    let summary = match code {
                        Some(c) => format!("dev server exited (code {c})"),
                        None => "dev server exited".to_string(),
                    };
                    let _ = app_wait.emit(
                        &channel(&id_wait),
                        DevServerEvent::exited(&id_wait, summary),
                    );
                    return;
                }
            }
        })
        .ok();

    Ok(())
}

/// Stop the managed dev server for `terminal_id` (kill its process + drain).
/// Idempotent: stopping an id with no running server is a no-op success.
#[tauri::command]
pub async fn stop_dev_server(terminal_id: String) -> Result<(), String> {
    // Remove under the lock, stop outside it (stop joins a thread).
    let proc = REGISTRY.lock().remove(&terminal_id);
    if let Some(proc) = proc {
        proc.stop();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Preview reachability (the WSL2 localhost fix, host-resolution half).
//
// A dev server runs INSIDE WSL; the preview iframe is a WINDOWS process. The
// frontend asks the backend, once, for the host it should substitute for a
// detected/typed `localhost`/`127.0.0.1` URL so the iframe actually reaches the
// server. On unix (the WSL dev build, and Linux/macOS native) `localhost` is
// already correct, so we return None and the frontend leaves the URL alone.
// On Windows we return the WSL distro's interface IP (its `eth0` address as
// seen on the shared/mirrored network), which IS reachable from Windows for a
// server bound to `0.0.0.0` (see `host_binding_prefix`).
// ---------------------------------------------------------------------------

/// The WSL distro's primary IPv4 address as seen from the Windows host (the
/// shared interface in mirrored mode; the NAT'd `eth0` otherwise). Queried via
/// `wsl.exe -e bash -lc 'hostname -I'` and trimmed to the first address. `None` if the
/// lookup fails (the frontend then keeps `localhost`, which is still correct in
/// mirrored mode for a `0.0.0.0`-bound server).
#[cfg(windows)]
fn wsl_host_ip() -> Option<String> {
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("wsl.exe");
    // `-e` (exec) runs bash DIRECTLY. A bare `--` re-joins the tail through the
    // default shell, splitting the quoted `hostname -I` script into separate
    // words (see the note on tmux.rs::pane_info_command).
    c.arg("-d")
        .arg(crate::files::host_distro())
        .arg("-e")
        .arg("bash")
        .arg("-lc")
        // `hostname -I` lists this host's addresses (space-separated); the first
        // is the primary interface. `ip route get 1` would also work but this is
        // simpler and matches how the rest of T-Hub probes WSL.
        .arg("hostname -I");
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                                   // Bounded (WSL_PROBE): a trivial `hostname -I`; a cold/wedged WSL must not park
                                   // the `preview_host` handler this runs on.
    let out =
        crate::bounded_exec::output_with_timeout(c, crate::bounded_exec::WSL_PROBE_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.split_whitespace().next()?.trim();
    // Sanity: looks like a dotted IPv4 and isn't loopback (which wouldn't help).
    if first.is_empty() || first.starts_with("127.") || !first.contains('.') {
        return None;
    }
    Some(first.to_string())
}

/// Return the host the preview iframe should use in place of `localhost` /
/// `127.0.0.1` to reach a WSL-bound dev server, or `None` when no rewrite is
/// needed (unix builds, where the WebView and the server share a loopback).
///
/// On Windows this is the WSL interface IP. Cached for the process lifetime —
/// the address is stable for a WSL session and the lookup spawns `wsl.exe`.
#[tauri::command]
pub async fn preview_host() -> Result<Option<String>, String> {
    #[cfg(windows)]
    {
        use std::sync::OnceLock;
        static CACHE: OnceLock<Option<String>> = OnceLock::new();
        Ok(CACHE.get_or_init(wsl_host_ip).clone())
    }
    #[cfg(not(windows))]
    {
        // Linux/macOS (incl. the WSL dev build): the dev server and the WebView
        // are on the same loopback; `localhost` already reaches it.
        Ok(None)
    }
}

/// Core of [`probe_tcp`]: does `host:port` accept a TCP connection within
/// `timeout_ms`? Split out (sync) so the command is a thin wrapper and the unit
/// test can exercise it without an async runtime.
fn tcp_reachable(host: &str, port: u16, timeout_ms: u64) -> Result<bool, String> {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    let host = host.trim();
    if host.is_empty() {
        return Err("empty host".to_string());
    }
    // Resolve the host:port to socket addresses (handles "localhost", IPv4, and
    // IPv6); try each until one connects within the budget.
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve {host}:{port}: {e}"))?;
    let budget = Duration::from_millis(timeout_ms.clamp(50, 10_000));
    for addr in addrs {
        if TcpStream::connect_timeout(&addr, budget).is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Probe whether `host:port` accepts a TCP connection within `timeout_ms`,
/// from the SAME process/host as the WebView (so the result reflects what the
/// preview iframe would see). Lets the frontend tell "connection refused / not
/// up" apart from "up but refused framing", and surface a precise message
/// instead of the silent watchdog "blocked".
///
/// Returns `Ok(true)` if the TCP handshake succeeds, `Ok(false)` if it is
/// refused or times out. A malformed `host`/`port` is an `Err`.
#[tauri::command]
pub async fn probe_tcp(host: String, port: u16, timeout_ms: u64) -> Result<bool, String> {
    tcp_reachable(&host, port, timeout_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_name_is_per_terminal() {
        assert_eq!(channel("abc123"), "devserver://abc123");
    }

    #[test]
    fn event_kinds_are_tagged() {
        assert_eq!(DevServerEvent::started("x").kind, "started");
        assert_eq!(DevServerEvent::line("x", "hi".into()).kind, "line");
        assert_eq!(DevServerEvent::exited("x", "bye".into()).kind, "exited");
        assert_eq!(DevServerEvent::line("x", "hi".into()).line, "hi");
    }

    #[cfg(windows)]
    #[test]
    fn unc_to_posix_recovers_wsl_paths() {
        assert_eq!(
            unc_to_posix("\\\\wsl.localhost\\Ubuntu-24.04\\home\\natkins\\proj"),
            Some("/home/natkins/proj".to_string())
        );
        // Bare POSIX passes through; a real Windows drive path does not map.
        assert_eq!(unc_to_posix("/home/x"), Some("/home/x".to_string()));
        assert_eq!(unc_to_posix("C:\\Users\\natha"), None);
    }

    /// On unix the command runner builds a `sh -lc` invocation; spawning a quick
    /// `echo` and stopping it should round-trip through the registry without
    /// error. (Requires a real `sh`, present in the WSL dev shell.) The host-
    /// binding prefix must not break a plain command.
    #[cfg(not(windows))]
    #[test]
    fn build_command_runs_sh_on_unix() {
        let mut cmd = build_command("/tmp", "echo t-hub-devserver-test");
        let out = cmd.output().expect("sh -lc echo should run");
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("t-hub-devserver-test"), "got: {text:?}");
    }

    /// The host-binding prefix exports HOST=0.0.0.0 (the WSL2 preview fix) and is
    /// a syntactically complete statement that leaves the user's command intact —
    /// running it as `sh -lc` and echoing $HOST must see the override.
    #[cfg(not(windows))]
    #[test]
    fn host_binding_prefix_sets_host_for_the_command() {
        let mut cmd = build_command("/tmp", "printf '%s' \"$HOST\"");
        let out = cmd.output().expect("sh -lc should run");
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            text.trim(),
            "0.0.0.0",
            "HOST should be forced to all-ifaces"
        );
    }

    /// The TCP probe should connect to a port we open and report it refused once
    /// closed. Uses an ephemeral listener so the test is hermetic.
    ///
    /// De-flaked: instead of a single probe per phase (which assumes the OS has
    /// already settled the socket into the expected state), each phase polls
    /// `tcp_reachable` with a deadline until the expected reachability is observed.
    /// The open phase is normally instant; the *closed* phase is the one that can
    /// lag — dropping the listener releases the port asynchronously, so a fresh
    /// probe can momentarily still connect (e.g. to a half-open socket) on a loaded
    /// box. Polling until refused (or a short timeout) removes the fixed-time
    /// assumption while still asserting the same open→closed transition.
    #[test]
    fn tcp_reachable_detects_open_then_closed() {
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        // Poll `tcp_reachable` until it returns `want`, or fail after `deadline`.
        // Each probe carries a tight connect budget so the loop is responsive; the
        // overall deadline (not any single probe) bounds the wait.
        fn poll_until_reachable(host: &str, port: u16, want: bool, deadline: Duration) -> bool {
            let start = Instant::now();
            loop {
                if tcp_reachable(host, port, 50).unwrap() == want {
                    return true;
                }
                if start.elapsed() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let port = listener.local_addr().unwrap().port();

        // Open: a listener is accepting, so the handshake succeeds (effectively
        // immediate, but poll for symmetry / to absorb any scheduling hiccup).
        assert!(
            poll_until_reachable("127.0.0.1", port, true, Duration::from_secs(2)),
            "expected the open port to accept a connection"
        );

        // Closed: drop the listener, then poll until a fresh probe is refused. The
        // refusal may not be observable on the very first probe after drop, so we
        // wait (bounded) for the port to be released rather than assuming a fixed
        // settle time.
        drop(listener);
        assert!(
            poll_until_reachable("127.0.0.1", port, false, Duration::from_secs(2)),
            "expected the closed port to refuse once the listener is released"
        );
    }
}
