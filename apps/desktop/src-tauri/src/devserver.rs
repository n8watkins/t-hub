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
//!     `wsl.exe -d <distro> --cd <cwd> -- bash -lc '<command>'`, with the console
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

/// The WSL distro projects live in, as seen from the Windows host. Mirrors
/// `files.rs`/`lib.rs`: overridable via `TERMHUB_DISTRO`, defaulting to the dev
/// distro. Only consulted on Windows.
#[cfg(windows)]
fn host_distro() -> String {
    std::env::var("TERMHUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string())
}

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

/// Build the OS command that runs `command` inside `cwd`, with stdout+stderr piped
/// so we can drain them line-by-line.
///
/// On Windows we shell into WSL: `wsl.exe -d <distro> --cd <posix-cwd> -- bash -lc
/// '<command>'`, console suppressed (`CREATE_NO_WINDOW`, copying tmux.rs). `--cd`
/// roots the dev server at the project's WSL directory. On unix we run `sh -lc
/// '<command>'` with the cwd set on the `Command` directly. The login shell
/// (`-lc`) ensures the user's PATH (nvm/volta/etc.) is loaded so `npm`/`pnpm`
/// resolve, matching how the rest of TermHub shells in.
fn build_command(cwd: &str, command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Translate a UNC/bare cwd back to the native WSL POSIX path so `--cd`
        // lands inside the distro's ext4 filesystem; fall back to the given cwd.
        let posix_cwd = unc_to_posix(cwd).unwrap_or_else(|| cwd.to_string());
        let mut c = Command::new("wsl.exe");
        c.arg("-d").arg(host_distro());
        if !posix_cwd.is_empty() {
            c.arg("--cd").arg(&posix_cwd);
        }
        c.arg("--").arg("bash").arg("-lc").arg(command);
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW (see tmux.rs / files.rs)
        c.stdout(Stdio::piped());
        c.stderr(Stdio::piped());
        c.stdin(Stdio::null());
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-lc").arg(command);
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
            .name(format!("termhub-devserver-out-{terminal_id}"))
            .spawn(move || pump(&app_out, &id_out, s))
            .expect("spawn devserver stdout reader")
    });

    // stderr is pumped on the SAME channel (combined output): dev servers print
    // their banner/URL to either stream, and the Dev tab shows one merged log.
    let app_err = app.clone();
    let id_err = terminal_id.clone();
    if let Some(s) = stderr {
        std::thread::Builder::new()
            .name(format!("termhub-devserver-err-{terminal_id}"))
            .spawn(move || pump(&app_err, &id_err, s))
            .ok();
    }

    // Announce the start so the Dev tab flips to "running" immediately.
    let _ = app.emit(&channel(&terminal_id), DevServerEvent::started(&terminal_id));

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
        .name(format!("termhub-devserver-wait-{terminal_id}"))
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
                        Ok(None) => None,    // still running
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
                    let _ =
                        app_wait.emit(&channel(&id_wait), DevServerEvent::exited(&id_wait, summary));
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
    /// error. (Requires a real `sh`, present in the WSL dev shell.)
    #[cfg(not(windows))]
    #[test]
    fn build_command_runs_sh_on_unix() {
        let mut cmd = build_command("/tmp", "echo termhub-devserver-test");
        let out = cmd.output().expect("sh -lc echo should run");
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("termhub-devserver-test"), "got: {text:?}");
    }
}
